/*
 * FreeRTOS Kernel <DEVELOPMENT BRANCH>
 * Copyright (C) 2021 Amazon.com, Inc. or its affiliates. All Rights Reserved.
 *
 * SPDX-License-Identifier: MIT
 *
 * [AMENDMENT] This is the Cortex-A53 (AArch64) port for FreeRusTOS.
 * Ported from portable/GCC/ARM_AARCH64/port.c and portASM.S
 *
 * This port is for ARM Cortex-A53 and other ARMv8-A processors in AArch64 mode.
 * It uses:
 * - GIC (Generic Interrupt Controller) for interrupt priority masking
 * - SVC instruction for context switching (EL1 mode)
 * - Platform-specific timer for tick interrupts
 *
 * Key differences from 32-bit ARM:
 * - 64-bit registers (X0-X30)
 * - 16-byte stack alignment required
 * - Exception Levels (EL0/EL1/EL2/EL3) instead of processor modes
 * - DAIF register for interrupt masking
 * - STP/LDP for efficient register pair save/restore
 */

//! ARM Cortex-A53 (AArch64) Port Implementation
//!
//! This port provides hardware-specific implementations for ARM Cortex-A53:
//! - Critical sections using GIC priority masking
//! - Context switching via SVC exception (EL1)
//! - First task start via vPortRestoreTaskContext
//! - Tick timer is platform-specific (user-provided)
//!
//! ## Usage
//!
//! Enable with `--features port-cortex-a53` and compile for `aarch64-unknown-none`.
//!
//! ## Configuration Required
//!
//! Users must define GIC addresses in their configuration before starting scheduler.

use core::arch::global_asm;
use core::ffi::c_void;

use crate::config::*;
use crate::types::*;

// =============================================================================
// Port Constants
// =============================================================================

/// Stack growth direction: -1 for descending
pub const portSTACK_GROWTH: BaseType_t = -1;

/// Byte alignment requirement for stack (AArch64 requires 16-byte)
pub const portBYTE_ALIGNMENT: usize = 16;

/// Architecture name string for this port
pub const portARCH_NAME: &str = "ARM Cortex-A53 (AArch64)";

// =============================================================================
// AArch64 Constants
// =============================================================================

/// Initial PSTATE: EL1h (EL1, SP_EL1)
const portINITIAL_PSTATE: StackType_t = 0x05; // EL1h, interrupts enabled

/// No critical nesting value
const portNO_CRITICAL_NESTING: u64 = 0;

/// No floating point context flag
const portNO_FLOATING_POINT_CONTEXT: StackType_t = 0;

/// Unmask all interrupt priorities
const portUNMASK_VALUE: u32 = 0xFF;

/// Number of FPU register words (32 x 128-bit = 64 x 64-bit)
const portFPU_REGISTER_WORDS: usize = 64;

// =============================================================================
// GIC Configuration
// =============================================================================

/// GIC CPU interface register offsets
const portICCPMR_PRIORITY_MASK_OFFSET: usize = 0x04;
const portICCIAR_INTERRUPT_ACKNOWLEDGE_OFFSET: usize = 0x0C;
const portICCEOIR_END_OF_INTERRUPT_OFFSET: usize = 0x10;
const portICCBPR_BINARY_POINT_OFFSET: usize = 0x08;

/// Number of unique interrupt priorities (default 32)
pub const configUNIQUE_INTERRUPT_PRIORITIES: u32 = 32;

/// Priority shift (32 priorities = 5 bits, shift by 3)
const portPRIORITY_SHIFT: u32 = 3;

/// Binary point maximum value
const portMAX_BINARY_POINT_VALUE: u32 = 2;

/// Lowest interrupt priority
pub const portLOWEST_INTERRUPT_PRIORITY: u32 = configUNIQUE_INTERRUPT_PRIORITIES - 1;

// =============================================================================
// GIC Address Configuration
// =============================================================================

/// GIC base address - configured by user
/// For QEMU virt: 0x08000000
#[no_mangle]
pub static mut ulGICDistributorBase: u64 = 0x0800_0000;

/// GIC CPU interface base
/// For QEMU virt: 0x08010000
#[no_mangle]
pub static mut ulGICCPUInterfaceBase: u64 = 0x0801_0000;

/// Maximum API call interrupt priority
#[no_mangle]
pub static mut ulMaxAPIPriority: u32 = 18;

/// Computed max API priority mask
fn get_max_api_priority_mask() -> u32 {
    unsafe { ulMaxAPIPriority << portPRIORITY_SHIFT }
}

/// Get ICCPMR address
fn get_iccpmr_address() -> *mut u32 {
    unsafe { (ulGICCPUInterfaceBase as usize + portICCPMR_PRIORITY_MASK_OFFSET) as *mut u32 }
}

/// Get ICCIAR address
fn get_icciar_address() -> *const u32 {
    unsafe { (ulGICCPUInterfaceBase as usize + portICCIAR_INTERRUPT_ACKNOWLEDGE_OFFSET) as *const u32 }
}

