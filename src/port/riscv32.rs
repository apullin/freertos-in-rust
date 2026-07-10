/*
 * FreeRTOS Kernel <DEVELOPMENT BRANCH>
 * Copyright (C) 2021 Amazon.com, Inc. or its affiliates. All Rights Reserved.
 *
 * SPDX-License-Identifier: MIT
 *
 * [AMENDMENT] This is the Rust port of the RISC-V (RV32) port layer.
 * Ported from: portable/GCC/RISC-V/port.c and portASM.S
 *
 * This port targets RV32I/RV32IMAC cores with standard CLINT timer.
 * Tested on QEMU sifive_e machine.
 */

//! RISC-V RV32 Port Layer
//!
//! This module provides the hardware abstraction layer for RISC-V 32-bit cores.
//! It handles context switching, interrupt management, and timer setup.
//!
//! # Target
//! - RV32I, RV32IMAC (without FPU for now)
//! - Standard SiFive CLINT for timer
//! - Machine mode execution

use core::arch::global_asm;
use core::ffi::c_void;

use crate::config::*;
// The kernel task functions are called from assembly via their symbol names
#[cfg(feature = "tickless-idle")]
use crate::kernel::tasks::{eTaskConfirmSleepModeStatus, vTaskStepTick};
#[allow(unused_imports)]
use crate::kernel::tasks::{pxCurrentTCB, vTaskSwitchContext, xTaskIncrementTick};
use crate::types::*;

// =============================================================================
// Port Constants
// =============================================================================

/// Stack grows downward on RISC-V
pub const portSTACK_GROWTH: BaseType_t = -1;

/// RISC-V ABI requires 16-byte stack alignment
pub const portBYTE_ALIGNMENT: usize = 16;
pub const portBYTE_ALIGNMENT_MASK: usize = portBYTE_ALIGNMENT - 1;

/// Architecture name for diagnostics
pub const portARCH_NAME: &str = "RISC-V RV32";

/// Tick period in milliseconds
pub const portTICK_PERIOD_MS: TickType_t = 1000 / configTICK_RATE_HZ;

// =============================================================================
// CLINT (Core Local Interruptor) Addresses
// =============================================================================

/// CLINT base address (SiFive standard, used by QEMU sifive_e/virt)
const CLINT_BASE: usize = 0x0200_0000;

/// MTIME register - 64-bit free-running timer
const MTIME_ADDR: usize = CLINT_BASE + 0xBFF8;

/// MTIMECMP register - timer compare for hart 0
const MTIMECMP_ADDR: usize = CLINT_BASE + 0x4000;

// =============================================================================
// Stack Frame Layout
// =============================================================================

/// Context size in words (31 words for RV32I)
/// Layout:
///   [0]  = mepc (return address)
///   [1]  = mstatus
///   [2]  = x1 (ra)
///   [3]  = x5 (t0)
///   ...
///   [29] = x31 (t6)
///   [30] = xCriticalNesting
const portCONTEXT_SIZE: usize = 31;
const portCONTEXT_SIZE_BYTES: usize = portCONTEXT_SIZE * 4;

/// Offset of critical nesting in context (word index)
const portCRITICAL_NESTING_OFFSET: usize = 30;

/// Initial mstatus value: MPIE=1 (enable interrupts on mret), MPP=M-mode
/// MPIE is bit 7, MPP is bits 12:11 (0b11 = M-mode)
const portINITIAL_MSTATUS: u32 = 0x1880; // MPIE=1, MPP=M-mode

// =============================================================================
// Critical Section Management
// =============================================================================

/// Critical nesting counter
/// Uses static mut since RV32I without A extension doesn't have atomics
#[no_mangle]
pub static mut xCriticalNesting: usize = 0xAAAAAAAA; // Non-zero to catch uninitialized use

/// Disable interrupts by clearing mstatus.MIE (bit 3)
#[inline(always)]
pub fn portDISABLE_INTERRUPTS() {
    unsafe {
        core::arch::asm!(
            "csrc mstatus, {mie_bit}",
            mie_bit = in(reg) 0x8,
            options(nostack)
        );
    }
}

