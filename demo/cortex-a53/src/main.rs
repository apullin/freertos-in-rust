//! Minimal FreeRusTOS Demo - ARM Cortex-A53 (AArch64)
//!
//! Just creates one task and starts the scheduler.

#![no_std]
#![no_main]
#![allow(non_snake_case)]
#![allow(static_mut_refs)]

use core::arch::global_asm;
use core::ffi::c_void;
use core::panic::PanicInfo;
use core::ptr;

// =============================================================================
// Startup Assembly
// =============================================================================

global_asm!(
    ".section .text.boot",
    ".global _start",

    "_start:",
    "msr daifset, #0xF",

    // Park secondary cores
    "mrs x0, mpidr_el1",
    "and x0, x0, #0xFF",
    "cbnz x0, park_core",

    // Check EL
    "mrs x0, CurrentEL",
    "lsr x0, x0, #2",
    "and x0, x0, #3",
    "cmp x0, #3",
    "b.eq 3f",
    "cmp x0, #2",
    "b.eq 2f",
    "b 1f",

    "park_core:",
    "wfi",
    "b park_core",

    "3:",
    "mov x0, #(1 << 10) | (1 << 0)",
    "msr scr_el3, x0",
    "mov x0, #0x05",
    "msr spsr_el3, x0",
    "adr x0, 1f",
    "msr elr_el3, x0",
    "eret",

    "2:",
    "mov x0, #(1 << 31)",
    "msr hcr_el2, x0",
    "mov x0, #0x05",
    "msr spsr_el2, x0",
    "adr x0, 1f",
    "msr elr_el2, x0",
    "eret",

    "1:",
    "ldr x0, =__stack_end",
    "mov sp, x0",
    "mov x0, #0",
    "orr x0, x0, #(1 << 12)",
    "orr x0, x0, #(1 << 2)",
    "msr sctlr_el1, x0",
    "isb",

    "ldr x1, =__bss_start",
    "ldr x2, =__bss_end",
    "4:",
    "cmp x1, x2",
    "b.ge 5f",
    "str xzr, [x1], #8",
    "b 4b",
    "5:",

    "mov x0, #(3 << 20)",
    "msr cpacr_el1, x0",
    "isb",

    "bl main",
    "6:",
    "wfi",
    "b 6b",
);

// Allocator
#[cfg(feature = "use-heap-4")]
use freertos_in_rust::memory::FreeRtosAllocator;
#[cfg(feature = "use-heap-4")]
#[global_allocator]
static ALLOCATOR: FreeRtosAllocator = FreeRtosAllocator;

use freertos_in_rust::kernel::tasks::{vTaskDelay, vTaskStartScheduler, xTaskCreateStatic, StaticTask_t};
use freertos_in_rust::types::*;

// Hardware
const UART0_BASE: usize = 0x0900_0000;
const UART_DR: usize = UART0_BASE + 0x00;
const UART_FR: usize = UART0_BASE + 0x18;

const GIC_DIST_BASE: usize = 0x0800_0000;
const GIC_CPU_BASE: usize = 0x0801_0000;

const GICD_CTLR: usize = GIC_DIST_BASE + 0x000;
const GICD_ISENABLER: usize = GIC_DIST_BASE + 0x100;
const GICD_IPRIORITYR: usize = GIC_DIST_BASE + 0x400;
const GICD_ITARGETSR: usize = GIC_DIST_BASE + 0x800;
const GICC_CTLR: usize = GIC_CPU_BASE + 0x00;
const GICC_PMR: usize = GIC_CPU_BASE + 0x04;

const TIMER_IRQ: u32 = 30;

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    print("[PANIC] ");
    if let Some(loc) = info.location() {
        println(loc.file());
    } else {
        println("unknown");
    }
    loop { unsafe { core::arch::asm!("wfi"); } }
}

// =============================================================================
// Simple UART Output
// =============================================================================
// NOTE: A holistic solution for debug printing is not yet solidified and will
// require better understanding of what Rust offers for no_std environments
// (e.g., core::fmt::Write adds ~40KB). For now, demos use direct print functions.

fn uart_putc(c: u8) {
    unsafe {
        while (ptr::read_volatile(UART_FR as *const u32) & (1 << 5)) != 0 {}
        ptr::write_volatile(UART_DR as *mut u32, c as u32);
    }
}

fn print(s: &str) {
    for c in s.bytes() { uart_putc(c); }
}

