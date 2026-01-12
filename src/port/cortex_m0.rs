/*
 * FreeRTOS Kernel <DEVELOPMENT BRANCH>
 * Copyright (C) 2021 Amazon.com, Inc. or its affiliates. All Rights Reserved.
 *
 * SPDX-License-Identifier: MIT
 *
 * [AMENDMENT] This is the Cortex-M0/M0+ port for FreeRusTOS.
 * Ported from portable/GCC/ARM_CM0/port.c and portasm.c
 *
 * This port is for ARM Cortex-M0/M0+ (ARMv6-M, no FPU).
 * It uses:
 * - SysTick for tick interrupts
 * - PendSV for context switching
 * - SVC for starting the first task
 * - PRIMASK for interrupt masking (no BASEPRI on ARMv6-M)
 *
 * Key differences from Cortex-M3/M4:
 * - Uses PRIMASK instead of BASEPRI (no priority-based masking)
 * - Thumb-1 only: `movs`, `adds`, `subs`, etc. (not `mov`, `add`)
 * - Cannot use `{r4-r11}` in ldmia/stmia - must split into two operations
 * - High registers (r8-r11) must be moved to low registers (r4-r7) before store
 * - No VTOR on some devices - don't reset MSP via VTOR in vStartFirstTask
 * - EXC_RETURN stored in stack frame
 */

//! ARM Cortex-M0/M0+ Port Implementation
//!
//! This port provides hardware-specific implementations for ARM Cortex-M0/M0+:
//! - Critical sections using PRIMASK register (disables all interrupts)
//! - Context switching via PendSV exception
//! - First task start via SVC exception
//! - Tick timer using SysTick
//!
//! ## Usage
//!
//! Enable with `--features port-cortex-m0` and compile for `thumbv6m-none-eabi`.

use core::arch::{global_asm, naked_asm};
use core::ffi::c_void;

use crate::config::*;
use crate::types::*;

// =============================================================================
// Port Constants
// =============================================================================

/// Stack growth direction: -1 for descending (Cortex-M uses descending stack)
pub const portSTACK_GROWTH: BaseType_t = -1;

/// Byte alignment requirement for stack
pub const portBYTE_ALIGNMENT: usize = 8;

/// Initial xPSR value - thumb bit set
const portINITIAL_XPSR: StackType_t = 0x0100_0000;

/// Initial EXC_RETURN value for thread mode, PSP, no FPU
/// Bit[3] = 1: Return to Thread mode
/// Bit[2] = 1: Return using PSP
const portINITIAL_EXC_RETURN: StackType_t = 0xFFFF_FFFD;

/// Priority for PendSV and SysTick (lowest priority = 255)
const portMIN_INTERRUPT_PRIORITY: u8 = 255;

/// NVIC ICSR register address (for pending PendSV)
const NVIC_ICSR: *mut u32 = 0xE000_ED04 as *mut u32;

/// Bit to pend PendSV
const NVIC_PENDSVSET_BIT: u32 = 1 << 28;

/// SysTick control register
const SYST_CSR: *mut u32 = 0xE000_E010 as *mut u32;
/// SysTick reload value register
const SYST_RVR: *mut u32 = 0xE000_E014 as *mut u32;
/// SysTick current value register
const SYST_CVR: *mut u32 = 0xE000_E018 as *mut u32;

/// SysTick enable bit
const SYST_CSR_ENABLE: u32 = 1 << 0;
/// SysTick interrupt enable bit
const SYST_CSR_TICKINT: u32 = 1 << 1;
/// SysTick clock source (processor clock)
const SYST_CSR_CLKSOURCE: u32 = 1 << 2;

// =============================================================================
// Critical Section Management
// =============================================================================

/// Critical section nesting counter.
/// This is safe to use as a static mut because:
/// 1. Interrupts are disabled before any access (via PRIMASK)
/// 2. Cortex-M0 doesn't have atomic instructions for fetch_add/fetch_sub
static mut CRITICAL_NESTING: u32 = 0;

