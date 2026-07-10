//! FreeRTOS-in-Rust Demo Application - ARM Cortex-A9
//!
//! This demo demonstrates the FreeRTOS kernel running on Cortex-A9
//! using the safe Rust wrappers with idiomatic Rust patterns:
//! - Static initialization with contained unsafe blocks
//! - .expect() for error handling on creation
//! - RAII guards for mutex access
//!
//! Target: QEMU vexpress-a9 machine
//! Output is via UART0 (PL011).

#![no_std]
#![no_main]
#![allow(non_snake_case)]
#![allow(static_mut_refs)] // Task stacks/TCBs must be static mut for FreeRTOS

use core::ffi::c_void;
use core::panic::PanicInfo;
use core::ptr;

// Allocator selection via Cargo features
#[cfg(feature = "use-heap-4")]
use freertos_in_rust::memory::FreeRtosAllocator;
#[cfg(feature = "use-heap-4")]
#[global_allocator]
static ALLOCATOR: FreeRtosAllocator = FreeRtosAllocator;

// Import safe wrappers
use freertos_in_rust::kernel::tasks::StaticTask_t;
use freertos_in_rust::start_scheduler;
use freertos_in_rust::sync::{BinarySemaphore, Mutex, TaskContext, TaskHandle, Timer};
use freertos_in_rust::types::*;

// =============================================================================
// Hardware Addresses for vexpress-a9
// =============================================================================

const UART0_BASE: usize = 0x1000_9000;
const UART_DR: usize = UART0_BASE + 0x00;
const UART_FR: usize = UART0_BASE + 0x18;

#[allow(dead_code)]
const TIMER0_BASE: usize = 0x1001_1000;
#[allow(dead_code)]
const TIMER_LOAD: usize = TIMER0_BASE + 0x00;
#[allow(dead_code)]
const TIMER_VALUE: usize = TIMER0_BASE + 0x04;
#[allow(dead_code)]
const TIMER_CONTROL: usize = TIMER0_BASE + 0x08;
#[allow(dead_code)]
const TIMER_INTCLR: usize = TIMER0_BASE + 0x0C;

const GIC_DIST_BASE: usize = 0x1E00_1000;
const GIC_CPU_BASE: usize = 0x1E00_0100;

const GICD_CTLR: usize = GIC_DIST_BASE + 0x000;
const GICD_ISENABLER: usize = GIC_DIST_BASE + 0x100;
const GICD_IPRIORITYR: usize = GIC_DIST_BASE + 0x400;
const GICD_ITARGETSR: usize = GIC_DIST_BASE + 0x800;

#[allow(dead_code)]
const GICC_CTLR: usize = GIC_CPU_BASE + 0x00;
#[allow(dead_code)]
const GICC_PMR: usize = GIC_CPU_BASE + 0x04;
#[allow(dead_code)]
const GICC_IAR: usize = GIC_CPU_BASE + 0x0C;
#[allow(dead_code)]
const GICC_EOIR: usize = GIC_CPU_BASE + 0x10;

const TIMER_IRQ: u32 = 34;

// =============================================================================
// Panic Handler
// =============================================================================

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    println("[PANIC] ");
    if let Some(location) = info.location() {
        print("at ");
        println(location.file());
    }
    loop {
        unsafe {
            core::arch::asm!("wfi");
        }
    }
}

// =============================================================================
// UART Output (PL011)
// =============================================================================

fn uart_putc(c: u8) {
    unsafe {
        while (ptr::read_volatile(UART_FR as *const u32) & (1 << 5)) != 0 {}
        ptr::write_volatile(UART_DR as *mut u32, c as u32);
    }
}

fn print(s: &str) {
    for c in s.bytes() {
        uart_putc(c);
    }
}

fn println(s: &str) {
    print(s);
    uart_putc(b'\r');
    uart_putc(b'\n');
}

