//! FreeRusTOS Demo Application - Cortex-M0
//!
//! This demo demonstrates the FreeRTOS kernel running on Cortex-M0:
//! - Task creation and scheduling
//! - Mutex with priority inheritance
//! - Binary semaphore (no inheritance)
//! - Software timers
//!
//! Note: Cortex-M0 doesn't have atomic instructions, so this demo
//! uses static mut with mutex protection instead of AtomicU32.
//!
//! Output is via semihosting - requires a debugger connection.

#![no_std]
#![no_main]
#![allow(non_snake_case)]
#![allow(static_mut_refs)]

extern crate panic_semihosting;

use core::ffi::c_void;
use core::ptr;

use cortex_m_rt::entry;
use cortex_m_semihosting::hprintln;

// The port provides SVCall, PendSV, and SysTick handlers with the proper
// export names for cortex-m-rt to find them in the vector table.
// No need for explicit linking - cortex-m-rt finds them by name.

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

// Import FreeRTOS types and functions
use freertos_in_rust::kernel::queue::{
    xQueueCreateMutex, xQueueGenericCreate, xQueueGenericSend, xQueueSemaphoreTake,
    queueQUEUE_TYPE_MUTEX, queueQUEUE_TYPE_BINARY_SEMAPHORE, queueSEND_TO_BACK, QueueHandle_t,
};
use freertos_in_rust::kernel::tasks::{
    xTaskCreateStatic, vTaskStartScheduler, vTaskDelay, StaticTask_t,
    ulTaskGetRunTimeCounter, ulTaskGetRunTimePercent,
    ulTaskGetIdleRunTimeCounter, ulTaskGetIdleRunTimePercent,
    ulTaskGetTotalRunTime,
};
use freertos_in_rust::kernel::timers::*;
use freertos_in_rust::kernel::stream_buffer::{
    xStreamBufferCreate, xStreamBufferSend, xStreamBufferReceive,
    xStreamBufferSpacesAvailable, xStreamBufferBytesAvailable,
    StreamBufferHandle_t,
};
use freertos_in_rust::kernel::event_groups::{
    xEventGroupCreate, xEventGroupSetBits, xEventGroupWaitBits,
    EventBits_t,
};
use freertos_in_rust::types::*;

// =============================================================================
// Shared Resources
// =============================================================================

/// Mutex handle - protects shared_counter
static mut MUTEX_HANDLE: QueueHandle_t = ptr::null_mut();

/// Semaphore handle - for signaling between tasks
static mut SEMAPHORE_HANDLE: QueueHandle_t = ptr::null_mut();

/// Timer handle - periodic timer
static mut TIMER_HANDLE: TimerHandle_t = ptr::null_mut();

/// Stream buffer handle - for producer/consumer demo
static mut STREAM_BUFFER_HANDLE: StreamBufferHandle_t = ptr::null_mut();

/// Event group handle - for task synchronization demo
static mut EVENT_GROUP_HANDLE: EventGroupHandle_t = ptr::null_mut();

/// Shared counter protected by mutex
/// Note: On CM0 we use static mut because there are no atomic instructions.
/// The mutex provides synchronization for this counter.
static mut SHARED_COUNTER: u32 = 0;

/// Timer tick counter
/// Note: Only accessed from timer callback context, so static mut is safe.
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
    vTaskStartScheduler();

    // Should never reach here
    hprintln!("[Main] ERROR: Scheduler returned!");
    loop {}
}

// =============================================================================
// Initialization Functions
// =============================================================================

fn create_sync_primitives() {
    hprintln!("[Init] Creating mutex...");

    // Create a mutex (priority inheritance enabled)
    unsafe {
        MUTEX_HANDLE = xSemaphoreCreateMutex();
        if MUTEX_HANDLE.is_null() {
            hprintln!("[Init] ERROR: Failed to create mutex!");
        } else {
            hprintln!("[Init] Mutex created successfully");
        }
    }

    hprintln!("[Init] Creating binary semaphore...");

    // Create a binary semaphore (no priority inheritance)
    unsafe {
        SEMAPHORE_HANDLE = xSemaphoreCreateBinary();
        if SEMAPHORE_HANDLE.is_null() {
            hprintln!("[Init] ERROR: Failed to create semaphore!");
        } else {
            hprintln!("[Init] Semaphore created successfully");
        }
    }
}

