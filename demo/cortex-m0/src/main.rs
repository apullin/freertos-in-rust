//! FreeRTOS-in-Rust Demo Application - Cortex-M0
//!
//! This demo demonstrates the FreeRTOS kernel running on Cortex-M0
//! using the safe Rust wrappers with idiomatic Rust patterns:
//! - Static initialization with contained unsafe blocks
//! - .expect() for error handling on creation
//! - RAII guards for mutex access
//!
//! Note: Cortex-M0 doesn't have atomic instructions, so this demo
//! uses Mutex<T> for all shared data protection.
//!
//! Output is via semihosting - requires a debugger connection.

#![no_std]
#![no_main]
#![allow(non_snake_case)]
#![allow(static_mut_refs)] // Task stacks/TCBs must be static mut for FreeRTOS

extern crate panic_semihosting;

use core::ffi::c_void;

use cortex_m_rt::entry;
use cortex_m_semihosting::hprintln;

// Allocator selection via Cargo features
#[cfg(feature = "use-heap-4")]
use freertos_in_rust::memory::FreeRtosAllocator;
#[cfg(feature = "use-heap-4")]
#[global_allocator]
static ALLOCATOR: FreeRtosAllocator = FreeRtosAllocator;

#[cfg(feature = "use-embedded-alloc")]
use embedded_alloc::LlffHeap as Heap;
#[cfg(feature = "use-embedded-alloc")]
#[global_allocator]
static HEAP: Heap = Heap::empty();

// Import safe wrappers
use freertos_in_rust::kernel::event_groups::EventBits_t;
use freertos_in_rust::kernel::tasks::{
    ulTaskGetIdleRunTimeCounter, ulTaskGetIdleRunTimePercent, ulTaskGetRunTimeCounter,
    ulTaskGetRunTimePercent, ulTaskGetTotalRunTime, StaticTask_t,
};
use freertos_in_rust::start_scheduler;
use freertos_in_rust::sync::{
    BinarySemaphore, EventGroup, Mutex, StreamBuffer, StreamBufferReceiver, StreamBufferSender,
    TaskContext, TaskHandle, Timer,
};
use freertos_in_rust::types::*;

// =============================================================================
// Shared Resources
//
// These are initialized once before the scheduler starts, then accessed
// read-only by tasks. The unsafe block for initialization is contained to
// the init functions; subsequent access through the wrappers is safe.
// =============================================================================

/// Mutex protecting the shared counter - demonstrates priority inheritance
static mut COUNTER_MUTEX: Option<Mutex<u32>> = None;

/// Binary semaphore for signaling between tasks
static mut SEMAPHORE: Option<BinarySemaphore> = None;

/// Periodic software timer
static mut TIMER: Option<Timer> = None;

/// Stream buffer for producer/consumer demo
static mut STREAM_BUFFER: Option<StreamBuffer> = None;

/// Sole producer endpoint borrowed from `STREAM_BUFFER` after initialization.
static mut STREAM_SENDER: Option<StreamBufferSender<'static>> = None;

/// Sole consumer endpoint borrowed from `STREAM_BUFFER` after initialization.
static mut STREAM_RECEIVER: Option<StreamBufferReceiver<'static>> = None;

/// Event group for task synchronization
static mut EVENT_GROUP: Option<EventGroup> = None;

/// Timer tick counter (no atomics on CM0, so just use raw counter in callback)
static mut TIMER_TICKS: u32 = 0;

// Helper macros for safe access to initialized resources
// These panic if called before initialization (programming error)
macro_rules! get_mutex {
    () => {
        unsafe { COUNTER_MUTEX.as_ref().expect("Mutex not initialized") }
    };
}

macro_rules! get_semaphore {
    () => {
        unsafe { SEMAPHORE.as_ref().expect("Semaphore not initialized") }
    };
}

macro_rules! get_stream_sender {
    () => {
        unsafe {
            STREAM_SENDER
                .as_mut()
                .expect("Stream sender not initialized")
        }
    };
}

macro_rules! get_stream_receiver {
    () => {
        unsafe {
            STREAM_RECEIVER
                .as_mut()
                .expect("Stream receiver not initialized")
        }
    };
}

