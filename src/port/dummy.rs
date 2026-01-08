/*
 * FreeRTOS Kernel <DEVELOPMENT BRANCH>
 * Copyright (C) 2021 Amazon.com, Inc. or its affiliates. All Rights Reserved.
 *
 * SPDX-License-Identifier: MIT
 *
 * [AMENDMENT] This is the dummy port for FreeRusTOS.
 * It provides stub implementations that compile but panic at runtime.
 * Used during early development to validate kernel translation.
 *
 * Based on portable/GCC/ARM_CM3/portmacro.h structure.
 */

//! Dummy Port Implementation
//!
//! This port provides stub implementations that allow the kernel to compile
//! without real hardware support. All functions that would perform hardware
//! operations will panic at runtime.
//!
//! **WARNING**: Do not use this port in production! It is only for development
//! and testing of the kernel translation.

use crate::types::*;
use core::sync::atomic::{AtomicUsize, Ordering};

// =============================================================================
// Port Constants
// =============================================================================

/// Stack growth direction: -1 for descending (most common), +1 for ascending
pub const portSTACK_GROWTH: BaseType_t = -1;

/// Byte alignment requirement for stack and heap allocations
pub const portBYTE_ALIGNMENT: usize = 8;

// =============================================================================
// Critical Section Management
// =============================================================================

/// Critical section nesting counter
/// [AMENDMENT] Using atomic for thread-safety during host testing
static CRITICAL_NESTING: AtomicUsize = AtomicUsize::new(0);

/// Enter a critical section (disable interrupts)
///
/// [AMENDMENT] In the dummy port, this just increments a counter.
/// A real port would disable interrupts here.
#[inline(always)]
pub fn portENTER_CRITICAL() {
    portDISABLE_INTERRUPTS();
    CRITICAL_NESTING.fetch_add(1, Ordering::SeqCst);
}

/// Exit a critical section (potentially re-enable interrupts)
///
/// [AMENDMENT] In the dummy port, this decrements the counter.
/// A real port would re-enable interrupts when nesting reaches zero.
#[inline(always)]
pub fn portEXIT_CRITICAL() {
    let prev = CRITICAL_NESTING.fetch_sub(1, Ordering::SeqCst);
    if prev == 1 {
        portENABLE_INTERRUPTS();
    }
}

/// Disable interrupts
///
/// [AMENDMENT] Dummy implementation - no-op.
/// A real port would disable interrupts at the hardware level.
#[inline(always)]
pub fn portDISABLE_INTERRUPTS() {
    // Dummy: no-op
    // Real port: disable interrupts (e.g., cpsid i on Cortex-M)
}

/// Enable interrupts
///
/// [AMENDMENT] Dummy implementation - no-op.
/// A real port would enable interrupts at the hardware level.
#[inline(always)]
pub fn portENABLE_INTERRUPTS() {
    // Dummy: no-op
    // Real port: enable interrupts (e.g., cpsie i on Cortex-M)
}

/// Set interrupt mask from ISR (save and disable)
///
/// Returns the previous interrupt state for restoration.
#[inline(always)]
pub fn portSET_INTERRUPT_MASK_FROM_ISR() -> UBaseType_t {
    // Dummy: return 0, no actual masking
    0
}

/// Clear interrupt mask from ISR (restore previous state)
#[inline(always)]
pub fn portCLEAR_INTERRUPT_MASK_FROM_ISR(_uxSavedInterruptStatus: UBaseType_t) {
    // Dummy: no-op
}

// =============================================================================
// Context Switching / Yield
// =============================================================================

