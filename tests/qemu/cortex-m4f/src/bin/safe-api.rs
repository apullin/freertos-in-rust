//! Bounded end-to-end coverage for the sound task-context veneer.

#![no_std]
#![no_main]
#![allow(static_mut_refs)]

extern crate panic_semihosting;

use core::ffi::c_void;
use core::sync::atomic::{AtomicU32, Ordering};

use cortex_m_rt::entry;
use freertos_in_rust::kernel::event_groups::StaticEventGroup_t;
use freertos_in_rust::kernel::queue::StaticQueue_t;
use freertos_in_rust::kernel::stream_buffer::StaticStreamBuffer_t;
use freertos_in_rust::kernel::tasks::StaticTask_t;
use freertos_in_rust::kernel::timers::StaticTimer_t;
use freertos_in_rust::sync::{
    BinarySemaphore, CountingSemaphore, EventGroup, MessageBuffer, Mutex, Queue, StreamBuffer,
    TaskContext, TaskHandle, Timer,
};
use freertos_in_rust::types::{StackType_t, TickType_t, TimerHandle_t, UBaseType_t};
use freertos_qemu_cortex_m4f::{finish, start_scheduler, tap};

const STACK_WORDS: usize = 192;
const WORKER_PRIORITY: UBaseType_t = 2;
const REPORTER_PRIORITY: UBaseType_t = 4;
const REPORT_TICKS: TickType_t = 30;

const CONTEXT_SETUP: u32 = 1 << 0;
const QUEUE_SETUP: u32 = 1 << 1;
const SEMAPHORE_SETUP: u32 = 1 << 2;
const MUTEX_SETUP: u32 = 1 << 3;
const EVENT_SETUP: u32 = 1 << 4;
const STREAM_SETUP: u32 = 1 << 5;
const MESSAGE_SETUP: u32 = 1 << 6;
const TIMER_SETUP: u32 = 1 << 7;
const TASK_HANDLE_SETUP: u32 = 1 << 8;

static PRE_SCHEDULER_RESULTS: AtomicU32 = AtomicU32::new(0);
static DYNAMIC_WORKER_CONTEXT: AtomicU32 = AtomicU32::new(0);
static STATIC_WORKER_CONTEXT: AtomicU32 = AtomicU32::new(0);
static DYNAMIC_TIMER_CALLBACKS: AtomicU32 = AtomicU32::new(0);
static STATIC_TIMER_CALLBACKS: AtomicU32 = AtomicU32::new(0);
static TIMER_COOKIE: AtomicU32 = AtomicU32::new(0x51afe);

static mut QUEUE_STORAGE: [u32; 2] = [0; 2];
static mut QUEUE_CONTROL: StaticQueue_t = StaticQueue_t::new();
static mut BINARY_SEMAPHORE_CONTROL: StaticQueue_t = StaticQueue_t::new();
static mut COUNTING_SEMAPHORE_CONTROL: StaticQueue_t = StaticQueue_t::new();
static mut MUTEX_CONTROL: StaticQueue_t = StaticQueue_t::new();
static mut EVENT_CONTROL: StaticEventGroup_t = StaticEventGroup_t::new();
static mut STREAM_STORAGE: [u8; 17] = [0; 17];
static mut STREAM_CONTROL: StaticStreamBuffer_t = StaticStreamBuffer_t::new();
static mut MESSAGE_STORAGE: [u8; 33] = [0; 33];
static mut MESSAGE_CONTROL: StaticStreamBuffer_t = StaticStreamBuffer_t::new();
static mut TIMER_CONTROL: StaticTimer_t = StaticTimer_t::new();

static mut STATIC_WORKER_STACK: [StackType_t; STACK_WORDS] = [0; STACK_WORDS];
static mut STATIC_WORKER_TCB: StaticTask_t = StaticTask_t::new();
static mut REPORTER_STACK: [StackType_t; STACK_WORDS] = [0; STACK_WORDS];
static mut REPORTER_TCB: StaticTask_t = StaticTask_t::new();

fn record(bit: u32, passed: bool) {
    if passed {
        PRE_SCHEDULER_RESULTS.fetch_or(bit, Ordering::Relaxed);
    }
}