/// Enable interrupts by setting mstatus.MIE (bit 3)
#[inline(always)]
pub fn portENABLE_INTERRUPTS() {
    unsafe {
        core::arch::asm!(
            "csrs mstatus, {mie_bit}",
            mie_bit = in(reg) 0x8,
            options(nostack)
        );
    }
}

/// Enter critical section
pub fn portENTER_CRITICAL() {
    portDISABLE_INTERRUPTS();
    unsafe {
        xCriticalNesting = xCriticalNesting
            .checked_add(1)
            .expect("RISC-V critical nesting overflow");
    }
}

/// Exit critical section
pub fn portEXIT_CRITICAL() {
    unsafe {
        configASSERT(xCriticalNesting > 0);
        if xCriticalNesting == 0 {
            return;
        }
        xCriticalNesting -= 1;
        if xCriticalNesting == 0 {
            portENABLE_INTERRUPTS();
        }
    }
}

/// Save interrupt mask and disable interrupts (for ISR use)
#[inline(always)]
pub fn portSET_INTERRUPT_MASK_FROM_ISR() -> UBaseType_t {
    let mstatus: u32;
    unsafe {
        core::arch::asm!(
            "csrrc {mstatus}, mstatus, {mie_bit}",
            mstatus = out(reg) mstatus,
            mie_bit = in(reg) 0x8,
            options(nostack)
        );
    }
    mstatus as UBaseType_t
}

/// Restore interrupt mask (for ISR use)
#[inline(always)]
pub fn portCLEAR_INTERRUPT_MASK_FROM_ISR(saved_mstatus: UBaseType_t) {
    unsafe {
        if (saved_mstatus as u32 & 0x8) != 0 {
            core::arch::asm!(
                "csrs mstatus, {mie_bit}",
                mie_bit = in(reg) 0x8,
                options(nostack)
            );
        } else {
            core::arch::asm!(
                "csrc mstatus, {mie_bit}",
                mie_bit = in(reg) 0x8,
                options(nostack)
            );
        }
    }
}

// =============================================================================
// Context Switching
// =============================================================================

/// Trigger a context switch via ecall (environment call)
#[inline(always)]
pub fn portYIELD() {
    unsafe {
        core::arch::asm!("ecall", options(nostack));
    }
}

/// Yield from ISR if required
#[inline(always)]
pub fn portYIELD_FROM_ISR(xSwitchRequired: BaseType_t) {
    if xSwitchRequired != crate::types::pdFALSE {
        // The common interrupt epilogue consumes this before restoring a task.
        unsafe {
            core::ptr::write_volatile(&raw mut xPortYieldPending, 1);
        }
    }
}

/// Alias for portYIELD_FROM_ISR
#[inline(always)]
pub fn portEND_SWITCHING_ISR(xSwitchRequired: BaseType_t) {
    portYIELD_FROM_ISR(xSwitchRequired);
}

/// Deferred context-switch request consumed by the interrupt epilogue.
#[no_mangle]
static mut xPortYieldPending: u32 = 0;

/// Number of active asynchronous interrupt frames.
#[no_mangle]
static mut uxPortInterruptNesting: UBaseType_t = 0;

/// Application machine-external interrupt dispatcher.
pub type ExternalInterruptHandler_t = extern "C" fn(UBaseType_t) -> BaseType_t;

static mut pxExternalInterruptHandler: Option<ExternalInterruptHandler_t> = None;

/// Install the machine-external interrupt dispatcher.
///
/// The callback runs on the port ISR stack and must claim and complete the
/// platform interrupt at its controller. Its return value follows the normal
/// `xHigherPriorityTaskWoken` convention.
///
/// # Safety
///
/// Install or clear this only while machine external interrupts cannot run,
/// normally before starting the scheduler.
pub unsafe fn vPortSetExternalInterruptHandler(pxHandler: Option<ExternalInterruptHandler_t>) {
    pxExternalInterruptHandler = pxHandler;
}

