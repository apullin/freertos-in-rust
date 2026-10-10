//! Cortex-A9 QEMU test BSP for the vexpress-a9 machine.

#![no_std]
#![allow(non_snake_case)]

use core::arch::global_asm;
use core::ffi::c_void;
use core::fmt::{self, Write};
use core::panic::PanicInfo;
use core::ptr;
use core::sync::atomic::{AtomicU32, Ordering};

use freertos_in_rust::kernel::tasks::{xTaskCreateStatic, StaticTask_t};
use freertos_in_rust::types::{StackType_t, TaskFunction_t, UBaseType_t};
use freertos_qemu_test_common::Tap;

global_asm!(include_str!("startup.S"));

#[cfg(feature = "smp-heap")]
#[global_allocator]
static ALLOCATOR: freertos_in_rust::memory::FreeRtosAllocator =
    freertos_in_rust::memory::FreeRtosAllocator;

/// Release QEMU vexpress's secondary boot stub through SYS_FLAGS and SEV.
#[cfg(feature = "smp")]
pub fn start_secondary() {
    extern "C" {
        fn secondary_start();
    }
    unsafe {
        // SCU coherency and the Cortex-A9 SMP bit. This test leaves caches/MMU off.
        ptr::write_volatile(0x1e00_0000 as *mut u32, 1);
        core::arch::asm!(
            "mrc p15, 0, r0, c1, c0, 1", "orr r0, r0, #(1 << 6)",
            "mcr p15, 0, r0, c1, c0, 1", "isb", out("r0") _, options(nostack)
        );
        ptr::write_volatile(0x1000_0034 as *mut u32, u32::MAX);
        ptr::write_volatile(0x1000_0030 as *mut u32, secondary_start as *const () as u32);
        core::arch::asm!("dsb", "sev", options(nostack));
    }
}

#[no_mangle]
pub extern "C" fn secondary_main() -> ! {
    #[cfg(feature = "smp")]
    unsafe {
        freertos_in_rust::port::vPortStartSecondaryCore()
    }
    #[cfg(not(feature = "smp"))]
    loop {
        core::hint::spin_loop();
    }
}

pub const TIMER_BASE: usize = 0x1001_1000;
pub const TIMER_LOAD: usize = TIMER_BASE;
pub const TIMER_VALUE: usize = TIMER_BASE + 0x04;
pub const TIMER_CONTROL: usize = TIMER_BASE + 0x08;
pub const TIMER_INTCLR: usize = TIMER_BASE + 0x0c;
pub const TIMER_RIS: usize = TIMER_BASE + 0x10;
pub const TIMER_BGLOAD: usize = TIMER_BASE + 0x18;
pub const TIMER_COUNTS_PER_TICK: u32 = 1000;
pub const TIMER_PERIODIC_RELOAD: u32 = TIMER_COUNTS_PER_TICK - 1;

const AUX_TIMER_BASE: usize = 0x1001_2000;
const AUX_TIMER_LOAD: usize = AUX_TIMER_BASE;
const AUX_TIMER_CONTROL: usize = AUX_TIMER_BASE + 0x08;
const AUX_TIMER_INTCLR: usize = AUX_TIMER_BASE + 0x0c;

const UART_BASE: usize = 0x1000_9000;
const UART_DR: usize = UART_BASE;
const UART_FR: usize = UART_BASE + 0x18;

const GIC_DIST_BASE: usize = 0x1e00_1000;
const GIC_CPU_BASE: usize = 0x1e00_0100;
const GICD_CTLR: usize = GIC_DIST_BASE;
const GICD_ISENABLER: usize = GIC_DIST_BASE + 0x100;
const GICD_IPRIORITYR: usize = GIC_DIST_BASE + 0x400;
const GICD_ITARGETSR: usize = GIC_DIST_BASE + 0x800;
const GICC_CTLR: usize = GIC_CPU_BASE;
const GICC_PMR: usize = GIC_CPU_BASE + 0x04;
const GICC_BPR: usize = GIC_CPU_BASE + 0x08;

const TIMER_IRQ: u32 = 34;
const AUX_TIMER_IRQ: u32 = 35;
static TIMER_INTERRUPTS: AtomicU32 = AtomicU32::new(0);
static AUX_TIMER_INTERRUPTS: AtomicU32 = AtomicU32::new(0);
static AUXILIARY_HOOK: AtomicU32 = AtomicU32::new(0);

/// Install a test callback to be invoked in the auxiliary timer ISR.
pub fn set_auxiliary_hook(hook: extern "C" fn()) {
    AUXILIARY_HOOK.store(hook as *const () as u32, Ordering::Release);
}

