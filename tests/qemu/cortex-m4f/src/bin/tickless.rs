//! Cortex-M SysTick tickless-idle accounting and interrupt suppression test.

#![no_std]
#![no_main]
#![allow(static_mut_refs)]

extern crate panic_semihosting;

use core::ffi::c_void;
use core::sync::atomic::{AtomicU32, Ordering};

use cortex_m_rt::entry;
use freertos_in_rust::kernel::tasks::xTaskGetTickCount;
use freertos_in_rust::port::portGET_RUN_TIME_COUNTER_VALUE;
use freertos_in_rust::sync::TaskContext;
use freertos_in_rust::types::{StackType_t, TickType_t, UBaseType_t};
use freertos_qemu_cortex_m4f::{finish, spawn_static, start_scheduler, tap, AlignedTaskBuffer};

#[cfg(not(feature = "tickless"))]
compile_error!("the tickless suite must be built with --features tickless");

const STACK_WORDS: usize = 192;
const SLEEPER_PRIORITY: UBaseType_t = 2;
const REPORTER_PRIORITY: UBaseType_t = 3;
const SLEEP_TICKS: TickType_t = 180;
const REPORT_TICKS: TickType_t = 200;

static SLEEPER_START: AtomicU32 = AtomicU32::new(u32::MAX);
static SLEEPER_END: AtomicU32 = AtomicU32::new(u32::MAX);

static mut SLEEPER_STACK: [StackType_t; STACK_WORDS] = [0; STACK_WORDS];
static mut SLEEPER_TCB: AlignedTaskBuffer = AlignedTaskBuffer::new();
static mut REPORTER_STACK: [StackType_t; STACK_WORDS] = [0; STACK_WORDS];
static mut REPORTER_TCB: AlignedTaskBuffer = AlignedTaskBuffer::new();

fn tick_count() -> TickType_t {
    xTaskGetTickCount()
}

extern "C" fn sleeper(_parameter: *mut c_void) {
    // Safety: a FreeRTOS task entry runs in task context for its entire call.
    let context = unsafe { TaskContext::assume() };
    let start = tick_count();
    SLEEPER_START.store(start, Ordering::Release);
    freertos_in_rust::delay(&context, SLEEP_TICKS);
    SLEEPER_END.store(tick_count(), Ordering::Release);

    loop {
        freertos_in_rust::delay(&context, 1000);
    }
}

extern "C" fn reporter(_parameter: *mut c_void) {
    // Safety: a FreeRTOS task entry runs in task context for its entire call.
    let context = unsafe { TaskContext::assume() };
    let report_start = tick_count();
    let systick_start = portGET_RUN_TIME_COUNTER_VALUE();
    freertos_in_rust::delay(&context, REPORT_TICKS);
    let systick_interrupts = portGET_RUN_TIME_COUNTER_VALUE().wrapping_sub(systick_start);
    let report_elapsed = tick_count().wrapping_sub(report_start);
    let sleeper_start = SLEEPER_START.load(Ordering::Acquire);
    let sleeper_end = SLEEPER_END.load(Ordering::Acquire);
    let sleeper_elapsed = sleeper_end.wrapping_sub(sleeper_start);

    let mut output = tap();
    output.check(
        (REPORT_TICKS..=REPORT_TICKS + 2).contains(&report_elapsed),
        "suppressed_systick_periods_advance_kernel_time",
    );
    output.check(
        sleeper_start != u32::MAX
            && sleeper_end != u32::MAX
            && (SLEEP_TICKS..=SLEEP_TICKS + 2).contains(&sleeper_elapsed),
        "delayed_task_wakes_at_tickless_deadline",
    );
    output.check(
        systick_interrupts > 0 && systick_interrupts < 20,
        "long_idle_uses_far_fewer_systick_irqs_than_ticks",
    );
    output.diagnostic(format_args!(
        "report={}->{} elapsed={} sleeper={}->{} elapsed={} systick_irqs={}",
        report_start,
        report_start.wrapping_add(report_elapsed),
        report_elapsed,
        sleeper_start,
        sleeper_end,
        sleeper_elapsed,
        systick_interrupts,
    ));

    finish(output, 3)
}

#[entry]
fn main() -> ! {
    unsafe {
        spawn_static(
            b"tickless-sleeper\0",
            SLEEPER_STACK.as_mut_ptr(),
            STACK_WORDS,
            core::ptr::addr_of_mut!(SLEEPER_TCB),
            SLEEPER_PRIORITY,
            sleeper,
        );
        spawn_static(
            b"tickless-report\0",
            REPORTER_STACK.as_mut_ptr(),
            STACK_WORDS,
            core::ptr::addr_of_mut!(REPORTER_TCB),
            REPORTER_PRIORITY,
            reporter,
        );
    }

    start_scheduler()
}
