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
            options(nomem, nostack)
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
            options(nomem, nostack)
        );
    }
}

/// Enter critical section
pub fn portENTER_CRITICAL() {
    portDISABLE_INTERRUPTS();
    unsafe {
        xCriticalNesting += 1;
    }
}

/// Exit critical section
pub fn portEXIT_CRITICAL() {
    unsafe {
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
            "csrr {mstatus}, mstatus",
            "csrc mstatus, {mie_bit}",
            mstatus = out(reg) mstatus,
            mie_bit = in(reg) 0x8,
            options(nomem, nostack)
        );
    }
    mstatus as UBaseType_t
}

/// Restore interrupt mask (for ISR use)
#[inline(always)]
pub fn portCLEAR_INTERRUPT_MASK_FROM_ISR(saved_mstatus: UBaseType_t) {
    unsafe {
        core::arch::asm!(
            "csrw mstatus, {mstatus}",
            mstatus = in(reg) saved_mstatus as u32,
            options(nomem, nostack)
        );
    }
}

// =============================================================================
// Context Switching
// =============================================================================

/// Trigger a context switch via ecall (environment call)
#[inline(always)]
pub fn portYIELD() {
    unsafe {
        core::arch::asm!("ecall", options(nomem, nostack));
    }
}

/// Yield from ISR if required
#[inline(always)]
pub fn portYIELD_FROM_ISR(xSwitchRequired: BaseType_t) {
    if xSwitchRequired != crate::types::pdFALSE {
        // Set a flag that the trap handler will check
        unsafe {
            YIELD_PENDING = true;
        }
    }
}

/// Alias for portYIELD_FROM_ISR
#[inline(always)]
pub fn portEND_SWITCHING_ISR(xSwitchRequired: BaseType_t) {
    portYIELD_FROM_ISR(xSwitchRequired);
}

/// Flag indicating a yield is pending from ISR
static mut YIELD_PENDING: bool = false;

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
pub fn pxPortInitialiseStack(
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

/// Timer increment for one tick
static mut TIMER_INCREMENT: u64 = 0;

/// Next timer compare value
#[no_mangle]
pub static mut ullNextTime: u64 = 0;

/// Set up the timer to generate tick interrupts
#[no_mangle]
pub extern "C" fn vPortSetupTimerInterrupt() {
    unsafe {
        // Calculate timer increment for one tick
        // RISC-V uses MTIME which may run at a different frequency than CPU
        TIMER_INCREMENT = (configMTIME_HZ / configTICK_RATE_HZ as u32) as u64;

        // Read current MTIME value
        let mtime_lo = core::ptr::read_volatile(MTIME_ADDR as *const u32);
        let mtime_hi = core::ptr::read_volatile((MTIME_ADDR + 4) as *const u32);
        let mtime = ((mtime_hi as u64) << 32) | (mtime_lo as u64);

        // Set first compare value
        ullNextTime = mtime + TIMER_INCREMENT;

        // Write to MTIMECMP (write high word first with max value to avoid spurious interrupt)
        core::ptr::write_volatile((MTIMECMP_ADDR + 4) as *mut u32, 0xFFFFFFFF);
        core::ptr::write_volatile(MTIMECMP_ADDR as *mut u32, ullNextTime as u32);
        core::ptr::write_volatile((MTIMECMP_ADDR + 4) as *mut u32, (ullNextTime >> 32) as u32);

        // Prepare next compare value
        ullNextTime += TIMER_INCREMENT;

        // Enable machine timer interrupt (mie.MTIE = bit 7)
        core::arch::asm!(
            "csrs mie, {mtie}",
            mtie = in(reg) 0x80,
            options(nomem, nostack)
        );
    }
}

/// Update timer compare register for next tick
#[no_mangle]
pub extern "C" fn vPortUpdateTimerCompare() {
    unsafe {
        // Write to MTIMECMP
        core::ptr::write_volatile((MTIMECMP_ADDR + 4) as *mut u32, 0xFFFFFFFF);
        core::ptr::write_volatile(MTIMECMP_ADDR as *mut u32, ullNextTime as u32);
        core::ptr::write_volatile((MTIMECMP_ADDR + 4) as *mut u32, (ullNextTime >> 32) as u32);

        // Prepare next compare value
        ullNextTime += TIMER_INCREMENT;
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

        // Set up the timer
        vPortSetupTimerInterrupt();

        // Set trap vector to our handler (direct mode)
        core::arch::asm!(
            "la t0, freertos_risc_v_trap_handler",
            "csrw mtvec, t0",
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
        // Set MIE bit so interrupts are enabled after mret
        "ori t0, t0, 0x8",
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

    # Check if machine timer interrupt (mcause & 0x7FF == 7)
    andi a0, a0, 0x7FF
    li t1, 7
    bne a0, t1, unhandled_interrupt

    # Machine timer interrupt - switch to ISR stack
    la sp, xISRStackTop
    lw sp, 0(sp)

    # Update timer for next tick
    call vPortUpdateTimerCompare

    # Process tick
    call xTaskIncrementTick
    beqz a0, restore_context        # No context switch needed

    # Context switch needed
    call vTaskSwitchContext
    j restore_context

unhandled_exception:
unhandled_interrupt:
    # Loop forever on unhandled trap
    j unhandled_exception

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
    // Check if we're in an interrupt by looking at mstatus.MIE
    // When in a trap handler, MIE is cleared
    let mstatus: u32;
    unsafe {
        core::arch::asm!(
            "csrr {mstatus}, mstatus",
            mstatus = out(reg) mstatus,
            options(nomem, nostack)
        );
    }
    // If MIE is 0 and we're running, we're in an interrupt
    if (mstatus & 0x8) == 0 {
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
        core::arch::asm!("fence iorw, iorw", options(nomem, nostack));
    }
}

// =============================================================================
// Run-time Stats (Optional)
// =============================================================================

#[cfg(feature = "generate-run-time-stats")]
static mut RUN_TIME_COUNTER: u32 = 0;

#[cfg(feature = "generate-run-time-stats")]
pub fn portCONFIGURE_TIMER_FOR_RUN_TIME_STATS() {
    unsafe {
        RUN_TIME_COUNTER = 0;
    }
}

#[cfg(feature = "generate-run-time-stats")]
pub fn portGET_RUN_TIME_COUNTER_VALUE() -> u32 {
    // Use MTIME as the run-time counter source
    unsafe { core::ptr::read_volatile(MTIME_ADDR as *const u32) }
}

// =============================================================================
// Tickless Idle (Optional - not implemented for RISC-V yet)
// =============================================================================

#[cfg(feature = "tickless-idle")]
pub fn vPortSuppressTicksAndSleep(_xExpectedIdleTime: TickType_t) {
    // TODO: Implement tickless idle for RISC-V
    // For now, just do a WFI
    unsafe {
        core::arch::asm!("wfi", options(nomem, nostack));
    }
}
