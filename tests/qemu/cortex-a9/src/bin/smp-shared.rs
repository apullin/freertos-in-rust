//! Static-only SMP without affinity: shared ready lists and tick preemption.
#![no_std]
#![no_main]

use core::ffi::c_void;
use core::ptr;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use freertos_in_rust::kernel::tasks::*;
use freertos_in_rust::port::*;
use freertos_in_rust::types::*;
use freertos_qemu_cortex_a9::{finish, init, start_scheduler, start_secondary, tap};

const WORKERS: usize = 6;
const WORDS: usize = 768;
static mut STACKS: [[StackType_t; WORDS]; WORKERS + 1] = [[0; WORDS]; WORKERS + 1];
static mut TCBS: [StaticTask_t; WORKERS + 1] = [const { StaticTask_t::new() }; WORKERS + 1];
static RUNS: [AtomicU32; WORKERS] = [const { AtomicU32::new(0) }; WORKERS];
static CORES: AtomicU32 = AtomicU32::new(0);
static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn worker(parameter: *mut c_void) {
    let identity = parameter as usize;
    // No explicit yield: the tick owner must send reschedule IPIs to core 1
    // as well as rotating its own equal-priority ready tasks.
    while !STOP.load(Ordering::Acquire) {
        RUNS[identity].fetch_add(1, Ordering::Relaxed);
        CORES.fetch_or(1 << portGET_CORE_ID(), Ordering::Relaxed);
    }
    loop {
        unsafe {
            vTaskDelay(1000);
        }
    }
}

extern "C" fn reporter(_: *mut c_void) {
    unsafe {
        vTaskDelay(100);
        taskENTER_CRITICAL();
        let a = xTaskGetCurrentTaskHandleForCore(0);
        let b = xTaskGetCurrentTaskHandleForCore(1);
        let distinct = !a.is_null() && !b.is_null() && a != b;
        taskEXIT_CRITICAL();
        STOP.store(true, Ordering::Release);
        vTaskDelay(5);
        let mut every_worker = true;
        let mut i = 0;
        while i < WORKERS {
            every_worker &= RUNS[i].load(Ordering::Relaxed) > 100;
            i += 1;
        }
        let mut out = tap();
        out.check(
            CORES.load(Ordering::Relaxed) == 3,
            "static_only_tasks_execute_on_both_cores_without_affinity",
        );
        out.check(
            every_worker,
            "tick_preemption_rotates_all_shared_ready_tasks",
        );
        out.check(distinct, "cores_never_select_the_same_running_task");
        out.check(
            uxTaskGetNumberOfTasks() == (WORKERS + 3) as u32,
            "static_only_scheduler_creates_two_idle_tasks",
        );
        finish(out, 4)
    }
}

#[no_mangle]
pub extern "C" fn main() -> ! {
    init();
    unsafe {
        let mut i = 0;
        while i <= WORKERS {
            let (entry, priority): (TaskFunction_t, UBaseType_t) = if i == WORKERS {
                (reporter, 4)
            } else {
                (worker, 2)
            };
            assert!(!xTaskCreateStatic(
                entry,
                b"smp-shared\0".as_ptr(),
                WORDS,
                i as *mut c_void,
                priority,
                ptr::addr_of_mut!(STACKS[i]).cast(),
                ptr::addr_of_mut!(TCBS[i])
            )
            .is_null());
            i += 1;
        }
    }
    start_secondary();
    start_scheduler()
}