/// Called by the assembly trap path for machine-external interrupts.
#[no_mangle]
extern "C" fn xPortHandleExternalInterrupt(uxCause: UBaseType_t) -> BaseType_t {
    let pxHandler = unsafe { pxExternalInterruptHandler };
    configASSERT(pxHandler.is_some());

    if let Some(pxHandler) = pxHandler {
        pxHandler(uxCause)
    } else {
        pdFALSE
    }
}

// =============================================================================
// Stack Initialization
// =============================================================================

/// Task return address (should never be reached)
#[no_mangle]
extern "C" fn prvTaskExitError() -> ! {
    // A task should never return from its function
    portDISABLE_INTERRUPTS();
    loop {
        unsafe {
            core::arch::asm!("wfi", options(nomem, nostack));
        }
    }
}

/// Initialize a task's stack
///
/// Creates the initial stack frame that will be restored when the task
/// first runs. The frame simulates what would be saved during a context switch.
///
/// # Safety
///
/// `pxTopOfStack` must be non-null and lie near the high end of a uniquely
/// writable allocation with enough lower-addressed room for the aligned
/// `portCONTEXT_SIZE` frame. The allocation must remain live and stationary
/// for the task lifetime. `pvParameters` must satisfy `pxCode` when invoked.
pub unsafe fn pxPortInitialiseStack(
    pxTopOfStack: *mut StackType_t,
    pxCode: TaskFunction_t,
    pvParameters: *mut c_void,
) -> *mut StackType_t {
    unsafe {
        let mut pxStack = pxTopOfStack;

        // Ensure 16-byte alignment
        pxStack = ((pxStack as usize) & !0xF) as *mut StackType_t;

        // Allocate space for context (31 words)
        pxStack = pxStack.sub(portCONTEXT_SIZE);

        // [0] mepc - task entry point
        *pxStack.add(0) = pxCode as StackType_t;

        // [1] mstatus - MPIE=1 (interrupts enabled on mret), MPP=M-mode
        *pxStack.add(1) = portINITIAL_MSTATUS;

        // [2] x1 (ra) - return address (prvTaskExitError)
        *pxStack.add(2) = prvTaskExitError as StackType_t;

        // [3]-[7] x5-x9 (t0-t2, s0/fp, s1) - initialize to 0
        for i in 3..8 {
            *pxStack.add(i) = 0;
        }

        // [8] x10 (a0) - first argument = pvParameters
        *pxStack.add(8) = pvParameters as StackType_t;

        // [9]-[29] x11-x31 - initialize to 0
        for i in 9..30 {
            *pxStack.add(i) = 0;
        }

        // [30] xCriticalNesting - starts at 0
        *pxStack.add(portCRITICAL_NESTING_OFFSET) = 0;

        pxStack
    }
}

// =============================================================================
// Timer Configuration
// =============================================================================

/// Whole MTIME counts in each kernel tick.
static mut TIMER_INCREMENT: u64 = 0;

/// Numerator left over after dividing MTIME frequency by tick frequency.
static mut TIMER_REMAINDER: u32 = 0;

/// Fractional phase after the compare value in `ullNextTime`.
static mut TIMER_FRACTION: u32 = 0;

/// Next timer compare value
#[no_mangle]
pub static mut ullNextTime: u64 = 0;

/// Read RV32's 64-bit MTIME without returning a torn value at low-word wrap.
#[inline(always)]
pub fn ullPortReadMtime() -> u64 {
    unsafe {
        let pulTimeLow = MTIME_ADDR as *const u32;
        let pulTimeHigh = (MTIME_ADDR + 4) as *const u32;
        let mut ulTimeHigh = core::ptr::read_volatile(pulTimeHigh);

        loop {
            let ulTimeLow = core::ptr::read_volatile(pulTimeLow);
            let ulTimeHighAfter = core::ptr::read_volatile(pulTimeHigh);

            if ulTimeHigh == ulTimeHighAfter {
                return ((ulTimeHigh as u64) << 32) | ulTimeLow as u64;
            }

            ulTimeHigh = ulTimeHighAfter;
        }
    }
}

