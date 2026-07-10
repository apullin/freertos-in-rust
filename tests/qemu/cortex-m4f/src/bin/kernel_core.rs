//! Regressions for task APIs which must stop executing after blocking.

#![no_std]
#![no_main]
#![allow(static_mut_refs)]

extern crate panic_semihosting;

use core::ffi::c_void;
use core::ptr;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use cortex_m_rt::entry;
use freertos_in_rust::kernel::queue::{
    queueQUEUE_TYPE_MUTEX, queueSEND_TO_BACK, xQueueCreateMutexStatic, xQueueCreateStatic,
    xQueueGenericSend, xQueueReceive, xQueueSemaphoreTake, QueueHandle_t, StaticQueue_t,
};
use freertos_in_rust::kernel::stream_buffer::{
    xStreamBufferCreate, xStreamBufferReceive, StreamBufferHandle_t,
};
use freertos_in_rust::kernel::tasks::{
    uxTaskPriorityGet, vTaskDelay, xTaskDelayUntil, xTaskGetTickCount, xTaskNotifyWait,
};
use freertos_in_rust::types::{pdFALSE, pdPASS, StackType_t, TickType_t};
use freertos_qemu_cortex_m4f::{finish, spawn_static, start_scheduler, tap, AlignedTaskBuffer};

#[cfg(feature = "small-memory")]
const STACK_WORDS: usize = 128;
#[cfg(not(feature = "small-memory"))]
const STACK_WORDS: usize = 256;
const REPORTER_STACK_WORDS: usize = 256;
const WORKER_PRIORITY: u32 = 2;
const REPORTER_PRIORITY: u32 = 3;
const BLOCK_TICKS: TickType_t = 20;
const REPORT_TICKS: TickType_t = 45;
const MAX_TICK_JITTER: TickType_t = 2;
const MUTEX_LOW_PRIORITY: u32 = 1;
const MUTEX_MEDIUM_PRIORITY: u32 = 2;
const MUTEX_HIGH_PRIORITY: u32 = 3;
const QUEUE_TIMEOUT_TICKS: TickType_t = 12;

static DELAY_START: AtomicU32 = AtomicU32::new(u32::MAX);
static DELAY_END: AtomicU32 = AtomicU32::new(u32::MAX);
static NOTIFY_START: AtomicU32 = AtomicU32::new(u32::MAX);
static NOTIFY_END: AtomicU32 = AtomicU32::new(u32::MAX);
static NOTIFY_RESULT: AtomicU32 = AtomicU32::new(u32::MAX);
static STREAM_START: AtomicU32 = AtomicU32::new(u32::MAX);
static STREAM_END: AtomicU32 = AtomicU32::new(u32::MAX);
static STREAM_RESULT: AtomicU32 = AtomicU32::new(u32::MAX);
static MUTEX_RELEASE_STARTED: AtomicBool = AtomicBool::new(false);
static MUTEX_INHERITED_PRIORITY: AtomicU32 = AtomicU32::new(u32::MAX);
static MUTEX_HIGH_RESULT: AtomicU32 = AtomicU32::new(u32::MAX);
static MUTEX_HIGH_ACQUIRED_TICK: AtomicU32 = AtomicU32::new(u32::MAX);
static QUEUE_TIMEOUT_START: AtomicU32 = AtomicU32::new(u32::MAX);
static QUEUE_TIMEOUT_END: AtomicU32 = AtomicU32::new(u32::MAX);
static QUEUE_TIMEOUT_RESULT: AtomicU32 = AtomicU32::new(u32::MAX);

static mut STREAM: StreamBufferHandle_t = ptr::null_mut();
static mut MUTEX: QueueHandle_t = ptr::null_mut();
static mut MUTEX_BUFFER: StaticQueue_t = StaticQueue_t::new();
static mut TIMEOUT_QUEUE: QueueHandle_t = ptr::null_mut();
static mut TIMEOUT_QUEUE_BUFFER: StaticQueue_t = StaticQueue_t::new();
static mut TIMEOUT_QUEUE_STORAGE: [u32; 1] = [0; 1];

