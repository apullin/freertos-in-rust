//! FreeRusTOS Demo Application - ARM Cortex-A9
//!
//! This demo demonstrates the FreeRTOS kernel running on Cortex-A9:
//! - Task creation and scheduling
//! - Mutex with priority inheritance
//! - Binary semaphore
//! - Software timers
//!
//! Target: QEMU vexpress-a9 machine
//! Output is via UART0 (PL011).

#![no_std]
#![no_main]
#![allow(non_snake_case)]
#![allow(static_mut_refs)]

use core::ffi::c_void;
use core::panic::PanicInfo;
use core::ptr;

// Allocator selection via Cargo features
#[cfg(feature = "use-heap-4")]
use freertos_in_rust::memory::FreeRtosAllocator;
#[cfg(feature = "use-heap-4")]
#[global_allocator]
static ALLOCATOR: FreeRtosAllocator = FreeRtosAllocator;

// Import FreeRTOS types and functions
use freertos_in_rust::kernel::queue::{
    queueQUEUE_TYPE_BINARY_SEMAPHORE, queueQUEUE_TYPE_MUTEX, queueSEND_TO_BACK,
    xQueueCreateMutex, xQueueGenericCreate, xQueueGenericSend, xQueueSemaphoreTake, QueueHandle_t,
};
use freertos_in_rust::kernel::tasks::{vTaskDelay, vTaskStartScheduler, xTaskCreateStatic, StaticTask_t};
use freertos_in_rust::kernel::timers::*;
use freertos_in_rust::types::*;

// =============================================================================
// Hardware Addresses for vexpress-a9
// =============================================================================

/// PL011 UART0 base address
const UART0_BASE: usize = 0x1000_9000;
const UART_DR: usize = UART0_BASE + 0x00; // Data Register
const UART_FR: usize = UART0_BASE + 0x18; // Flag Register

/// SP804 Timer base address
const TIMER0_BASE: usize = 0x1001_1000;
const TIMER_LOAD: usize = TIMER0_BASE + 0x00;
const TIMER_VALUE: usize = TIMER0_BASE + 0x04;
const TIMER_CONTROL: usize = TIMER0_BASE + 0x08;
const TIMER_INTCLR: usize = TIMER0_BASE + 0x0C;

/// GIC (Generic Interrupt Controller)
const GIC_DIST_BASE: usize = 0x1E00_1000;
const GIC_CPU_BASE: usize = 0x1E00_0100;

// GIC Distributor registers
const GICD_CTLR: usize = GIC_DIST_BASE + 0x000;
const GICD_ISENABLER: usize = GIC_DIST_BASE + 0x100;
const GICD_IPRIORITYR: usize = GIC_DIST_BASE + 0x400;
const GICD_ITARGETSR: usize = GIC_DIST_BASE + 0x800;

// GIC CPU Interface registers
const GICC_CTLR: usize = GIC_CPU_BASE + 0x00;
const GICC_PMR: usize = GIC_CPU_BASE + 0x04;
const GICC_IAR: usize = GIC_CPU_BASE + 0x0C;
const GICC_EOIR: usize = GIC_CPU_BASE + 0x10;

/// Timer interrupt ID (SP804 timer 0 on vexpress-a9)
const TIMER_IRQ: u32 = 34; // IRQ 34 for timer 0

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

/// Write a single byte to UART
fn uart_putc(c: u8) {
    unsafe {
        // Wait for TX FIFO not full (bit 5 of FR = TXFF)
        while (ptr::read_volatile(UART_FR as *const u32) & (1 << 5)) != 0 {}
        ptr::write_volatile(UART_DR as *mut u32, c as u32);
    }
}

/// Write a string to UART
fn print(s: &str) {
    for c in s.bytes() {
        uart_putc(c);
    }
}

/// Print a line to UART
fn println(s: &str) {
    print(s);
    uart_putc(b'\r');
    uart_putc(b'\n');
}

/// Print a number in decimal
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

