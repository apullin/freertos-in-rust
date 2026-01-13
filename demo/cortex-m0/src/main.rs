//! FreeRusTOS Demo Application - Cortex-M0
//!
//! This demo demonstrates the FreeRTOS kernel running on Cortex-M0
//! using the safe Rust wrappers:
//! - Task creation with TaskHandle::spawn_static()
//! - Mutex<T> with priority inheritance and RAII guards
//! - BinarySemaphore for signaling
//! - Timer for periodic callbacks
//! - StreamBuffer for byte stream transfers
//! - EventGroup for task synchronization
//!
//! Note: Cortex-M0 doesn't have atomic instructions, so this demo
//! uses Mutex<T> for all shared data protection.
//!
//! Output is via semihosting - requires a debugger connection.

#![no_std]
#![no_main]
#![allow(non_snake_case)]
#![allow(static_mut_refs)]

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
use freertos_in_rust::sync::{
    BinarySemaphore, EventGroup, Mutex, StreamBuffer, TaskHandle, Timer,
};
use freertos_in_rust::kernel::tasks::{
    vTaskDelay, StaticTask_t,
    ulTaskGetRunTimeCounter, ulTaskGetRunTimePercent,
    ulTaskGetIdleRunTimeCounter, ulTaskGetIdleRunTimePercent,
    ulTaskGetTotalRunTime,
};
use freertos_in_rust::kernel::event_groups::EventBits_t;
use freertos_in_rust::types::*;
use freertos_in_rust::start_scheduler;

// =============================================================================
// Shared Resources (using safe wrappers)
// =============================================================================

/// Mutex protecting the shared counter - demonstrates priority inheritance
static mut MUTEX: Option<Mutex<u32>> = None;

/// Binary semaphore for signaling between tasks
static mut SEMAPHORE: Option<BinarySemaphore> = None;

/// Periodic software timer
static mut TIMER: Option<Timer> = None;

/// Stream buffer for producer/consumer demo
static mut STREAM_BUFFER: Option<StreamBuffer> = None;

/// Event group for task synchronization
static mut EVENT_GROUP: Option<EventGroup> = None;

/// Timer tick counter (protected by its own mutex since CM0 lacks atomics)
static mut TIMER_TICKS: u32 = 0;

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
    // Initialize allocator (only needed for embedded-alloc)
    #[cfg(feature = "use-embedded-alloc")]
    {
        use core::mem::MaybeUninit;
        const HEAP_SIZE: usize = 16384;
        static mut HEAP_MEM: [MaybeUninit<u8>; HEAP_SIZE] = [MaybeUninit::uninit(); HEAP_SIZE];
        unsafe { HEAP.init(HEAP_MEM.as_ptr() as usize, HEAP_SIZE) }
    }

    hprintln!("========================================");
    hprintln!("   FreeRusTOS Demo - Cortex-M0");
    hprintln!("========================================");
    hprintln!("");

    // Create synchronization primitives
    create_sync_primitives();

    // Create stream buffer for producer/consumer demo
    create_stream_buffer();

    // Create event group for synchronization demo
    create_event_group();

    // Create tasks
    create_tasks();

    // Create software timer
    create_timer();

    hprintln!("[Main] Starting scheduler...");
    hprintln!("");

    // Start the scheduler - this never returns
    start_scheduler();

    // Should never reach here
    hprintln!("[Main] ERROR: Scheduler returned!");
    loop {}
}

// =============================================================================
// Initialization Functions
// =============================================================================

fn create_sync_primitives() {
    hprintln!("[Init] Creating mutex...");

    unsafe {
        MUTEX = Mutex::new(0);
        if MUTEX.is_some() {
            hprintln!("[Init] Mutex created successfully");
        } else {
            hprintln!("[Init] ERROR: Failed to create mutex!");
        }
    }

    hprintln!("[Init] Creating binary semaphore...");

    unsafe {
        SEMAPHORE = BinarySemaphore::new();
        if SEMAPHORE.is_some() {
            hprintln!("[Init] Semaphore created successfully");
        } else {
            hprintln!("[Init] ERROR: Failed to create semaphore!");
        }
    }
}