static mut DELAY_STACK: [StackType_t; STACK_WORDS] = [0; STACK_WORDS];
static mut DELAY_TCB: AlignedTaskBuffer = AlignedTaskBuffer::new();
static mut NOTIFY_STACK: [StackType_t; STACK_WORDS] = [0; STACK_WORDS];
static mut NOTIFY_TCB: AlignedTaskBuffer = AlignedTaskBuffer::new();
static mut STREAM_STACK: [StackType_t; STACK_WORDS] = [0; STACK_WORDS];
static mut STREAM_TCB: AlignedTaskBuffer = AlignedTaskBuffer::new();
static mut REPORTER_STACK: [StackType_t; REPORTER_STACK_WORDS] = [0; REPORTER_STACK_WORDS];
static mut REPORTER_TCB: AlignedTaskBuffer = AlignedTaskBuffer::new();
static mut MUTEX_LOW_STACK: [StackType_t; STACK_WORDS] = [0; STACK_WORDS];
static mut MUTEX_LOW_TCB: AlignedTaskBuffer = AlignedTaskBuffer::new();
static mut MUTEX_MEDIUM_STACK: [StackType_t; STACK_WORDS] = [0; STACK_WORDS];
static mut MUTEX_MEDIUM_TCB: AlignedTaskBuffer = AlignedTaskBuffer::new();
static mut MUTEX_HIGH_STACK: [StackType_t; STACK_WORDS] = [0; STACK_WORDS];
static mut MUTEX_HIGH_TCB: AlignedTaskBuffer = AlignedTaskBuffer::new();
static mut QUEUE_TIMEOUT_STACK: [StackType_t; STACK_WORDS] = [0; STACK_WORDS];
static mut QUEUE_TIMEOUT_TCB: AlignedTaskBuffer = AlignedTaskBuffer::new();
static mut QUEUE_WAKER_STACK: [StackType_t; STACK_WORDS] = [0; STACK_WORDS];
static mut QUEUE_WAKER_TCB: AlignedTaskBuffer = AlignedTaskBuffer::new();

fn park_worker() -> ! {
    loop {
        cortex_m::asm::nop();
    }
}

fn sleep_worker() -> ! {
    loop {
        unsafe { vTaskDelay(1000) };
    }
}

extern "C" fn delay_until_worker(_parameter: *mut c_void) {
    let mut previous_wake = xTaskGetTickCount();
    DELAY_START.store(previous_wake as u32, Ordering::Relaxed);

    // SAFETY: `previous_wake` is uniquely owned by this task and the call is
    // made from task context.
    unsafe { xTaskDelayUntil(&mut previous_wake, BLOCK_TICKS) };

    DELAY_END.store(xTaskGetTickCount() as u32, Ordering::Relaxed);
    park_worker()
}

extern "C" fn notification_worker(_parameter: *mut c_void) {
    let start = xTaskGetTickCount();
    NOTIFY_START.store(start as u32, Ordering::Relaxed);

    let result = unsafe { xTaskNotifyWait(0, u32::MAX, ptr::null_mut(), BLOCK_TICKS) };

    NOTIFY_RESULT.store(result as u32, Ordering::Relaxed);
    NOTIFY_END.store(xTaskGetTickCount() as u32, Ordering::Relaxed);
    park_worker()
}

extern "C" fn stream_worker(_parameter: *mut c_void) {
    let mut byte = 0u8;
    let start = xTaskGetTickCount();
    STREAM_START.store(start as u32, Ordering::Relaxed);

    let received = unsafe {
        xStreamBufferReceive(
            STREAM,
            core::ptr::addr_of_mut!(byte).cast::<c_void>(),
            1,
            BLOCK_TICKS,
        )
    };

    STREAM_RESULT.store(received as u32, Ordering::Relaxed);
    STREAM_END.store(xTaskGetTickCount() as u32, Ordering::Relaxed);
    park_worker()
}

extern "C" fn mutex_low_worker(_parameter: *mut c_void) {
    let take_result = unsafe { xQueueSemaphoreTake(MUTEX, 0) };
    if take_result != pdPASS {
        sleep_worker();
    }

    let start = xTaskGetTickCount();
    while xTaskGetTickCount().wrapping_sub(start) < 8 {
        if xTaskGetTickCount().wrapping_sub(start) >= 3
            && MUTEX_INHERITED_PRIORITY.load(Ordering::Relaxed) == u32::MAX
        {
            let priority = unsafe { uxTaskPriorityGet(ptr::null_mut()) };
            MUTEX_INHERITED_PRIORITY.store(priority, Ordering::Relaxed);
        }
    }

    /* Once the mutex give drops this task back to its base priority, the
     * medium task becomes runnable. Set the flag first so that task can go to
     * sleep and allow this function to finish the give. */
    MUTEX_RELEASE_STARTED.store(true, Ordering::Release);
    unsafe {
        let _ = xQueueGenericSend(MUTEX, ptr::null(), 0, queueSEND_TO_BACK);
    }
    sleep_worker()
}

extern "C" fn mutex_medium_worker(_parameter: *mut c_void) {
    unsafe { vTaskDelay(3) };

    /* Without priority inheritance this task starves the low-priority mutex
     * holder. With inheritance, the holder runs through its critical section
     * first and sets the release flag. */
    while !MUTEX_RELEASE_STARTED.load(Ordering::Acquire) {
        core::hint::spin_loop();
    }
    sleep_worker()
}