fn println(s: &str) {
    print(s);
    uart_putc(b'\r');
    uart_putc(b'\n');
}

fn print_dec(mut n: u64) {
    if n == 0 { uart_putc(b'0'); return; }
    let mut buf = [0u8; 20];
    let mut i = 0;
    while n > 0 { buf[i] = (n % 10) as u8 + b'0'; n /= 10; i += 1; }
    while i > 0 { i -= 1; uart_putc(buf[i]); }
}

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
        ptr::write_volatile((GICD_ISENABLER + reg_offset) as *mut u32, bit);
        ptr::write_volatile((GICD_IPRIORITYR + irq as usize) as *mut u8, 0xA0);
        ptr::write_volatile((GICD_ITARGETSR + irq as usize) as *mut u8, 0x01);
    }
}

fn timer_get_frequency() -> u64 {
    let freq: u64;
    unsafe { core::arch::asm!("mrs {}, cntfrq_el0", out(reg) freq); }
    freq
}

fn timer_get_count() -> u64 {
    let count: u64;
    unsafe { core::arch::asm!("mrs {}, cntpct_el0", out(reg) count); }
    count
}

fn timer_set_compare(val: u64) {
    unsafe { core::arch::asm!("msr cntp_cval_el0, {}", in(reg) val); }
}

fn timer_set_ctrl(enable: bool, imask: bool) {
    let val: u64 = (enable as u64) | ((imask as u64) << 1);
    unsafe { core::arch::asm!("msr cntp_ctl_el0, {}", in(reg) val); }
}

fn timer_init() {
    let freq = timer_get_frequency();
    let ticks_per_tick = freq / 1000;
    let current = timer_get_count();
    timer_set_compare(current + ticks_per_tick);
    timer_set_ctrl(true, false);
    gic_enable_irq(TIMER_IRQ);
}

fn timer_clear_and_reload() {
    let freq = timer_get_frequency();
    let ticks_per_tick = freq / 1000;
    let current = timer_get_count();
    timer_set_compare(current + ticks_per_tick);
}

#[no_mangle]
pub extern "C" fn vApplicationIRQHandler(ulICCIAR: u32) {
    let irq_id = ulICCIAR & 0x3FF;
    if irq_id == TIMER_IRQ {
        timer_clear_and_reload();
        freertos_in_rust::port::FreeRTOS_Tick_Handler();
    }
}

// Task
const STACK_SIZE: usize = 512;

#[repr(align(16))]
struct AlignedStack([StackType_t; STACK_SIZE]);

static mut TASK_STACK: AlignedStack = AlignedStack([0; STACK_SIZE]);
static mut TASK_TCB: StaticTask_t = StaticTask_t::new();

#[no_mangle]
pub extern "C" fn main() -> ! {
    println("========================================");
    println("   FreeRusTOS Minimal - Cortex-A53");
    println("========================================");
    println("");

    println("[Init] GIC...");
    gic_init();

    println("[Init] Port GIC...");
    freertos_in_rust::port::vPortConfigureGIC(
        GIC_DIST_BASE as u64,
        GIC_CPU_BASE as u64,
        18,
    );

    println("[Init] Timer...");
    timer_init();

    #[cfg(feature = "tickless-idle")]
    {
        println("[Init] Tickless idle...");
        freertos_in_rust::port::vPortConfigureTicklessIdle(1000);
    }

    println("[Init] Creating task...");
    unsafe {
        let result = xTaskCreateStatic(
            simple_task,
            b"Task\0".as_ptr(),
            STACK_SIZE,
            ptr::null_mut(),
            1,
            TASK_STACK.0.as_mut_ptr(),
            &mut TASK_TCB as *mut StaticTask_t,
        );

        if result.is_null() {
            println("[Init] Task creation FAILED!");
            loop { core::arch::asm!("wfi"); }
        }
    }

    println("[Init] Starting scheduler...");
    unsafe { core::arch::asm!("msr daifclr, #2"); }
    vTaskStartScheduler();

    println("[Init] ERROR: Scheduler returned!");
    loop { unsafe { core::arch::asm!("wfi"); } }
}

extern "C" fn simple_task(_: *mut c_void) {
    let mut count = 0u64;
    loop {
        print("[Task] count=");
        print_dec(count);
        println("");
        count += 1;
        vTaskDelay(pdMS_TO_TICKS(1000));
    }
}
