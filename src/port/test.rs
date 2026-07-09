/*
 * FreeRTOS Kernel <DEVELOPMENT BRANCH>
 * Copyright (C) 2021 Amazon.com, Inc. or its affiliates. All Rights Reserved.
 *
 * SPDX-License-Identifier: MIT
 *
 * [AMENDMENT] This host-only port models the observable state of a FreeRTOS
 * port so kernel tests can make deterministic assertions about interrupt
 * masks, critical nesting, yields, ticks, and scheduler start/stop requests.
 */

//! Deterministic host test port.
//!
//! This port does not execute task contexts.  It records port operations and
//! exposes a snapshot API for single-threaded kernel tests.  Stateful tests
//! must call [`test_port_reset`] before use and run with `--test-threads=1`.

use crate::types::*;
use core::sync::atomic::{compiler_fence, AtomicBool, AtomicI32, AtomicUsize, Ordering};

// =============================================================================
// Port Constants
// =============================================================================

/// Stack growth direction: descending.
pub const portSTACK_GROWTH: BaseType_t = -1;

/// Byte alignment used by the host test heap and task stacks.
pub const portBYTE_ALIGNMENT: usize = 8;

/// Architecture name string for this port.
pub const portARCH_NAME: &str = "Test";

const INTERRUPTS_ENABLED: usize = 0;
const INTERRUPTS_MASKED: usize = 1;

// =============================================================================
// Instrumented State
// =============================================================================

static INTERRUPT_MASK: AtomicUsize = AtomicUsize::new(INTERRUPTS_ENABLED);
static CRITICAL_NESTING: AtomicUsize = AtomicUsize::new(0);
static INTERRUPT_DISABLE_COUNT: AtomicUsize = AtomicUsize::new(0);
static INTERRUPT_ENABLE_COUNT: AtomicUsize = AtomicUsize::new(0);
static ISR_MASK_SET_COUNT: AtomicUsize = AtomicUsize::new(0);
static ISR_MASK_CLEAR_COUNT: AtomicUsize = AtomicUsize::new(0);

static YIELD_COUNT: AtomicUsize = AtomicUsize::new(0);
static ISR_YIELD_CALL_COUNT: AtomicUsize = AtomicUsize::new(0);
static ISR_YIELD_REQUEST_COUNT: AtomicUsize = AtomicUsize::new(0);
static INSIDE_INTERRUPT: AtomicBool = AtomicBool::new(false);

static TICK_SETUP_COUNT: AtomicUsize = AtomicUsize::new(0);
static TICK_INTERRUPT_COUNT: AtomicUsize = AtomicUsize::new(0);
static SUPPRESS_TICKS_COUNT: AtomicUsize = AtomicUsize::new(0);
static LAST_EXPECTED_IDLE_TICKS: AtomicUsize = AtomicUsize::new(0);

static SCHEDULER_START_COUNT: AtomicUsize = AtomicUsize::new(0);
static SCHEDULER_END_COUNT: AtomicUsize = AtomicUsize::new(0);
static SCHEDULER_RUNNING: AtomicBool = AtomicBool::new(false);
static SCHEDULER_START_RESULT: AtomicI32 = AtomicI32::new(pdFALSE);

static STACK_INITIALISE_COUNT: AtomicUsize = AtomicUsize::new(0);

#[cfg(feature = "generate-run-time-stats")]
static RUN_TIME_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// Complete observable state of the host test port.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TestPortSnapshot {
    pub interrupts_masked: bool,
    pub critical_nesting: usize,
    pub interrupt_disable_count: usize,
    pub interrupt_enable_count: usize,
    pub isr_mask_set_count: usize,
    pub isr_mask_clear_count: usize,
    pub yield_count: usize,
    pub isr_yield_call_count: usize,
    pub isr_yield_request_count: usize,
    pub inside_interrupt: bool,
    pub tick_setup_count: usize,
    pub tick_interrupt_count: usize,
    pub suppress_ticks_count: usize,
    pub last_expected_idle_ticks: TickType_t,
    pub scheduler_start_count: usize,
    pub scheduler_end_count: usize,
    pub scheduler_running: bool,
    pub stack_initialise_count: usize,
}