/// Get ICCEOIR address
fn get_icceoir_address() -> *mut u32 {
    unsafe { (ulGICCPUInterfaceBase as usize + portICCEOIR_END_OF_INTERRUPT_OFFSET) as *mut u32 }
}

// GIC addresses for assembly (as 64-bit values)
#[no_mangle]
pub static mut ullICCPMR: u64 = 0;
#[no_mangle]
pub static mut ullICCIAR: u64 = 0;
#[no_mangle]
pub static mut ullICCEOIR: u64 = 0;
#[no_mangle]
pub static mut ullMaxAPIPriorityMask: u64 = 0;

/// Configure GIC addresses - call before starting scheduler
#[no_mangle]
pub extern "C" fn vPortConfigureGIC(
    gic_dist_base: u64,
    gic_cpu_base: u64,
    max_api_priority: u32,
) {
    unsafe {
        ulGICDistributorBase = gic_dist_base;
        ulGICCPUInterfaceBase = gic_cpu_base;
        ulMaxAPIPriority = max_api_priority;

        // Update assembly-accessible addresses
        ullICCPMR = gic_cpu_base + portICCPMR_PRIORITY_MASK_OFFSET as u64;
        ullICCIAR = gic_cpu_base + portICCIAR_INTERRUPT_ACKNOWLEDGE_OFFSET as u64;
        ullICCEOIR = gic_cpu_base + portICCEOIR_END_OF_INTERRUPT_OFFSET as u64;
        ullMaxAPIPriorityMask = (max_api_priority << portPRIORITY_SHIFT) as u64;
    }
}

// =============================================================================
// Port State Variables (64-bit for AArch64)
// =============================================================================

/// Critical section nesting counter
#[no_mangle]
pub static mut ullCriticalNesting: u64 = 9999;

/// FPU context flag for current task
#[no_mangle]
pub static mut ullPortTaskHasFPUContext: u64 = 0;

/// Yield required flag
#[no_mangle]
pub static mut ullPortYieldRequired: u64 = 0;

/// Interrupt nesting depth
#[no_mangle]
pub static mut ullPortInterruptNesting: u64 = 0;

// =============================================================================
// Critical Section Management
// =============================================================================

/// Enter a critical section
#[inline(always)]
pub fn portENTER_CRITICAL() {
    vPortEnterCritical();
}

/// Exit a critical section
#[inline(always)]
pub fn portEXIT_CRITICAL() {
    vPortExitCritical();
}

/// Enter critical section implementation
#[no_mangle]
pub extern "C" fn vPortEnterCritical() {
    ulPortSetInterruptMask();

    unsafe {
        ullCriticalNesting += 1;

        if ullCriticalNesting == 1 {
            configASSERT(ullPortInterruptNesting == 0);
        }
    }
}

/// Exit critical section implementation
#[no_mangle]
pub extern "C" fn vPortExitCritical() {
    unsafe {
        if ullCriticalNesting > portNO_CRITICAL_NESTING {
            ullCriticalNesting -= 1;

            if ullCriticalNesting == portNO_CRITICAL_NESTING {
                portCLEAR_INTERRUPT_MASK();
            }
        }
    }
}

/// Disable interrupts
#[inline(always)]
pub fn portDISABLE_INTERRUPTS() -> UBaseType_t {
    ulPortSetInterruptMask()
}

/// Enable interrupts
#[inline(always)]
pub fn portENABLE_INTERRUPTS() {
    vPortClearInterruptMask(0);
}

/// Set interrupt mask
#[no_mangle]
pub extern "C" fn ulPortSetInterruptMask() -> u64 {
    let ulReturn: u64;

    unsafe {
        // Disable IRQ
        core::arch::asm!("msr daifset, #2", options(nomem, nostack));
        core::arch::asm!("dsb sy", options(nomem, nostack));
        core::arch::asm!("isb sy", options(nomem, nostack));

        let iccpmr = get_iccpmr_address();
        let mask_value = get_max_api_priority_mask();
        let current = core::ptr::read_volatile(iccpmr);

        if current == mask_value {
            ulReturn = pdTRUE as u64;
        } else {
            ulReturn = pdFALSE as u64;
            core::ptr::write_volatile(iccpmr, mask_value);
            core::arch::asm!("dsb sy", options(nomem, nostack));
            core::arch::asm!("isb sy", options(nomem, nostack));
        }

        // Re-enable IRQ
        core::arch::asm!("msr daifclr, #2", options(nomem, nostack));
        core::arch::asm!("dsb sy", options(nomem, nostack));
        core::arch::asm!("isb sy", options(nomem, nostack));
    }

    ulReturn
}

/// Clear interrupt mask
#[no_mangle]
pub extern "C" fn vPortClearInterruptMask(ulNewMaskValue: u64) {
    if ulNewMaskValue == pdFALSE as u64 {
        portCLEAR_INTERRUPT_MASK();
    }
}