macro_rules! get_event_group {
    () => {
        unsafe { EVENT_GROUP.as_ref().expect("EventGroup not initialized") }
    };
}

// =============================================================================
// Task Priorities
// =============================================================================

const PRIORITY_LOW: UBaseType_t = 1;
const PRIORITY_MEDIUM: UBaseType_t = 2;
const PRIORITY_HIGH: UBaseType_t = 3;

// =============================================================================
// Task Stack Sizes (in words, not bytes)
// =============================================================================

const STACK_SIZE: usize = 128; // 512 bytes per task

// Static task stacks and TCBs
static mut LOW_TASK_STACK: [StackType_t; STACK_SIZE] = [0; STACK_SIZE];
static mut LOW_TASK_TCB: StaticTask_t = StaticTask_t::new();

static mut MEDIUM_TASK_STACK: [StackType_t; STACK_SIZE] = [0; STACK_SIZE];
static mut MEDIUM_TASK_TCB: StaticTask_t = StaticTask_t::new();

static mut HIGH_TASK_STACK: [StackType_t; STACK_SIZE] = [0; STACK_SIZE];
static mut HIGH_TASK_TCB: StaticTask_t = StaticTask_t::new();

static mut SEM_WAITER_STACK: [StackType_t; STACK_SIZE] = [0; STACK_SIZE];
static mut SEM_WAITER_TCB: StaticTask_t = StaticTask_t::new();

static mut PRODUCER_TASK_STACK: [StackType_t; STACK_SIZE] = [0; STACK_SIZE];
static mut PRODUCER_TASK_TCB: StaticTask_t = StaticTask_t::new();

static mut CONSUMER_TASK_STACK: [StackType_t; STACK_SIZE] = [0; STACK_SIZE];
static mut CONSUMER_TASK_TCB: StaticTask_t = StaticTask_t::new();

static mut EVENT_SENDER_STACK: [StackType_t; STACK_SIZE] = [0; STACK_SIZE];
static mut EVENT_SENDER_TCB: StaticTask_t = StaticTask_t::new();

static mut EVENT_WAITER_STACK: [StackType_t; STACK_SIZE] = [0; STACK_SIZE];
static mut EVENT_WAITER_TCB: StaticTask_t = StaticTask_t::new();

static mut RUNTIME_STATS_STACK: [StackType_t; STACK_SIZE] = [0; STACK_SIZE];
static mut RUNTIME_STATS_TCB: StaticTask_t = StaticTask_t::new();

// =============================================================================
// Entry Point
// =============================================================================

#[entry]
fn main() -> ! {
    // Safety: startup is single-threaded and no interrupt invokes kernel APIs
    // before the scheduler is started.
    let context = unsafe { TaskContext::assume() };

    // Initialize allocator (only needed for embedded-alloc)
    #[cfg(feature = "use-embedded-alloc")]
    {
        use core::mem::MaybeUninit;
        const HEAP_SIZE: usize = 16384;
        static mut HEAP_MEM: [MaybeUninit<u8>; HEAP_SIZE] = [MaybeUninit::uninit(); HEAP_SIZE];
        unsafe { HEAP.init(HEAP_MEM.as_ptr() as usize, HEAP_SIZE) }
    }

    hprintln!("========================================");
    hprintln!("   FreeRTOS-in-Rust Demo - Cortex-M0");
    hprintln!("========================================");
    hprintln!("");

    // Create all resources - uses .expect() for clean error handling
    create_sync_primitives(&context);
    create_stream_buffer(&context);
    create_event_group(&context);
    create_tasks(&context);
    create_timer(&context);

    hprintln!("[Main] Starting scheduler...");
    hprintln!("");

    start_scheduler(context)
}

// =============================================================================
// Initialization Functions
// =============================================================================

fn create_sync_primitives(context: &TaskContext) {
    hprintln!("[Init] Creating mutex...");
    let mutex = Mutex::new(context, 0).expect("Failed to create mutex");
    unsafe {
        COUNTER_MUTEX = Some(mutex);
    }
    hprintln!("[Init] Mutex created successfully");

    hprintln!("[Init] Creating binary semaphore...");
    let sem = BinarySemaphore::new(context).expect("Failed to create semaphore");
    unsafe {
        SEMAPHORE = Some(sem);
    }
    hprintln!("[Init] Semaphore created successfully");
}

