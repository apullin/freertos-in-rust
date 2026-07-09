//! Cortex-A53 QEMU test BSP and common kernel setup.

#![no_std]
#![allow(non_snake_case)]

use core::arch::global_asm;
use core::ffi::c_void;
use core::fmt::{self, Write};
use core::panic::PanicInfo;
use core::ptr;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use freertos_in_rust::kernel::tasks::{xTaskCreateStatic, StaticTask_t};
use freertos_in_rust::memory::FreeRtosAllocator;
use freertos_in_rust::types::{StackType_t, TaskFunction_t, UBaseType_t};
use freertos_qemu_test_common::Tap;

global_asm!(
    ".section .text.boot",
    ".global _start",
    "_start:",
    "msr daifset, #0xf",
    // The single-core test firmware parks any accidentally supplied secondaries.
    "mrs x0, mpidr_el1",
    "and x0, x0, #0xff",
    "cbnz x0, 9f",
    // QEMU may enter at EL3, EL2, or EL1 depending on machine options.
    "mrs x0, CurrentEL",
    "lsr x0, x0, #2",
    "and x0, x0, #3",
    "cmp x0, #3",
    "b.eq 3f",
    "cmp x0, #2",
    "b.eq 2f",
    "b 1f",
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
    // MMU off, instruction and data caches on.
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
    // Enable FP/SIMD at EL1 for the port preservation test.
    "mov x0, #(3 << 20)",
    "msr cpacr_el1, x0",
    "isb",
    "bl main",
    "8:",
    "wfi",
    "b 8b",
    "9:",
    "wfe",
    "b 9b",
);

#[global_allocator]
static ALLOCATOR: FreeRtosAllocator = FreeRtosAllocator;

const UART0_BASE: usize = 0x0900_0000;
const UART_DR: usize = UART0_BASE;
const UART_FR: usize = UART0_BASE + 0x18;

const GIC_DIST_BASE: usize = 0x0800_0000;
const GIC_CPU_BASE: usize = 0x0801_0000;
const GICD_CTLR: usize = GIC_DIST_BASE;
const GICD_ISENABLER: usize = GIC_DIST_BASE + 0x100;
const GICD_IPRIORITYR: usize = GIC_DIST_BASE + 0x400;
const GICC_CTLR: usize = GIC_CPU_BASE;
const GICC_PMR: usize = GIC_CPU_BASE + 0x04;
const GICC_BPR: usize = GIC_CPU_BASE + 0x08;

const TIMER_IRQ: u32 = 30;
const TICK_RATE_HZ: u64 = 1000;

static TIMER_INTERRUPTS: AtomicU64 = AtomicU64::new(0);
static TICK_ISR_HOOK: AtomicUsize = AtomicUsize::new(0);

/// Optional test-firmware hook called from the physical timer ISR.
pub type TickIsrHook = extern "C" fn();

/// Install a hook before starting the scheduler.
pub fn set_tick_isr_hook(hook: TickIsrHook) {
    TICK_ISR_HOOK.store(hook as usize, Ordering::Release);
}

/// Opaque TCB storage with the AArch64 port's maximum alignment.
#[repr(C, align(16))]
pub struct AlignedTaskBuffer {
    tcb: StaticTask_t,
}

impl AlignedTaskBuffer {
    pub const fn new() -> Self {
        Self {
            tcb: StaticTask_t::new(),
        }
    }
}

impl Default for AlignedTaskBuffer {
    fn default() -> Self {
        Self::new()
    }
}

/// Create one statically allocated test task.
///
/// # Safety
///
/// The stack and TCB must remain uniquely owned for the life of the scheduler.
/// `name` must be a static, NUL-terminated byte string.
pub unsafe fn spawn_static(
    name: &'static [u8],
    stack: *mut StackType_t,
    stack_depth: usize,
    task_buffer: *mut AlignedTaskBuffer,
    priority: UBaseType_t,
    task: TaskFunction_t,
) {
    assert!(name.last() == Some(&0));
    assert!(stack_depth > 0);

    let handle = xTaskCreateStatic(
        task,
        name.as_ptr(),
        stack_depth,
        ptr::null_mut::<c_void>(),
        priority,
        stack,
        ptr::addr_of_mut!((*task_buffer).tcb),
    );
    assert!(!handle.is_null());
}