/// Write one RV32 MTIMECMP value without exposing a transient earlier compare.
#[inline(always)]
unsafe fn prvWriteMtimeCompare(ullCompare: u64) {
    /* The low word is first raised to its maximum, then the high and final low
     * words are installed. This is the sequence recommended by the RISC-V
     * privileged specification for RV32. */
    core::ptr::write_volatile(MTIMECMP_ADDR as *mut u32, u32::MAX);
    core::ptr::write_volatile((MTIMECMP_ADDR + 4) as *mut u32, (ullCompare >> 32) as u32);
    core::ptr::write_volatile(MTIMECMP_ADDR as *mut u32, ullCompare as u32);
}

/// Advance a fractional tick phase by `ullTicks` timer intervals.
#[inline(always)]
unsafe fn prvTimerAdvance(ulInitialFraction: u32, ullTicks: u64) -> (u64, u32) {
    let ullFraction =
        (ulInitialFraction as u64).wrapping_add(ullTicks.wrapping_mul(TIMER_REMAINDER as u64));
    let ullTickRate = configTICK_RATE_HZ as u64;
    let ullExtraCounts = ullFraction / ullTickRate;
    let ulFinalFraction = (ullFraction % ullTickRate) as u32;
    let ullCounts = ullTicks
        .wrapping_mul(TIMER_INCREMENT)
        .wrapping_add(ullExtraCounts);

    (ullCounts, ulFinalFraction)
}

/// Advance the programmed compare value by one exact fractional tick.
#[inline(always)]
unsafe fn prvAdvanceNextTime() {
    let (ullIncrement, ulNewFraction) = prvTimerAdvance(TIMER_FRACTION, 1);
    TIMER_FRACTION = ulNewFraction;
    ullNextTime = ullNextTime.wrapping_add(ullIncrement);
}

/// Set up the timer to generate tick interrupts
#[no_mangle]
pub extern "C" fn vPortSetupTimerInterrupt() {
    unsafe {
        let ulMtimeHz = configMTIME_HZ;
        let ulTickRate = configTICK_RATE_HZ as u32;
        configASSERT(ulTickRate > 0);
        configASSERT(ulMtimeHz >= ulTickRate);

        TIMER_INCREMENT = (ulMtimeHz / ulTickRate) as u64;
        TIMER_REMAINDER = ulMtimeHz % ulTickRate;
        TIMER_FRACTION = 0;

        ullNextTime = ullPortReadMtime();
        prvAdvanceNextTime();
        prvWriteMtimeCompare(ullNextTime);
    }
}

/// Update timer compare register for next tick
#[no_mangle]
pub extern "C" fn vPortUpdateTimerCompare() {
    unsafe {
        prvAdvanceNextTime();
        prvWriteMtimeCompare(ullNextTime);
    }
}

// =============================================================================
// Scheduler Control
// =============================================================================

/// ISR stack
static mut ISR_STACK: [u8; 1024] = [0; 1024];

/// ISR stack top pointer (for assembly)
#[no_mangle]
pub static mut xISRStackTop: usize = 0;

/// Start the scheduler
///
/// This function sets up the trap handler, enables interrupts, and starts
/// the first task. It never returns.
pub fn xPortStartScheduler() -> BaseType_t {
    unsafe {
        // Initialize ISR stack top (grows downward, 16-byte aligned)
        xISRStackTop = ((&ISR_STACK as *const _ as usize) + ISR_STACK.len()) & !0xF;

        // Initialize critical nesting
        xCriticalNesting = 0;
        xPortYieldPending = 0;
        uxPortInterruptNesting = 0;

        // Set trap vector to our handler (direct mode)
        core::arch::asm!(
            "la t0, freertos_risc_v_trap_handler",
            "csrw mtvec, t0",
            options(nomem, nostack)
        );

        // Set up the timer, then enable machine timer and external sources.
        vPortSetupTimerInterrupt();
        core::arch::asm!(
            "csrs mie, {interrupts}",
            interrupts = in(reg) 0x880,
            options(nomem, nostack)
        );

        // Start the first task (never returns)
        vRestoreContextOfFirstTask();
    }

    // Should never reach here
    #[allow(unreachable_code)]
    0
}