/// Clear interrupt mask implementation
#[inline(always)]
fn portCLEAR_INTERRUPT_MASK() {
    unsafe {
        core::arch::asm!("msr daifset, #2", options(nomem, nostack));

        let iccpmr = get_iccpmr_address();
        core::ptr::write_volatile(iccpmr, portUNMASK_VALUE);

        core::arch::asm!("dsb sy", options(nomem, nostack));
        core::arch::asm!("isb sy", options(nomem, nostack));
        core::arch::asm!("msr daifclr, #2", options(nomem, nostack));
        core::arch::asm!("dsb sy", options(nomem, nostack));
        core::arch::asm!("isb sy", options(nomem, nostack));
    }
}

/// Set interrupt mask from ISR
#[inline(always)]
pub fn portSET_INTERRUPT_MASK_FROM_ISR() -> UBaseType_t {
    ulPortSetInterruptMask() as UBaseType_t
}

/// Clear interrupt mask from ISR
#[inline(always)]
pub fn portCLEAR_INTERRUPT_MASK_FROM_ISR(uxSavedInterruptStatus: UBaseType_t) {
    vPortClearInterruptMask(uxSavedInterruptStatus as u64);
}

// =============================================================================
// Context Switching / Yield
// =============================================================================

/// Trigger a context switch via SVC (EL1 mode)
#[inline(always)]
pub fn portYIELD() {
    unsafe {
        core::arch::asm!("svc 0", options(nomem, nostack));
    }
}

/// Yield from ISR
#[inline(always)]
pub fn portYIELD_FROM_ISR(xSwitchRequired: BaseType_t) {
    if xSwitchRequired != pdFALSE {
        unsafe {
            ullPortYieldRequired = pdTRUE as u64;
        }
    }
}

/// End switching ISR
#[inline(always)]
pub fn portEND_SWITCHING_ISR(xSwitchRequired: BaseType_t) {
    portYIELD_FROM_ISR(xSwitchRequired);
}

// =============================================================================
// Stack Initialization
// =============================================================================

/// Initialize a task's stack for AArch64
///
/// Stack layout (high to low, all 64-bit values):
/// - X1, X0 (parameters)
/// - X3, X2
/// - ... X29, X28
/// - XZR, X30 (LR)
/// - SPSR, ELR (return address)
/// - FPU flag, Critical nesting
/// - [Optional FPU registers Q0-Q31 if FPU context]
pub fn pxPortInitialiseStack(
    pxTopOfStack: *mut StackType_t,
    pxCode: TaskFunction_t,
    pvParameters: *mut c_void,
) -> *mut StackType_t {
    unsafe {
        let mut pxStack = pxTopOfStack;

        // Use wrapping_sub to avoid debug assertions on pointer arithmetic
        // X1
        pxStack = pxStack.wrapping_sub(1);
        *pxStack = 0x0101010101010101u64;

        // X0 - first argument (pvParameters)
        pxStack = pxStack.wrapping_sub(1);
        *pxStack = pvParameters as StackType_t;

        // X3, X2
        pxStack = pxStack.wrapping_sub(1);
        *pxStack = 0x0303030303030303u64;
        pxStack = pxStack.wrapping_sub(1);
        *pxStack = 0x0202020202020202u64;

        // X5, X4
        pxStack = pxStack.wrapping_sub(1);
        *pxStack = 0x0505050505050505u64;
        pxStack = pxStack.wrapping_sub(1);
        *pxStack = 0x0404040404040404u64;

        // X7, X6
        pxStack = pxStack.wrapping_sub(1);
        *pxStack = 0x0707070707070707u64;
        pxStack = pxStack.wrapping_sub(1);
        *pxStack = 0x0606060606060606u64;

        // X9, X8
        pxStack = pxStack.wrapping_sub(1);
        *pxStack = 0x0909090909090909u64;
        pxStack = pxStack.wrapping_sub(1);
        *pxStack = 0x0808080808080808u64;

        // X11, X10
        pxStack = pxStack.wrapping_sub(1);
        *pxStack = 0x1111111111111111u64;
        pxStack = pxStack.wrapping_sub(1);
        *pxStack = 0x1010101010101010u64;

        // X13, X12
        pxStack = pxStack.wrapping_sub(1);
        *pxStack = 0x1313131313131313u64;
        pxStack = pxStack.wrapping_sub(1);
        *pxStack = 0x1212121212121212u64;

        // X15, X14
        pxStack = pxStack.wrapping_sub(1);
        *pxStack = 0x1515151515151515u64;
        pxStack = pxStack.wrapping_sub(1);
        *pxStack = 0x1414141414141414u64;

        // X17, X16
        pxStack = pxStack.wrapping_sub(1);
        *pxStack = 0x1717171717171717u64;
        pxStack = pxStack.wrapping_sub(1);
        *pxStack = 0x1616161616161616u64;

        // X19, X18
        pxStack = pxStack.wrapping_sub(1);
        *pxStack = 0x1919191919191919u64;
        pxStack = pxStack.wrapping_sub(1);
        *pxStack = 0x1818181818181818u64;

        // X21, X20
        pxStack = pxStack.wrapping_sub(1);
        *pxStack = 0x2121212121212121u64;
        pxStack = pxStack.wrapping_sub(1);
        *pxStack = 0x2020202020202020u64;

        // X23, X22
        pxStack = pxStack.wrapping_sub(1);
        *pxStack = 0x2323232323232323u64;
        pxStack = pxStack.wrapping_sub(1);
        *pxStack = 0x2222222222222222u64;

        // X25, X24
        pxStack = pxStack.wrapping_sub(1);
        *pxStack = 0x2525252525252525u64;
        pxStack = pxStack.wrapping_sub(1);
        *pxStack = 0x2424242424242424u64;

        // X27, X26
        pxStack = pxStack.wrapping_sub(1);
        *pxStack = 0x2727272727272727u64;
        pxStack = pxStack.wrapping_sub(1);
        *pxStack = 0x2626262626262626u64;

        // X29, X28
        pxStack = pxStack.wrapping_sub(1);
        *pxStack = 0x2929292929292929u64;
        pxStack = pxStack.wrapping_sub(1);
        *pxStack = 0x2828282828282828u64;

        // XZR (zero), X30 (LR - return to task exit error)
        pxStack = pxStack.wrapping_sub(1);
        *pxStack = 0x00;
        pxStack = pxStack.wrapping_sub(1);
        *pxStack = prvTaskExitError as StackType_t;

        // SPSR_EL1
        pxStack = pxStack.wrapping_sub(1);
        *pxStack = portINITIAL_PSTATE;

        // ELR_EL1 (return address = task entry point)
        pxStack = pxStack.wrapping_sub(1);
        *pxStack = pxCode as StackType_t;

        // Critical nesting (0 = interrupts enabled)
        pxStack = pxStack.wrapping_sub(1);
        *pxStack = portNO_CRITICAL_NESTING;

        // FPU context flag (no FPU context initially)
        pxStack = pxStack.wrapping_sub(1);
        *pxStack = portNO_FLOATING_POINT_CONTEXT;

        pxStack
    }
}