/// Enter a critical section (disable interrupts via PRIMASK)
#[inline(always)]
pub fn portENTER_CRITICAL() {
    portDISABLE_INTERRUPTS();
    unsafe {
        CRITICAL_NESTING += 1;
        core::arch::asm!("dsb", options(nomem, nostack));
        core::arch::asm!("isb", options(nomem, nostack));
    }
}

/// Exit a critical section (potentially re-enable interrupts)
#[inline(always)]
pub fn portEXIT_CRITICAL() {
    unsafe {
        CRITICAL_NESTING -= 1;
        if CRITICAL_NESTING == 0 {
            portENABLE_INTERRUPTS();
        }
    }
}

/// Disable interrupts by setting PRIMASK
///
/// Note: Unlike BASEPRI on Cortex-M3/M4, PRIMASK disables ALL interrupts,
/// including the tick interrupt. There is no priority-based masking.
#[inline(always)]
pub fn portDISABLE_INTERRUPTS() {
    unsafe {
        core::arch::asm!("cpsid i", options(nomem, nostack));
    }
}

/// Enable interrupts by clearing PRIMASK
#[inline(always)]
pub fn portENABLE_INTERRUPTS() {
    unsafe {
        core::arch::asm!("cpsie i", options(nomem, nostack));
    }
}

/// Set interrupt mask from ISR (save PRIMASK and disable)
#[inline(always)]
pub fn portSET_INTERRUPT_MASK_FROM_ISR() -> UBaseType_t {
    let primask: u32;
    unsafe {
        core::arch::asm!("mrs {}, PRIMASK", out(reg) primask, options(nomem, nostack));
        core::arch::asm!("cpsid i", options(nomem, nostack));
    }
    primask as UBaseType_t
}

/// Clear interrupt mask from ISR (restore PRIMASK)
#[inline(always)]
pub fn portCLEAR_INTERRUPT_MASK_FROM_ISR(uxSavedInterruptStatus: UBaseType_t) {
    unsafe {
        core::arch::asm!("msr PRIMASK, {}", in(reg) uxSavedInterruptStatus as u32, options(nomem, nostack));
    }
}

// =============================================================================
// Context Switching / Yield
// =============================================================================

/// Trigger a context switch by pending PendSV
#[inline(always)]
pub fn portYIELD() {
    unsafe {
        core::ptr::write_volatile(NVIC_ICSR, NVIC_PENDSVSET_BIT);
        core::arch::asm!("dsb", options(nomem, nostack));
        core::arch::asm!("isb", options(nomem, nostack));
    }
}

/// Yield from ISR if needed
#[inline(always)]
pub fn portYIELD_FROM_ISR(xSwitchRequired: BaseType_t) {
    if xSwitchRequired != pdFALSE {
        portYIELD();
    }
}

/// End switching ISR (same as yield from ISR)
#[inline(always)]
pub fn portEND_SWITCHING_ISR(xSwitchRequired: BaseType_t) {
    portYIELD_FROM_ISR(xSwitchRequired);
}

// =============================================================================
// Stack Initialization
// =============================================================================