extern "C" fn mutex_high_worker(_parameter: *mut c_void) {
    unsafe { vTaskDelay(2) };
    let result = unsafe { xQueueSemaphoreTake(MUTEX, 40) };
    MUTEX_HIGH_RESULT.store(result as u32, Ordering::Relaxed);

    if result == pdPASS {
        MUTEX_HIGH_ACQUIRED_TICK.store(xTaskGetTickCount() as u32, Ordering::Relaxed);
        unsafe {
            let _ = xQueueGenericSend(MUTEX, ptr::null(), 0, queueSEND_TO_BACK);
        }
    }
    sleep_worker()
}

extern "C" fn queue_timeout_worker(_parameter: *mut c_void) {
    let mut value = 0u32;
    let start = xTaskGetTickCount();
    QUEUE_TIMEOUT_START.store(start as u32, Ordering::Relaxed);
    let result = unsafe {
        xQueueReceive(
            TIMEOUT_QUEUE,
            core::ptr::addr_of_mut!(value).cast::<c_void>(),
            QUEUE_TIMEOUT_TICKS,
        )
    };
    QUEUE_TIMEOUT_RESULT.store(result as u32, Ordering::Relaxed);
    QUEUE_TIMEOUT_END.store(xTaskGetTickCount() as u32, Ordering::Relaxed);
    sleep_worker()
}

extern "C" fn queue_waker_worker(_parameter: *mut c_void) {
    let value = 0x1234_5678u32;
    let mut stolen = 0u32;
    let wake_delays: [TickType_t; 3] = [2, 4, 4];
    let mut index = 0usize;

    while index < wake_delays.len() {
        unsafe { vTaskDelay(wake_delays[index]) };
        unsafe {
            let _ = xQueueGenericSend(
                TIMEOUT_QUEUE,
                (&value as *const u32).cast::<c_void>(),
                0,
                queueSEND_TO_BACK,
            );
            let _ = xQueueReceive(
                TIMEOUT_QUEUE,
                core::ptr::addr_of_mut!(stolen).cast::<c_void>(),
                0,
            );
        }
        index += 1;
    }

    sleep_worker()
}

fn elapsed(start: &AtomicU32, end: &AtomicU32) -> Option<TickType_t> {
    let start = start.load(Ordering::Relaxed);
    let end = end.load(Ordering::Relaxed);

    if start == u32::MAX || end == u32::MAX {
        None
    } else {
        Some((end.wrapping_sub(start)) as TickType_t)
    }
}

fn blocked_for_expected_ticks(measured: Option<TickType_t>) -> bool {
    match measured {
        Some(ticks) => {
            ticks >= BLOCK_TICKS.saturating_sub(MAX_TICK_JITTER)
                && ticks <= BLOCK_TICKS.saturating_add(MAX_TICK_JITTER)
        }
        None => false,
    }
}

extern "C" fn reporter(_parameter: *mut c_void) {
    unsafe { vTaskDelay(REPORT_TICKS) };

    let delay_elapsed = elapsed(&DELAY_START, &DELAY_END);
    let notify_elapsed = elapsed(&NOTIFY_START, &NOTIFY_END);
    let stream_elapsed = elapsed(&STREAM_START, &STREAM_END);
    let notify_result = NOTIFY_RESULT.load(Ordering::Relaxed);
    let stream_result = STREAM_RESULT.load(Ordering::Relaxed);

    let mut output = tap();
    output.check(
        blocked_for_expected_ticks(delay_elapsed),
        "delay_until_blocks",
    );
    output.check(
        blocked_for_expected_ticks(notify_elapsed) && notify_result == pdFALSE as u32,
        "notification_wait_times_out_after_deadline",
    );
    output.check(
        blocked_for_expected_ticks(stream_elapsed) && stream_result == 0,
        "stream_receive_times_out_after_deadline",
    );
    let mutex_priority = MUTEX_INHERITED_PRIORITY.load(Ordering::Relaxed);
    let mutex_result = MUTEX_HIGH_RESULT.load(Ordering::Relaxed);
    let mutex_tick = MUTEX_HIGH_ACQUIRED_TICK.load(Ordering::Relaxed);
    output.check(
        MUTEX_RELEASE_STARTED.load(Ordering::Acquire)
            && mutex_priority == MUTEX_HIGH_PRIORITY
            && mutex_result == pdPASS as u32
            && mutex_tick != u32::MAX
            && mutex_tick <= 15,
        "mutex_priority_inheritance_prevents_medium_task_inversion",
    );
    let queue_timeout_elapsed = elapsed(&QUEUE_TIMEOUT_START, &QUEUE_TIMEOUT_END);
    let queue_timeout_result = QUEUE_TIMEOUT_RESULT.load(Ordering::Relaxed);
    output.check(
        match queue_timeout_elapsed {
            Some(ticks) => {
                ticks >= QUEUE_TIMEOUT_TICKS.saturating_sub(MAX_TICK_JITTER)
                    && ticks <= QUEUE_TIMEOUT_TICKS.saturating_add(MAX_TICK_JITTER)
                    && queue_timeout_result == pdFALSE as u32
            }
            None => false,
        },
        "queue_retries_share_one_timeout_deadline",
    );
    output.diagnostic(format_args!(
        "delay={:?} notify={:?}/{} stream={:?}/{} mutex=priority:{}/result:{}/tick:{} queue_timeout={:?}/{}",
        delay_elapsed,
        notify_elapsed,
        notify_result,
        stream_elapsed,
        stream_result,
        mutex_priority,
        mutex_result,
        mutex_tick,
        queue_timeout_elapsed,
        queue_timeout_result,
    ));

    finish(output, 5)
}

