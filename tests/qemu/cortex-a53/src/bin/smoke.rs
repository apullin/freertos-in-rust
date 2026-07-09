//! Bounded Cortex-A53 scheduler, tick, and two-task smoke test.

#![no_std]
#![no_main]
#![allow(static_mut_refs)]

use core::ffi::c_void;
use core::sync::atomic::{AtomicU64, Ordering};

use freertos_in_rust::kernel::tasks::{vTaskDelay, xTaskGetTickCount};
use freertos_in_rust::types::{StackType_t, TickType_t};
use freertos_qemu_cortex_a53::{
    finish, init, spawn_static, start_scheduler, tap, timer_interrupts, AlignedTaskBuffer,
};

const STACK_WORDS: usize = 512;
const WORKER_PRIORITY: u64 = 2;
const REPORTER_PRIORITY: u64 = 3;
const OBSERVATION_TICKS: TickType_t = 40;

static WORKER_A_RUNS: AtomicU64 = AtomicU64::new(0);
static WORKER_B_RUNS: AtomicU64 = AtomicU64::new(0);

static mut WORKER_A_STACK: [StackType_t; STACK_WORDS] = [0; STACK_WORDS];
static mut WORKER_A_TCB: AlignedTaskBuffer = AlignedTaskBuffer::new();
static mut WORKER_B_STACK: [StackType_t; STACK_WORDS] = [0; STACK_WORDS];
static mut WORKER_B_TCB: AlignedTaskBuffer = AlignedTaskBuffer::new();
static mut REPORTER_STACK: [StackType_t; STACK_WORDS] = [0; STACK_WORDS];
static mut REPORTER_TCB: AlignedTaskBuffer = AlignedTaskBuffer::new();

extern "C" fn worker_a(_parameter: *mut c_void) {
    loop {
        WORKER_A_RUNS.fetch_add(1, Ordering::Relaxed);
        vTaskDelay(2);
    }
}

extern "C" fn worker_b(_parameter: *mut c_void) {
    loop {
        WORKER_B_RUNS.fetch_add(1, Ordering::Relaxed);
        vTaskDelay(2);
    }
}

extern "C" fn reporter(_parameter: *mut c_void) {
    let start = xTaskGetTickCount();
    vTaskDelay(OBSERVATION_TICKS);
    let elapsed = xTaskGetTickCount().wrapping_sub(start);
    let worker_a_runs = WORKER_A_RUNS.load(Ordering::Relaxed);
    let worker_b_runs = WORKER_B_RUNS.load(Ordering::Relaxed);
    let interrupts = timer_interrupts();

    let mut output = tap();
    output.check(
        worker_a_runs >= 10 && worker_b_runs >= 10,
        "scheduler_runs_both_tasks",
    );
    output.check(elapsed >= OBSERVATION_TICKS, "kernel_tick_advances");
    output.check(interrupts > 0, "physical_timer_interrupts_reach_kernel");
    output.diagnostic(format_args!(
        "elapsed={elapsed} worker_a={worker_a_runs} worker_b={worker_b_runs} irq={interrupts}"
    ));
    finish(output, 3)
}

#[no_mangle]
pub extern "C" fn main() -> ! {
    init();

    unsafe {
        spawn_static(
            b"smoke-a\0",
            WORKER_A_STACK.as_mut_ptr(),
            STACK_WORDS,
            core::ptr::addr_of_mut!(WORKER_A_TCB),
            WORKER_PRIORITY,
            worker_a,
        );
        spawn_static(
            b"smoke-b\0",
            WORKER_B_STACK.as_mut_ptr(),
            STACK_WORDS,
            core::ptr::addr_of_mut!(WORKER_B_TCB),
            WORKER_PRIORITY,
            worker_b,
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