#[allow(dead_code)]
fn print_num(mut n: u32) {
    if n == 0 {
        uart_putc(b'0');
        return;
    }
    let mut digits = [0u8; 10];
    let mut i = 0;
    while n > 0 {
        digits[i] = (n % 10) as u8 + b'0';
        n /= 10;
        i += 1;
    }
    while i > 0 {
        i -= 1;
        uart_putc(digits[i]);
    }
}

// =============================================================================
// GIC and Timer Setup
// =============================================================================

fn gic_init() {
    unsafe {
        ptr::write_volatile(GICD_CTLR as *mut u32, 0);
        ptr::write_volatile(GICD_CTLR as *mut u32, 1);
        ptr::write_volatile(GICC_CTLR as *mut u32, 1);
        ptr::write_volatile(GICC_PMR as *mut u32, 0xFF);
    }
}

fn gic_enable_irq(irq: u32) {
    unsafe {
        let reg_offset = (irq / 32) as usize * 4;
        let bit = 1u32 << (irq % 32);
        let addr = (GICD_ISENABLER + reg_offset) as *mut u32;
        ptr::write_volatile(addr, bit);

        let priority_offset = irq as usize;
        let priority_addr = (GICD_IPRIORITYR + priority_offset) as *mut u8;
        ptr::write_volatile(priority_addr, 0xA0);

        let target_offset = irq as usize;
        let target_addr = (GICD_ITARGETSR + target_offset) as *mut u8;
        ptr::write_volatile(target_addr, 0x01);
    }
}

fn timer_init() {
    unsafe {
        ptr::write_volatile(TIMER_CONTROL as *mut u32, 0);
        let reload = 1000 - 1;
        ptr::write_volatile(TIMER_LOAD as *mut u32, reload);
        ptr::write_volatile(TIMER_CONTROL as *mut u32, 0xE2);
        gic_enable_irq(TIMER_IRQ);
    }
}

fn timer_clear_interrupt() {
    unsafe {
        ptr::write_volatile(TIMER_INTCLR as *mut u32, 1);
    }
}

// =============================================================================
// IRQ Handler
// =============================================================================

#[no_mangle]
pub extern "C" fn vApplicationFPUSafeIRQHandler(ulICCIAR: u32) {
    let irq_id = ulICCIAR & 0x3FF;
    if irq_id == TIMER_IRQ {
        timer_clear_interrupt();
        freertos_in_rust::port::FreeRTOS_Tick_Handler();
    }
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
// Task Priorities and Stack
// =============================================================================

const PRIORITY_LOW: UBaseType_t = 1;
const PRIORITY_MEDIUM: UBaseType_t = 2;
const PRIORITY_HIGH: UBaseType_t = 3;

const STACK_SIZE: usize = 512;

static mut LOW_TASK_STACK: [StackType_t; STACK_SIZE] = [0; STACK_SIZE];
static mut LOW_TASK_TCB: StaticTask_t = StaticTask_t::new();

static mut MEDIUM_TASK_STACK: [StackType_t; STACK_SIZE] = [0; STACK_SIZE];
static mut MEDIUM_TASK_TCB: StaticTask_t = StaticTask_t::new();

static mut HIGH_TASK_STACK: [StackType_t; STACK_SIZE] = [0; STACK_SIZE];
static mut HIGH_TASK_TCB: StaticTask_t = StaticTask_t::new();

// =============================================================================
// Entry Point
// =============================================================================

#[no_mangle]
pub extern "C" fn main() -> ! {
    // Safety: startup is single-threaded and owns kernel initialization.
    let context = unsafe { TaskContext::assume() };
    println("========================================");
    println("   FreeRTOS-in-Rust Demo - Cortex-A9");
    println("========================================");
    println("");

    println("[Init] Initializing GIC...");
    gic_init();

    println("[Init] Initializing timer...");
    timer_init();

    println("[Init] Configuring tickless idle...");
    freertos_in_rust::port::vPortConfigureTimerForTickless(TIMER0_BASE, 1000, 0xFFFFFFFF);

    create_sync_primitives(&context);
    create_tasks(&context);
    create_timer(&context);

    println("[Main] Starting scheduler...");
    println("");

    unsafe {
        core::arch::asm!("cpsie i");
    }

    start_scheduler(context)
}

// =============================================================================
// Initialization Functions
// =============================================================================

fn create_sync_primitives(context: &TaskContext) {
    println("[Init] Creating mutex...");
    let mutex = Mutex::new(context, 0).expect("Failed to create mutex");
    unsafe {
        COUNTER_MUTEX = Some(mutex);
    }
    println("[Init] Mutex created successfully");

    println("[Init] Creating binary semaphore...");
    let sem = BinarySemaphore::new(context).expect("Failed to create semaphore");
    unsafe {
        SEMAPHORE = Some(sem);
    }
    println("[Init] Semaphore created successfully");
}

fn create_tasks(context: &TaskContext) {
    println("[Init] Creating tasks...");

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
        println("[Init] Low priority task created");

        TaskHandle::spawn_static(
            context,
            b"MedTask\0",
            &mut MEDIUM_TASK_STACK,
            &mut MEDIUM_TASK_TCB,
            PRIORITY_MEDIUM,
            task_medium_priority,
        )
        .expect("Failed to create medium priority task");
        println("[Init] Medium priority task created");

        TaskHandle::spawn_static(
            context,
            b"HighTask\0",
            &mut HIGH_TASK_STACK,
            &mut HIGH_TASK_TCB,
            PRIORITY_HIGH,
            task_high_priority,
        )
        .expect("Failed to create high priority task");
        println("[Init] High priority task created");
    }
}

