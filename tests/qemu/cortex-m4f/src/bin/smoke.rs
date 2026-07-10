//! Bounded scheduler, tick, and equal-priority scheduling smoke test.

#![no_std]
#![no_main]
#![allow(static_mut_refs)]

extern crate panic_semihosting;

use core::ffi::c_void;
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use cortex_m_rt::entry;
use freertos_in_rust::kernel::tasks::{
    taskENTER_CRITICAL, taskEXIT_CRITICAL, vTaskDelay, xTaskGetTickCount,
};
use freertos_in_rust::types::{StackType_t, TickType_t};
use freertos_qemu_cortex_m4f::{finish, spawn_static, start_scheduler, tap, AlignedTaskBuffer};

#[cfg(feature = "small-memory")]
const STACK_WORDS: usize = 128;
#[cfg(not(feature = "small-memory"))]
const STACK_WORDS: usize = 256;
const WORKER_PRIORITY: u32 = 2;
const REPORTER_PRIORITY: u32 = 3;
const OBSERVATION_TICKS: TickType_t = 30;
const RECORDED_EVENTS: usize = 8;

static WORKER_A_RUNS: AtomicU32 = AtomicU32::new(0);
static WORKER_B_RUNS: AtomicU32 = AtomicU32::new(0);
static EVENT_COUNT: AtomicUsize = AtomicUsize::new(0);
static EVENT_SEQUENCE: AtomicU32 = AtomicU32::new(0);
static START_TICK: AtomicU32 = AtomicU32::new(0);

static mut WORKER_A_STACK: [StackType_t; STACK_WORDS] = [0; STACK_WORDS];
static mut WORKER_A_TCB: AlignedTaskBuffer = AlignedTaskBuffer::new();
static mut WORKER_B_STACK: [StackType_t; STACK_WORDS] = [0; STACK_WORDS];
static mut WORKER_B_TCB: AlignedTaskBuffer = AlignedTaskBuffer::new();
static mut REPORTER_STACK: [StackType_t; STACK_WORDS] = [0; STACK_WORDS];
static mut REPORTER_TCB: AlignedTaskBuffer = AlignedTaskBuffer::new();

fn record_worker(worker: u32, counter: &AtomicU32) {
    /* ARMv6-M supports atomic loads/stores but not atomic read-modify-write
     * instructions. The scheduler critical section supplies the required
     * single-core exclusion portably across every shared Cortex-M smoke test. */
    taskENTER_CRITICAL();
    counter.store(
        counter.load(Ordering::Relaxed).wrapping_add(1),
        Ordering::Relaxed,
    );

    let event_index = EVENT_COUNT.load(Ordering::Relaxed);
    EVENT_COUNT.store(event_index.wrapping_add(1), Ordering::Relaxed);
    if event_index < RECORDED_EVENTS {
        EVENT_SEQUENCE.store(
            EVENT_SEQUENCE.load(Ordering::Relaxed) | (worker << (event_index * 2)),
            Ordering::Relaxed,
        );
    }
    taskEXIT_CRITICAL();
}

extern "C" fn worker_a(_parameter: *mut c_void) {
    loop {
        record_worker(1, &WORKER_A_RUNS);
        unsafe { vTaskDelay(2) };
    }
}

extern "C" fn worker_b(_parameter: *mut c_void) {
    loop {
        record_worker(2, &WORKER_B_RUNS);
        unsafe { vTaskDelay(2) };
    }
}

fn recorded_workers_alternate(events: usize, sequence: u32) -> bool {
    if events < 6 {
        return false;
    }

    let events_to_check = events.min(RECORDED_EVENTS);
    let mut index = 0;
    let mut previous = 0;

    while index < events_to_check {
        let current = (sequence >> (index * 2)) & 0x3;
        if current == 0 || current == previous {
            return false;
        }
        previous = current;
        index += 1;
    }

    true
}

extern "C" fn reporter(_parameter: *mut c_void) {
    let start_tick = xTaskGetTickCount();
    START_TICK.store(start_tick as u32, Ordering::Relaxed);
    unsafe { vTaskDelay(OBSERVATION_TICKS) };

    let end_tick = xTaskGetTickCount();
    let elapsed = end_tick.wrapping_sub(START_TICK.load(Ordering::Relaxed) as TickType_t);
    let worker_a_runs = WORKER_A_RUNS.load(Ordering::Relaxed);
    let worker_b_runs = WORKER_B_RUNS.load(Ordering::Relaxed);
    let events = EVENT_COUNT.load(Ordering::Relaxed);
    let sequence = EVENT_SEQUENCE.load(Ordering::Relaxed);

    let mut output = tap();
    output.check(
        worker_a_runs >= 6 && worker_b_runs >= 6,
        "scheduler_runs_both_tasks",
    );
    output.check(elapsed >= OBSERVATION_TICKS, "kernel_tick_advances");
    output.check(
        recorded_workers_alternate(events, sequence),
        "equal_priority_tasks_alternate",
    );
    output.diagnostic(format_args!(
        "elapsed={} worker_a={} worker_b={} events={} sequence=0x{:08x}",
        elapsed, worker_a_runs, worker_b_runs, events, sequence
    ));

    finish(output, 3)
}

#[entry]
fn main() -> ! {
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