/// Reset all port-owned test state.
///
/// This does not reset the kernel's global scheduler state.  Kernel tests must
/// reset that state separately before firing a simulated tick.
pub fn test_port_reset() {
    INTERRUPT_MASK.store(INTERRUPTS_ENABLED, Ordering::SeqCst);
    CRITICAL_NESTING.store(0, Ordering::SeqCst);
    INTERRUPT_DISABLE_COUNT.store(0, Ordering::SeqCst);
    INTERRUPT_ENABLE_COUNT.store(0, Ordering::SeqCst);
    ISR_MASK_SET_COUNT.store(0, Ordering::SeqCst);
    ISR_MASK_CLEAR_COUNT.store(0, Ordering::SeqCst);
    YIELD_COUNT.store(0, Ordering::SeqCst);
    ISR_YIELD_CALL_COUNT.store(0, Ordering::SeqCst);
    ISR_YIELD_REQUEST_COUNT.store(0, Ordering::SeqCst);
    INSIDE_INTERRUPT.store(false, Ordering::SeqCst);
    TICK_SETUP_COUNT.store(0, Ordering::SeqCst);
    TICK_INTERRUPT_COUNT.store(0, Ordering::SeqCst);
    SUPPRESS_TICKS_COUNT.store(0, Ordering::SeqCst);
    LAST_EXPECTED_IDLE_TICKS.store(0, Ordering::SeqCst);
    SCHEDULER_START_COUNT.store(0, Ordering::SeqCst);
    SCHEDULER_END_COUNT.store(0, Ordering::SeqCst);
    SCHEDULER_RUNNING.store(false, Ordering::SeqCst);
    SCHEDULER_START_RESULT.store(pdFALSE, Ordering::SeqCst);
    STACK_INITIALISE_COUNT.store(0, Ordering::SeqCst);

    #[cfg(feature = "generate-run-time-stats")]
    RUN_TIME_COUNTER.store(0, Ordering::SeqCst);
}

/// Capture a consistent-enough snapshot for single-threaded tests.
pub fn test_port_snapshot() -> TestPortSnapshot {
    TestPortSnapshot {
        interrupts_masked: INTERRUPT_MASK.load(Ordering::SeqCst) != INTERRUPTS_ENABLED,
        critical_nesting: CRITICAL_NESTING.load(Ordering::SeqCst),
        interrupt_disable_count: INTERRUPT_DISABLE_COUNT.load(Ordering::SeqCst),
        interrupt_enable_count: INTERRUPT_ENABLE_COUNT.load(Ordering::SeqCst),
        isr_mask_set_count: ISR_MASK_SET_COUNT.load(Ordering::SeqCst),
        isr_mask_clear_count: ISR_MASK_CLEAR_COUNT.load(Ordering::SeqCst),
        yield_count: YIELD_COUNT.load(Ordering::SeqCst),
        isr_yield_call_count: ISR_YIELD_CALL_COUNT.load(Ordering::SeqCst),
        isr_yield_request_count: ISR_YIELD_REQUEST_COUNT.load(Ordering::SeqCst),
        inside_interrupt: INSIDE_INTERRUPT.load(Ordering::SeqCst),
        tick_setup_count: TICK_SETUP_COUNT.load(Ordering::SeqCst),
        tick_interrupt_count: TICK_INTERRUPT_COUNT.load(Ordering::SeqCst),
        suppress_ticks_count: SUPPRESS_TICKS_COUNT.load(Ordering::SeqCst),
        last_expected_idle_ticks: LAST_EXPECTED_IDLE_TICKS.load(Ordering::SeqCst) as TickType_t,
        scheduler_start_count: SCHEDULER_START_COUNT.load(Ordering::SeqCst),
        scheduler_end_count: SCHEDULER_END_COUNT.load(Ordering::SeqCst),
        scheduler_running: SCHEDULER_RUNNING.load(Ordering::SeqCst),
        stack_initialise_count: STACK_INITIALISE_COUNT.load(Ordering::SeqCst),
    }
}

/// Control what [`xPortIsInsideInterrupt`] reports.
pub fn test_port_set_inside_interrupt(inside_interrupt: bool) {
    INSIDE_INTERRUPT.store(inside_interrupt, Ordering::SeqCst);
}

/// Select the value returned by [`xPortStartScheduler`].
pub fn test_port_set_scheduler_start_result(result: BaseType_t) {
    SCHEDULER_START_RESULT.store(result, Ordering::SeqCst);
}

