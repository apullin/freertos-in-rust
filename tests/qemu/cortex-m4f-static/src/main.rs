//! Allocator-free scheduler, idle-task, queue, and timer-daemon qualification.

#![no_std]
#![no_main]
#![allow(non_snake_case, static_mut_refs)]

extern crate panic_semihosting;

use core::ffi::c_void;
use core::fmt;
use core::sync::atomic::{AtomicU32, Ordering};

use cortex_m_rt::entry;
use cortex_m_semihosting::{debug, hprint};
use freertos_in_rust::config::{configSUPPORT_DYNAMIC_ALLOCATION, configTIMER_TASK_STACK_DEPTH};
use freertos_in_rust::kernel::queue::StaticQueue_t;
use freertos_in_rust::kernel::tasks::{uxTaskGetNumberOfTasks, xTaskGetTickCount, StaticTask_t};
use freertos_in_rust::kernel::timers::{vTimerSetTimerTaskMemory, StaticTimer_t};
use freertos_in_rust::port::{vPortSVCHandler, xPortPendSVHandler, xPortSysTickHandler};
use freertos_in_rust::sync::{Queue, TaskContext, TaskHandle, Timer};
use freertos_in_rust::types::{pdPASS, StackType_t, TickType_t, TimerHandle_t, UBaseType_t};
use freertos_qemu_test_common::Tap;

const _: () = assert!(configSUPPORT_DYNAMIC_ALLOCATION == 0);

const STACK_WORDS: usize = 192;
const WORKER_PRIORITY: UBaseType_t = 2;
const REPORTER_PRIORITY: UBaseType_t = 4;
const REPORT_TICKS: TickType_t = 24;
const QUEUED_VALUE: u32 = 0x57a7_1c00;

const CONTEXT_SETUP: u32 = 1 << 0;
const QUEUE_SETUP: u32 = 1 << 1;
const TIMER_DAEMON_SETUP: u32 = 1 << 2;
const TASK_SETUP: u32 = 1 << 3;

#[used]
static HANDLERS: [unsafe extern "C" fn(); 3] =
    [vPortSVCHandler, xPortPendSVHandler, xPortSysTickHandler];

static SETUP_RESULTS: AtomicU32 = AtomicU32::new(0);
static WORKER_RAN: AtomicU32 = AtomicU32::new(0);
static QUEUE_SEND_RESULT: AtomicU32 = AtomicU32::new(0);
static TIMER_CALLBACKS: AtomicU32 = AtomicU32::new(0);

static mut QUEUE_STORAGE: [u32; 2] = [0; 2];
static mut QUEUE_CONTROL: StaticQueue_t = StaticQueue_t::new();
static mut TEST_QUEUE: Option<Queue<u32>> = None;

static mut TIMER_CONTROL: StaticTimer_t = StaticTimer_t::new();
static mut TIMER_TASK_TCB: StaticTask_t = StaticTask_t::new();
static mut TIMER_TASK_STACK: [StackType_t; configTIMER_TASK_STACK_DEPTH] =
    [0; configTIMER_TASK_STACK_DEPTH];

static mut WORKER_STACK: [StackType_t; STACK_WORDS] = [0; STACK_WORDS];
static mut WORKER_TCB: StaticTask_t = StaticTask_t::new();
static mut REPORTER_STACK: [StackType_t; STACK_WORDS] = [0; STACK_WORDS];
static mut REPORTER_TCB: StaticTask_t = StaticTask_t::new();

struct SemihostingOutput;

impl fmt::Write for SemihostingOutput {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        hprint!("{}", value);
        Ok(())
    }
}

fn tap() -> Tap<SemihostingOutput> {
    Tap::new(SemihostingOutput)
}

fn finish(mut tap: Tap<SemihostingOutput>, planned_tests: usize) -> ! {
    let passed = tap.finish(planned_tests);
    debug::exit(if passed {
        debug::EXIT_SUCCESS
    } else {
        debug::EXIT_FAILURE
    });

    loop {
        cortex_m::asm::wfi();
    }
}

fn record(bit: u32, passed: bool) {
    if passed {
        SETUP_RESULTS.fetch_or(bit, Ordering::Relaxed);
    }
}

extern "C" fn timer_callback(_timer: TimerHandle_t) {
    TIMER_CALLBACKS.fetch_add(1, Ordering::Release);
}

extern "C" fn worker(_parameter: *mut c_void) {
    // Safety: a FreeRTOS task entry runs in task context for its entire call.
    let context = unsafe { TaskContext::assume() };
    let context_ok = context
        .current()
        .map(|current| current.priority() == WORKER_PRIORITY)
        .unwrap_or(false);
    let queue_sent = unsafe {
        TEST_QUEUE
            .as_ref()
            .map(|queue| queue.try_send(&context, &QUEUED_VALUE))
            .unwrap_or(false)
    };

    WORKER_RAN.store(context_ok as u32, Ordering::Release);
    QUEUE_SEND_RESULT.store(queue_sent as u32, Ordering::Release);

    loop {
        freertos_in_rust::delay(&context, 1000);
    }
}