/// End the scheduler (not really supported)
pub fn vPortEndScheduler() {
    // Not implemented - scheduler runs forever
}

/// Restore context of first task and start execution
#[no_mangle]
unsafe extern "C" fn vRestoreContextOfFirstTask() -> ! {
    core::arch::asm!(
        // Load pxCurrentTCB
        "la t1, pxCurrentTCB",
        "lw sp, 0(t1)", // sp = pxCurrentTCB
        "lw sp, 0(sp)", // sp = pxCurrentTCB->pxTopOfStack
        // Load mepc from stack[0]
        "lw t0, 0(sp)",
        "csrw mepc, t0",
        // Load mstatus from stack[1]
        "lw t0, 4(sp)",
        // MPIE in the initial frame enables interrupts atomically at mret.
        // MIE must remain clear while registers are still being restored.
        "csrw mstatus, t0",
        // Load xCriticalNesting from stack[30]
        "lw t0, 120(sp)", // 30 * 4 = 120
        "la t1, xCriticalNesting",
        "sw t0, 0(t1)",
        // Restore registers
        "lw x1,   8(sp)",  // ra
        "lw x5,  12(sp)",  // t0
        "lw x6,  16(sp)",  // t1
        "lw x7,  20(sp)",  // t2
        "lw x8,  24(sp)",  // s0/fp
        "lw x9,  28(sp)",  // s1
        "lw x10, 32(sp)",  // a0
        "lw x11, 36(sp)",  // a1
        "lw x12, 40(sp)",  // a2
        "lw x13, 44(sp)",  // a3
        "lw x14, 48(sp)",  // a4
        "lw x15, 52(sp)",  // a5
        "lw x16, 56(sp)",  // a6
        "lw x17, 60(sp)",  // a7
        "lw x18, 64(sp)",  // s2
        "lw x19, 68(sp)",  // s3
        "lw x20, 72(sp)",  // s4
        "lw x21, 76(sp)",  // s5
        "lw x22, 80(sp)",  // s6
        "lw x23, 84(sp)",  // s7
        "lw x24, 88(sp)",  // s8
        "lw x25, 92(sp)",  // s9
        "lw x26, 96(sp)",  // s10
        "lw x27, 100(sp)", // s11
        "lw x28, 104(sp)", // t3
        "lw x29, 108(sp)", // t4
        "lw x30, 112(sp)", // t5
        "lw x31, 116(sp)", // t6
        // Restore sp and return
        "addi sp, sp, 124", // portCONTEXT_SIZE_BYTES
        "mret",
        options(noreturn)
    );
}

// =============================================================================
// Trap Handler (Assembly)
// =============================================================================