fn create_stream_buffer(context: &TaskContext) {
    hprintln!("[Init] Creating stream buffer (128 bytes, trigger=1)...");
    let stream = StreamBuffer::new(context, 128, 1).expect("Failed to create stream buffer");
    // Safety: startup has exclusive access. The owner remains permanently in
    // `STREAM_BUFFER`; each endpoint is installed once and used by one task.
    unsafe {
        STREAM_BUFFER = Some(stream);
        let stream = STREAM_BUFFER.as_mut().expect("stream owner disappeared");
        let (sender, receiver) = stream.split();
        STREAM_SENDER = Some(sender);
        STREAM_RECEIVER = Some(receiver);
    }
    hprintln!("[Init] Stream buffer created successfully");
}

fn create_event_group(context: &TaskContext) {
    hprintln!("[Init] Creating event group...");
    let events = EventGroup::new(context).expect("Failed to create event group");
    unsafe {
        EVENT_GROUP = Some(events);
    }
    hprintln!("[Init] Event group created successfully");
}

fn create_tasks(context: &TaskContext) {
    hprintln!("[Init] Creating tasks...");

    // Note: Task stacks/TCBs are static mut (required by FreeRTOS)
    // Access is contained to this initialization function
    unsafe {
        TaskHandle::spawn_static(
            context,
            b"LowTask\0",
            &mut LOW_TASK_STACK,
            &mut LOW_TASK_TCB,
            PRIORITY_LOW,
            task_low_priority,
        )
        .expect("Failed to create low priority task");
        hprintln!(
            "[Init] Low priority task created (priority {})",
            PRIORITY_LOW
        );

        TaskHandle::spawn_static(
            context,
            b"MedTask\0",
            &mut MEDIUM_TASK_STACK,
            &mut MEDIUM_TASK_TCB,
            PRIORITY_MEDIUM,
            task_medium_priority,
        )
        .expect("Failed to create medium priority task");
        hprintln!(
            "[Init] Medium priority task created (priority {})",
            PRIORITY_MEDIUM
        );

        TaskHandle::spawn_static(
            context,
            b"HighTask\0",
            &mut HIGH_TASK_STACK,
            &mut HIGH_TASK_TCB,
            PRIORITY_HIGH,
            task_high_priority,
        )
        .expect("Failed to create high priority task");
        hprintln!(
            "[Init] High priority task created (priority {})",
            PRIORITY_HIGH
        );

        TaskHandle::spawn_static(
            context,
            b"SemWait\0",
            &mut SEM_WAITER_STACK,
            &mut SEM_WAITER_TCB,
            PRIORITY_MEDIUM,
            task_semaphore_waiter,
        )
        .expect("Failed to create semaphore waiter task");
        hprintln!(
            "[Init] Semaphore waiter task created (priority {})",
            PRIORITY_MEDIUM
        );

        TaskHandle::spawn_static(
            context,
            b"Producer\0",
            &mut PRODUCER_TASK_STACK,
            &mut PRODUCER_TASK_TCB,
            PRIORITY_LOW,
            task_stream_producer,
        )
        .expect("Failed to create producer task");
        hprintln!(
            "[Init] Stream producer task created (priority {})",
            PRIORITY_LOW
        );

        TaskHandle::spawn_static(
            context,
            b"Consumer\0",
            &mut CONSUMER_TASK_STACK,
            &mut CONSUMER_TASK_TCB,
            PRIORITY_MEDIUM,
            task_stream_consumer,
        )
        .expect("Failed to create consumer task");
        hprintln!(
            "[Init] Stream consumer task created (priority {})",
            PRIORITY_MEDIUM
        );

        TaskHandle::spawn_static(
            context,
            b"EvtSend\0",
            &mut EVENT_SENDER_STACK,
            &mut EVENT_SENDER_TCB,
            PRIORITY_LOW,
            task_event_sender,
        )
        .expect("Failed to create event sender task");
        hprintln!(
            "[Init] Event sender task created (priority {})",
            PRIORITY_LOW
        );

        TaskHandle::spawn_static(
            context,
            b"EvtWait\0",
            &mut EVENT_WAITER_STACK,
            &mut EVENT_WAITER_TCB,
            PRIORITY_MEDIUM,
            task_event_waiter,
        )
        .expect("Failed to create event waiter task");
        hprintln!(
            "[Init] Event waiter task created (priority {})",
            PRIORITY_MEDIUM
        );

        TaskHandle::spawn_static(
            context,
            b"RunStats\0",
            &mut RUNTIME_STATS_STACK,
            &mut RUNTIME_STATS_TCB,
            PRIORITY_LOW,
            task_runtime_stats,
        )
        .expect("Failed to create runtime stats task");
        hprintln!(
            "[Init] Runtime stats task created (priority {})",
            PRIORITY_LOW
        );
    }
}