fn exercise_queues(context: &TaskContext) -> bool {
    let dynamic = match Queue::<u32>::new(context, 2) {
        Some(queue) => queue,
        None => return false,
    };
    let dynamic_ok = dynamic.is_empty(context)
        && dynamic.try_send(context, &0x1234_5678)
        && dynamic.len(context) == 1
        && dynamic.try_peek(context) == Some(0x1234_5678)
        && dynamic.try_receive(context) == Some(0x1234_5678)
        && dynamic.is_empty(context);

    let static_queue = match unsafe {
        Queue::<u32>::new_static(context, &mut QUEUE_STORAGE, &mut QUEUE_CONTROL)
    } {
        Some(queue) => queue,
        None => return false,
    };
    let static_ok = static_queue.try_send(context, &0xa5a5_5a5a)
        && static_queue.try_peek(context) == Some(0xa5a5_5a5a)
        && static_queue.try_receive(context) == Some(0xa5a5_5a5a)
        && static_queue.is_empty(context);

    dynamic_ok && static_ok
}

fn exercise_semaphores(context: &TaskContext) -> bool {
    let dynamic_binary = match BinarySemaphore::new(context) {
        Some(semaphore) => semaphore,
        None => return false,
    };
    let dynamic_binary_ok = !dynamic_binary.is_available(context)
        && dynamic_binary.give(context)
        && dynamic_binary.is_available(context)
        && dynamic_binary.try_take(context)
        && !dynamic_binary.is_available(context);

    let static_binary =
        match unsafe { BinarySemaphore::new_static(context, &mut BINARY_SEMAPHORE_CONTROL) } {
            Some(semaphore) => semaphore,
            None => return false,
        };
    let static_binary_ok = static_binary.give(context)
        && static_binary.try_take(context)
        && !static_binary.try_take(context);

    let dynamic_counting = match CountingSemaphore::new(context, 3, 2) {
        Some(semaphore) => semaphore,
        None => return false,
    };
    let dynamic_counting_ok = dynamic_counting.try_take(context)
        && dynamic_counting.try_take(context)
        && !dynamic_counting.try_take(context)
        && dynamic_counting.give(context)
        && dynamic_counting.try_take(context);

    let static_counting = match unsafe {
        CountingSemaphore::new_static(context, 3, 1, &mut COUNTING_SEMAPHORE_CONTROL)
    } {
        Some(semaphore) => semaphore,
        None => return false,
    };
    let static_counting_ok = static_counting.try_take(context)
        && !static_counting.try_take(context)
        && static_counting.give(context)
        && static_counting.try_take(context);

    dynamic_binary_ok && static_binary_ok && dynamic_counting_ok && static_counting_ok
}

fn exercise_mutexes(context: &TaskContext) -> bool {
    let dynamic = match Mutex::new(context, 7_u32) {
        Some(mutex) => mutex,
        None => return false,
    };
    let dynamic_first = match dynamic.try_lock(context) {
        Some(mut guard) => {
            *guard = 11;
            true
        }
        None => false,
    };
    let dynamic_value = match dynamic.try_lock(context) {
        Some(guard) => *guard,
        None => 0,
    };

    let static_mutex = match unsafe { Mutex::new_static(context, 13_u32, &mut MUTEX_CONTROL) } {
        Some(mutex) => mutex,
        None => return false,
    };
    let static_first = match static_mutex.try_lock(context) {
        Some(mut guard) => {
            *guard = 17;
            true
        }
        None => false,
    };
    let static_value = match static_mutex.try_lock(context) {
        Some(guard) => *guard,
        None => 0,
    };

    dynamic_first && dynamic_value == 11 && static_first && static_value == 17
}

fn exercise_events(context: &TaskContext) -> bool {
    const DYNAMIC_BITS: u32 = 0x3;
    const STATIC_BITS: u32 = 0xc;

    let dynamic = match EventGroup::new(context) {
        Some(events) => events,
        None => return false,
    };
    let dynamic_set = dynamic.set(context, DYNAMIC_BITS);
    let dynamic_wait = dynamic.try_wait_all(context, DYNAMIC_BITS);
    let dynamic_before_clear = dynamic.clear(context, DYNAMIC_BITS);
    let dynamic_ok = dynamic_set & DYNAMIC_BITS == DYNAMIC_BITS
        && dynamic_wait.is_some()
        && dynamic_before_clear & DYNAMIC_BITS == DYNAMIC_BITS
        && dynamic.get(context) & DYNAMIC_BITS == 0;

    let static_events = match unsafe { EventGroup::new_static(context, &mut EVENT_CONTROL) } {
        Some(events) => events,
        None => return false,
    };
    let static_set = static_events.set(context, STATIC_BITS);
    let static_wait = static_events.try_wait_any(context, STATIC_BITS);
    let static_before_clear = static_events.clear(context, STATIC_BITS);
    let static_ok = static_set & STATIC_BITS == STATIC_BITS
        && static_wait.is_some()
        && static_before_clear & STATIC_BITS == STATIC_BITS
        && static_events.get(context) & STATIC_BITS == 0;

    dynamic_ok && static_ok
}