global_asm!(
    r#"
.section .text.freertos_risc_v_trap_handler
.global freertos_risc_v_trap_handler
.align 4

freertos_risc_v_trap_handler:
    # Save context to current task's stack
    addi sp, sp, -124               # portCONTEXT_SIZE_BYTES

    # Save registers
    sw x1,   8(sp)                  # ra
    sw x5,  12(sp)                  # t0
    sw x6,  16(sp)                  # t1
    sw x7,  20(sp)                  # t2
    sw x8,  24(sp)                  # s0/fp
    sw x9,  28(sp)                  # s1
    sw x10, 32(sp)                  # a0
    sw x11, 36(sp)                  # a1
    sw x12, 40(sp)                  # a2
    sw x13, 44(sp)                  # a3
    sw x14, 48(sp)                  # a4
    sw x15, 52(sp)                  # a5
    sw x16, 56(sp)                  # a6
    sw x17, 60(sp)                  # a7
    sw x18, 64(sp)                  # s2
    sw x19, 68(sp)                  # s3
    sw x20, 72(sp)                  # s4
    sw x21, 76(sp)                  # s5
    sw x22, 80(sp)                  # s6
    sw x23, 84(sp)                  # s7
    sw x24, 88(sp)                  # s8
    sw x25, 92(sp)                  # s9
    sw x26, 96(sp)                  # s10
    sw x27, 100(sp)                 # s11
    sw x28, 104(sp)                 # t3
    sw x29, 108(sp)                 # t4
    sw x30, 112(sp)                 # t5
    sw x31, 116(sp)                 # t6

    # Save xCriticalNesting
    la t0, xCriticalNesting
    lw t0, 0(t0)
    sw t0, 120(sp)                  # [30] = xCriticalNesting

    # Save mstatus
    csrr t0, mstatus
    sw t0, 4(sp)                    # [1] = mstatus

    # Save sp to current TCB
    la t0, pxCurrentTCB
    lw t0, 0(t0)
    sw sp, 0(t0)

    # Check mcause
    csrr a0, mcause
    bltz a0, handle_interrupt       # MSB set = interrupt

handle_exception:
    # Save mepc + 4 (skip ecall instruction)
    csrr t0, mepc
    addi t0, t0, 4
    sw t0, 0(sp)                    # [0] = mepc

    # Check if ecall (mcause == 11)
    li t1, 11
    bne a0, t1, unhandled_exception

    # ecall = context switch request
    # Switch to ISR stack
    la sp, xISRStackTop
    lw sp, 0(sp)

    call vTaskSwitchContext
    j restore_context

handle_interrupt:
    # Save mepc unchanged (async interrupt)
    csrr t0, mepc
    sw t0, 0(sp)                    # [0] = mepc

    # Switch to the aligned ISR stack before calling Rust code.
    la sp, xISRStackTop
    lw sp, 0(sp)

    # Record real interrupt context independently of mstatus.MIE. Critical
    # sections also clear MIE and must not be mistaken for an ISR.
    la t0, uxPortInterruptNesting
    lw t1, 0(t0)
    addi t1, t1, 1
    sw t1, 0(t0)

    # Check the asynchronous cause code.
    andi a0, a0, 0x7FF
    li t1, 7                       # Machine timer interrupt.
    beq a0, t1, handle_timer_interrupt
    li t1, 11                      # Machine external interrupt.
    beq a0, t1, handle_external_interrupt
    j unhandled_interrupt

handle_timer_interrupt:
    call vPortUpdateTimerCompare
    call xTaskIncrementTick
    beqz a0, complete_interrupt

    # Defer all ISR-requested switches to one common epilogue.
    la t0, xPortYieldPending
    li t1, 1
    sw t1, 0(t0)
    j complete_interrupt

handle_external_interrupt:
    # a0 remains the decoded machine-external cause (11). The registered
    # application callback claims/completes the platform interrupt controller.
    call xPortHandleExternalInterrupt
    beqz a0, complete_interrupt

    la t0, xPortYieldPending
    li t1, 1
    sw t1, 0(t0)

complete_interrupt:
    # Atomically consume a request made either by the tick path, the external
    # dispatcher return value, or portYIELD_FROM_ISR() inside a callback.
    la t0, xPortYieldPending
    lw t1, 0(t0)
    sw zero, 0(t0)
    beqz t1, interrupt_switch_complete

    call vTaskSwitchContext

interrupt_switch_complete:
    la t0, uxPortInterruptNesting
    lw t1, 0(t0)
    addi t1, t1, -1
    sw t1, 0(t0)
    j restore_context

unhandled_exception:
    # Loop forever on unhandled trap
    j unhandled_exception

unhandled_interrupt:
    # Preserve an accurate nesting count for debugger inspection, then stop.
    j unhandled_interrupt

restore_context:
    # Load pxCurrentTCB
    la t1, pxCurrentTCB
    lw t1, 0(t1)
    lw sp, 0(t1)                    # sp = pxCurrentTCB->pxTopOfStack

    # Restore mepc
    lw t0, 0(sp)
    csrw mepc, t0

    # Restore mstatus
    lw t0, 4(sp)
    csrw mstatus, t0

    # Restore xCriticalNesting
    lw t0, 120(sp)
    la t1, xCriticalNesting
    sw t0, 0(t1)

    # Restore registers
    lw x1,   8(sp)                  # ra
    lw x5,  12(sp)                  # t0
    lw x6,  16(sp)                  # t1
    lw x7,  20(sp)                  # t2
    lw x8,  24(sp)                  # s0/fp
    lw x9,  28(sp)                  # s1
    lw x10, 32(sp)                  # a0
    lw x11, 36(sp)                  # a1
    lw x12, 40(sp)                  # a2
    lw x13, 44(sp)                  # a3
    lw x14, 48(sp)                  # a4
    lw x15, 52(sp)                  # a5
    lw x16, 56(sp)                  # a6
    lw x17, 60(sp)                  # a7
    lw x18, 64(sp)                  # s2
    lw x19, 68(sp)                  # s3
    lw x20, 72(sp)                  # s4
    lw x21, 76(sp)                  # s5
    lw x22, 80(sp)                  # s6
    lw x23, 84(sp)                  # s7
    lw x24, 88(sp)                  # s8
    lw x25, 92(sp)                  # s9
    lw x26, 96(sp)                  # s10
    lw x27, 100(sp)                 # s11
    lw x28, 104(sp)                 # t3
    lw x29, 108(sp)                 # t4
    lw x30, 112(sp)                 # t5
    lw x31, 116(sp)                 # t6

    # Restore sp
    addi sp, sp, 124

    # Return from trap
    mret
"#
);