/// Initialize a task's stack
///
/// Sets up the initial stack frame so the task can be started by the scheduler.
/// The stack frame mimics what the hardware saves on exception entry, plus
/// the registers we save manually in PendSV.
///
/// Stack layout (high to low address) - CM0/CM0+:
/// - xPSR (thumb bit set)
/// - PC (task entry point)
/// - LR (task exit error handler)
/// - R12, R3, R2, R1, R0 (R0 = pvParameters)
/// - R11, R10, R9, R8, R7, R6, R5, R4
/// - EXC_RETURN (0xFFFFFFFD)
pub fn pxPortInitialiseStack(
    pxTopOfStack: *mut StackType_t,
    pxCode: TaskFunction_t,
    pvParameters: *mut c_void,
) -> *mut StackType_t {
    unsafe {
        let mut pxStack = pxTopOfStack;

        // Offset for 8-byte alignment
        pxStack = pxStack.sub(1);

        // xPSR - thumb bit must be set
        *pxStack = portINITIAL_XPSR;
        pxStack = pxStack.sub(1);

        // PC - task entry point
        *pxStack = pxCode as StackType_t;
        pxStack = pxStack.sub(1);

        // LR - return address (prvTaskExitError would go here)
        *pxStack = prvTaskExitError as StackType_t;
        pxStack = pxStack.sub(1);

        // R12 - skip initialization
        pxStack = pxStack.sub(1);
        // R3
        pxStack = pxStack.sub(1);
        // R2
        pxStack = pxStack.sub(1);
        // R1
        pxStack = pxStack.sub(1);

        // R0 - first argument = pvParameters
        *pxStack = pvParameters as StackType_t;

        // R11, R10, R9, R8, R7, R6, R5, R4 - skip initialization (8 words)
        // Plus 1 word for EXC_RETURN = 9 words total
        pxStack = pxStack.sub(9);

        // EXC_RETURN - stored at bottom of manual context
        *pxStack = portINITIAL_EXC_RETURN;

        pxStack
    }
}

/// Called if a task returns from its implementing function
fn prvTaskExitError() -> ! {
    portDISABLE_INTERRUPTS();
    loop {
        unsafe {
            core::arch::asm!("wfi", options(nomem, nostack));
        }
    }
}

// =============================================================================
// Exception Handlers
// =============================================================================

// Exception handlers defined via global_asm to avoid symbol conflicts with cortex-m-rt.
// These use .global and .thumb_func to define proper Thumb function symbols.
global_asm!(
    ".syntax unified",
    ".section .text.SVCall",
    ".global SVCall",
    ".thumb_func",
    "SVCall:",
    // Check if we're using MSP or PSP (bit 2 of LR)
    "movs r0, #4",
    "mov r1, lr",
    "tst r0, r1",
    "beq 1f",
    // Using PSP
    "mrs r0, psp",
    "b 2f",
    // Using MSP
    "1:",
    "mrs r0, msp",
    "2:",
    // Call the C handler
    "ldr r3, =vPortSVCHandler_C",
    "bx r3",
);

/// Rust alias for SVCall handler (for API compatibility)
#[no_mangle]
pub unsafe extern "C" fn vPortSVCHandler() {
    // This is a stub - the actual implementation is in global_asm above
    // This function exists only for the public API
    extern "C" {
        fn SVCall();
    }
    SVCall();
}

/// C part of SVC handler - starts the first task
#[no_mangle]
pub extern "C" fn vPortSVCHandler_C(_pulCallerStackAddress: *mut u32) {
    // For non-MPU port, we just restore the first task's context
    // SAFETY: This is called from the SVC handler to start the first task.
    // The assembly in vRestoreContextOfFirstTask restores context and returns to thread mode.
    unsafe {
        vRestoreContextOfFirstTask();
    }
}

/// Restore the context of the first task
///
/// This is called from vPortSVCHandler_C to start the first task.
/// Thumb-1 compatible assembly.
#[unsafe(naked)]
#[no_mangle]
pub unsafe extern "C" fn vRestoreContextOfFirstTask() {
    naked_asm!(
        ".syntax unified",
        // Get pxCurrentTCB address
        "ldr r2, =pxCurrentTCB",
        "ldr r1, [r2]", // r1 = pxCurrentTCB
        "ldr r0, [r1]", // r0 = pxCurrentTCB->pxTopOfStack
        // Pop EXC_RETURN into r2
        "ldm r0!, {{r2}}", // r2 = EXC_RETURN
        // Set CONTROL register to use PSP in thread mode
        "movs r1, #2",
        "msr CONTROL, r1",
        // Discard r4-r11 from stack (we skip restoring them as they're not used yet)
        // Stack currently has: [EXC_RETURN already popped] r4-r7, r8-r11
        "adds r0, #32", // Skip 8 registers (32 bytes)
        // Set PSP to point to hardware-saved context (r0, r1, r2, r3, r12, lr, pc, xpsr)
        "msr psp, r0",
        "isb",
        // Return to thread mode using the EXC_RETURN in r2
        "bx r2",
    );
}