fn create_stream_buffer() {
    hprintln!("[Init] Creating stream buffer (128 bytes, trigger=1)...");

    unsafe {
        STREAM_BUFFER = StreamBuffer::new(128, 1);
        if STREAM_BUFFER.is_some() {
            hprintln!("[Init] Stream buffer created successfully");
        } else {
            hprintln!("[Init] ERROR: Failed to create stream buffer!");
        }
    }
}

fn create_event_group() {
    hprintln!("[Init] Creating event group...");

    unsafe {
        EVENT_GROUP = EventGroup::new();
        if EVENT_GROUP.is_some() {
            hprintln!("[Init] Event group created successfully");
        } else {
            hprintln!("[Init] ERROR: Failed to create event group!");
        }
    }
}

fn create_tasks() {
    hprintln!("[Init] Creating tasks...");

    unsafe {
        let result = TaskHandle::spawn_static(
            b"LowTask\0",
            &mut LOW_TASK_STACK,
            &mut LOW_TASK_TCB,
            PRIORITY_LOW,
            task_low_priority,
        );
        if result.is_some() {
            hprintln!("[Init] Low priority task created (priority {})", PRIORITY_LOW);
        } else {
            hprintln!("[Init] ERROR: Failed to create low priority task!");
        }

        let result = TaskHandle::spawn_static(
            b"MedTask\0",
            &mut MEDIUM_TASK_STACK,
            &mut MEDIUM_TASK_TCB,
            PRIORITY_MEDIUM,
            task_medium_priority,
        );
        if result.is_some() {
            hprintln!("[Init] Medium priority task created (priority {})", PRIORITY_MEDIUM);
        } else {
            hprintln!("[Init] ERROR: Failed to create medium priority task!");
        }

        let result = TaskHandle::spawn_static(
            b"HighTask\0",
            &mut HIGH_TASK_STACK,
            &mut HIGH_TASK_TCB,
            PRIORITY_HIGH,
            task_high_priority,
        );
        if result.is_some() {
            hprintln!("[Init] High priority task created (priority {})", PRIORITY_HIGH);
        } else {
            hprintln!("[Init] ERROR: Failed to create high priority task!");
        }

        let result = TaskHandle::spawn_static(
            b"SemWait\0",
            &mut SEM_WAITER_STACK,
            &mut SEM_WAITER_TCB,
            PRIORITY_MEDIUM,
            task_semaphore_waiter,
        );
        if result.is_some() {
            hprintln!("[Init] Semaphore waiter task created (priority {})", PRIORITY_MEDIUM);
        } else {
            hprintln!("[Init] ERROR: Failed to create semaphore waiter task!");
        }

        let result = TaskHandle::spawn_static(
            b"Producer\0",
            &mut PRODUCER_TASK_STACK,
            &mut PRODUCER_TASK_TCB,
            PRIORITY_LOW,
            task_stream_producer,
        );
        if result.is_some() {
            hprintln!("[Init] Stream producer task created (priority {})", PRIORITY_LOW);
        } else {
            hprintln!("[Init] ERROR: Failed to create producer task!");
        }

        let result = TaskHandle::spawn_static(
            b"Consumer\0",
            &mut CONSUMER_TASK_STACK,
            &mut CONSUMER_TASK_TCB,
            PRIORITY_MEDIUM,
            task_stream_consumer,
        );
        if result.is_some() {
            hprintln!("[Init] Stream consumer task created (priority {})", PRIORITY_MEDIUM);
        } else {
            hprintln!("[Init] ERROR: Failed to create consumer task!");
        }

        let result = TaskHandle::spawn_static(
            b"EvtSend\0",
            &mut EVENT_SENDER_STACK,
            &mut EVENT_SENDER_TCB,
            PRIORITY_LOW,
            task_event_sender,
        );
        if result.is_some() {
            hprintln!("[Init] Event sender task created (priority {})", PRIORITY_LOW);
        } else {
            hprintln!("[Init] ERROR: Failed to create event sender task!");
        }

        let result = TaskHandle::spawn_static(
            b"EvtWait\0",
            &mut EVENT_WAITER_STACK,
            &mut EVENT_WAITER_TCB,
            PRIORITY_MEDIUM,
            task_event_waiter,
        );
        if result.is_some() {
            hprintln!("[Init] Event waiter task created (priority {})", PRIORITY_MEDIUM);
        } else {
            hprintln!("[Init] ERROR: Failed to create event waiter task!");
        }

        let result = TaskHandle::spawn_static(
            b"RunStats\0",
            &mut RUNTIME_STATS_STACK,
            &mut RUNTIME_STATS_TCB,
            PRIORITY_LOW,
            task_runtime_stats,
        );
        if result.is_some() {
            hprintln!("[Init] Runtime stats task created (priority {})", PRIORITY_LOW);
        } else {
            hprintln!("[Init] ERROR: Failed to create runtime stats task!");
        }
    }
}