/// Called if a task returns from its implementing function
fn prvTaskExitError() -> ! {
    configASSERT(false);
    portDISABLE_INTERRUPTS();
    loop {
        unsafe {
            core::arch::asm!("wfi", options(nomem, nostack));
        }
    }
}

// =============================================================================
// Scheduler Start
// =============================================================================

/// Start the scheduler
#[no_mangle]
pub extern "C" fn xPortStartScheduler() -> BaseType_t {
    unsafe {
        // Disable interrupts
        core::arch::asm!("msr daifset, #2", options(nomem, nostack));

        // Start first task
        vPortRestoreTaskContext();
    }

    0
}

/// End the scheduler
pub fn vPortEndScheduler() {
    configASSERT(false);
    loop {
        unsafe {
            core::arch::asm!("wfi", options(nomem, nostack));
        }
    }
}

// =============================================================================
// Tick Handler
// =============================================================================

/// FreeRTOS tick handler
#[no_mangle]
pub extern "C" fn FreeRTOS_Tick_Handler() {
    unsafe {
        // Mask interrupts
        let iccpmr = get_iccpmr_address();
        core::ptr::write_volatile(iccpmr, get_max_api_priority_mask());
        core::arch::asm!("dsb sy", "isb sy", options(nomem, nostack));
    }

    // Increment tick
    if crate::kernel::tasks::xTaskIncrementTick() != pdFALSE {
        unsafe {
            ullPortYieldRequired = pdTRUE as u64;
        }
    }

    // Unmask
    portCLEAR_INTERRUPT_MASK();
}

// =============================================================================
// FPU Support
// =============================================================================

/// Mark current task as using FPU
#[no_mangle]
pub extern "C" fn vPortTaskUsesFPU() {
    unsafe {
        ullPortTaskHasFPUContext = pdTRUE as u64;
    }
}

// =============================================================================
// Utility Functions
// =============================================================================

/// Check if in interrupt context
#[inline(always)]
pub fn xPortIsInsideInterrupt() -> BaseType_t {
    unsafe {
        if ullPortInterruptNesting > 0 {
            pdTRUE
        } else {
            pdFALSE
        }
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
        core::arch::asm!("dmb sy", options(nomem, nostack));
    }
}

// =============================================================================
// Run-time Stats
// =============================================================================

#[cfg(feature = "generate-run-time-stats")]
static mut ulRunTimeCounterValue: crate::config::configRUN_TIME_COUNTER_TYPE = 0;

#[cfg(feature = "generate-run-time-stats")]
#[inline(always)]
pub fn portCONFIGURE_TIMER_FOR_RUN_TIME_STATS() {
    unsafe { ulRunTimeCounterValue = 0; }
}

#[cfg(feature = "generate-run-time-stats")]
#[inline(always)]
pub fn portGET_RUN_TIME_COUNTER_VALUE() -> crate::config::configRUN_TIME_COUNTER_TYPE {
    unsafe { ulRunTimeCounterValue }
}

