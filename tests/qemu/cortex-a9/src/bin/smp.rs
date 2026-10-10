//! Bounded two-core scheduler, exclusion, and migration regression.
#![no_std]
#![no_main]

use core::ffi::c_void;
use core::ptr;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use freertos_in_rust::kernel::tasks::*;
use freertos_in_rust::port::*;
use freertos_in_rust::types::*;
use freertos_qemu_cortex_a9::{finish, init, start_scheduler, start_secondary, tap};

const WORDS: usize = 1024;
static mut STACKS: [[StackType_t; WORDS]; 4] = [[0; WORDS]; 4];
static mut TCBS: [StaticTask_t; 4] = [const { StaticTask_t::new() }; 4];
static STOP: AtomicBool = AtomicBool::new(false);
static FINISHED: AtomicU32 = AtomicU32::new(0);
static RUNS: [AtomicU32; 2] = [const { AtomicU32::new(0) }; 2];
static ERRORS: AtomicU32 = AtomicU32::new(0);
static MIGRATIONS: AtomicU32 = AtomicU32::new(0);
static mut SHARED_COUNT: u32 = 0;

extern "C" fn worker(parameter: *mut c_void) {
    let identity = parameter as usize;
    let mut runs = 0;
    loop {
        if STOP.load(Ordering::Acquire) {
            break;
        }
        taskENTER_CRITICAL();
        // Ordinary (not atomic) memory must be excluded across both CPUs.
        unsafe {
            SHARED_COUNT = SHARED_COUNT.wrapping_add(1);
        }
        if portGET_CORE_ID() as usize != identity {
            ERRORS.fetch_or(1, Ordering::Relaxed);
        }
        taskEXIT_CRITICAL();
        runs += 1;
        portYIELD();
    }
    RUNS[identity].store(runs, Ordering::Release);
    FINISHED.fetch_add(1, Ordering::Release);
    loop {
        unsafe {
            vTaskDelay(1000);
        }
    }
}

extern "C" fn migrator(_: *mut c_void) {
    let mut i = 0;
    while i < 200 {
        let next = 1 - portGET_CORE_ID();
        unsafe {
            vTaskCoreAffinitySet(ptr::null_mut(), 1 << next);
        }
        if portGET_CORE_ID() != next {
            ERRORS.fetch_or(2, Ordering::Relaxed);
        }
        MIGRATIONS.fetch_add(1, Ordering::Release);
        i += 1;
    }
    loop {
        unsafe {
            vTaskDelay(1000);
        }
    }
}

extern "C" fn reporter(_: *mut c_void) {
    unsafe {
        vTaskDelay(300);
    }
    STOP.store(true, Ordering::Release);
    let start = xTaskGetTickCount();
    while FINISHED.load(Ordering::Acquire) < 2 && xTaskGetTickCount().wrapping_sub(start) < 100 {
        unsafe {
            vTaskDelay(1);
        }
    }
    let a = RUNS[0].load(Ordering::Acquire);
    let b = RUNS[1].load(Ordering::Acquire);
    taskENTER_CRITICAL();
    let count = unsafe { SHARED_COUNT };
    taskEXIT_CRITICAL();
    let errors = ERRORS.load(Ordering::Relaxed);
    let migrations = MIGRATIONS.load(Ordering::Acquire);
    let mut out = tap();
    out.check(a > 100 && b > 100, "both_cores_schedule_application_tasks");
    out.check(count == a + b, "critical_sections_exclude_the_other_core");
    out.check(errors & 1 == 0, "pinned_tasks_respect_affinity");
    out.check(
        migrations == 200 && errors & 2 == 0,
        "running_task_migrates_between_cores",
    );
    out.diagnostic(format_args!(
        "runs={a}/{b} count={count} migrations={migrations} errors={errors}"
    ));
    finish(out, 4)
}

#[no_mangle]
pub extern "C" fn main() -> ! {
    init();
    unsafe {
        let entries: [TaskFunction_t; 4] = [worker, worker, migrator, reporter];
        let priorities = [2, 2, 3, 4];
        let masks = [1, 2, 1, 1];
        let mut index = 0;
        while index < 4 {
            let handle = xTaskCreateStatic(
                entries[index],
                b"smp\0".as_ptr(),
                WORDS,
                index as *mut c_void,
                priorities[index],
                ptr::addr_of_mut!(STACKS[index]).cast(),
                ptr::addr_of_mut!(TCBS[index]),
            );
            assert!(!handle.is_null());
            vTaskCoreAffinitySet(handle, masks[index]);
            index += 1;
        }
    }
    start_secondary();
    start_scheduler()
}