fn create_timer() {
    hprintln!("[Init] Creating software timer...");

    unsafe {
        TIMER = Timer::new_periodic(
            b"Timer1\0",
            pdMS_TO_TICKS(1000),
            timer_callback,
        );

        if let Some(ref timer) = TIMER {
            hprintln!("[Init] Timer created successfully");
            timer.start();
        } else {
            hprintln!("[Init] ERROR: Failed to create timer!");
        }
    }
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

extern "C" fn task_low_priority(_pvParameters: *mut c_void) {
    let mut iteration: u32 = 0;

    loop {
        iteration += 1;
        hprintln!("");
        hprintln!("[Low #{}] Attempting to take mutex...", iteration);

        unsafe {
            if let Some(ref mutex) = MUTEX {
                let mut guard = mutex.lock();
                hprintln!("[Low #{}] Mutex acquired! Doing work...", iteration);

                for _ in 0..3 {
                    *guard += 1;
                    let count = *guard;
                    hprintln!("[Low #{}] Working... counter = {}", iteration, count);
                    vTaskDelay(pdMS_TO_TICKS(100));
                }

                hprintln!("[Low #{}] Signaling semaphore...", iteration);
                if let Some(ref sem) = SEMAPHORE {
                    sem.give();
                }

                hprintln!("[Low #{}] Releasing mutex", iteration);
            }
        }

        vTaskDelay(pdMS_TO_TICKS(500));
    }
}

extern "C" fn task_medium_priority(_pvParameters: *mut c_void) {
    let mut iteration: u32 = 0;

    vTaskDelay(pdMS_TO_TICKS(50));

    loop {
        iteration += 1;
        hprintln!("[Med #{}] Running", iteration);
        vTaskDelay(pdMS_TO_TICKS(200));
    }
}

extern "C" fn task_high_priority(_pvParameters: *mut c_void) {
    let mut iteration: u32 = 0;

    loop {
        iteration += 1;

        vTaskDelay(pdMS_TO_TICKS(150));

        hprintln!("[High #{}] Attempting to take mutex (should trigger priority inheritance)...", iteration);

        unsafe {
            if let Some(ref mutex) = MUTEX {
                let mut guard = mutex.lock();
                hprintln!("[High #{}] Mutex acquired!", iteration);

                *guard += 10;
                let count = *guard;
                hprintln!("[High #{}] Counter now = {}", iteration, count);

                hprintln!("[High #{}] Releasing mutex", iteration);
            }
        }

        vTaskDelay(pdMS_TO_TICKS(1000));
    }
}

extern "C" fn task_semaphore_waiter(_pvParameters: *mut c_void) {
    let mut wakeups: u32 = 0;

    loop {
        hprintln!("[SemWait] Waiting for semaphore...");

        unsafe {
            if let Some(ref sem) = SEMAPHORE {
                sem.take();
                wakeups += 1;
                hprintln!("[SemWait] Semaphore received! Wakeup #{}", wakeups);

                if let Some(ref mutex) = MUTEX {
                    let guard = mutex.lock();
                    let count = *guard;
                    hprintln!("[SemWait] Current counter value: {}", count);
                }
            }
        }
    }
}

extern "C" fn task_stream_producer(_pvParameters: *mut c_void) {
    let mut sequence: u8 = 0;
    let mut iteration: u32 = 0;

    vTaskDelay(pdMS_TO_TICKS(200));

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

        unsafe {
            if let Some(ref stream) = STREAM_BUFFER {
                let space = stream.spaces();
                hprintln!("[Producer #{}] Sending 8 bytes (seq={}), space={}", iteration, sequence, space);

                let sent = stream.send_timeout(&message, pdMS_TO_TICKS(100));

                if sent == message.len() {
                    hprintln!("[Producer #{}] Sent {} bytes successfully", iteration, sent);
                } else {
                    hprintln!("[Producer #{}] Only sent {} of {} bytes (buffer full?)", iteration, sent, message.len());
                }
            }
        }

        sequence = sequence.wrapping_add(8);
        vTaskDelay(pdMS_TO_TICKS(750));
    }
}