fn exercise_streams(context: &TaskContext) -> bool {
    let mut dynamic = match StreamBuffer::new(context, 16, 1) {
        Some(stream) => stream,
        None => return false,
    };
    let dynamic_ok = {
        let (mut sender, mut receiver) = dynamic.split();
        let mut output = [0_u8; 8];
        sender.try_send(context, b"dynamic") == 7
            && receiver.try_receive(context, &mut output) == 7
            && &output[..7] == b"dynamic"
    };

    let mut static_stream = match unsafe {
        StreamBuffer::new_static(context, &mut STREAM_STORAGE, 1, &mut STREAM_CONTROL)
    } {
        Some(stream) => stream,
        None => return false,
    };
    let static_ok = {
        let (mut sender, mut receiver) = static_stream.split();
        let mut output = [0_u8; 8];
        sender.try_send(context, b"static") == 6
            && receiver.try_receive(context, &mut output) == 6
            && &output[..6] == b"static"
    };

    dynamic_ok && static_ok
}

fn exercise_messages(context: &TaskContext) -> bool {
    let mut dynamic = match MessageBuffer::new(context, 32) {
        Some(messages) => messages,
        None => return false,
    };
    let dynamic_ok = {
        let (mut sender, mut receiver) = dynamic.split();
        let mut output = [0_u8; 16];
        sender.try_send(context, b"dynamic-msg")
            && receiver.try_receive(context, &mut output) == 11
            && &output[..11] == b"dynamic-msg"
    };

    let mut static_messages = match unsafe {
        MessageBuffer::new_static(context, &mut MESSAGE_STORAGE, &mut MESSAGE_CONTROL)
    } {
        Some(messages) => messages,
        None => return false,
    };
    let static_ok = {
        let (mut sender, mut receiver) = static_messages.split();
        let mut output = [0_u8; 16];
        sender.try_send(context, b"static-msg")
            && receiver.try_receive(context, &mut output) == 10
            && &output[..10] == b"static-msg"
    };

    dynamic_ok && static_ok
}

extern "C" fn dynamic_timer_callback(_timer: TimerHandle_t) {
    DYNAMIC_TIMER_CALLBACKS.fetch_add(1, Ordering::Relaxed);
}

extern "C" fn static_timer_callback(_timer: TimerHandle_t) {
    STATIC_TIMER_CALLBACKS.fetch_add(1, Ordering::Relaxed);
}

extern "C" fn dynamic_worker(_parameter: *mut c_void) {
    // Safety: a FreeRTOS task entry runs in task context for its entire call.
    let context = unsafe { TaskContext::assume() };
    let valid = context
        .current()
        .map(|current| current.priority() == WORKER_PRIORITY)
        .unwrap_or(false);
    DYNAMIC_WORKER_CONTEXT.store(valid as u32, Ordering::Release);

    loop {
        freertos_in_rust::delay(&context, 1000);
    }
}

extern "C" fn static_worker(_parameter: *mut c_void) {
    // Safety: a FreeRTOS task entry runs in task context for its entire call.
    let context = unsafe { TaskContext::assume() };
    let valid = context
        .current()
        .map(|current| current.priority() == WORKER_PRIORITY)
        .unwrap_or(false);
    STATIC_WORKER_CONTEXT.store(valid as u32, Ordering::Release);

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

    freertos_in_rust::delay(&context, REPORT_TICKS);

    let setup = PRE_SCHEDULER_RESULTS.load(Ordering::Acquire);
    let mut output = tap();
    output.check(
        setup & CONTEXT_SETUP != 0 && runtime_context,
        "task_context_distinguishes_setup_and_running_task",
    );
    output.check(
        setup & QUEUE_SETUP != 0,
        "safe_dynamic_and_static_queues_round_trip",
    );
    output.check(
        setup & SEMAPHORE_SETUP != 0,
        "safe_dynamic_and_static_semaphores_round_trip",
    );
    output.check(
        setup & MUTEX_SETUP != 0,
        "safe_dynamic_and_static_mutexes_guard_data",
    );
    output.check(
        setup & EVENT_SETUP != 0,
        "safe_dynamic_and_static_event_groups_manage_bits",
    );
    output.check(
        setup & STREAM_SETUP != 0,
        "safe_dynamic_and_static_stream_endpoints_transfer_bytes",
    );
    output.check(
        setup & MESSAGE_SETUP != 0,
        "safe_dynamic_and_static_message_endpoints_preserve_messages",
    );
    output.check(
        setup & TIMER_SETUP != 0,
        "safe_dynamic_and_static_timers_accept_commands",
    );
    output.check(
        DYNAMIC_TIMER_CALLBACKS.load(Ordering::Acquire) >= 2
            && STATIC_TIMER_CALLBACKS.load(Ordering::Acquire) == 1
            && Timer::daemon_task_handle(&context).is_some(),
        "safe_timer_callbacks_run_in_daemon_task",
    );
    output.check(
        setup & TASK_HANDLE_SETUP != 0
            && DYNAMIC_WORKER_CONTEXT.load(Ordering::Acquire) != 0
            && STATIC_WORKER_CONTEXT.load(Ordering::Acquire) != 0,
        "safe_dynamic_and_static_task_handles_run",
    );
    output.diagnostic(format_args!(
        "setup=0x{:03x} workers={}/{} timers={}/{}",
        setup,
        DYNAMIC_WORKER_CONTEXT.load(Ordering::Relaxed),
        STATIC_WORKER_CONTEXT.load(Ordering::Relaxed),
        DYNAMIC_TIMER_CALLBACKS.load(Ordering::Relaxed),
        STATIC_TIMER_CALLBACKS.load(Ordering::Relaxed),
    ));

    finish(output, 10)
}