// PendSV Handler via global_asm
global_asm!(
    ".syntax unified",
    ".section .text.PendSV",
    ".global PendSV",
    ".thumb_func",
    "PendSV:",
    // Get PSP (current task's stack pointer)
    "mrs r0, psp",
    // Get pxCurrentTCB address
    "ldr r2, =pxCurrentTCB",
    "ldr r1, [r2]", // r1 = pxCurrentTCB
    // Make space on stack for LR(EXC_RETURN) + r4-r11 = 9 words = 36 bytes
    "subs r0, r0, #36",
    // Save new top of stack into TCB
    "str r0, [r1]",
    // Save LR (EXC_RETURN) and r4-r7
    "mov r3, lr",           // r3 = EXC_RETURN
    "stmia r0!, {{r3-r7}}", // Store EXC_RETURN, r4, r5, r6, r7
    // Move r8-r11 to r4-r7, then store
    "mov r4, r8",
    "mov r5, r9",
    "mov r6, r10",
    "mov r7, r11",
    "stmia r0!, {{r4-r7}}", // Store r8-r11 (via r4-r7)
    // Disable interrupts during context switch
    "cpsid i",
    // Call vTaskSwitchContext to select next task
    "bl vTaskSwitchContext",
    // Re-enable interrupts
    "cpsie i",
    // Get new task's TCB
    "ldr r2, =pxCurrentTCB",
    "ldr r1, [r2]", // r1 = new pxCurrentTCB
    "ldr r0, [r1]", // r0 = new pxCurrentTCB->pxTopOfStack
    // Move past EXC_RETURN and r4-r7 to get to r8-r11
    "adds r0, r0, #20", // Skip 5 words (20 bytes)
    // Restore r8-r11 from stack (load into r4-r7, move to r8-r11)
    "ldmia r0!, {{r4-r7}}",
    "mov r8, r4",
    "mov r9, r5",
    "mov r10, r6",
    "mov r11, r7",
    // PSP now points past the saved context
    "msr psp, r0",
    // Go back to start of saved context to restore EXC_RETURN and r4-r7
    "subs r0, r0, #36",
    "ldmia r0!, {{r3-r7}}", // r3 = EXC_RETURN, r4-r7 restored
    // Return using EXC_RETURN in r3
    "bx r3",
);

/// Rust alias for PendSV handler (for API compatibility)
#[no_mangle]
pub unsafe extern "C" fn xPortPendSVHandler() {
    extern "C" {
        fn PendSV();
    }
    PendSV();
}

// SysTick Handler wrapper via global_asm - calls xPortSysTickHandler_impl
global_asm!(
    ".syntax unified",
    ".section .text.SysTick",
    ".global SysTick",
    ".thumb_func",
    "SysTick:",
    "push {{lr}}",
    "bl xPortSysTickHandler_impl",
    "pop {{pc}}",
);

/// SysTick Handler implementation
#[no_mangle]
extern "C" fn xPortSysTickHandler_impl() {
    let saved = portSET_INTERRUPT_MASK_FROM_ISR();

    // Increment the run-time counter for run-time stats
    #[cfg(feature = "generate-run-time-stats")]
    portINCREMENT_RUN_TIME_COUNTER();

    crate::trace::traceISR_ENTER();

    if crate::kernel::tasks::xTaskIncrementTick() != pdFALSE {
        crate::trace::traceISR_EXIT_TO_SCHEDULER();
        unsafe {
            core::ptr::write_volatile(NVIC_ICSR, NVIC_PENDSVSET_BIT);
        }
    } else {
        crate::trace::traceISR_EXIT();
    }

    portCLEAR_INTERRUPT_MASK_FROM_ISR(saved);
}

/// Rust alias for SysTick handler (for API compatibility)
#[no_mangle]
pub extern "C" fn xPortSysTickHandler() {
    xPortSysTickHandler_impl();
}

// =============================================================================
// Scheduler Start
// =============================================================================

