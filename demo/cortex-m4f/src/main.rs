//! FreeRusTOS Demo Application
//!
//! This demo demonstrates the FreeRTOS kernel running on Cortex-M4F:
//! - Task creation and scheduling
//! - Mutex with priority inheritance
//! - Binary semaphore (no inheritance)
//! - Software timers
//!
//! Output is via semihosting - requires a debugger connection.

#![no_std]
#![no_main]
#![allow(non_snake_case)]
#![allow(static_mut_refs)]

extern crate panic_semihosting;

use core::ffi::c_void;
use core::ptr;
use core::sync::atomic::{AtomicU32, Ordering};

use cortex_m_rt::entry;
use cortex_m_semihosting::hprintln;

// Import the port's exception handlers to ensure they're linked.
// The port provides SVCall, PendSV, and SysTick handlers that cortex-m-rt
// will use in the vector table.
use freertos_in_rust::port::{vPortSVCHandler, xPortPendSVHandler, xPortSysTickHandler};

// Force the linker to include the exception handlers.
#[used]
static HANDLERS: [unsafe extern "C" fn(); 3] = [
    vPortSVCHandler,
    xPortPendSVHandler,
    xPortSysTickHandler,
];

use embedded_alloc::LlffHeap as Heap;

// Import FreeRTOS types and functions
use freertos_in_rust::kernel::queue::{
    xQueueCreateMutex, xQueueGenericCreate, xQueueGenericSend, xQueueSemaphoreTake,
    queueQUEUE_TYPE_MUTEX, queueQUEUE_TYPE_BINARY_SEMAPHORE, queueSEND_TO_BACK, QueueHandle_t,
};
use freertos_in_rust::kernel::tasks::{
    xTaskCreateStatic, vTaskStartScheduler, vTaskDelay, StaticTask_t,
};
use freertos_in_rust::kernel::timers::*;
use freertos_in_rust::types::*;

// =============================================================================
// Heap Allocator Setup
// =============================================================================

#[global_allocator]
static HEAP: Heap = Heap::empty();

// Heap memory region
const HEAP_SIZE: usize = 4096;
static mut HEAP_MEM: [u8; HEAP_SIZE] = [0u8; HEAP_SIZE];

// =============================================================================
// Shared Resources
// =============================================================================

/// Mutex handle - protects shared_counter
static mut MUTEX_HANDLE: QueueHandle_t = ptr::null_mut();

/// Semaphore handle - for signaling between tasks
static mut SEMAPHORE_HANDLE: QueueHandle_t = ptr::null_mut();

/// Timer handle - periodic timer
static mut TIMER_HANDLE: TimerHandle_t = ptr::null_mut();

/// Shared counter protected by mutex
static SHARED_COUNTER: AtomicU32 = AtomicU32::new(0);

/// Timer tick counter
static TIMER_TICKS: AtomicU32 = AtomicU32::new(0);

// =============================================================================
// Task Priorities
// =============================================================================

const PRIORITY_LOW: UBaseType_t = 1;
const PRIORITY_MEDIUM: UBaseType_t = 2;
const PRIORITY_HIGH: UBaseType_t = 3;

// =============================================================================
// Task Stack Sizes (in words, not bytes)
// =============================================================================

const STACK_SIZE: usize = 256;

// Static task stacks and TCBs
static mut LOW_TASK_STACK: [StackType_t; STACK_SIZE] = [0; STACK_SIZE];
static mut LOW_TASK_TCB: StaticTask_t = StaticTask_t::new();

static mut MEDIUM_TASK_STACK: [StackType_t; STACK_SIZE] = [0; STACK_SIZE];
static mut MEDIUM_TASK_TCB: StaticTask_t = StaticTask_t::new();

static mut HIGH_TASK_STACK: [StackType_t; STACK_SIZE] = [0; STACK_SIZE];
static mut HIGH_TASK_TCB: StaticTask_t = StaticTask_t::new();

static mut SEM_WAITER_STACK: [StackType_t; STACK_SIZE] = [0; STACK_SIZE];
static mut SEM_WAITER_TCB: StaticTask_t = StaticTask_t::new();

// =============================================================================
// Entry Point
// =============================================================================

#[entry]
fn main() -> ! {
    // Initialize heap allocator
    unsafe {
        HEAP.init(HEAP_MEM.as_ptr() as usize, HEAP_SIZE);
    }

    hprintln!("========================================");
    hprintln!("   FreeRusTOS Demo - Cortex-M4F");
    hprintln!("========================================");
    hprintln!("");

    // Create synchronization primitives
    create_sync_primitives();

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
    let ticks = TIMER_TICKS.fetch_add(1, Ordering::SeqCst) + 1;
    hprintln!("[Timer] Tick #{}", ticks);
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
                    let count = SHARED_COUNTER.fetch_add(1, Ordering::SeqCst) + 1;
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
                let count = SHARED_COUNTER.fetch_add(10, Ordering::SeqCst) + 10;
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
                let count = SHARED_COUNTER.load(Ordering::SeqCst);
                hprintln!("[SemWait] Current counter value: {}", count);
            }
        }
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
