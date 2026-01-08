//! FreeRusTOS Demo Application - RISC-V RV32
//!
//! This demo demonstrates the FreeRTOS kernel running on RISC-V:
//! - Task creation and scheduling
//! - Mutex with priority inheritance
//! - Binary semaphore
//! - Software timers
//!
//! Target: QEMU sifive_e machine
//! Output is via UART0.

#![no_std]
#![no_main]
#![allow(non_snake_case)]
#![allow(static_mut_refs)]

use core::ffi::c_void;
use core::ptr;
use core::panic::PanicInfo;

use riscv_rt::entry;

// Allocator selection via Cargo features
#[cfg(feature = "use-heap-4")]
use freertos_in_rust::memory::FreeRtosAllocator;
#[cfg(feature = "use-heap-4")]
#[global_allocator]
static ALLOCATOR: FreeRtosAllocator = FreeRtosAllocator;

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
// Panic Handler
// =============================================================================

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        unsafe { core::arch::asm!("wfi"); }
    }
}

// =============================================================================
// UART Output (SiFive UART0)
// =============================================================================

/// SiFive UART0 base address (sifive_e machine)
const UART0_BASE: usize = 0x10013000;
const UART_TXDATA: usize = UART0_BASE + 0x00;

/// Write a single byte to UART
fn uart_putc(c: u8) {
    unsafe {
        let txdata = UART_TXDATA as *mut u32;
        // Wait for TX FIFO not full (bit 31 = full)
        while core::ptr::read_volatile(txdata) & 0x8000_0000 != 0 {}
        core::ptr::write_volatile(txdata, c as u32);
    }
}

/// Write a string to UART
fn uart_print(s: &str) {
    for c in s.bytes() {
        uart_putc(c);
    }
}

/// Print a line to UART
fn println(s: &str) {
    uart_print(s);
    uart_putc(b'\r');
    uart_putc(b'\n');
}

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
static mut SHARED_COUNTER: u32 = 0;

/// Timer tick counter
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

const STACK_SIZE: usize = 256; // 1KB per task (RISC-V needs larger stacks)

// Static task stacks and TCBs
static mut LOW_TASK_STACK: [StackType_t; STACK_SIZE] = [0; STACK_SIZE];
static mut LOW_TASK_TCB: StaticTask_t = StaticTask_t::new();

static mut MEDIUM_TASK_STACK: [StackType_t; STACK_SIZE] = [0; STACK_SIZE];
static mut MEDIUM_TASK_TCB: StaticTask_t = StaticTask_t::new();

static mut HIGH_TASK_STACK: [StackType_t; STACK_SIZE] = [0; STACK_SIZE];
static mut HIGH_TASK_TCB: StaticTask_t = StaticTask_t::new();

// =============================================================================
// Entry Point
// =============================================================================

#[entry]
fn main() -> ! {
    println("========================================");
    println("   FreeRusTOS Demo - RISC-V RV32");
    println("========================================");
    println("");

    // Create synchronization primitives
    create_sync_primitives();

    // Create tasks
    create_tasks();

    // Create software timer
    create_timer();

    println("[Main] Starting scheduler...");
    println("");

    // Start the scheduler - this never returns
    vTaskStartScheduler();

    // Should never reach here
    println("[Main] ERROR: Scheduler returned!");
    loop {
        unsafe { core::arch::asm!("wfi"); }
    }
}

// =============================================================================
// Initialization Functions
// =============================================================================

fn create_sync_primitives() {
    println("[Init] Creating mutex...");

    unsafe {
        MUTEX_HANDLE = xSemaphoreCreateMutex();
        if MUTEX_HANDLE.is_null() {
            println("[Init] ERROR: Failed to create mutex!");
        } else {
            println("[Init] Mutex created successfully");
        }
    }

    println("[Init] Creating binary semaphore...");

    unsafe {
        SEMAPHORE_HANDLE = xSemaphoreCreateBinary();
        if SEMAPHORE_HANDLE.is_null() {
            println("[Init] ERROR: Failed to create semaphore!");
        } else {
            println("[Init] Semaphore created successfully");
        }
    }
}

fn create_tasks() {
    println("[Init] Creating tasks...");

    unsafe {
        // Low priority task
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
            println("[Init] ERROR: Failed to create low priority task!");
        } else {
            println("[Init] Low priority task created");
        }

        // Medium priority task
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
            println("[Init] ERROR: Failed to create medium priority task!");
        } else {
            println("[Init] Medium priority task created");
        }

        // High priority task
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
            println("[Init] ERROR: Failed to create high priority task!");
        } else {
            println("[Init] High priority task created");
        }
    }
}

fn create_timer() {
    println("[Init] Creating software timer...");

    unsafe {
        TIMER_HANDLE = xTimerCreate(
            b"Timer1\0".as_ptr(),
            pdMS_TO_TICKS(1000), // 1 second period
            pdTRUE,              // Auto-reload
            ptr::null_mut(),     // Timer ID
            timer_callback,
        );

        if TIMER_HANDLE.is_null() {
            println("[Init] ERROR: Failed to create timer!");
        } else {
            println("[Init] Timer created successfully");
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
    }
    println("[Timer] Tick!");
}

// =============================================================================
// Task Functions
// =============================================================================

extern "C" fn task_low_priority(_pvParameters: *mut c_void) {
    loop {
        println("[Low] Taking mutex...");

        unsafe {
            if xSemaphoreTake(MUTEX_HANDLE, portMAX_DELAY) == pdTRUE {
                println("[Low] Mutex acquired!");

                SHARED_COUNTER += 1;
                println("[Low] Working...");

                vTaskDelay(pdMS_TO_TICKS(200));

                // Signal semaphore
                xSemaphoreGive(SEMAPHORE_HANDLE);

                println("[Low] Releasing mutex");
                xSemaphoreGive(MUTEX_HANDLE);
            }
        }

        vTaskDelay(pdMS_TO_TICKS(500));
    }
}

extern "C" fn task_medium_priority(_pvParameters: *mut c_void) {
    vTaskDelay(pdMS_TO_TICKS(50));

    loop {
        println("[Med] Running");
        vTaskDelay(pdMS_TO_TICKS(300));
    }
}

extern "C" fn task_high_priority(_pvParameters: *mut c_void) {
    loop {
        vTaskDelay(pdMS_TO_TICKS(150));

        println("[High] Taking mutex...");

        unsafe {
            if xSemaphoreTake(MUTEX_HANDLE, portMAX_DELAY) == pdTRUE {
                println("[High] Mutex acquired!");

                SHARED_COUNTER += 10;

                println("[High] Releasing mutex");
                xSemaphoreGive(MUTEX_HANDLE);
            }
        }

        vTaskDelay(pdMS_TO_TICKS(1000));
    }
}

// =============================================================================
// Semaphore Helper Functions
// =============================================================================

unsafe fn xSemaphoreCreateMutex() -> QueueHandle_t {
    xQueueCreateMutex(queueQUEUE_TYPE_MUTEX)
}

unsafe fn xSemaphoreCreateBinary() -> QueueHandle_t {
    xQueueGenericCreate(1, 0, queueQUEUE_TYPE_BINARY_SEMAPHORE)
}

unsafe fn xSemaphoreTake(xSemaphore: QueueHandle_t, xBlockTime: TickType_t) -> BaseType_t {
    xQueueSemaphoreTake(xSemaphore, xBlockTime)
}

unsafe fn xSemaphoreGive(xSemaphore: QueueHandle_t) -> BaseType_t {
    xQueueGenericSend(xSemaphore, ptr::null(), 0, queueSEND_TO_BACK)
}