/// Start the first task
///
/// On CM0, we don't reset MSP via VTOR because VTOR is optional on CM0/CM0+.
/// Instead, just enable interrupts and trigger SVC.
#[unsafe(naked)]
unsafe extern "C" fn prvPortStartFirstTask() {
    naked_asm!(
        ".syntax unified",
        // Don't reset MSP via VTOR as VTOR is optional on CM0/CM0+

        // Enable interrupts
        "cpsie i",
        // Synchronization barriers
        "dsb",
        "isb",
        // Trigger SVC to start the first task
        // SVC number 0 = start scheduler
        "svc #0",
        // Should never get here
        "nop",
    );
}

/// Set up the SysTick timer
pub fn vPortSetupTimerInterrupt() {
    unsafe {
        // Disable SysTick and configure clock source first (QEMU workaround)
        core::ptr::write_volatile(SYST_CSR, SYST_CSR_CLKSOURCE);

        // Clear current value
        core::ptr::write_volatile(SYST_CVR, 0);

        // Set reload value = (CPU clock / tick rate) - 1
        let reload = (configCPU_CLOCK_HZ / configTICK_RATE_HZ as u32) - 1;
        core::ptr::write_volatile(SYST_RVR, reload);

        // Initialize tickless idle variables
        #[cfg(feature = "tickless-idle")]
        prvSetupTicklessIdle();

        // Enable SysTick with processor clock and interrupt
        core::ptr::write_volatile(
            SYST_CSR,
            SYST_CSR_ENABLE | SYST_CSR_TICKINT | SYST_CSR_CLKSOURCE,
        );
    }
}

/// Start the scheduler
///
/// Sets up interrupt priorities, the tick timer, and starts the first task.
/// This function does not return.
pub fn xPortStartScheduler() -> BaseType_t {
    // Set PendSV and SysTick to lowest priority
    // SHPR3 register: PendSV priority in bits [23:16], SysTick in bits [31:24]
    const SHPR3: *mut u32 = 0xE000_ED20 as *mut u32;
    unsafe {
        let mut shpr3 = core::ptr::read_volatile(SHPR3);
        shpr3 |= (portMIN_INTERRUPT_PRIORITY as u32) << 16; // PendSV
        shpr3 |= (portMIN_INTERRUPT_PRIORITY as u32) << 24; // SysTick
        core::ptr::write_volatile(SHPR3, shpr3);
    }

    // Initialize critical nesting count
    // SAFETY: This is called at scheduler start before any tasks are running.
    unsafe {
        CRITICAL_NESTING = 0;
    }

    // Set up the tick timer
    vPortSetupTimerInterrupt();

    // Start the first task
    unsafe {
        prvPortStartFirstTask();
    }

    // Should never get here
    0
}

/// End the scheduler (not really supported on Cortex-M)
pub fn vPortEndScheduler() {
    loop {
        unsafe {
            core::arch::asm!("wfi", options(nomem, nostack));
        }
    }
}

// =============================================================================
// Utility Functions
// =============================================================================

/// Check if currently executing in an interrupt context
#[inline(always)]
pub fn xPortIsInsideInterrupt() -> BaseType_t {
    let ipsr: u32;
    unsafe {
        core::arch::asm!("mrs {}, ipsr", out(reg) ipsr);
    }
    if ipsr != 0 {
        pdTRUE
    } else {
        pdFALSE
    }
}

/// No-operation
#[inline(always)]
pub fn portNOP() {
    unsafe {
        core::arch::asm!("nop", options(nomem, nostack));
    }
}

/// Memory barrier
#[inline(always)]
pub fn portMEMORY_BARRIER() {
    unsafe {
        core::arch::asm!("dmb", options(nomem, nostack));
    }
}

// =============================================================================
// Architecture Name
// =============================================================================

/// Architecture name string for this port
pub const portARCH_NAME: &str = "ARM Cortex-M0";

// =============================================================================
// Run-time Stats Timer Support
// =============================================================================

