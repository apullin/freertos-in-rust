//! Cortex-A9 SP804 tickless-reload and run-time-counter regression.

#![no_std]
#![no_main]
#![allow(static_mut_refs)]

use core::ffi::c_void;
use core::sync::atomic::{AtomicU32, Ordering};

use freertos_in_rust::kernel::tasks::{vTaskDelay, xTaskGetTickCount};
use freertos_in_rust::port::portGET_RUN_TIME_COUNTER_VALUE;
use freertos_in_rust::types::{StackType_t, TickType_t};
use freertos_qemu_cortex_a9::{
    arm_auxiliary_wake, auxiliary_interrupts, finish, init, spawn_static, start_scheduler, tap,
    timer_interrupts, timer_register, AlignedTaskBuffer, TIMER_PERIODIC_RELOAD,
};

const STACK_WORDS: usize = 512;
const SLEEP_TICKS: TickType_t = 250;
const SLEEP_CYCLES: usize = 3;
const REPORT_TICKS: TickType_t = SLEEP_TICKS * SLEEP_CYCLES as TickType_t + 30;
const NORMAL_TICKS: TickType_t = 8;

static SLEEP_ELAPSED: [AtomicU32; SLEEP_CYCLES] =
    [const { AtomicU32::new(u32::MAX) }; SLEEP_CYCLES];
static SLEEP_RUNTIME: AtomicU32 = AtomicU32::new(0);

static mut SLEEPER_STACK: [StackType_t; STACK_WORDS] = [0; STACK_WORDS];
static mut SLEEPER_TCB: AlignedTaskBuffer = AlignedTaskBuffer::new();
static mut REPORTER_STACK: [StackType_t; STACK_WORDS] = [0; STACK_WORDS];
static mut REPORTER_TCB: AlignedTaskBuffer = AlignedTaskBuffer::new();

extern "C" fn sleeper(_parameter: *mut c_void) {
    let runtime_start = portGET_RUN_TIME_COUNTER_VALUE();

    let mut cycle = 0;
    while cycle < SLEEP_CYCLES {
        let start = xTaskGetTickCount();
        // Safety: this callback is the live sleeper task and the scheduler is
        // running and unsuspended.
        unsafe { vTaskDelay(SLEEP_TICKS) };
        SLEEP_ELAPSED[cycle].store(xTaskGetTickCount().wrapping_sub(start), Ordering::Release);
        cycle += 1;
    }

    SLEEP_RUNTIME.store(
        portGET_RUN_TIME_COUNTER_VALUE().wrapping_sub(runtime_start),
        Ordering::Release,
    );

    loop {
        // Safety: this is the live sleeper task in ordinary task context.
        unsafe { vTaskDelay(1000) };
    }
}

extern "C" fn reporter(_parameter: *mut c_void) {
    let report_start = xTaskGetTickCount();
    let runtime_start = portGET_RUN_TIME_COUNTER_VALUE();
    // Wake early once, without a tick interrupt, while the first long SP804
    // interval is active. The idle task must account only the elapsed prefix.
    arm_auxiliary_wake(20_000);
    // Safety: this is the live reporter task with the scheduler running.
    unsafe { vTaskDelay(REPORT_TICKS) };
    let report_elapsed = xTaskGetTickCount().wrapping_sub(report_start);
    let idle_runtime = portGET_RUN_TIME_COUNTER_VALUE().wrapping_sub(runtime_start);
    let suppressed_interrupts = timer_interrupts();
    let auxiliary_wakes = auxiliary_interrupts();

    let load_after_sleep = timer_register(0x00);
    let background_after_sleep = timer_register(0x18);
    let raw_status_after_sleep = timer_register(0x10);

    // Keep a task runnable so the idle task cannot suppress this interval.
    let normal_tick_start = xTaskGetTickCount();
    let normal_irq_start = timer_interrupts();
    let normal_runtime_start = portGET_RUN_TIME_COUNTER_VALUE();
    while xTaskGetTickCount().wrapping_sub(normal_tick_start) < NORMAL_TICKS {
        core::hint::spin_loop();
    }
    let normal_tick_delta = xTaskGetTickCount().wrapping_sub(normal_tick_start);
    let normal_irq_delta = timer_interrupts().wrapping_sub(normal_irq_start);
    let normal_runtime_delta = portGET_RUN_TIME_COUNTER_VALUE().wrapping_sub(normal_runtime_start);

    let delays_ok = SLEEP_ELAPSED.iter().all(|elapsed| {
        let elapsed = elapsed.load(Ordering::Acquire);
        (SLEEP_TICKS..=SLEEP_TICKS + 1).contains(&elapsed)
    });

    let mut output = tap();
    output.check(delays_ok, "repeated_tickless_delays_wake_at_deadlines");
    output.check(
        (REPORT_TICKS..=REPORT_TICKS + 1).contains(&report_elapsed),
        "tickless_elapsed_accounting_preserves_kernel_time",
    );
    output.check(
        suppressed_interrupts > 0 && suppressed_interrupts < 20,
        "long_idle_suppresses_sp804_interrupts",
    );
    output.check(
        auxiliary_wakes == 1,
        "non_tick_interrupt_wakes_long_interval_early",
    );
    output.check(
        load_after_sleep == TIMER_PERIODIC_RELOAD
            && background_after_sleep == TIMER_PERIODIC_RELOAD
            && raw_status_after_sleep == 0,
        "sp804_background_load_restores_periodic_reload",
    );
    output.check(
        idle_runtime > 0
            && SLEEP_RUNTIME.load(Ordering::Acquire) > 0
            && u64::from(idle_runtime) > u64::from(normal_runtime_delta) * 50,
        "mpcore_runtime_counter_advances_through_wfi",
    );
    output.check(
        normal_tick_delta >= NORMAL_TICKS
            && (NORMAL_TICKS..=NORMAL_TICKS + 1).contains(&normal_irq_delta)
            && normal_runtime_delta > 0,
        "periodic_ticks_resume_after_suppressed_intervals",
    );
    output.diagnostic(format_args!(
        "report={report_elapsed} irq={suppressed_interrupts} aux={auxiliary_wakes} load={load_after_sleep} bg={background_after_sleep} ris={raw_status_after_sleep} idle_runtime={idle_runtime} normal_tick={normal_tick_delta} normal_irq={normal_irq_delta} normal_runtime={normal_runtime_delta} delays=[{},{},{}]",
        SLEEP_ELAPSED[0].load(Ordering::Relaxed),
        SLEEP_ELAPSED[1].load(Ordering::Relaxed),
        SLEEP_ELAPSED[2].load(Ordering::Relaxed),
    ));
    finish(output, 7)
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
            2,
            sleeper,
        );
        spawn_static(
            b"report\0",
            REPORTER_STACK.as_mut_ptr(),
            STACK_WORDS,
            core::ptr::addr_of_mut!(REPORTER_TCB),
            3,
            reporter,
        );
    }

    start_scheduler()
}