fn create_timer(context: &TaskContext) {
    hprintln!("[Init] Creating software timer...");
    let timer = Timer::new_periodic(context, c"Timer1", pdMS_TO_TICKS(1000), timer_callback)
        .expect("Failed to create timer");
    assert!(timer.start(context), "Failed to start timer");
    unsafe {
        TIMER = Some(timer);
    }
    hprintln!("[Init] Timer created successfully");
}

// =============================================================================
// Timer Callback
// =============================================================================

extern "C" fn timer_callback(_xTimer: TimerHandle_t) {
    unsafe {
        TIMER_TICKS += 1;
        hprintln!("[Timer] Tick #{}", TIMER_TICKS);
    }
}

// =============================================================================
// Task Functions
// =============================================================================

/// Low priority task - demonstrates Mutex with RAII guard and priority inheritance
extern "C" fn task_low_priority(_pvParameters: *mut c_void) {
    // Safety: FreeRTOS invokes task entry points only in task context.
    let context = unsafe { TaskContext::assume() };
    let mut iteration: u32 = 0;
    let mutex = get_mutex!();
    let semaphore = get_semaphore!();

    loop {
        iteration += 1;
        hprintln!("");
        hprintln!("[Low #{}] Attempting to take mutex...", iteration);

        {
            let mut guard = mutex.lock(&context).expect("mutex acquisition failed");
            hprintln!("[Low #{}] Mutex acquired! Doing work...", iteration);

            for _ in 0..3 {
                *guard += 1;
                let count = *guard;
                hprintln!("[Low #{}] Working... counter = {}", iteration, count);
                freertos_in_rust::delay(&context, pdMS_TO_TICKS(100));
            }

            hprintln!("[Low #{}] Signaling semaphore...", iteration);
            assert!(semaphore.give(&context), "semaphore give failed");

            let final_count = *guard;
            hprintln!(
                "[Low #{}] Releasing mutex (counter={})",
                iteration,
                final_count
            );
        } // guard dropped here, mutex released

        freertos_in_rust::delay(&context, pdMS_TO_TICKS(500));
    }
}

/// Medium priority task - periodic work
extern "C" fn task_medium_priority(_pvParameters: *mut c_void) {
    // Safety: FreeRTOS invokes task entry points only in task context.
    let context = unsafe { TaskContext::assume() };
    let mut iteration: u32 = 0;
    freertos_in_rust::delay(&context, pdMS_TO_TICKS(50));

    loop {
        iteration += 1;
        hprintln!("[Med #{}] Running", iteration);
        freertos_in_rust::delay(&context, pdMS_TO_TICKS(200));
    }
}

/// High priority task - demonstrates priority inheritance when blocked on mutex
extern "C" fn task_high_priority(_pvParameters: *mut c_void) {
    // Safety: FreeRTOS invokes task entry points only in task context.
    let context = unsafe { TaskContext::assume() };
    let mut iteration: u32 = 0;
    let mutex = get_mutex!();

    loop {
        iteration += 1;
        freertos_in_rust::delay(&context, pdMS_TO_TICKS(150));

        hprintln!(
            "[High #{}] Attempting to take mutex (should trigger priority inheritance)...",
            iteration
        );

        {
            let mut guard = mutex.lock(&context).expect("mutex acquisition failed");
            hprintln!("[High #{}] Mutex acquired!", iteration);

            *guard += 10;
            let count = *guard;
            hprintln!("[High #{}] Counter now = {}", iteration, count);

            hprintln!("[High #{}] Releasing mutex", iteration);
        } // guard dropped, mutex released

        freertos_in_rust::delay(&context, pdMS_TO_TICKS(1000));
    }
}