/// Run-time stats counter value.
#[cfg(feature = "generate-run-time-stats")]
static mut ulRunTimeCounterValue: crate::config::configRUN_TIME_COUNTER_TYPE = 0;

/// Configure the timer for run-time stats collection.
#[cfg(feature = "generate-run-time-stats")]
#[inline(always)]
pub fn portCONFIGURE_TIMER_FOR_RUN_TIME_STATS() {
    unsafe {
        ulRunTimeCounterValue = 0;
    }
}

/// Get the current run-time counter value.
#[cfg(feature = "generate-run-time-stats")]
#[inline(always)]
pub fn portGET_RUN_TIME_COUNTER_VALUE() -> crate::config::configRUN_TIME_COUNTER_TYPE {
    unsafe { ulRunTimeCounterValue }
}

/// Increment the run-time counter.
#[cfg(feature = "generate-run-time-stats")]
#[inline(always)]
pub fn portINCREMENT_RUN_TIME_COUNTER() {
    unsafe {
        ulRunTimeCounterValue = ulRunTimeCounterValue.wrapping_add(1);
    }
}

// =============================================================================
// Tickless Idle Support
// =============================================================================

/// Number of timer counts that make up one tick period.
#[cfg(feature = "tickless-idle")]
static mut ulTimerCountsForOneTick: u32 = 0;

/// Maximum number of tick periods that can be suppressed.
#[cfg(feature = "tickless-idle")]
static mut xMaximumPossibleSuppressedTicks: TickType_t = 0;

/// Compensation value for the time the SysTick is stopped.
#[cfg(feature = "tickless-idle")]
static mut ulStoppedTimerCompensation: u32 = 0;

/// SysTick COUNT flag bit
#[cfg(feature = "tickless-idle")]
const portNVIC_SYSTICK_COUNT_FLAG_BIT: u32 = 1 << 16;

/// Bit in ICSR to clear pending SysTick interrupt
#[cfg(feature = "tickless-idle")]
const portNVIC_PEND_SYSTICK_CLEAR_BIT: u32 = 1 << 25;

/// Initialize tickless idle variables.
#[cfg(feature = "tickless-idle")]
fn prvSetupTicklessIdle() {
    unsafe {
        ulTimerCountsForOneTick = configCPU_CLOCK_HZ / configTICK_RATE_HZ as u32;
        xMaximumPossibleSuppressedTicks = (0x00FF_FFFF / ulTimerCountsForOneTick) as TickType_t;
        ulStoppedTimerCompensation = 94; // Fiddle factor from C port
    }
}