/// Initialize the GIC
fn gic_init() {
    unsafe {
        // Disable distributor
        ptr::write_volatile(GICD_CTLR as *mut u32, 0);

        // Enable distributor
        ptr::write_volatile(GICD_CTLR as *mut u32, 1);

        // Enable CPU interface
        ptr::write_volatile(GICC_CTLR as *mut u32, 1);

        // Set priority mask to allow all interrupts
        ptr::write_volatile(GICC_PMR as *mut u32, 0xFF);
    }
}

/// Enable a specific interrupt in the GIC
fn gic_enable_irq(irq: u32) {
    unsafe {
        let reg_offset = (irq / 32) as usize * 4;
        let bit = 1u32 << (irq % 32);
        let addr = (GICD_ISENABLER + reg_offset) as *mut u32;
        ptr::write_volatile(addr, bit);

        // Set priority (lower = higher priority)
        let priority_offset = irq as usize;
        let priority_addr = (GICD_IPRIORITYR + priority_offset) as *mut u8;
        ptr::write_volatile(priority_addr, 0xA0); // Priority 10

        // Target CPU 0
        let target_offset = irq as usize;
        let target_addr = (GICD_ITARGETSR + target_offset) as *mut u8;
        ptr::write_volatile(target_addr, 0x01);
    }
}

/// Initialize the SP804 timer for tick interrupts
fn timer_init() {
    unsafe {
        // Disable timer
        ptr::write_volatile(TIMER_CONTROL as *mut u32, 0);

        // Set reload value for 1ms tick (assuming 1MHz timer clock)
        // vexpress-a9 timer runs at 1MHz
        let reload = 1000 - 1; // 1ms at 1MHz
        ptr::write_volatile(TIMER_LOAD as *mut u32, reload);

        // Enable timer with:
        // - Bit 7: Enable
        // - Bit 6: Periodic mode
        // - Bit 5: Interrupt enable
        // - Bit 1: 32-bit counter
        // - Bit 0: Wrapping mode (not one-shot)
        ptr::write_volatile(TIMER_CONTROL as *mut u32, 0xE2);

        // Enable timer interrupt in GIC
        gic_enable_irq(TIMER_IRQ);
    }
}

/// Clear timer interrupt
fn timer_clear_interrupt() {
    unsafe {
        ptr::write_volatile(TIMER_INTCLR as *mut u32, 1);
    }
}

// =============================================================================
// IRQ Handler (called from assembly)
// =============================================================================

/// Application FPU-safe IRQ handler - called by the weak default vApplicationIRQHandler
/// which saves/restores FPU context before calling this function.
#[no_mangle]
pub extern "C" fn vApplicationFPUSafeIRQHandler(ulICCIAR: u32) {
    let irq_id = ulICCIAR & 0x3FF;

    if irq_id == TIMER_IRQ {
        // Clear the timer interrupt first
        timer_clear_interrupt();

        // Call FreeRTOS tick handler
        freertos_in_rust::port::FreeRTOS_Tick_Handler();
    }
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

const STACK_SIZE: usize = 512; // 2KB per task

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

#[no_mangle]
pub extern "C" fn main() -> ! {
    println("========================================");
    println("   FreeRusTOS Demo - Cortex-A9");
    println("========================================");
    println("");

    // Initialize hardware
    println("[Init] Initializing GIC...");
    gic_init();

    println("[Init] Initializing timer...");
    timer_init();

    // Create synchronization primitives
    create_sync_primitives();

    // Create tasks
    create_tasks();

    // Create software timer
    create_timer();

    println("[Main] Starting scheduler...");
    println("");

    // Enable interrupts before starting scheduler
    unsafe {
        core::arch::asm!("cpsie i");
    }

    // Start the scheduler - this never returns
    vTaskStartScheduler();

    // Should never reach here
    println("[Main] ERROR: Scheduler returned!");
    loop {
        unsafe {
            core::arch::asm!("wfi");
        }
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