extern "C" fn task_stream_consumer(_pvParameters: *mut c_void) {
    let mut total_received: u32 = 0;
    let mut iteration: u32 = 0;
    let mut buffer: [u8; 32] = [0u8; 32];

    vTaskDelay(pdMS_TO_TICKS(500));

    loop {
        iteration += 1;

        unsafe {
            if let Some(ref stream) = STREAM_BUFFER {
                let available = stream.available();
                hprintln!("[Consumer #{}] Waiting for data, available={}", iteration, available);

                let received = stream.receive_timeout(&mut buffer, pdMS_TO_TICKS(2000));

                if received > 0 {
                    total_received += received as u32;
                    hprintln!("[Consumer #{}] Received {} bytes, total={}", iteration, received, total_received);

                    if received >= 4 {
                        hprintln!("[Consumer #{}] Data: [{}, {}, {}, {}, ...]",
                            iteration, buffer[0], buffer[1], buffer[2], buffer[3]);
                    }
                } else {
                    hprintln!("[Consumer #{}] Timeout - no data received", iteration);
                }
            }
        }

        vTaskDelay(pdMS_TO_TICKS(100));
    }
}

// =============================================================================
// Event Group Constants
// =============================================================================

const EVENT_BIT_DATA_READY: EventBits_t = 1 << 0;
const EVENT_BIT_ACK: EventBits_t = 1 << 1;

extern "C" fn task_event_sender(_pvParameters: *mut c_void) {
    let mut iteration: u32 = 0;

    vTaskDelay(pdMS_TO_TICKS(300));

    loop {
        iteration += 1;

        unsafe {
            if let Some(ref events) = EVENT_GROUP {
                hprintln!("[EvtSend #{}] Setting DATA_READY bit", iteration);
                let bits_after = events.set(EVENT_BIT_DATA_READY);
                hprintln!("[EvtSend #{}] Bits after set: 0x{:02X}", iteration, bits_after);

                hprintln!("[EvtSend #{}] Waiting for ACK bit...", iteration);
                if let Some(bits) = events.wait_any_clear_timeout(EVENT_BIT_ACK, pdMS_TO_TICKS(2000)) {
                    hprintln!("[EvtSend #{}] ACK received! bits=0x{:02X}", iteration, bits);
                } else {
                    hprintln!("[EvtSend #{}] Timeout waiting for ACK", iteration);
                }
            }
        }

        vTaskDelay(pdMS_TO_TICKS(1500));
    }
}

extern "C" fn task_event_waiter(_pvParameters: *mut c_void) {
    let mut iteration: u32 = 0;

    loop {
        iteration += 1;

        unsafe {
            if let Some(ref events) = EVENT_GROUP {
                hprintln!("[EvtWait #{}] Waiting for DATA_READY bit...", iteration);

                if let Some(bits) = events.wait_any_clear_timeout(EVENT_BIT_DATA_READY, pdMS_TO_TICKS(3000)) {
                    hprintln!("[EvtWait #{}] DATA_READY received! bits=0x{:02X}", iteration, bits);

                    hprintln!("[EvtWait #{}] Processing...", iteration);
                    vTaskDelay(pdMS_TO_TICKS(100));

                    hprintln!("[EvtWait #{}] Sending ACK", iteration);
                    events.set(EVENT_BIT_ACK);
                } else {
                    hprintln!("[EvtWait #{}] Timeout - no data ready", iteration);
                }
            }
        }

        vTaskDelay(pdMS_TO_TICKS(50));
    }
}

// =============================================================================
// Runtime Statistics Task
// =============================================================================

extern "C" fn task_runtime_stats(_pvParameters: *mut c_void) {
    use core::ptr;
    let mut iteration: u32 = 0;

    vTaskDelay(pdMS_TO_TICKS(1500));

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

        let my_counter = ulTaskGetRunTimeCounter(ptr::null_mut());
        let my_percent = ulTaskGetRunTimePercent(ptr::null_mut());
        hprintln!("This Task:      {} ticks ({}%)", my_counter, my_percent);

        hprintln!("================================================");
        hprintln!("");

        vTaskDelay(pdMS_TO_TICKS(5000));
    }
}