/// Set the test port's simulated tick-interrupt counter.
pub fn test_port_set_tick_count(ticks: TickType_t) {
    TICK_INTERRUPT_COUNT.store(ticks as usize, Ordering::SeqCst);
}

/// Advance the test port's simulated clock without touching kernel state.
pub fn test_port_advance_tick_count(ticks: TickType_t) -> TickType_t {
    TICK_INTERRUPT_COUNT
        .fetch_add(ticks as usize, Ordering::SeqCst)
        .wrapping_add(ticks as usize) as TickType_t
}

/// Deliver one tick to the kernel and perform the corresponding ISR yield.
///
/// The caller must initialize or reset the kernel scheduler globals first.
pub fn test_port_fire_tick() -> BaseType_t {
    TICK_INTERRUPT_COUNT.fetch_add(1, Ordering::SeqCst);
    let xSwitchRequired = crate::kernel::tasks::xTaskIncrementTick();
    portYIELD_FROM_ISR(xSwitchRequired);
    xSwitchRequired
}

// =============================================================================
// Critical Section Management
// =============================================================================

#[inline(always)]
pub fn portENTER_CRITICAL() {
    portDISABLE_INTERRUPTS();
    CRITICAL_NESTING.fetch_add(1, Ordering::SeqCst);
}

#[inline(always)]
pub fn portEXIT_CRITICAL() {
    let previous = CRITICAL_NESTING
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |nesting| {
            nesting.checked_sub(1)
        })
        .expect("portEXIT_CRITICAL called with nesting equal to zero");

    if previous == 1 {
        portENABLE_INTERRUPTS();
    }
}

#[inline(always)]
pub fn portDISABLE_INTERRUPTS() {
    INTERRUPT_DISABLE_COUNT.fetch_add(1, Ordering::SeqCst);
    INTERRUPT_MASK.store(INTERRUPTS_MASKED, Ordering::SeqCst);
}

#[inline(always)]
pub fn portENABLE_INTERRUPTS() {
    INTERRUPT_ENABLE_COUNT.fetch_add(1, Ordering::SeqCst);
    INTERRUPT_MASK.store(INTERRUPTS_ENABLED, Ordering::SeqCst);
}

#[inline(always)]
pub fn portSET_INTERRUPT_MASK_FROM_ISR() -> UBaseType_t {
    ISR_MASK_SET_COUNT.fetch_add(1, Ordering::SeqCst);
    INTERRUPT_MASK.swap(INTERRUPTS_MASKED, Ordering::SeqCst) as UBaseType_t
}

#[inline(always)]
pub fn portCLEAR_INTERRUPT_MASK_FROM_ISR(uxSavedInterruptStatus: UBaseType_t) {
    ISR_MASK_CLEAR_COUNT.fetch_add(1, Ordering::SeqCst);
    INTERRUPT_MASK.store(uxSavedInterruptStatus as usize, Ordering::SeqCst);
}

// =============================================================================
// Context Switching / Yield
// =============================================================================

#[inline(always)]
pub fn portYIELD() {
    YIELD_COUNT.fetch_add(1, Ordering::SeqCst);
}

#[inline(always)]
pub fn portYIELD_FROM_ISR(xSwitchRequired: BaseType_t) {
    ISR_YIELD_CALL_COUNT.fetch_add(1, Ordering::SeqCst);

    if xSwitchRequired != pdFALSE {
        ISR_YIELD_REQUEST_COUNT.fetch_add(1, Ordering::SeqCst);
        portYIELD();
    }
}

#[inline(always)]
pub fn portEND_SWITCHING_ISR(xSwitchRequired: BaseType_t) {
    portYIELD_FROM_ISR(xSwitchRequired);
}

// =============================================================================
// Scheduler Start/Stop
// =============================================================================

pub fn xPortStartScheduler() -> BaseType_t {
    SCHEDULER_START_COUNT.fetch_add(1, Ordering::SeqCst);
    vPortSetupTimerInterrupt();

    let result = SCHEDULER_START_RESULT.load(Ordering::SeqCst);
    SCHEDULER_RUNNING.store(result != pdFALSE, Ordering::SeqCst);
    result
}

pub fn vPortEndScheduler() {
    SCHEDULER_END_COUNT.fetch_add(1, Ordering::SeqCst);
    SCHEDULER_RUNNING.store(false, Ordering::SeqCst);
}

// =============================================================================
// Stack Initialization
// =============================================================================