#[repr(C, align(8))]
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
/// The stack and TCB must remain uniquely owned while the scheduler runs, and
/// `name` must remain a readable NUL-terminated string.
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

    // Safety: the caller supplies stable, exclusive task storage and this BSP
    // passes a null task parameter.
    let handle = unsafe {
        xTaskCreateStatic(
            task,
            name.as_ptr(),
            stack_depth,
            ptr::null_mut::<c_void>(),
            priority,
            stack,
            ptr::addr_of_mut!((*task_buffer).tcb),
        )
    };
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

pub fn timer_interrupts() -> u32 {
    TIMER_INTERRUPTS.load(Ordering::Relaxed)
}

pub fn auxiliary_interrupts() -> u32 {
    AUX_TIMER_INTERRUPTS.load(Ordering::Relaxed)
}

/// Arm the second vexpress SP804 as a one-shot non-tick wake source.
pub fn arm_auxiliary_wake(counts: u32) {
    unsafe {
        ptr::write_volatile(AUX_TIMER_CONTROL as *mut u32, 0);
        ptr::write_volatile(AUX_TIMER_LOAD as *mut u32, counts.saturating_sub(1));
        ptr::write_volatile(AUX_TIMER_INTCLR as *mut u32, 1);
        // Enable, interrupt-enable, 32-bit, and one-shot.
        ptr::write_volatile(AUX_TIMER_CONTROL as *mut u32, 0xa3);
    }
}

pub fn timer_register(offset: usize) -> u32 {
    unsafe { ptr::read_volatile((TIMER_BASE + offset) as *const u32) }
}

/// Configure the GIC and SP804 timer while IRQs remain masked.
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
        ptr::write_volatile((GICD_IPRIORITYR + TIMER_IRQ as usize) as *mut u8, 0xa0);
        ptr::write_volatile((GICD_ITARGETSR + TIMER_IRQ as usize) as *mut u8, 1);

        let auxiliary_enable_bit = 1_u32 << (AUX_TIMER_IRQ % 32);
        ptr::write_volatile(
            (GICD_ISENABLER + enable_offset) as *mut u32,
            auxiliary_enable_bit,
        );
        ptr::write_volatile((GICD_IPRIORITYR + AUX_TIMER_IRQ as usize) as *mut u8, 0xa0);
        ptr::write_volatile((GICD_ITARGETSR + AUX_TIMER_IRQ as usize) as *mut u8, 1);

        ptr::write_volatile(TIMER_CONTROL as *mut u32, 0);
        ptr::write_volatile(TIMER_LOAD as *mut u32, TIMER_PERIODIC_RELOAD);
        ptr::write_volatile(TIMER_BGLOAD as *mut u32, TIMER_PERIODIC_RELOAD);
        ptr::write_volatile(TIMER_INTCLR as *mut u32, 1);
        ptr::write_volatile(TIMER_CONTROL as *mut u32, 0xe2);
    }

    #[cfg(feature = "tickless")]
    freertos_in_rust::port::vPortConfigureTimerForTickless(
        TIMER_BASE,
        TIMER_COUNTS_PER_TICK,
        u32::MAX,
    );
}

pub fn start_scheduler() -> ! {
    // Safety: test initialization is single-threaded with IRQs masked, and the
    // capability is consumed before the scheduler enables task execution.
    let context = unsafe { freertos_in_rust::sync::TaskContext::assume() };
    freertos_in_rust::start_scheduler(context)
}

#[no_mangle]
pub extern "C" fn vApplicationFPUSafeIRQHandler(interrupt_acknowledge: u32) {
    let interrupt_id = interrupt_acknowledge & 0x3ff;
    if interrupt_id == TIMER_IRQ {
        unsafe { ptr::write_volatile(TIMER_INTCLR as *mut u32, 1) };
        TIMER_INTERRUPTS.fetch_add(1, Ordering::Relaxed);
        freertos_in_rust::port::FreeRTOS_Tick_Handler();
    } else if interrupt_id == AUX_TIMER_IRQ {
        unsafe { ptr::write_volatile(AUX_TIMER_INTCLR as *mut u32, 1) };
        AUX_TIMER_INTERRUPTS.fetch_add(1, Ordering::Relaxed);
        let hook = AUXILIARY_HOOK.load(Ordering::Acquire);
        if hook != 0 {
            let callback: extern "C" fn() = unsafe { core::mem::transmute(hook) };
            callback();
        }
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