// =============================================================================
// Utility Functions
// =============================================================================

/// Check if currently executing in an interrupt context
pub fn xPortIsInsideInterrupt() -> BaseType_t {
    let uxNesting = unsafe { core::ptr::read_volatile(&raw const uxPortInterruptNesting) };
    if uxNesting != 0 {
        crate::types::pdTRUE
    } else {
        crate::types::pdFALSE
    }
}

/// No-op instruction
#[inline(always)]
pub fn portNOP() {
    unsafe {
        core::arch::asm!("nop", options(nomem, nostack, preserves_flags));
    }
}

/// Memory barrier
#[inline(always)]
pub fn portMEMORY_BARRIER() {
    unsafe {
        core::arch::asm!("fence iorw, iorw", options(nostack));
    }
}

// =============================================================================
// Run-time Stats (Optional)
// =============================================================================

#[cfg(feature = "generate-run-time-stats")]
pub fn portCONFIGURE_TIMER_FOR_RUN_TIME_STATS() {}

#[cfg(feature = "generate-run-time-stats")]
pub fn portGET_RUN_TIME_COUNTER_VALUE() -> u32 {
    // The task kernel subtracts its scheduler-start baseline.
    ullPortReadMtime() as u32
}

// =============================================================================
// Tickless Idle
// =============================================================================

#[cfg(feature = "tickless-idle")]
static mut uxTicklessSleepCount: UBaseType_t = 0;
#[cfg(feature = "tickless-idle")]
static mut xLastSuppressedTicks: TickType_t = 0;
#[cfg(feature = "tickless-idle")]
static mut xLastCompletedSleepTicks: TickType_t = 0;

/// Number of non-aborted tickless sleeps entered since boot.
#[cfg(feature = "tickless-idle")]
pub fn uxPortGetTicklessSleepCount() -> UBaseType_t {
    unsafe { core::ptr::read_volatile(&raw const uxTicklessSleepCount) }
}

/// Most recent expected-idle interval programmed by the tickless path.
#[cfg(feature = "tickless-idle")]
pub fn xPortGetLastSuppressedTicks() -> TickType_t {
    unsafe { core::ptr::read_volatile(&raw const xLastSuppressedTicks) }
}

/// Complete ticks stepped during the most recent tickless sleep.
#[cfg(feature = "tickless-idle")]
pub fn xPortGetLastCompletedSleepTicks() -> TickType_t {
    unsafe { core::ptr::read_volatile(&raw const xLastCompletedSleepTicks) }
}