#[cfg(feature = "generate-run-time-stats")]
#[inline(always)]
pub fn portINCREMENT_RUN_TIME_COUNTER() {
    unsafe { ulRunTimeCounterValue = ulRunTimeCounterValue.wrapping_add(1); }
}

// =============================================================================
// Tickless Idle Configuration (ARM Generic Timer)
// =============================================================================

/// Timer frequency (from CNTFRQ_EL0) - configured at runtime
#[cfg(feature = "tickless-idle")]
static mut ulTimerFrequency: u64 = 0;

/// Timer counts per tick (frequency / configTICK_RATE_HZ)
#[cfg(feature = "tickless-idle")]
static mut ulTimerCountsForOneTick: u64 = 0;

/// Maximum ticks that can be suppressed (limited by 64-bit compare)
#[cfg(feature = "tickless-idle")]
static mut ulMaximumPossibleSuppressedTicks: TickType_t = 0;

/// Compensation for time spent stopping/starting timer
#[cfg(feature = "tickless-idle")]
const ulStoppedTimerCompensation: u64 = 100;

/// Configure tickless idle - must be called before starting scheduler
#[cfg(feature = "tickless-idle")]
#[no_mangle]
pub extern "C" fn vPortConfigureTicklessIdle(tick_rate_hz: u32) {
    unsafe {
        // Read timer frequency from system register
        core::arch::asm!("mrs {}, cntfrq_el0", out(reg) ulTimerFrequency);

        if ulTimerFrequency > 0 && tick_rate_hz > 0 {
            ulTimerCountsForOneTick = ulTimerFrequency / tick_rate_hz as u64;

            // Maximum suppressed ticks - use a reasonable limit to avoid overflow
            // Even with 64-bit compare, limit to avoid very long sleeps
            ulMaximumPossibleSuppressedTicks = 0xFFFF_FFFF as TickType_t;
        }
    }
}

/// Tickless idle implementation for Cortex-A53 with ARM Generic Timer
#[cfg(feature = "tickless-idle")]
pub fn vPortSuppressTicksAndSleep(xExpectedIdleTime: TickType_t) {
    use crate::types::eSleepModeStatus;

    unsafe {
        // Check if timer is configured
        if ulTimerCountsForOneTick == 0 {
            return;
        }

        let mut xExpectedIdleTime = xExpectedIdleTime;

        // Limit to maximum possible
        if xExpectedIdleTime > ulMaximumPossibleSuppressedTicks {
            xExpectedIdleTime = ulMaximumPossibleSuppressedTicks;
        }

        // Disable interrupts via DAIF
        core::arch::asm!("msr daifset, #2", options(nomem, nostack));
        core::arch::asm!("dsb sy", options(nomem, nostack));
        core::arch::asm!("isb sy", options(nomem, nostack));

        // Check if sleep should be aborted
        if crate::kernel::tasks::eTaskConfirmSleepModeStatus() == eSleepModeStatus::eAbortSleep {
            core::arch::asm!("msr daifclr, #2", options(nomem, nostack));
            return;
        }

        // Read current timer compare value (when next tick would fire)
        let ulCurrentCompare: u64;
        core::arch::asm!("mrs {}, cntp_cval_el0", out(reg) ulCurrentCompare);

        // Read current timer count
        let ulCurrentCount: u64;
        core::arch::asm!("mrs {}, cntpct_el0", out(reg) ulCurrentCount);

        // Calculate counts remaining until next tick
        let ulCountsRemaining = if ulCurrentCompare > ulCurrentCount {
            ulCurrentCompare - ulCurrentCount
        } else {
            0 // Already past compare value
        };

        // Calculate new compare value for expected idle time
        // Start from current count to be precise
        let ulNewCompare = ulCurrentCount
            + ulCountsRemaining
            + (ulTimerCountsForOneTick * (xExpectedIdleTime as u64 - 1));

        // Apply compensation for time spent in this function
        let ulNewCompare = if ulNewCompare > ulStoppedTimerCompensation {
            ulNewCompare - ulStoppedTimerCompensation
        } else {
            ulNewCompare
        };

        // Set new compare value
        core::arch::asm!("msr cntp_cval_el0, {}", in(reg) ulNewCompare);
        core::arch::asm!("dsb sy", options(nomem, nostack));
        core::arch::asm!("isb sy", options(nomem, nostack));

        // Sleep until interrupt (WFI = Wait For Interrupt)
        core::arch::asm!("wfi", options(nomem, nostack));
        core::arch::asm!("isb sy", options(nomem, nostack));

        // Re-enable interrupts briefly to let the wake interrupt execute
        core::arch::asm!("msr daifclr, #2", options(nomem, nostack));
        core::arch::asm!("dsb sy", options(nomem, nostack));
        core::arch::asm!("isb sy", options(nomem, nostack));

        // Disable interrupts again for tick compensation
        core::arch::asm!("msr daifset, #2", options(nomem, nostack));
        core::arch::asm!("dsb sy", options(nomem, nostack));
        core::arch::asm!("isb sy", options(nomem, nostack));

        // Read current counter to see how much time elapsed
        let ulTimerCountNow: u64;
        core::arch::asm!("mrs {}, cntpct_el0", out(reg) ulTimerCountNow);

        // Calculate how many complete ticks elapsed
        let ulElapsedCounts = if ulTimerCountNow > ulCurrentCount {
            ulTimerCountNow - ulCurrentCount
        } else {
            // Counter wrapped (very unlikely with 64-bit counter)
            0
        };

        let ulCompletedTicks = ulElapsedCounts / ulTimerCountsForOneTick;

        // Step the tick count forward
        if ulCompletedTicks > 0 {
            crate::kernel::tasks::vTaskStepTick(ulCompletedTicks as TickType_t);
        }

        // Calculate remaining counts until next tick
        let ulRemainingCounts = ulTimerCountsForOneTick
            - (ulElapsedCounts % ulTimerCountsForOneTick);

        // Set compare value for next tick
        let ulNextCompare = ulTimerCountNow + ulRemainingCounts;
        core::arch::asm!("msr cntp_cval_el0, {}", in(reg) ulNextCompare);
        core::arch::asm!("dsb sy", options(nomem, nostack));
        core::arch::asm!("isb sy", options(nomem, nostack));

        // Re-enable interrupts
        core::arch::asm!("msr daifclr, #2", options(nomem, nostack));
    }
}