fn create_stream_buffer() {
    hprintln!("[Init] Creating stream buffer (128 bytes, trigger=1)...");

    unsafe {
        // Create a stream buffer: 128 bytes capacity, trigger level 1
        // Trigger level 1 means receiver unblocks as soon as any data arrives
        STREAM_BUFFER_HANDLE = xStreamBufferCreate(128, 1);
        if STREAM_BUFFER_HANDLE.is_null() {
            hprintln!("[Init] ERROR: Failed to create stream buffer!");
        } else {
            hprintln!("[Init] Stream buffer created successfully");
        }
    }
}

fn create_event_group() {
    hprintln!("[Init] Creating event group...");

    unsafe {
        EVENT_GROUP_HANDLE = xEventGroupCreate();
        if EVENT_GROUP_HANDLE.is_null() {
            hprintln!("[Init] ERROR: Failed to create event group!");
        } else {
            hprintln!("[Init] Event group created successfully");
        }
    }
}

fn create_tasks() {
    hprintln!("[Init] Creating tasks...");

    unsafe {
        // Low priority task - takes mutex, does work, signals semaphore
        let result = xTaskCreateStatic(
            task_low_priority,
            b"LowTask\0".as_ptr(),
            STACK_SIZE,
            ptr::null_mut(),
            PRIORITY_LOW,
            LOW_TASK_STACK.as_mut_ptr(),
            &mut LOW_TASK_TCB as *mut StaticTask_t,
        );
        if result.is_null() {
            hprintln!("[Init] ERROR: Failed to create low priority task!");
        } else {
            hprintln!("[Init] Low priority task created (priority {})", PRIORITY_LOW);
        }

        // Medium priority task - tries to run during mutex contention
        let result = xTaskCreateStatic(
            task_medium_priority,
            b"MedTask\0".as_ptr(),
            STACK_SIZE,
            ptr::null_mut(),
            PRIORITY_MEDIUM,
            MEDIUM_TASK_STACK.as_mut_ptr(),
            &mut MEDIUM_TASK_TCB as *mut StaticTask_t,
        );
        if result.is_null() {
            hprintln!("[Init] ERROR: Failed to create medium priority task!");
        } else {
            hprintln!("[Init] Medium priority task created (priority {})", PRIORITY_MEDIUM);
        }

        // High priority task - takes mutex (causes priority inheritance)
        let result = xTaskCreateStatic(
            task_high_priority,
            b"HighTask\0".as_ptr(),
            STACK_SIZE,
            ptr::null_mut(),
            PRIORITY_HIGH,
            HIGH_TASK_STACK.as_mut_ptr(),
            &mut HIGH_TASK_TCB as *mut StaticTask_t,
        );
        if result.is_null() {
            hprintln!("[Init] ERROR: Failed to create high priority task!");
        } else {
            hprintln!("[Init] High priority task created (priority {})", PRIORITY_HIGH);
        }

        // Semaphore waiter task - waits on binary semaphore
        let result = xTaskCreateStatic(
            task_semaphore_waiter,
            b"SemWait\0".as_ptr(),
            STACK_SIZE,
            ptr::null_mut(),
            PRIORITY_MEDIUM,
            SEM_WAITER_STACK.as_mut_ptr(),
            &mut SEM_WAITER_TCB as *mut StaticTask_t,
        );
        if result.is_null() {
            hprintln!("[Init] ERROR: Failed to create semaphore waiter task!");
        } else {
            hprintln!("[Init] Semaphore waiter task created (priority {})", PRIORITY_MEDIUM);
        }

        // Stream buffer producer task - sends data to stream buffer
        let result = xTaskCreateStatic(
            task_stream_producer,
            b"Producer\0".as_ptr(),
            STACK_SIZE,
            ptr::null_mut(),
            PRIORITY_LOW,
            PRODUCER_TASK_STACK.as_mut_ptr(),
            &mut PRODUCER_TASK_TCB as *mut StaticTask_t,
        );
        if result.is_null() {
            hprintln!("[Init] ERROR: Failed to create producer task!");
        } else {
            hprintln!("[Init] Stream producer task created (priority {})", PRIORITY_LOW);
        }

        // Stream buffer consumer task - receives data from stream buffer
        let result = xTaskCreateStatic(
            task_stream_consumer,
            b"Consumer\0".as_ptr(),
            STACK_SIZE,
            ptr::null_mut(),
            PRIORITY_MEDIUM,
            CONSUMER_TASK_STACK.as_mut_ptr(),
            &mut CONSUMER_TASK_TCB as *mut StaticTask_t,
        );
        if result.is_null() {
            hprintln!("[Init] ERROR: Failed to create consumer task!");
        } else {
            hprintln!("[Init] Stream consumer task created (priority {})", PRIORITY_MEDIUM);
        }

        // Event group sender task - sets event bits
        let result = xTaskCreateStatic(
            task_event_sender,
            b"EvtSend\0".as_ptr(),
            STACK_SIZE,
            ptr::null_mut(),
            PRIORITY_LOW,
            EVENT_SENDER_STACK.as_mut_ptr(),
            &mut EVENT_SENDER_TCB as *mut StaticTask_t,
        );
        if result.is_null() {
            hprintln!("[Init] ERROR: Failed to create event sender task!");
        } else {
            hprintln!("[Init] Event sender task created (priority {})", PRIORITY_LOW);
        }

        // Event group waiter task - waits for event bits
        let result = xTaskCreateStatic(
            task_event_waiter,
            b"EvtWait\0".as_ptr(),
            STACK_SIZE,
            ptr::null_mut(),
            PRIORITY_MEDIUM,
            EVENT_WAITER_STACK.as_mut_ptr(),
            &mut EVENT_WAITER_TCB as *mut StaticTask_t,
        );
        if result.is_null() {
            hprintln!("[Init] ERROR: Failed to create event waiter task!");
        } else {
            hprintln!("[Init] Event waiter task created (priority {})", PRIORITY_MEDIUM);
        }

        // Runtime statistics task - periodically prints CPU usage
        let result = xTaskCreateStatic(
            task_runtime_stats,
            b"RunStats\0".as_ptr(),
            STACK_SIZE,
            ptr::null_mut(),
            PRIORITY_LOW,
            RUNTIME_STATS_STACK.as_mut_ptr(),
            &mut RUNTIME_STATS_TCB as *mut StaticTask_t,
        );
        if result.is_null() {
            hprintln!("[Init] ERROR: Failed to create runtime stats task!");
        } else {
            hprintln!("[Init] Runtime stats task created (priority {})", PRIORITY_LOW);
        }
    }
}