/// Suppress periodic MTIME interrupts while the idle task sleeps.
///
/// A timer-expiry wake steps all but the final tick, then leaves MTIP pending;
/// the normal trap path accounts the final tick and advances the shared
/// fractional phase. An early external-interrupt wake instead installs the
/// first still-future fractional tick boundary.
#[cfg(feature = "tickless-idle")]
pub fn vPortSuppressTicksAndSleep(xExpectedIdleTime: TickType_t) {
    if xExpectedIdleTime == 0 {
        return;
    }

    unsafe {
        let uxSavedMstatus = portSET_INTERRUPT_MASK_FROM_ISR();

        // Safety: the idle task suspended the scheduler and MIE is masked for
        // this tickless confirmation point.
        if eTaskConfirmSleepModeStatus() == crate::types::eSleepModeStatus::eAbortSleep {
            portCLEAR_INTERRUPT_MASK_FROM_ISR(uxSavedMstatus);
            return;
        }

        let ullOriginalNextTime = ullNextTime;
        let ulOriginalFraction = TIMER_FRACTION;
        let ullAdditionalIntervals = xExpectedIdleTime.saturating_sub(1) as u64;
        let (ullSuppressedCounts, ulTargetFraction) =
            prvTimerAdvance(ulOriginalFraction, ullAdditionalIntervals);
        let ullWakeTime = ullOriginalNextTime.wrapping_add(ullSuppressedCounts);

        ullNextTime = ullWakeTime;
        TIMER_FRACTION = ulTargetFraction;
        prvWriteMtimeCompare(ullWakeTime);

        uxTicklessSleepCount = uxTicklessSleepCount.wrapping_add(1);
        xLastSuppressedTicks = xExpectedIdleTime;
        xLastCompletedSleepTicks = 0;

        core::arch::asm!("wfi", options(nostack));

        let ullNow = ullPortReadMtime();
        let xCompleteTicks: TickType_t;

        if ullNow >= ullWakeTime {
            /* MTIP is pending. Keep the elapsed compare value installed so
             * restoring MIE enters the normal timer trap for the final tick. */
            xCompleteTicks = xExpectedIdleTime.saturating_sub(1);
        } else {
            /* An external interrupt woke WFI early. Count boundaries at or
             * before ullNow, then program the first boundary still in the
             * future. */
            let mut ullElapsedIntervals = 0u64;

            if ullNow >= ullOriginalNextTime {
                let ullCountsAfterFirst = ullNow - ullOriginalNextTime;
                let ulMtimeHz = configMTIME_HZ as u64;
                let ulTickRate = configTICK_RATE_HZ as u64;

                ullElapsedIntervals = ullCountsAfterFirst.saturating_mul(ulTickRate) / ulMtimeHz;
                ullElapsedIntervals = ullElapsedIntervals.min(ullAdditionalIntervals);

                while ullElapsedIntervals < ullAdditionalIntervals {
                    let (ullCounts, _) =
                        prvTimerAdvance(ulOriginalFraction, ullElapsedIntervals + 1);
                    if ullCounts > ullCountsAfterFirst {
                        break;
                    }
                    ullElapsedIntervals += 1;
                }

                while ullElapsedIntervals > 0 {
                    let (ullCounts, _) = prvTimerAdvance(ulOriginalFraction, ullElapsedIntervals);
                    if ullCounts <= ullCountsAfterFirst {
                        break;
                    }
                    ullElapsedIntervals -= 1;
                }
            }

            let ullComplete = if ullNow >= ullOriginalNextTime {
                1u64.saturating_add(ullElapsedIntervals)
            } else {
                0
            };
            xCompleteTicks = ullComplete.min(xExpectedIdleTime as u64) as TickType_t;

            let (ullNextDelta, ulNextFraction) = prvTimerAdvance(ulOriginalFraction, ullComplete);
            ullNextTime = ullOriginalNextTime.wrapping_add(ullNextDelta);
            TIMER_FRACTION = ulNextFraction;
            prvWriteMtimeCompare(ullNextTime);
        }

        xLastCompletedSleepTicks = xCompleteTicks;
        if xCompleteTicks > 0 {
            // Safety: MIE remains masked, the scheduler is suspended, and the
            // elapsed count was bounded by the programmed compare interval.
            vTaskStepTick(xCompleteTicks);
        }

        portCLEAR_INTERRUPT_MASK_FROM_ISR(uxSavedMstatus);
    }
}
