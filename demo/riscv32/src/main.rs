//! FreeRusTOS Demo Application - RISC-V RV32
//!
//! This demo demonstrates the FreeRTOS kernel running on RISC-V
//! using the safe Rust wrappers:
//! - TaskHandle::spawn_static() for task creation
//! - Mutex<T> with priority inheritance
//! - BinarySemaphore for signaling
//! - Timer for periodic callbacks
//!
//! Target: QEMU sifive_e machine
//! Output is via UART0.

#![no_std]
#![no_main]
#![allow(non_snake_case)]
#![allow(static_mut_refs)]

use core::ffi::c_void;
use core::panic::PanicInfo;

use riscv_rt::entry;

// Allocator selection via Cargo features
#[cfg(feature = "use-heap-4")]
use freertos_in_rust::memory::FreeRtosAllocator;
#[cfg(feature = "use-heap-4")]
#[global_allocator]
static ALLOCATOR: FreeRtosAllocator = FreeRtosAllocator;

// Import safe wrappers
use freertos_in_rust::sync::{BinarySemaphore, Mutex, TaskHandle, Timer};
use freertos_in_rust::kernel::tasks::{vTaskDelay, StaticTask_t};
use freertos_in_rust::types::*;
use freertos_in_rust::start_scheduler;

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

const UART0_BASE: usize = 0x10013000;
const UART_TXDATA: usize = UART0_BASE + 0x00;

fn uart_putc(c: u8) {
    unsafe {
        let txdata = UART_TXDATA as *mut u32;
        while core::ptr::read_volatile(txdata) & 0x8000_0000 != 0 {}
        core::ptr::write_volatile(txdata, c as u32);
    }
}

fn uart_print(s: &str) {
    for c in s.bytes() { uart_putc(c); }
}

fn println(s: &str) {
    uart_print(s);
    uart_putc(b'\r');
    uart_putc(b'\n');
}

// =============================================================================
// Shared Resources (using safe wrappers)
// =============================================================================

static mut MUTEX: Option<Mutex<u32>> = None;
static mut SEMAPHORE: Option<BinarySemaphore> = None;
static mut TIMER: Option<Timer> = None;
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

const STACK_SIZE: usize = 256;

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

    create_sync_primitives();
    create_tasks();
    create_timer();

    println("[Main] Starting scheduler...");
    println("");

    start_scheduler();

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
        MUTEX = Mutex::new(0);
        if MUTEX.is_some() {
            println("[Init] Mutex created successfully");
        } else {
            println("[Init] ERROR: Failed to create mutex!");
        }
    }

    println("[Init] Creating binary semaphore...");

    unsafe {
        SEMAPHORE = BinarySemaphore::new();
        if SEMAPHORE.is_some() {
            println("[Init] Semaphore created successfully");
        } else {
            println("[Init] ERROR: Failed to create semaphore!");
        }
    }
}

fn create_tasks() {
    println("[Init] Creating tasks...");

    unsafe {
        let result = TaskHandle::spawn_static(
            b"LowTask\0",
            &mut LOW_TASK_STACK,
            &mut LOW_TASK_TCB,
            PRIORITY_LOW,
            task_low_priority,
        );
        if result.is_some() {
            println("[Init] Low priority task created");
        } else {
            println("[Init] ERROR: Failed to create low priority task!");
        }

        let result = TaskHandle::spawn_static(
            b"MedTask\0",
            &mut MEDIUM_TASK_STACK,
            &mut MEDIUM_TASK_TCB,
            PRIORITY_MEDIUM,
            task_medium_priority,
        );
        if result.is_some() {
            println("[Init] Medium priority task created");
        } else {
            println("[Init] ERROR: Failed to create medium priority task!");
        }

        let result = TaskHandle::spawn_static(
            b"HighTask\0",
            &mut HIGH_TASK_STACK,
            &mut HIGH_TASK_TCB,
            PRIORITY_HIGH,
            task_high_priority,
        );
        if result.is_some() {
            println("[Init] High priority task created");
        } else {
            println("[Init] ERROR: Failed to create high priority task!");
        }
    }
}

fn create_timer() {
    println("[Init] Creating software timer...");

    unsafe {
        TIMER = Timer::new_periodic(
            b"Timer1\0",
            pdMS_TO_TICKS(1000),
            timer_callback,
        );

        if let Some(ref timer) = TIMER {
            println("[Init] Timer created successfully");
            timer.start();
        } else {
            println("[Init] ERROR: Failed to create timer!");
        }
    }
}

// =============================================================================
// Timer Callback
// =============================================================================

extern "C" fn timer_callback(_xTimer: TimerHandle_t) {
    unsafe { TIMER_TICKS += 1; }
    println("[Timer] Tick!");
}

// =============================================================================
// Task Functions
// =============================================================================

extern "C" fn task_low_priority(_pvParameters: *mut c_void) {
    loop {
        println("[Low] Taking mutex...");

        unsafe {
            if let Some(ref mutex) = MUTEX {
                let mut guard = mutex.lock();
                println("[Low] Mutex acquired!");

                *guard += 1;
                println("[Low] Working...");

                vTaskDelay(pdMS_TO_TICKS(200));

                if let Some(ref sem) = SEMAPHORE {
                    sem.give();
                }

                println("[Low] Releasing mutex");
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
            if let Some(ref mutex) = MUTEX {
                let mut guard = mutex.lock();
                println("[High] Mutex acquired!");

                *guard += 10;

                println("[High] Releasing mutex");
            }
        }

        vTaskDelay(pdMS_TO_TICKS(1000));
    }
}