fn create_timer(context: &TaskContext) {
    println("[Init] Creating software timer...");
    let timer = Timer::new_periodic(context, c"Timer1", pdMS_TO_TICKS(1000), timer_callback)
        .expect("Failed to create timer");
    assert!(timer.start(context), "Failed to start timer");
    unsafe {
        TIMER = Some(timer);
    }
    println("[Init] Timer created successfully");
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
    // Safety: FreeRTOS invokes task entry points only in task context.
    let context = unsafe { TaskContext::assume() };
    let mutex = get_mutex!();
    let semaphore = get_semaphore!();

    loop {
        println("[Low] Taking mutex...");

        {
            let mut guard = mutex.lock(&context).expect("mutex acquisition failed");
            println("[Low] Mutex acquired!");

            *guard += 1;
            println("[Low] Working...");

            freertos_in_rust::delay(&context, pdMS_TO_TICKS(200));

            assert!(semaphore.give(&context), "semaphore give failed");

            println("[Low] Releasing mutex");
        }

        freertos_in_rust::delay(&context, pdMS_TO_TICKS(500));
    }
}

extern "C" fn task_medium_priority(_pvParameters: *mut c_void) {
    // Safety: FreeRTOS invokes task entry points only in task context.
    let context = unsafe { TaskContext::assume() };
    freertos_in_rust::delay(&context, pdMS_TO_TICKS(50));

    loop {
        println("[Med] Running");
        freertos_in_rust::delay(&context, pdMS_TO_TICKS(300));
    }
}

extern "C" fn task_high_priority(_pvParameters: *mut c_void) {
    // Safety: FreeRTOS invokes task entry points only in task context.
    let context = unsafe { TaskContext::assume() };
    let mutex = get_mutex!();

    loop {
        freertos_in_rust::delay(&context, pdMS_TO_TICKS(150));

        println("[High] Taking mutex...");

        {
            let mut guard = mutex.lock(&context).expect("mutex acquisition failed");
            println("[High] Mutex acquired!");

            *guard += 10;

            println("[High] Releasing mutex");
        }

        freertos_in_rust::delay(&context, pdMS_TO_TICKS(1000));
    }
}