/// Semaphore waiter task - waits on binary semaphore
extern "C" fn task_semaphore_waiter(_pvParameters: *mut c_void) {
    // Safety: FreeRTOS invokes task entry points only in task context.
    let context = unsafe { TaskContext::assume() };
    let mut wakeups: u32 = 0;
    let semaphore = get_semaphore!();
    let mutex = get_mutex!();

    loop {
        hprintln!("[SemWait] Waiting for semaphore...");
        assert!(semaphore.take(&context), "semaphore take failed");
        wakeups += 1;
        hprintln!("[SemWait] Semaphore received! Wakeup #{}", wakeups);

        {
            let guard = mutex.lock(&context).expect("mutex acquisition failed");
            let count = *guard;
            hprintln!("[SemWait] Current counter value: {}", count);
        }
    }
}

/// Stream buffer producer task
extern "C" fn task_stream_producer(_pvParameters: *mut c_void) {
    // Safety: FreeRTOS invokes task entry points only in task context.
    let context = unsafe { TaskContext::assume() };
    let mut sequence: u8 = 0;
    let mut iteration: u32 = 0;
    let stream = get_stream_sender!();

    freertos_in_rust::delay(&context, pdMS_TO_TICKS(200));

    loop {
        iteration += 1;

        let message: [u8; 8] = [
            sequence,
            sequence.wrapping_add(1),
            sequence.wrapping_add(2),
            sequence.wrapping_add(3),
            sequence.wrapping_add(4),
            sequence.wrapping_add(5),
            sequence.wrapping_add(6),
            sequence.wrapping_add(7),
        ];

        let space = stream.spaces(&context);
        hprintln!(
            "[Producer #{}] Sending 8 bytes (seq={}), space={}",
            iteration,
            sequence,
            space
        );

        let sent = stream.send_timeout(&context, &message, pdMS_TO_TICKS(100));

        if sent == message.len() {
            hprintln!("[Producer #{}] Sent {} bytes successfully", iteration, sent);
        } else {
            hprintln!(
                "[Producer #{}] Only sent {} of {} bytes (buffer full?)",
                iteration,
                sent,
                message.len()
            );
        }

        sequence = sequence.wrapping_add(8);
        freertos_in_rust::delay(&context, pdMS_TO_TICKS(750));
    }
}

/// Stream buffer consumer task
extern "C" fn task_stream_consumer(_pvParameters: *mut c_void) {
    // Safety: FreeRTOS invokes task entry points only in task context.
    let context = unsafe { TaskContext::assume() };
    let mut total_received: u32 = 0;
    let mut iteration: u32 = 0;
    let mut buffer: [u8; 32] = [0u8; 32];
    let stream = get_stream_receiver!();

    freertos_in_rust::delay(&context, pdMS_TO_TICKS(500));

    loop {
        iteration += 1;

        let available = stream.available(&context);
        hprintln!(
            "[Consumer #{}] Waiting for data, available={}",
            iteration,
            available
        );

        let received = stream.receive_timeout(&context, &mut buffer, pdMS_TO_TICKS(2000));

        if received > 0 {
            total_received += received as u32;
            hprintln!(
                "[Consumer #{}] Received {} bytes, total={}",
                iteration,
                received,
                total_received
            );

            if received >= 4 {
                hprintln!(
                    "[Consumer #{}] Data: [{}, {}, {}, {}, ...]",
                    iteration,
                    buffer[0],
                    buffer[1],
                    buffer[2],
                    buffer[3]
                );
            }
        } else {
            hprintln!("[Consumer #{}] Timeout - no data received", iteration);
        }

        freertos_in_rust::delay(&context, pdMS_TO_TICKS(100));
    }
}

// =============================================================================
// Event Group Constants
// =============================================================================