#[entry]
fn main() -> ! {
    unsafe {
        STREAM = xStreamBufferCreate(32, 1);
        assert!(!STREAM.is_null());
        MUTEX =
            xQueueCreateMutexStatic(queueQUEUE_TYPE_MUTEX, core::ptr::addr_of_mut!(MUTEX_BUFFER));
        assert!(!MUTEX.is_null());
        TIMEOUT_QUEUE = xQueueCreateStatic(
            1,
            core::mem::size_of::<u32>() as u32,
            TIMEOUT_QUEUE_STORAGE.as_mut_ptr().cast::<u8>(),
            core::ptr::addr_of_mut!(TIMEOUT_QUEUE_BUFFER),
        );
        assert!(!TIMEOUT_QUEUE.is_null());

        spawn_static(
            b"delay-until\0",
            DELAY_STACK.as_mut_ptr(),
            STACK_WORDS,
            core::ptr::addr_of_mut!(DELAY_TCB),
            WORKER_PRIORITY,
            delay_until_worker,
        );
        spawn_static(
            b"notify-wait\0",
            NOTIFY_STACK.as_mut_ptr(),
            STACK_WORDS,
            core::ptr::addr_of_mut!(NOTIFY_TCB),
            WORKER_PRIORITY,
            notification_worker,
        );
        spawn_static(
            b"stream-wait\0",
            STREAM_STACK.as_mut_ptr(),
            STACK_WORDS,
            core::ptr::addr_of_mut!(STREAM_TCB),
            WORKER_PRIORITY,
            stream_worker,
        );
        spawn_static(
            b"mutex-low\0",
            MUTEX_LOW_STACK.as_mut_ptr(),
            STACK_WORDS,
            core::ptr::addr_of_mut!(MUTEX_LOW_TCB),
            MUTEX_LOW_PRIORITY,
            mutex_low_worker,
        );
        spawn_static(
            b"mutex-medium\0",
            MUTEX_MEDIUM_STACK.as_mut_ptr(),
            STACK_WORDS,
            core::ptr::addr_of_mut!(MUTEX_MEDIUM_TCB),
            MUTEX_MEDIUM_PRIORITY,
            mutex_medium_worker,
        );
        spawn_static(
            b"mutex-high\0",
            MUTEX_HIGH_STACK.as_mut_ptr(),
            STACK_WORDS,
            core::ptr::addr_of_mut!(MUTEX_HIGH_TCB),
            MUTEX_HIGH_PRIORITY,
            mutex_high_worker,
        );
        spawn_static(
            b"queue-timeout\0",
            QUEUE_TIMEOUT_STACK.as_mut_ptr(),
            STACK_WORDS,
            core::ptr::addr_of_mut!(QUEUE_TIMEOUT_TCB),
            MUTEX_LOW_PRIORITY,
            queue_timeout_worker,
        );
        spawn_static(
            b"queue-waker\0",
            QUEUE_WAKER_STACK.as_mut_ptr(),
            STACK_WORDS,
            core::ptr::addr_of_mut!(QUEUE_WAKER_TCB),
            MUTEX_HIGH_PRIORITY + 1,
            queue_waker_worker,
        );
        spawn_static(
            b"report\0",
            REPORTER_STACK.as_mut_ptr(),
            REPORTER_STACK_WORDS,
            core::ptr::addr_of_mut!(REPORTER_TCB),
            REPORTER_PRIORITY,
            reporter,
        );
    }

    start_scheduler()
}