pub fn pxPortInitialiseStack(
    pxTopOfStack: *mut StackType_t,
    _pxCode: TaskFunction_t,
    _pvParameters: *mut core::ffi::c_void,
) -> *mut StackType_t {
    STACK_INITIALISE_COUNT.fetch_add(1, Ordering::SeqCst);
    pxTopOfStack
}

// =============================================================================
// Tick Timer Setup
// =============================================================================

pub fn vPortSetupTimerInterrupt() {
    TICK_SETUP_COUNT.fetch_add(1, Ordering::SeqCst);
}

pub fn vPortSuppressTicksAndSleep(xExpectedIdleTime: TickType_t) {
    SUPPRESS_TICKS_COUNT.fetch_add(1, Ordering::SeqCst);
    LAST_EXPECTED_IDLE_TICKS.store(xExpectedIdleTime as usize, Ordering::SeqCst);
}

// =============================================================================
// Utility Functions
// =============================================================================

#[inline(always)]
pub fn xPortIsInsideInterrupt() -> BaseType_t {
    if INSIDE_INTERRUPT.load(Ordering::SeqCst) {
        pdTRUE
    } else {
        pdFALSE
    }
}

#[inline(always)]
pub fn portNOP() {
    core::hint::spin_loop();
}

#[inline(always)]
pub fn portMEMORY_BARRIER() {
    compiler_fence(Ordering::SeqCst);
}

// =============================================================================
// Run-time Stats Timer Support
// =============================================================================

#[cfg(feature = "generate-run-time-stats")]
#[inline(always)]
pub fn portCONFIGURE_TIMER_FOR_RUN_TIME_STATS() {
    RUN_TIME_COUNTER.store(0, Ordering::SeqCst);
}

#[cfg(feature = "generate-run-time-stats")]
#[inline(always)]
pub fn portGET_RUN_TIME_COUNTER_VALUE() -> crate::config::configRUN_TIME_COUNTER_TYPE {
    RUN_TIME_COUNTER.load(Ordering::SeqCst) as crate::config::configRUN_TIME_COUNTER_TYPE
}

#[cfg(feature = "generate-run-time-stats")]
#[inline(always)]
pub fn portINCREMENT_RUN_TIME_COUNTER() {
    RUN_TIME_COUNTER.fetch_add(1, Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_interrupt_yield_tick_and_scheduler_operations() {
        test_port_reset();

        portENTER_CRITICAL();
        portENTER_CRITICAL();
        portEXIT_CRITICAL();
        let saved_mask = portSET_INTERRUPT_MASK_FROM_ISR();
        portCLEAR_INTERRUPT_MASK_FROM_ISR(saved_mask);
        portYIELD_FROM_ISR(pdFALSE);
        portYIELD_FROM_ISR(pdTRUE);
        test_port_set_inside_interrupt(true);
        test_port_set_tick_count(4);
        assert_eq!(test_port_advance_tick_count(3), 7);
        vPortSuppressTicksAndSleep(11);
        test_port_set_scheduler_start_result(pdTRUE);
        assert_eq!(xPortStartScheduler(), pdTRUE);
        vPortEndScheduler();
        portEXIT_CRITICAL();

        let snapshot = test_port_snapshot();
        assert!(!snapshot.interrupts_masked);
        assert_eq!(snapshot.critical_nesting, 0);
        assert_eq!(snapshot.interrupt_disable_count, 2);
        assert_eq!(snapshot.interrupt_enable_count, 1);
        assert_eq!(snapshot.isr_mask_set_count, 1);
        assert_eq!(snapshot.isr_mask_clear_count, 1);
        assert_eq!(snapshot.yield_count, 1);
        assert_eq!(snapshot.isr_yield_call_count, 2);
        assert_eq!(snapshot.isr_yield_request_count, 1);
        assert_eq!(xPortIsInsideInterrupt(), pdTRUE);
        assert_eq!(snapshot.tick_interrupt_count, 7);
        assert_eq!(snapshot.tick_setup_count, 1);
        assert_eq!(snapshot.suppress_ticks_count, 1);
        assert_eq!(snapshot.last_expected_idle_ticks, 11);
        assert_eq!(snapshot.scheduler_start_count, 1);
        assert_eq!(snapshot.scheduler_end_count, 1);
        assert!(!snapshot.scheduler_running);
    }
}
