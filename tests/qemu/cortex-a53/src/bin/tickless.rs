//! Long-idle Cortex-A53 generic-timer tick suppression regression.

#![no_std]
#![no_main]
#![allow(static_mut_refs)]

use core::ffi::c_void;
use core::sync::atomic::{AtomicU64, Ordering};

use freertos_in_rust::kernel::tasks::{vTaskDelay, xTaskGetTickCount};
use freertos_in_rust::port::portGET_RUN_TIME_COUNTER_VALUE;
use freertos_in_rust::types::{StackType_t, TickType_t};
use freertos_qemu_cortex_a53::{
    finish, init, spawn_static, start_scheduler, tap, timer_interrupts, AlignedTaskBuffer,
};

const STACK_WORDS: usize = 512;
const SLEEPER_PRIORITY: u64 = 2;
const REPORTER_PRIORITY: u64 = 3;
const SLEEP_TICKS: TickType_t = 1000;
const REPORT_TICKS: TickType_t = 1020;

static SLEEPER_START: AtomicU64 = AtomicU64::new(u64::MAX);
static SLEEPER_END: AtomicU64 = AtomicU64::new(u64::MAX);

static mut SLEEPER_STACK: [StackType_t; STACK_WORDS] = [0; STACK_WORDS];
static mut SLEEPER_TCB: AlignedTaskBuffer = AlignedTaskBuffer::new();
static mut REPORTER_STACK: [StackType_t; STACK_WORDS] = [0; STACK_WORDS];
static mut REPORTER_TCB: AlignedTaskBuffer = AlignedTaskBuffer::new();

extern "C" fn sleeper(_parameter: *mut c_void) {
    let start = xTaskGetTickCount();
    SLEEPER_START.store(start, Ordering::Release);
    // Safety: sleeper is a live task and the scheduler is running.
    unsafe { vTaskDelay(SLEEP_TICKS) };
    SLEEPER_END.store(xTaskGetTickCount(), Ordering::Release);

    loop {
        // Safety: sleeper remains a live task in ordinary task context.
        unsafe { vTaskDelay(1000) };
    }
}

extern "C" fn reporter(_parameter: *mut c_void) {
    let report_start = xTaskGetTickCount();
    let runtime_start = portGET_RUN_TIME_COUNTER_VALUE();
    // Safety: reporter is a live task and the scheduler is running.
    unsafe { vTaskDelay(REPORT_TICKS) };
    let runtime_elapsed = portGET_RUN_TIME_COUNTER_VALUE().wrapping_sub(runtime_start);
    let report_elapsed = xTaskGetTickCount().wrapping_sub(report_start);
    let sleeper_start = SLEEPER_START.load(Ordering::Acquire);
    let sleeper_end = SLEEPER_END.load(Ordering::Acquire);
    let sleeper_elapsed = sleeper_end.wrapping_sub(sleeper_start);
    let interrupts = timer_interrupts();

    let mut output = tap();
    output.check(
        sleeper_start != u64::MAX
            && sleeper_end != u64::MAX
            && (SLEEP_TICKS..=SLEEP_TICKS + 1).contains(&sleeper_elapsed),
        "suppressed_ticks_wake_delayed_task_at_deadline",
    );
    output.check(
        (REPORT_TICKS..=REPORT_TICKS + 1).contains(&report_elapsed),
        "long_idle_preserves_kernel_time",
    );
    output.check(
        interrupts > 0 && interrupts < 20,
        "long_idle_suppresses_periodic_timer_irqs",
    );
    output.check(
        runtime_elapsed > 0,
        "generic_runtime_counter_advances_through_wfi",
    );
    output.diagnostic(format_args!(
        "report_elapsed={report_elapsed} sleeper={sleeper_start}->{sleeper_end}/{sleeper_elapsed} irq={interrupts} runtime={runtime_elapsed}"
    ));
    finish(output, 4)
}

#[no_mangle]
pub extern "C" fn main() -> ! {
    init();

    unsafe {
        spawn_static(
            b"sleeper\0",
            SLEEPER_STACK.as_mut_ptr(),
            STACK_WORDS,
            core::ptr::addr_of_mut!(SLEEPER_TCB),
            SLEEPER_PRIORITY,
            sleeper,
        );
        spawn_static(
            b"report\0",
            REPORTER_STACK.as_mut_ptr(),
            STACK_WORDS,
            core::ptr::addr_of_mut!(REPORTER_TCB),
            REPORTER_PRIORITY,
            reporter,
        );
    }

    start_scheduler()
}