/// Trigger a context switch (yield to scheduler)
///
/// [AMENDMENT] Dummy implementation panics - cannot actually context switch.
#[inline(always)]
pub fn portYIELD() {
    // In a real port, this would trigger PendSV or equivalent
    // For dummy port, we just note that a yield was requested
    // panic!("portYIELD called on dummy port - no real context switch available");

    // [AMENDMENT] For testing, we allow this to be a no-op rather than panic
    // This lets more code paths be exercised during development
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
// Scheduler Start/Stop
// =============================================================================

/// Start the scheduler
///
/// [AMENDMENT] Dummy implementation panics - cannot actually start scheduler.
/// A real port would:
/// 1. Set up the tick timer
/// 2. Initialize the first task's stack
/// 3. Start execution of the first task (never returns)
pub fn xPortStartScheduler() -> BaseType_t {
    panic!("xPortStartScheduler called on dummy port - cannot start real scheduler");
}

/// End the scheduler
///
/// [AMENDMENT] Most ports don't actually support stopping the scheduler.
pub fn vPortEndScheduler() {
    panic!("vPortEndScheduler called on dummy port");
}

// =============================================================================
// Stack Initialization
// =============================================================================

/// Initialize a task's stack
///
/// Sets up the initial stack frame so the task can be started by the scheduler.
///
/// # Arguments
/// * `pxTopOfStack` - Pointer to the top of the allocated stack
/// * `pxCode` - Task entry point function
/// * `pvParameters` - Parameter to pass to the task function
///
/// # Returns
/// Pointer to the new top of stack after initialization
///
/// [AMENDMENT] Dummy implementation returns the input pointer unchanged.
/// A real port would set up:
/// - Initial register values
/// - Return address (task function)
/// - Parameter in appropriate register
/// - Status register with interrupts enabled
pub fn pxPortInitialiseStack(
    pxTopOfStack: *mut StackType_t,
    _pxCode: TaskFunction_t,
    _pvParameters: *mut core::ffi::c_void,
) -> *mut StackType_t {
    // Dummy: return stack pointer unchanged
    // Real port: push initial context frame onto stack
    pxTopOfStack
}

// =============================================================================
// Tick Timer Setup
// =============================================================================

/// Set up the tick timer interrupt
///
/// [AMENDMENT] Dummy implementation - no-op.
/// A real port would configure the SysTick or equivalent timer.
pub fn vPortSetupTimerInterrupt() {
    // Dummy: no-op
    // Real port: configure tick timer (e.g., SysTick on Cortex-M)
}

// =============================================================================
// Utility Functions
// =============================================================================

/// Check if currently executing in an interrupt context
///
/// [AMENDMENT] Dummy implementation always returns false (not in ISR).
#[inline(always)]
pub fn xPortIsInsideInterrupt() -> BaseType_t {
    // Dummy: always report not in interrupt
    // Real port: check interrupt status register
    pdFALSE
}

/// No-operation
#[inline(always)]
pub fn portNOP() {
    // No operation
}

/// Memory barrier
///
/// [AMENDMENT] Using compiler fence as portable memory barrier.
#[inline(always)]
pub fn portMEMORY_BARRIER() {
    core::sync::atomic::compiler_fence(Ordering::SeqCst);
}

// =============================================================================
// Architecture Name
// =============================================================================

/// Architecture name string for this port
pub const portARCH_NAME: &str = "Dummy";

// =============================================================================
// Run-time Stats Timer Support
// =============================================================================

/// Run-time stats counter value.
/// This counter is incremented to provide a time base for run-time statistics.
#[cfg(feature = "generate-run-time-stats")]
static mut ulRunTimeCounterValue: crate::config::configRUN_TIME_COUNTER_TYPE = 0;

/// Configure the timer for run-time stats collection.
/// This is called from vTaskStartScheduler() before starting the scheduler.
#[cfg(feature = "generate-run-time-stats")]
#[inline(always)]
pub fn portCONFIGURE_TIMER_FOR_RUN_TIME_STATS() {
    unsafe {
        ulRunTimeCounterValue = 0;
    }
}

/// Get the current run-time counter value.
/// This is called from vTaskSwitchContext() to calculate task run times.
#[cfg(feature = "generate-run-time-stats")]
#[inline(always)]
pub fn portGET_RUN_TIME_COUNTER_VALUE() -> crate::config::configRUN_TIME_COUNTER_TYPE {
    unsafe { ulRunTimeCounterValue }
}

/// Increment the run-time counter.
/// This should be called from the tick interrupt to update the counter.
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

/// Suppress ticks and enter a low-power sleep mode (dummy implementation).
///
/// The dummy port does not support actual low-power sleep.
/// This is a no-op stub for compilation purposes.
pub fn vPortSuppressTicksAndSleep(_xExpectedIdleTime: crate::types::TickType_t) {
    // Dummy port - no actual sleep
}