/// Suppress ticks and enter a low-power sleep mode.
#[cfg(feature = "tickless-idle")]
pub fn vPortSuppressTicksAndSleep(xExpectedIdleTime: TickType_t) {
    use crate::types::eSleepModeStatus;

    unsafe {
        let mut xExpectedIdleTime = xExpectedIdleTime;

        if xExpectedIdleTime > xMaximumPossibleSuppressedTicks {
            xExpectedIdleTime = xMaximumPossibleSuppressedTicks;
        }

        // Disable interrupts (using PRIMASK, not BASEPRI)
        core::arch::asm!("cpsid i", options(nomem, nostack));
        core::arch::asm!("dsb", options(nomem, nostack));
        core::arch::asm!("isb", options(nomem, nostack));

        if crate::kernel::tasks::eTaskConfirmSleepModeStatus() == eSleepModeStatus::eAbortSleep {
            core::arch::asm!("cpsie i", options(nomem, nostack));
        } else {
            // Stop SysTick
            let systick_ctrl = core::ptr::read_volatile(SYST_CSR);
            core::ptr::write_volatile(SYST_CSR, systick_ctrl & !SYST_CSR_ENABLE);

            let mut ulSysTickDecrementsLeft = core::ptr::read_volatile(SYST_CVR);
            if ulSysTickDecrementsLeft == 0 {
                ulSysTickDecrementsLeft = ulTimerCountsForOneTick;
            }

            let mut ulReloadValue = ulSysTickDecrementsLeft
                + (ulTimerCountsForOneTick * (xExpectedIdleTime as u32 - 1));

            if (core::ptr::read_volatile(NVIC_ICSR) & (1 << 26)) != 0 {
                core::ptr::write_volatile(NVIC_ICSR, portNVIC_PEND_SYSTICK_CLEAR_BIT);
                ulReloadValue -= ulTimerCountsForOneTick;
            }

            if ulReloadValue > ulStoppedTimerCompensation {
                ulReloadValue -= ulStoppedTimerCompensation;
            }

            core::ptr::write_volatile(SYST_RVR, ulReloadValue);
            core::ptr::write_volatile(SYST_CVR, 0);
            core::ptr::write_volatile(
                SYST_CSR,
                SYST_CSR_ENABLE | SYST_CSR_TICKINT | SYST_CSR_CLKSOURCE,
            );

            // Sleep
            core::arch::asm!("dsb", options(nomem, nostack));
            core::arch::asm!("wfi", options(nomem, nostack));
            core::arch::asm!("isb", options(nomem, nostack));

            // Re-enable then disable interrupts
            core::arch::asm!("cpsie i", options(nomem, nostack));
            core::arch::asm!("dsb", options(nomem, nostack));
            core::arch::asm!("isb", options(nomem, nostack));
            core::arch::asm!("cpsid i", options(nomem, nostack));
            core::arch::asm!("dsb", options(nomem, nostack));
            core::arch::asm!("isb", options(nomem, nostack));

            // Disable SysTick without clearing COUNT flag
            core::ptr::write_volatile(SYST_CSR, SYST_CSR_TICKINT | SYST_CSR_CLKSOURCE);

            let ulCompleteTickPeriods: u32;
            let systick_ctrl = core::ptr::read_volatile(SYST_CSR);

            if (systick_ctrl & portNVIC_SYSTICK_COUNT_FLAG_BIT) != 0 {
                let current_value = core::ptr::read_volatile(SYST_CVR);
                let calc = (ulTimerCountsForOneTick - 1)
                    .wrapping_sub(ulReloadValue.wrapping_sub(current_value));
                let ulCalculatedLoadValue =
                    if calc <= ulStoppedTimerCompensation || calc > ulTimerCountsForOneTick {
                        ulTimerCountsForOneTick - 1
                    } else {
                        calc
                    };
                core::ptr::write_volatile(SYST_RVR, ulCalculatedLoadValue);
                ulCompleteTickPeriods = xExpectedIdleTime as u32 - 1;
            } else {
                let mut ulSysTickDecrementsLeft = core::ptr::read_volatile(SYST_CVR);
                if ulSysTickDecrementsLeft == 0 {
                    ulSysTickDecrementsLeft = ulReloadValue;
                }
                let ulCompletedSysTickDecrements =
                    (xExpectedIdleTime as u32 * ulTimerCountsForOneTick) - ulSysTickDecrementsLeft;
                ulCompleteTickPeriods = ulCompletedSysTickDecrements / ulTimerCountsForOneTick;
                let reload = ((ulCompleteTickPeriods + 1) * ulTimerCountsForOneTick)
                    - ulCompletedSysTickDecrements;
                core::ptr::write_volatile(SYST_RVR, reload);
            }

            core::ptr::write_volatile(SYST_CVR, 0);
            core::ptr::write_volatile(
                SYST_CSR,
                SYST_CSR_ENABLE | SYST_CSR_TICKINT | SYST_CSR_CLKSOURCE,
            );
            core::ptr::write_volatile(SYST_RVR, ulTimerCountsForOneTick - 1);

            crate::kernel::tasks::vTaskStepTick(ulCompleteTickPeriods as TickType_t);

            core::arch::asm!("cpsie i", options(nomem, nostack));
        }
    }
}

/// Stub for when tickless idle is not enabled
#[cfg(not(feature = "tickless-idle"))]
pub fn vPortSuppressTicksAndSleep(_xExpectedIdleTime: TickType_t) {
    // No-op when tickless idle is disabled
}