pub struct SerialOutput;

impl Write for SerialOutput {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        for byte in value.bytes() {
            uart_putc(byte);
        }
        Ok(())
    }
}

pub fn tap() -> Tap<SerialOutput> {
    Tap::new(SerialOutput)
}

pub fn finish(mut output: Tap<SerialOutput>, planned_tests: usize) -> ! {
    let _ = output.finish(planned_tests);
    loop {
        unsafe { core::arch::asm!("wfi", options(nomem, nostack)) };
    }
}

pub fn timer_interrupts() -> u64 {
    TIMER_INTERRUPTS.load(Ordering::Relaxed)
}

/// Configure the GIC and physical timer while IRQs remain masked.
pub fn init() {
    unsafe {
        ptr::write_volatile(GICD_CTLR as *mut u32, 0);
        ptr::write_volatile(GICD_CTLR as *mut u32, 1);
        ptr::write_volatile(GICC_CTLR as *mut u32, 1);
        ptr::write_volatile(GICC_PMR as *mut u32, 0xff);
        ptr::write_volatile(GICC_BPR as *mut u32, 0);

        let enable_offset = (TIMER_IRQ / 32) as usize * 4;
        let enable_bit = 1_u32 << (TIMER_IRQ % 32);
        ptr::write_volatile((GICD_ISENABLER + enable_offset) as *mut u32, enable_bit);
        // 0xa0 is below the maximum API-call priority mask (0x90).
        ptr::write_volatile((GICD_IPRIORITYR + TIMER_IRQ as usize) as *mut u8, 0xa0);
    }

    freertos_in_rust::port::vPortConfigureGIC(GIC_DIST_BASE as u64, GIC_CPU_BASE as u64, 18);
    freertos_in_rust::port::vPortConfigureTicklessIdle(TICK_RATE_HZ as u32);

    let frequency: u64;
    let count: u64;
    unsafe {
        core::arch::asm!("mrs {}, cntfrq_el0", out(reg) frequency);
        core::arch::asm!("mrs {}, cntpct_el0", out(reg) count);
        let compare = count + frequency / TICK_RATE_HZ;
        core::arch::asm!("msr cntp_cval_el0, {}", in(reg) compare);
        core::arch::asm!("msr cntp_ctl_el0, {}", in(reg) 1_u64);
        core::arch::asm!("dsb sy", "isb sy", options(nomem, nostack));
    }
}

pub fn start_scheduler() -> ! {
    freertos_in_rust::start_scheduler()
}

#[no_mangle]
pub extern "C" fn vApplicationIRQHandler(interrupt_acknowledge: u32) {
    if interrupt_acknowledge & 0x3ff == TIMER_IRQ {
        let frequency: u64;
        let count: u64;
        unsafe {
            core::arch::asm!("mrs {}, cntfrq_el0", out(reg) frequency);
            core::arch::asm!("mrs {}, cntpct_el0", out(reg) count);
            let compare = count + frequency / TICK_RATE_HZ;
            core::arch::asm!("msr cntp_cval_el0, {}", in(reg) compare);
        }
        TIMER_INTERRUPTS.fetch_add(1, Ordering::Relaxed);
        let hook = TICK_ISR_HOOK.load(Ordering::Acquire);
        if hook != 0 {
            // The setter accepts only function pointers and is called before
            // interrupts are enabled; the stored address remains immutable.
            let hook: TickIsrHook = unsafe { core::mem::transmute(hook) };
            hook();
        }
        freertos_in_rust::port::FreeRTOS_Tick_Handler();
    }
}

fn uart_putc(byte: u8) {
    unsafe {
        while ptr::read_volatile(UART_FR as *const u32) & (1 << 5) != 0 {}
        ptr::write_volatile(UART_DR as *mut u32, byte as u32);
    }
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    let mut output = SerialOutput;
    let _ = writeln!(output, "TAP version 13");
    let _ = writeln!(output, "not ok 1 - guest panic: {info}");
    let _ = writeln!(output, "1..1");
    let _ = writeln!(output, "FREERUSTOS_TEST_RESULT=FAIL");
    loop {
        unsafe { core::arch::asm!("wfi", options(nomem, nostack)) };
    }
}