/// Stub for when tickless idle is not enabled
#[cfg(not(feature = "tickless-idle"))]
pub fn vPortSuppressTicksAndSleep(_xExpectedIdleTime: TickType_t) {
    // No-op when tickless idle is disabled
}

// =============================================================================
// Assembly Handlers
// =============================================================================

extern "C" {
    fn vPortRestoreTaskContext();
}

global_asm!(
    ".align 8",

    // ==========================================================================
    // portSAVE_CONTEXT macro
    // ==========================================================================
    ".macro portSAVE_CONTEXT",
    // Switch to EL0 stack pointer
    "msr spsel, #0",

    // Save all general purpose registers
    "stp x0, x1, [sp, #-0x10]!",
    "stp x2, x3, [sp, #-0x10]!",
    "stp x4, x5, [sp, #-0x10]!",
    "stp x6, x7, [sp, #-0x10]!",
    "stp x8, x9, [sp, #-0x10]!",
    "stp x10, x11, [sp, #-0x10]!",
    "stp x12, x13, [sp, #-0x10]!",
    "stp x14, x15, [sp, #-0x10]!",
    "stp x16, x17, [sp, #-0x10]!",
    "stp x18, x19, [sp, #-0x10]!",
    "stp x20, x21, [sp, #-0x10]!",
    "stp x22, x23, [sp, #-0x10]!",
    "stp x24, x25, [sp, #-0x10]!",
    "stp x26, x27, [sp, #-0x10]!",
    "stp x28, x29, [sp, #-0x10]!",
    "stp x30, xzr, [sp, #-0x10]!",

    // Save SPSR and ELR (EL1)
    "mrs x3, spsr_el1",
    "mrs x2, elr_el1",
    "stp x2, x3, [sp, #-0x10]!",

    // Save critical nesting
    "ldr x0, =ullCriticalNesting",
    "ldr x3, [x0]",

    // Save FPU context flag
    "ldr x0, =ullPortTaskHasFPUContext",
    "ldr x2, [x0]",

    // Save FPU registers if needed
    "cmp x2, #0",
    "b.eq 1f",
    "stp q0, q1, [sp, #-0x20]!",
    "stp q2, q3, [sp, #-0x20]!",
    "stp q4, q5, [sp, #-0x20]!",
    "stp q6, q7, [sp, #-0x20]!",
    "stp q8, q9, [sp, #-0x20]!",
    "stp q10, q11, [sp, #-0x20]!",
    "stp q12, q13, [sp, #-0x20]!",
    "stp q14, q15, [sp, #-0x20]!",
    "stp q16, q17, [sp, #-0x20]!",
    "stp q18, q19, [sp, #-0x20]!",
    "stp q20, q21, [sp, #-0x20]!",
    "stp q22, q23, [sp, #-0x20]!",
    "stp q24, q25, [sp, #-0x20]!",
    "stp q26, q27, [sp, #-0x20]!",
    "stp q28, q29, [sp, #-0x20]!",
    "stp q30, q31, [sp, #-0x20]!",
    "1:",

    // Store critical nesting and FPU flag
    "stp x2, x3, [sp, #-0x10]!",

    // Save SP to TCB
    "ldr x0, =pxCurrentTCB",
    "ldr x1, [x0]",
    "mov x0, sp",
    "str x0, [x1]",

    // Switch back to ELx stack
    "msr spsel, #1",
    ".endm",

    // ==========================================================================
    // portRESTORE_CONTEXT macro
    // ==========================================================================
    ".macro portRESTORE_CONTEXT",
    // Switch to EL0 stack pointer
    "msr spsel, #0",

    // Get SP from TCB
    "ldr x0, =pxCurrentTCB",
    "ldr x1, [x0]",
    "ldr x0, [x1]",
    "mov sp, x0",

    // Restore critical nesting and FPU flag
    "ldp x2, x3, [sp], #0x10",

    // Set GIC priority mask based on critical nesting
    "ldr x0, =ullCriticalNesting",
    "mov x1, #255",
    "ldr x4, =ullICCPMR",
    "cmp x3, #0",
    "ldr x5, [x4]",
    "b.eq 2f",
    "ldr x6, =ullMaxAPIPriorityMask",
    "ldr x1, [x6]",
    "2:",
    "str w1, [x5]",
    "dsb sy",
    "isb sy",
    "str x3, [x0]",

    // Restore FPU context flag
    "ldr x0, =ullPortTaskHasFPUContext",
    "str x2, [x0]",

    // Restore FPU registers if needed
    "cmp x2, #0",
    "b.eq 3f",
    "ldp q30, q31, [sp], #0x20",
    "ldp q28, q29, [sp], #0x20",
    "ldp q26, q27, [sp], #0x20",
    "ldp q24, q25, [sp], #0x20",
    "ldp q22, q23, [sp], #0x20",
    "ldp q20, q21, [sp], #0x20",
    "ldp q18, q19, [sp], #0x20",
    "ldp q16, q17, [sp], #0x20",
    "ldp q14, q15, [sp], #0x20",
    "ldp q12, q13, [sp], #0x20",
    "ldp q10, q11, [sp], #0x20",
    "ldp q8, q9, [sp], #0x20",
    "ldp q6, q7, [sp], #0x20",
    "ldp q4, q5, [sp], #0x20",
    "ldp q2, q3, [sp], #0x20",
    "ldp q0, q1, [sp], #0x20",
    "3:",

    // Restore SPSR and ELR
    "ldp x2, x3, [sp], #0x10",
    "msr spsr_el1, x3",
    "msr elr_el1, x2",

    // Restore general purpose registers
    "ldp x30, xzr, [sp], #0x10",
    "ldp x28, x29, [sp], #0x10",
    "ldp x26, x27, [sp], #0x10",
    "ldp x24, x25, [sp], #0x10",
    "ldp x22, x23, [sp], #0x10",
    "ldp x20, x21, [sp], #0x10",
    "ldp x18, x19, [sp], #0x10",
    "ldp x16, x17, [sp], #0x10",
    "ldp x14, x15, [sp], #0x10",
    "ldp x12, x13, [sp], #0x10",
    "ldp x10, x11, [sp], #0x10",
    "ldp x8, x9, [sp], #0x10",
    "ldp x6, x7, [sp], #0x10",
    "ldp x4, x5, [sp], #0x10",
    "ldp x2, x3, [sp], #0x10",
    "ldp x0, x1, [sp], #0x10",

    // Switch to ELx stack and return
    "msr spsel, #1",
    "eret",
    ".endm",

    // ==========================================================================
    // FreeRTOS_SWI_Handler - SVC Handler (Context Switch)
    // ==========================================================================
    ".global FreeRTOS_SWI_Handler",
    ".align 8",
    "FreeRTOS_SWI_Handler:",
    "portSAVE_CONTEXT",
    "bl vTaskSwitchContext",
    "portRESTORE_CONTEXT",

    // ==========================================================================
    // vPortRestoreTaskContext - Start First Task
    // ==========================================================================
    ".global vPortRestoreTaskContext",
    ".align 8",
    "vPortRestoreTaskContext:",
    // Install vector table
    "ldr x1, =_freertos_vector_table",
    "msr vbar_el1, x1",
    "dsb sy",
    "isb sy",
    "portRESTORE_CONTEXT",

    // ==========================================================================
    // FreeRTOS_IRQ_Handler - IRQ Handler
    // ==========================================================================
    ".global FreeRTOS_IRQ_Handler",
    ".align 8",
    "FreeRTOS_IRQ_Handler:",
    // Save volatile registers
    "stp x0, x1, [sp, #-0x10]!",
    "stp x2, x3, [sp, #-0x10]!",
    "stp x4, x5, [sp, #-0x10]!",
    "stp x6, x7, [sp, #-0x10]!",
    "stp x8, x9, [sp, #-0x10]!",
    "stp x10, x11, [sp, #-0x10]!",
    "stp x12, x13, [sp, #-0x10]!",
    "stp x14, x15, [sp, #-0x10]!",
    "stp x16, x17, [sp, #-0x10]!",
    "stp x18, x19, [sp, #-0x10]!",
    "stp x29, x30, [sp, #-0x10]!",

    // Save SPSR and ELR
    "mrs x3, spsr_el1",
    "mrs x2, elr_el1",
    "stp x2, x3, [sp, #-0x10]!",

    // Increment nesting counter
    "ldr x5, =ullPortInterruptNesting",
    "ldr x1, [x5]",
    "add x6, x1, #1",
    "str x6, [x5]",
    "stp x1, x5, [sp, #-0x10]!",

    // Read ICCIAR
    "ldr x2, =ullICCIAR",
    "ldr x3, [x2]",
    "ldr w0, [x3]",
    "stp x0, x1, [sp, #-0x10]!",

    // Call IRQ handler
    "bl vApplicationIRQHandler",

    // Disable interrupts
    "msr daifset, #2",
    "dsb sy",
    "isb sy",

    // Restore ICCIAR and write to ICCEOIR
    "ldp x0, x1, [sp], #0x10",
    "ldr x4, =ullICCEOIR",
    "ldr x4, [x4]",
    "str w0, [x4]",

    // Restore nesting counter
    "ldp x1, x5, [sp], #0x10",
    "str x1, [x5]",

    // Check if context switch needed
    "cmp x1, #0",
    "b.ne 4f",
    "ldr x0, =ullPortYieldRequired",
    "ldr x1, [x0]",
    "cmp x1, #0",
    "b.eq 4f",

    // Clear yield flag and do context switch
    "mov x2, #0",
    "str x2, [x0]",

    // Restore volatile regs before context switch
    "ldp x4, x5, [sp], #0x10",
    "msr spsr_el1, x5",
    "msr elr_el1, x4",
    "dsb sy",
    "isb sy",
    "ldp x29, x30, [sp], #0x10",
    "ldp x18, x19, [sp], #0x10",
    "ldp x16, x17, [sp], #0x10",
    "ldp x14, x15, [sp], #0x10",
    "ldp x12, x13, [sp], #0x10",
    "ldp x10, x11, [sp], #0x10",
    "ldp x8, x9, [sp], #0x10",
    "ldp x6, x7, [sp], #0x10",
    "ldp x4, x5, [sp], #0x10",
    "ldp x2, x3, [sp], #0x10",
    "ldp x0, x1, [sp], #0x10",

    "portSAVE_CONTEXT",
    "bl vTaskSwitchContext",
    "portRESTORE_CONTEXT",

    "4:",
    // Exit without context switch
    "ldp x4, x5, [sp], #0x10",
    "msr spsr_el1, x5",
    "msr elr_el1, x4",
    "dsb sy",
    "isb sy",
    "ldp x29, x30, [sp], #0x10",
    "ldp x18, x19, [sp], #0x10",
    "ldp x16, x17, [sp], #0x10",
    "ldp x14, x15, [sp], #0x10",
    "ldp x12, x13, [sp], #0x10",
    "ldp x10, x11, [sp], #0x10",
    "ldp x8, x9, [sp], #0x10",
    "ldp x6, x7, [sp], #0x10",
    "ldp x4, x5, [sp], #0x10",
    "ldp x2, x3, [sp], #0x10",
    "ldp x0, x1, [sp], #0x10",
    "eret",
);