extern "C" fn reporter(_parameter: *mut c_void) {
    // Safety: a FreeRTOS task entry runs in task context for its entire call.
    let context = unsafe { TaskContext::assume() };
    let runtime_context = context
        .current()
        .map(|current| current.priority() == REPORTER_PRIORITY)
        .unwrap_or(false);
    let start = xTaskGetTickCount();
    freertos_in_rust::delay(&context, REPORT_TICKS);
    let elapsed = xTaskGetTickCount().wrapping_sub(start);

    let queue_value = unsafe {
        TEST_QUEUE
            .as_ref()
            .and_then(|queue| queue.try_receive(&context))
    };
    let setup = SETUP_RESULTS.load(Ordering::Acquire);
    let daemon_running = Timer::daemon_task_handle(&context).is_some();
    let task_count = uxTaskGetNumberOfTasks();

    let mut output = tap();
    output.check(
        configSUPPORT_DYNAMIC_ALLOCATION == 0,
        "kernel_was_built_without_dynamic_allocation",
    );
    output.check(
        setup & CONTEXT_SETUP != 0 && runtime_context,
        "task_context_changes_from_setup_to_running_task",
    );
    output.check(
        elapsed >= REPORT_TICKS,
        "allocator_free_scheduler_and_tick_run",
    );
    output.check(
        setup & TASK_SETUP != 0 && WORKER_RAN.load(Ordering::Acquire) != 0,
        "safe_static_tasks_are_scheduled",
    );
    output.check(
        setup & QUEUE_SETUP != 0
            && QUEUE_SEND_RESULT.load(Ordering::Acquire) != 0
            && queue_value == Some(QUEUED_VALUE),
        "safe_static_queue_transfers_between_tasks",
    );
    output.check(
        setup & TIMER_DAEMON_SETUP != 0 && daemon_running,
        "registered_static_timer_daemon_runs",
    );
    output.check(
        TIMER_CALLBACKS.load(Ordering::Acquire) == 1,
        "safe_static_one_shot_timer_fires_once",
    );
    output.check(
        daemon_running && task_count == 4,
        "scheduler_created_static_idle_task",
    );
    output.diagnostic(format_args!(
        "setup=0x{:x} elapsed={} worker={} queue={}/{:?} timer={} daemon={} tasks={}",
        setup,
        elapsed,
        WORKER_RAN.load(Ordering::Relaxed),
        QUEUE_SEND_RESULT.load(Ordering::Relaxed),
        queue_value,
        TIMER_CALLBACKS.load(Ordering::Relaxed),
        daemon_running,
        task_count,
    ));

    finish(output, 8)
}

#[entry]
fn main() -> ! {
    // Safety: startup is single-threaded and has exclusive kernel access.
    let context = unsafe { TaskContext::assume() };
    record(CONTEXT_SETUP, context.current().is_none());

    unsafe {
        TEST_QUEUE = Queue::new_static(&context, &mut QUEUE_STORAGE, &mut QUEUE_CONTROL);
    }
    let queue_setup = unsafe {
        TEST_QUEUE
            .as_ref()
            .map(|queue| {
                queue.is_empty(&context)
                    && queue.try_send(&context, &0x1234)
                    && queue.try_receive(&context) == Some(0x1234)
                    && queue.is_empty(&context)
            })
            .unwrap_or(false)
    };
    record(QUEUE_SETUP, queue_setup);

    let timer_daemon_registered = unsafe {
        vTimerSetTimerTaskMemory(
            &mut TIMER_TASK_TCB,
            TIMER_TASK_STACK.as_mut_ptr(),
            TIMER_TASK_STACK.len(),
        ) == pdPASS
    };
    record(TIMER_DAEMON_SETUP, timer_daemon_registered);

    let timer = unsafe {
        Timer::new_oneshot_static(
            &context,
            c"static-only",
            6,
            timer_callback,
            &mut TIMER_CONTROL,
        )
    }
    .expect("static timer creation failed");
    assert_eq!(timer.period(&context), 6);
    assert!(timer.start(&context));

    let worker_task = unsafe {
        TaskHandle::spawn_static(
            &context,
            b"static-worker",
            &mut WORKER_STACK,
            &mut WORKER_TCB,
            WORKER_PRIORITY,
            worker,
        )
    }
    .expect("static worker creation failed");
    let reporter_task = unsafe {
        TaskHandle::spawn_static(
            &context,
            b"static-reporter",
            &mut REPORTER_STACK,
            &mut REPORTER_TCB,
            REPORTER_PRIORITY,
            reporter,
        )
    }
    .expect("static reporter creation failed");
    record(
        TASK_SETUP,
        worker_task.borrow().priority(&context) == WORKER_PRIORITY
            && reporter_task.borrow().priority(&context) == REPORTER_PRIORITY
            && uxTaskGetNumberOfTasks() == 2,
    );

    // Relinquish deletion ownership while the static kernel objects remain
    // live for the scheduler's lifetime.
    let _ = timer.into_raw();
    let _ = worker_task.into_raw();
    let _ = reporter_task.into_raw();

    freertos_in_rust::start_scheduler(context)
}
