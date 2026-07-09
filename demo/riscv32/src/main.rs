//! FreeRusTOS Demo Application - RISC-V RV32
//!
//! This demo demonstrates the FreeRTOS kernel running on RISC-V
//! using the safe Rust wrappers with idiomatic Rust patterns:
//! - Static initialization with contained unsafe blocks
//! - .expect() for error handling on creation
//! - RAII guards for mutex access
//!
//! Target: QEMU sifive_e machine
//! Output is via UART0.

#![no_std]
#![no_main]
#![allow(non_snake_case)]
#![allow(static_mut_refs)] // Task stacks/TCBs must be static mut for FreeRTOS

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
// Shared Resources
//
// These are initialized once before the scheduler starts, then accessed
// read-only by tasks. The unsafe block for initialization is contained to
// the init functions; subsequent access through the wrappers is safe.
// =============================================================================

static mut COUNTER_MUTEX: Option<Mutex<u32>> = None;
static mut SEMAPHORE: Option<BinarySemaphore> = None;
static mut TIMER: Option<Timer> = None;
static mut TIMER_TICKS: u32 = 0;

// Helper macros for safe access to initialized resources
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
    let mutex = Mutex::new(0).expect("Failed to create mutex");
    unsafe { COUNTER_MUTEX = Some(mutex); }
    println("[Init] Mutex created successfully");

    println("[Init] Creating binary semaphore...");
    let sem = BinarySemaphore::new().expect("Failed to create semaphore");
    unsafe { SEMAPHORE = Some(sem); }
    println("[Init] Semaphore created successfully");
}

fn create_tasks() {
    println("[Init] Creating tasks...");

    unsafe {
        TaskHandle::spawn_static(
            b"LowTask\0",
            &mut LOW_TASK_STACK,
            &mut LOW_TASK_TCB,
            PRIORITY_LOW,
            task_low_priority,
        ).expect("Failed to create low priority task");
        println("[Init] Low priority task created");

        TaskHandle::spawn_static(
            b"MedTask\0",
            &mut MEDIUM_TASK_STACK,
            &mut MEDIUM_TASK_TCB,
            PRIORITY_MEDIUM,
            task_medium_priority,
        ).expect("Failed to create medium priority task");
        println("[Init] Medium priority task created");

        TaskHandle::spawn_static(
            b"HighTask\0",
            &mut HIGH_TASK_STACK,
            &mut HIGH_TASK_TCB,
            PRIORITY_HIGH,
            task_high_priority,
        ).expect("Failed to create high priority task");
        println("[Init] High priority task created");
    }
}

fn create_timer() {
    println("[Init] Creating software timer...");
    let timer = Timer::new_periodic(
        b"Timer1\0",
        pdMS_TO_TICKS(1000),
        timer_callback,
    ).expect("Failed to create timer");
    timer.start();
    unsafe { TIMER = Some(timer); }
    println("[Init] Timer created successfully");
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
    let mutex = get_mutex!();
    let semaphore = get_semaphore!();

    loop {
        println("[Low] Taking mutex...");

        {
            let mut guard = mutex.lock();
            println("[Low] Mutex acquired!");

            *guard += 1;
            println("[Low] Working...");

            vTaskDelay(pdMS_TO_TICKS(200));

            semaphore.give();

            println("[Low] Releasing mutex");
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
    let mutex = get_mutex!();

    loop {
        vTaskDelay(pdMS_TO_TICKS(150));

        println("[High] Taking mutex...");

        {
            let mut guard = mutex.lock();
            println("[High] Mutex acquired!");

            *guard += 10;

            println("[High] Releasing mutex");
        }

        vTaskDelay(pdMS_TO_TICKS(1000));
    }
}