// NOTE: User must provide vApplicationIRQHandler(ulICCIAR: u32)
// This is called from the IRQ handler assembly with the ICCIAR value.
// The user should check the IRQ ID and handle the interrupt appropriately.
extern "C" {
    fn vApplicationIRQHandler(ulICCIAR: u32);
}

// =============================================================================
// Vector Table
// =============================================================================

global_asm!(
    // AArch64 exception vector table
    // Each entry is 128 bytes (32 instructions)
    ".section .vectors, \"ax\"",
    ".global _freertos_vector_table",
    ".balign 2048",
    "_freertos_vector_table:",

    // Current EL with SP_EL0 (not used by FreeRTOS)
    ".balign 128",
    "curr_el_sp0_sync:",
    "b .",

    ".balign 128",
    "curr_el_sp0_irq:",
    "b .",

    ".balign 128",
    "curr_el_sp0_fiq:",
    "b .",

    ".balign 128",
    "curr_el_sp0_serror:",
    "b .",

    // Current EL with SP_ELx (EL1)
    ".balign 128",
    "curr_el_spx_sync:",
    "b FreeRTOS_SWI_Handler",

    ".balign 128",
    "curr_el_spx_irq:",
    "b FreeRTOS_IRQ_Handler",

    ".balign 128",
    "curr_el_spx_fiq:",
    "b .",

    ".balign 128",
    "curr_el_spx_serror:",
    "b .",

    // Lower EL using AArch64 (not used - all runs at EL1)
    ".balign 128",
    "lower_el_aarch64_sync:",
    "b .",

    ".balign 128",
    "lower_el_aarch64_irq:",
    "b .",

    ".balign 128",
    "lower_el_aarch64_fiq:",
    "b .",

    ".balign 128",
    "lower_el_aarch64_serror:",
    "b .",

    // Lower EL using AArch32 (not used)
    ".balign 128",
    "lower_el_aarch32_sync:",
    "b .",

    ".balign 128",
    "lower_el_aarch32_irq:",
    "b .",

    ".balign 128",
    "lower_el_aarch32_fiq:",
    "b .",

    ".balign 128",
    "lower_el_aarch32_serror:",
    "b .",
);