fn create_timer() {
    hprintln!("[Init] Creating software timer...");

    unsafe {
        // Create a 1-second periodic timer
        TIMER_HANDLE = xTimerCreate(
            b"Timer1\0".as_ptr(),
            pdMS_TO_TICKS(1000), // 1 second period
            pdTRUE,              // Auto-reload
            ptr::null_mut(),     // Timer ID
            timer_callback,
        );

        if TIMER_HANDLE.is_null() {
            hprintln!("[Init] ERROR: Failed to create timer!");
        } else {
            hprintln!("[Init] Timer created successfully");
            // Start the timer
            xTimerStart(TIMER_HANDLE, 0);
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

/// Low priority task
///
/// This task:
/// 1. Takes the mutex
/// 2. Does some "work" while holding it
/// 3. Signals the semaphore
/// 4. Releases the mutex
///
/// When the high priority task also tries to take the mutex,
/// this task's priority should be boosted (priority inheritance).
extern "C" fn task_low_priority(_pvParameters: *mut c_void) {
    let mut iteration: u32 = 0;

    loop {
        iteration += 1;
        hprintln!("");
        hprintln!("[Low #{}] Attempting to take mutex...", iteration);

        unsafe {
            // Take the mutex (block indefinitely)
            if xSemaphoreTake(MUTEX_HANDLE, portMAX_DELAY) == pdTRUE {
                hprintln!("[Low #{}] Mutex acquired! Doing work...", iteration);

                // Simulate work while holding the mutex
                // During this time, if high priority task tries to take mutex,
                // we should see priority inheritance
                for _ in 0..3 {
                    SHARED_COUNTER += 1;
                    let count = SHARED_COUNTER;
                    hprintln!("[Low #{}] Working... counter = {}", iteration, count);

                    // Small delay to give other tasks a chance to try for mutex
                    vTaskDelay(pdMS_TO_TICKS(100));
                }

                // Signal the semaphore (wakes up semaphore waiter)
                hprintln!("[Low #{}] Signaling semaphore...", iteration);
                xSemaphoreGive(SEMAPHORE_HANDLE);

                // Release the mutex
                hprintln!("[Low #{}] Releasing mutex", iteration);
                xSemaphoreGive(MUTEX_HANDLE);
            }
        }

        // Wait before next iteration
        vTaskDelay(pdMS_TO_TICKS(500));
    }
}

/// Medium priority task
///
/// This task runs at medium priority and just does periodic work.
/// When the low priority task has its priority boosted due to
/// mutex contention with the high priority task, this medium task
/// should NOT be able to preempt it (demonstrating priority inheritance).
extern "C" fn task_medium_priority(_pvParameters: *mut c_void) {
    let mut iteration: u32 = 0;

    // Initial delay to let low priority task start
    vTaskDelay(pdMS_TO_TICKS(50));

    loop {
        iteration += 1;
        hprintln!("[Med #{}] Running", iteration);

        // Just delay - we're checking that this task doesn't
        // preempt the boosted low priority task
        vTaskDelay(pdMS_TO_TICKS(200));
    }
}

/// High priority task
///
/// This task:
/// 1. Waits a bit to let low priority task take mutex first
/// 2. Tries to take the mutex
/// 3. This should trigger priority inheritance in low priority task
/// 4. Eventually gets the mutex when low releases it
extern "C" fn task_high_priority(_pvParameters: *mut c_void) {
    let mut iteration: u32 = 0;

    loop {
        iteration += 1;

        // Wait to let low priority task take mutex first
        vTaskDelay(pdMS_TO_TICKS(150));

        hprintln!("[High #{}] Attempting to take mutex (should trigger priority inheritance)...", iteration);

        unsafe {
            // Try to take mutex - low priority task currently holds it
            // This should cause low priority task's priority to be boosted
            if xSemaphoreTake(MUTEX_HANDLE, portMAX_DELAY) == pdTRUE {
                hprintln!("[High #{}] Mutex acquired!", iteration);

                // Increment counter
                SHARED_COUNTER += 10;
                let count = SHARED_COUNTER;
                hprintln!("[High #{}] Counter now = {}", iteration, count);

                // Release mutex
                hprintln!("[High #{}] Releasing mutex", iteration);
                xSemaphoreGive(MUTEX_HANDLE);
            }
        }

        // Wait before next iteration
        vTaskDelay(pdMS_TO_TICKS(1000));
    }
}

/// Semaphore waiter task
///
/// This task waits on a binary semaphore (no priority inheritance).
/// When signaled by the low priority task, it wakes up and runs.
extern "C" fn task_semaphore_waiter(_pvParameters: *mut c_void) {
    let mut wakeups: u32 = 0;

    loop {
        hprintln!("[SemWait] Waiting for semaphore...");

        unsafe {
            // Wait on semaphore (block indefinitely)
            // Note: Semaphores do NOT have priority inheritance
            if xSemaphoreTake(SEMAPHORE_HANDLE, portMAX_DELAY) == pdTRUE {
                wakeups += 1;
                hprintln!("[SemWait] Semaphore received! Wakeup #{}", wakeups);

                // Read the shared counter
                let count = SHARED_COUNTER;
                hprintln!("[SemWait] Current counter value: {}", count);
            }
        }
    }
}

/// Stream buffer producer task
///
/// Sends incrementing byte patterns to the stream buffer every 750ms.
/// Demonstrates stream buffer send functionality.
extern "C" fn task_stream_producer(_pvParameters: *mut c_void) {
    let mut sequence: u8 = 0;
    let mut iteration: u32 = 0;

    // Initial delay to let other tasks start
    vTaskDelay(pdMS_TO_TICKS(200));

    loop {
        iteration += 1;

        // Create a small message to send (8 bytes)
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
            let space = xStreamBufferSpacesAvailable(STREAM_BUFFER_HANDLE);
            hprintln!("[Producer #{}] Sending 8 bytes (seq={}), space={}", iteration, sequence, space);

            let sent = xStreamBufferSend(
                STREAM_BUFFER_HANDLE,
                message.as_ptr() as *const c_void,
                message.len(),
                pdMS_TO_TICKS(100), // Short timeout
            );

            if sent == message.len() {
                hprintln!("[Producer #{}] Sent {} bytes successfully", iteration, sent);
            } else {
                hprintln!("[Producer #{}] Only sent {} of {} bytes (buffer full?)", iteration, sent, message.len());
            }
        }

        sequence = sequence.wrapping_add(8);

        // Wait before sending next message
        vTaskDelay(pdMS_TO_TICKS(750));
    }
}

/// Stream buffer consumer task
///
/// Receives data from the stream buffer and validates the byte pattern.
/// Demonstrates stream buffer receive functionality.
extern "C" fn task_stream_consumer(_pvParameters: *mut c_void) {
    let mut total_received: u32 = 0;
    let mut iteration: u32 = 0;
    let mut buffer: [u8; 32] = [0u8; 32];

    // Initial delay
    vTaskDelay(pdMS_TO_TICKS(500));

    loop {
        iteration += 1;

        unsafe {
            let available = xStreamBufferBytesAvailable(STREAM_BUFFER_HANDLE);
            hprintln!("[Consumer #{}] Waiting for data, available={}", iteration, available);

            // Receive with timeout
            let received = xStreamBufferReceive(
                STREAM_BUFFER_HANDLE,
                buffer.as_mut_ptr() as *mut c_void,
                buffer.len(),
                pdMS_TO_TICKS(2000), // Wait up to 2 seconds
            );

            if received > 0 {
                total_received += received as u32;
                hprintln!("[Consumer #{}] Received {} bytes, total={}", iteration, received, total_received);

                // Print first few bytes received
                if received >= 4 {
                    hprintln!("[Consumer #{}] Data: [{}, {}, {}, {}, ...]",
                        iteration, buffer[0], buffer[1], buffer[2], buffer[3]);
                }
            } else {
                hprintln!("[Consumer #{}] Timeout - no data received", iteration);
            }
        }

        // Small delay before next receive
        vTaskDelay(pdMS_TO_TICKS(100));
    }
}

// =============================================================================
// Event Group Constants
// =============================================================================

/// Event bit 0: Data is ready
const EVENT_BIT_DATA_READY: EventBits_t = 1 << 0;
/// Event bit 1: Processing complete / acknowledgment
const EVENT_BIT_ACK: EventBits_t = 1 << 1;

/// Event group sender task
///
/// Periodically sets event bits to signal the waiter task.
/// Demonstrates xEventGroupSetBits functionality.
extern "C" fn task_event_sender(_pvParameters: *mut c_void) {
    let mut iteration: u32 = 0;

    // Initial delay to let waiter task start first
    vTaskDelay(pdMS_TO_TICKS(300));

    loop {
        iteration += 1;

        unsafe {
            // Set the "data ready" bit
            hprintln!("[EvtSend #{}] Setting DATA_READY bit", iteration);
            let bits_before = xEventGroupSetBits(EVENT_GROUP_HANDLE, EVENT_BIT_DATA_READY);
            hprintln!("[EvtSend #{}] Bits after set: 0x{:02X}", iteration, bits_before);

            // Wait for acknowledgment (waiter will set ACK bit)
            hprintln!("[EvtSend #{}] Waiting for ACK bit...", iteration);
            let bits = xEventGroupWaitBits(
                EVENT_GROUP_HANDLE,
                EVENT_BIT_ACK,
                pdTRUE,  // Clear ACK bit on exit
                pdFALSE, // Wait for ANY bit (OR)
                pdMS_TO_TICKS(2000),
            );

            if (bits & EVENT_BIT_ACK) != 0 {
                hprintln!("[EvtSend #{}] ACK received! bits=0x{:02X}", iteration, bits);
            } else {
                hprintln!("[EvtSend #{}] Timeout waiting for ACK", iteration);
            }
        }

        // Wait before next cycle
        vTaskDelay(pdMS_TO_TICKS(1500));
    }
}

/// Event group waiter task
///
/// Waits for event bits and responds with acknowledgment.
/// Demonstrates xEventGroupWaitBits with clear-on-exit.
extern "C" fn task_event_waiter(_pvParameters: *mut c_void) {
    let mut iteration: u32 = 0;

    loop {
        iteration += 1;

        unsafe {
            hprintln!("[EvtWait #{}] Waiting for DATA_READY bit...", iteration);

            // Wait for data ready bit (clear it on exit)
            let bits = xEventGroupWaitBits(
                EVENT_GROUP_HANDLE,
                EVENT_BIT_DATA_READY,
                pdTRUE,  // Clear DATA_READY on exit
                pdFALSE, // Wait for ANY bit (OR)
                pdMS_TO_TICKS(3000),
            );

            if (bits & EVENT_BIT_DATA_READY) != 0 {
                hprintln!("[EvtWait #{}] DATA_READY received! bits=0x{:02X}", iteration, bits);

                // Simulate processing
                hprintln!("[EvtWait #{}] Processing...", iteration);
                vTaskDelay(pdMS_TO_TICKS(100));

                // Send acknowledgment
                hprintln!("[EvtWait #{}] Sending ACK", iteration);
                let _ = xEventGroupSetBits(EVENT_GROUP_HANDLE, EVENT_BIT_ACK);
            } else {
                hprintln!("[EvtWait #{}] Timeout - no data ready", iteration);
            }
        }

        // Small delay
        vTaskDelay(pdMS_TO_TICKS(50));
    }
}

// =============================================================================
// Runtime Statistics Task
// =============================================================================

/// Runtime statistics task
///
/// Periodically prints CPU usage statistics for all tasks.
/// Demonstrates the generate-run-time-stats feature.
extern "C" fn task_runtime_stats(_pvParameters: *mut c_void) {
    let mut iteration: u32 = 0;

    // Let system run for a bit before collecting stats
    vTaskDelay(pdMS_TO_TICKS(1500));

    loop {
        iteration += 1;

        hprintln!("");
        hprintln!("========== Run-Time Statistics (#{}) ==========", iteration);

        // Get total run time
        let total_time = ulTaskGetTotalRunTime();
        hprintln!("Total Run Time: {} ticks", total_time);

        // Get idle task stats
        let idle_counter = ulTaskGetIdleRunTimeCounter();
        let idle_percent = ulTaskGetIdleRunTimePercent();
        hprintln!("Idle Task:      {} ticks ({}%)", idle_counter, idle_percent);

        // Calculate non-idle CPU usage
        let cpu_usage = 100 - idle_percent;
        hprintln!("CPU Usage:      {}%", cpu_usage);

        // Get current task's runtime (this task)
        let my_counter = ulTaskGetRunTimeCounter(ptr::null_mut());
        let my_percent = ulTaskGetRunTimePercent(ptr::null_mut());
        hprintln!("This Task:      {} ticks ({}%)", my_counter, my_percent);

        hprintln!("================================================");
        hprintln!("");

        // Wait 5 seconds before next stats report
        vTaskDelay(pdMS_TO_TICKS(5000));
    }
}

// =============================================================================
// Semaphore Helper Functions
// =============================================================================

/// Create a mutex (calls xQueueCreateMutex)
unsafe fn xSemaphoreCreateMutex() -> QueueHandle_t {
    xQueueCreateMutex(queueQUEUE_TYPE_MUTEX)
}

/// Create a binary semaphore
unsafe fn xSemaphoreCreateBinary() -> QueueHandle_t {
    xQueueGenericCreate(1, 0, queueQUEUE_TYPE_BINARY_SEMAPHORE)
}

/// Take a semaphore/mutex
unsafe fn xSemaphoreTake(xSemaphore: QueueHandle_t, xBlockTime: TickType_t) -> BaseType_t {
    xQueueSemaphoreTake(xSemaphore, xBlockTime)
}

/// Give a semaphore/mutex
unsafe fn xSemaphoreGive(xSemaphore: QueueHandle_t) -> BaseType_t {
    xQueueGenericSend(xSemaphore, ptr::null(), 0, queueSEND_TO_BACK)
}