const EVENT_BIT_DATA_READY: EventBits_t = 1 << 0;
const EVENT_BIT_ACK: EventBits_t = 1 << 1;

/// Event group sender task
extern "C" fn task_event_sender(_pvParameters: *mut c_void) {
    // Safety: FreeRTOS invokes task entry points only in task context.
    let context = unsafe { TaskContext::assume() };
    let mut iteration: u32 = 0;
    let events = get_event_group!();

    freertos_in_rust::delay(&context, pdMS_TO_TICKS(300));

    loop {
        iteration += 1;

        hprintln!("[EvtSend #{}] Setting DATA_READY bit", iteration);
        let bits_after = events.set(&context, EVENT_BIT_DATA_READY);
        hprintln!(
            "[EvtSend #{}] Bits after set: 0x{:02X}",
            iteration,
            bits_after
        );

        hprintln!("[EvtSend #{}] Waiting for ACK bit...", iteration);
        if let Some(bits) =
            events.wait_any_clear_timeout(&context, EVENT_BIT_ACK, pdMS_TO_TICKS(2000))
        {
            hprintln!("[EvtSend #{}] ACK received! bits=0x{:02X}", iteration, bits);
        } else {
            hprintln!("[EvtSend #{}] Timeout waiting for ACK", iteration);
        }

        freertos_in_rust::delay(&context, pdMS_TO_TICKS(1500));
    }
}

/// Event group waiter task
extern "C" fn task_event_waiter(_pvParameters: *mut c_void) {
    // Safety: FreeRTOS invokes task entry points only in task context.
    let context = unsafe { TaskContext::assume() };
    let mut iteration: u32 = 0;
    let events = get_event_group!();

    loop {
        iteration += 1;
        hprintln!("[EvtWait #{}] Waiting for DATA_READY bit...", iteration);

        if let Some(bits) =
            events.wait_any_clear_timeout(&context, EVENT_BIT_DATA_READY, pdMS_TO_TICKS(3000))
        {
            hprintln!(
                "[EvtWait #{}] DATA_READY received! bits=0x{:02X}",
                iteration,
                bits
            );

            hprintln!("[EvtWait #{}] Processing...", iteration);
            freertos_in_rust::delay(&context, pdMS_TO_TICKS(100));

            hprintln!("[EvtWait #{}] Sending ACK", iteration);
            events.set(&context, EVENT_BIT_ACK);
        } else {
            hprintln!("[EvtWait #{}] Timeout - no data ready", iteration);
        }

        freertos_in_rust::delay(&context, pdMS_TO_TICKS(50));
    }
}

// =============================================================================
// Runtime Statistics Task
// =============================================================================

extern "C" fn task_runtime_stats(_pvParameters: *mut c_void) {
    // Safety: FreeRTOS invokes task entry points only in task context.
    let context = unsafe { TaskContext::assume() };
    use core::ptr;
    let mut iteration: u32 = 0;

    freertos_in_rust::delay(&context, pdMS_TO_TICKS(1500));

    loop {
        iteration += 1;

        hprintln!("");
        hprintln!("========== Run-Time Statistics (#{}) ==========", iteration);

        let total_time = ulTaskGetTotalRunTime();
        hprintln!("Total Run Time: {} ticks", total_time);

        let idle_counter = ulTaskGetIdleRunTimeCounter();
        let idle_percent = ulTaskGetIdleRunTimePercent();
        hprintln!("Idle Task:      {} ticks ({}%)", idle_counter, idle_percent);

        let cpu_usage = 100 - idle_percent;
        hprintln!("CPU Usage:      {}%", cpu_usage);

        // SAFETY: null explicitly selects this live current task, and this
        // function executes in task context.
        let my_counter = unsafe { ulTaskGetRunTimeCounter(ptr::null_mut()) };
        let my_percent = unsafe { ulTaskGetRunTimePercent(ptr::null_mut()) };
        hprintln!("This Task:      {} ticks ({}%)", my_counter, my_percent);

        hprintln!("================================================");
        hprintln!("");

        freertos_in_rust::delay(&context, pdMS_TO_TICKS(5000));
    }
}