#[entry]
fn main() -> ! {
    // Safety: startup is single-threaded and has exclusive kernel access.
    let context = unsafe { TaskContext::assume() };
    record(CONTEXT_SETUP, context.current().is_none());
    record(QUEUE_SETUP, exercise_queues(&context));
    record(SEMAPHORE_SETUP, exercise_semaphores(&context));
    record(MUTEX_SETUP, exercise_mutexes(&context));
    record(EVENT_SETUP, exercise_events(&context));
    record(STREAM_SETUP, exercise_streams(&context));
    record(MESSAGE_SETUP, exercise_messages(&context));

    let dynamic_timer = Timer::new_periodic(&context, c"safe-dynamic", 4, dynamic_timer_callback)
        .expect("dynamic timer creation failed");
    dynamic_timer.set_id(&context, &TIMER_COOKIE);
    let dynamic_timer_ok = dynamic_timer.period(&context) == 4
        && dynamic_timer.get_id_raw(&context)
            == core::ptr::addr_of!(TIMER_COOKIE).cast_mut().cast()
        && dynamic_timer.start(&context);

    let static_timer = unsafe {
        Timer::new_oneshot_static(
            &context,
            c"safe-static",
            6,
            static_timer_callback,
            &mut TIMER_CONTROL,
        )
    }
    .expect("static timer creation failed");
    static_timer.set_id(&context, &TIMER_COOKIE);
    let static_timer_ok = static_timer.period(&context) == 6
        && static_timer.get_id_raw(&context) == core::ptr::addr_of!(TIMER_COOKIE).cast_mut().cast()
        && static_timer.start(&context);
    record(TIMER_SETUP, dynamic_timer_ok && static_timer_ok);

    let dynamic_task = TaskHandle::spawn(
        &context,
        b"safe-dynamic-task",
        STACK_WORDS,
        WORKER_PRIORITY,
        dynamic_worker,
    )
    .expect("dynamic task creation failed");
    let static_task = unsafe {
        TaskHandle::spawn_static(
            &context,
            b"safe-static-task",
            &mut STATIC_WORKER_STACK,
            &mut STATIC_WORKER_TCB,
            WORKER_PRIORITY,
            static_worker,
        )
    }
    .expect("static task creation failed");
    let reporter_task = unsafe {
        TaskHandle::spawn_static(
            &context,
            b"safe-reporter",
            &mut REPORTER_STACK,
            &mut REPORTER_TCB,
            REPORTER_PRIORITY,
            reporter,
        )
    }
    .expect("reporter task creation failed");

    let handles_ok = dynamic_task.priority(&context) == WORKER_PRIORITY
        && dynamic_task.borrow().priority(&context) == WORKER_PRIORITY
        && static_task.priority(&context) == WORKER_PRIORITY
        && static_task.borrow().priority(&context) == WORKER_PRIORITY
        && reporter_task.priority(&context) == REPORTER_PRIORITY;
    record(TASK_HANDLE_SETUP, handles_ok);

    // Relinquish deletion ownership while leaving each kernel object live for
    // the bounded scheduler run.
    let _ = dynamic_timer.into_raw();
    let _ = static_timer.into_raw();
    let _ = dynamic_task.into_raw();
    let _ = static_task.into_raw();
    let _ = reporter_task.into_raw();

    start_scheduler()
}
