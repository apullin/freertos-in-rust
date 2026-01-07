/*
 * FreeRTOS Kernel <DEVELOPMENT BRANCH>
 * Copyright (C) 2021 Amazon.com, Inc. or its affiliates. All Rights Reserved.
 *
 * SPDX-License-Identifier: MIT
 *
 * [AMENDMENT] This module is the Rust port of timers.c.
 * It provides software timer functionality for FreeRTOS.
 * A timer service task (daemon) processes timer events via a queue.
 */

//! Software Timer Implementation
//!
//! This module provides software timers for FreeRTOS.
//! Timers allow functions to execute at a set time in the future, or
//! periodically with a fixed frequency.
//!
//! ## Key Concepts
//!
//! - Timers are processed by a daemon task (timer service task)
//! - Timer commands are sent via a queue to the daemon
//! - Timers can be one-shot or auto-reload (periodic)
//!
//! ## API Functions
//!
//! - [`xTimerCreate`] / [`xTimerCreateStatic`] - Create a timer
//! - [`xTimerStart`] / [`xTimerStop`] - Start/stop a timer
//! - [`xTimerChangePeriod`] - Change timer period
//! - [`xTimerReset`] - Reset a timer (restart countdown)

#![allow(unused_variables)]
#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(dead_code)]
#![allow(static_mut_refs)]

use core::ffi::c_void;
use core::ptr;

use crate::config::*;
use crate::kernel::list::*;
use crate::kernel::queue::*;
use crate::kernel::tasks::*;
use crate::memory::*;
use crate::types::*;

// Import trace macros (exported at crate root via #[macro_export])
use crate::{
    traceENTER_xTimerCreateTimerTask, traceRETURN_xTimerCreateTimerTask,
    traceENTER_xTimerCreate, traceRETURN_xTimerCreate,
    traceENTER_xTimerCreateStatic, traceRETURN_xTimerCreateStatic,
    traceENTER_xTimerGenericCommandFromTask, traceRETURN_xTimerGenericCommandFromTask,
    traceENTER_xTimerGenericCommandFromISR, traceRETURN_xTimerGenericCommandFromISR,
    traceTIMER_COMMAND_SEND,
    traceENTER_xTimerGetTimerDaemonTaskHandle, traceRETURN_xTimerGetTimerDaemonTaskHandle,
    traceENTER_xTimerGetPeriod, traceRETURN_xTimerGetPeriod,
    traceENTER_vTimerSetReloadMode, traceRETURN_vTimerSetReloadMode,
    traceENTER_xTimerGetReloadMode, traceRETURN_xTimerGetReloadMode,
    traceENTER_uxTimerGetReloadMode, traceRETURN_uxTimerGetReloadMode,
    traceENTER_xTimerGetExpiryTime, traceRETURN_xTimerGetExpiryTime,
    traceENTER_pcTimerGetName, traceRETURN_pcTimerGetName,
    traceENTER_xTimerIsTimerActive, traceRETURN_xTimerIsTimerActive,
    traceENTER_pvTimerGetTimerID, traceRETURN_pvTimerGetTimerID,
    traceENTER_vTimerSetTimerID, traceRETURN_vTimerSetTimerID,
    traceENTER_xTimerGetStaticBuffer, traceRETURN_xTimerGetStaticBuffer,
    traceTIMER_CREATE, traceTIMER_EXPIRED, traceTIMER_COMMAND_RECEIVED,
};

#[cfg(feature = "trace-facility")]
use crate::{
    traceENTER_uxTimerGetTimerNumber, traceRETURN_uxTimerGetTimerNumber,
    traceENTER_vTimerSetTimerNumber, traceRETURN_vTimerSetTimerNumber,
};

#[cfg(feature = "pend-function-call")]
use crate::{
    traceENTER_xTimerPendFunctionCallFromISR, traceRETURN_xTimerPendFunctionCallFromISR,
    traceENTER_xTimerPendFunctionCall, traceRETURN_xTimerPendFunctionCall,
    tracePEND_FUNC_CALL_FROM_ISR, tracePEND_FUNC_CALL,
};

// =============================================================================
// Timer Constants
// =============================================================================

/// No delay constant
const tmrNO_DELAY: TickType_t = 0;

/// Maximum time before tick counter overflow
const tmrMAX_TIME_BEFORE_OVERFLOW: TickType_t = !0; // (TickType_t)-1

/// Timer service task name
pub const configTIMER_SERVICE_TASK_NAME: &[u8] = b"Tmr Svc\0";

// =============================================================================
// Timer Status Bits
// =============================================================================

/// Timer is currently active
const tmrSTATUS_IS_ACTIVE: u8 = 0x01;

/// Timer was statically allocated
const tmrSTATUS_IS_STATICALLY_ALLOCATED: u8 = 0x02;

/// Timer is an auto-reload (periodic) timer
const tmrSTATUS_IS_AUTORELOAD: u8 = 0x04;

// =============================================================================
// Timer Command IDs
// =============================================================================

/// Execute callback from ISR (negative = callback command)
pub const tmrCOMMAND_EXECUTE_CALLBACK_FROM_ISR: BaseType_t = -2;

/// Execute callback (negative = callback command)
pub const tmrCOMMAND_EXECUTE_CALLBACK: BaseType_t = -1;

/// Start timer (don't trace)
pub const tmrCOMMAND_START_DONT_TRACE: BaseType_t = 0;

/// Start timer
pub const tmrCOMMAND_START: BaseType_t = 1;

/// Reset timer
pub const tmrCOMMAND_RESET: BaseType_t = 2;

/// Stop timer
pub const tmrCOMMAND_STOP: BaseType_t = 3;

/// Change timer period
pub const tmrCOMMAND_CHANGE_PERIOD: BaseType_t = 4;

/// Delete timer
pub const tmrCOMMAND_DELETE: BaseType_t = 5;

/// First command that comes from ISR (used to distinguish task vs ISR commands)
pub const tmrFIRST_FROM_ISR_COMMAND: BaseType_t = 6;

/// Start timer from ISR
pub const tmrCOMMAND_START_FROM_ISR: BaseType_t = 6;

/// Reset timer from ISR
pub const tmrCOMMAND_RESET_FROM_ISR: BaseType_t = 7;

/// Stop timer from ISR
pub const tmrCOMMAND_STOP_FROM_ISR: BaseType_t = 8;

/// Change period from ISR
pub const tmrCOMMAND_CHANGE_PERIOD_FROM_ISR: BaseType_t = 9;

// =============================================================================
// Callback Function Types
// =============================================================================

/// Timer callback function prototype
/// Called when a timer expires
pub type TimerCallbackFunction_t = extern "C" fn(TimerHandle_t);

/// Pended function callback prototype
/// For xTimerPendFunctionCall
pub type PendedFunction_t = extern "C" fn(*mut c_void, u32);

// =============================================================================
// Timer Structure
// =============================================================================

/// Timer Control Block
///
/// The old naming convention (xTIMER) is used to prevent breaking
/// kernel-aware debuggers.
#[repr(C)]
pub struct xTIMER {
    /// Text name for debugging
    pub pcTimerName: *const u8,

    /// List item used to place timer in active timer lists
    pub xTimerListItem: ListItem_t,

    /// Timer period in ticks
    pub xTimerPeriodInTicks: TickType_t,

    /// User-supplied timer ID
    pub pvTimerID: *mut c_void,

    /// Callback function to execute when timer expires
    pub pxCallbackFunction: TimerCallbackFunction_t,

    /// Timer number for trace facility
    #[cfg(feature = "trace-facility")]
    pub uxTimerNumber: UBaseType_t,

    /// Status bits (active, static, auto-reload)
    pub ucStatus: u8,
}

/// Alias for xTIMER
pub type Timer_t = xTIMER;

// =============================================================================
// Timer Message Structures
// =============================================================================

/// Timer command parameters
#[repr(C)]
#[derive(Clone, Copy)]
pub struct TimerParameter_t {
    /// Optional value (e.g., new period for CHANGE_PERIOD)
    pub xMessageValue: TickType_t,
    /// Timer to operate on
    pub pxTimer: *mut Timer_t,
}

/// Callback parameters for pended functions
#[repr(C)]
#[derive(Clone, Copy)]
pub struct CallbackParameters_t {
    /// Function to call
    pub pxCallbackFunction: PendedFunction_t,
    /// First parameter
    pub pvParameter1: *mut c_void,
    /// Second parameter
    pub ulParameter2: u32,
}

/// Union for timer message payload
#[repr(C)]
#[derive(Clone, Copy)]
pub union DaemonTaskMessageUnion {
    /// Timer command parameters
    pub xTimerParameters: TimerParameter_t,
    /// Callback parameters (for pended functions)
    pub xCallbackParameters: CallbackParameters_t,
}

/// Message sent to the timer daemon task
#[repr(C)]
pub struct DaemonTaskMessage_t {
    /// Command ID (positive = timer command, negative = callback)
    pub xMessageID: BaseType_t,
    /// Message payload
    pub u: DaemonTaskMessageUnion,
}

// =============================================================================
// Static Timer Buffer (for xTimerCreateStatic)
// =============================================================================

/// Static buffer for timer allocation
///
/// Must be the same size as Timer_t for static allocation to work.
#[repr(C)]
pub struct StaticTimer_t {
    /// Placeholder to match Timer_t size - name
    pub pvDummy1: *mut c_void,
    /// Placeholder - list item
    pub xDummy2: StaticListItem_t,
    /// Placeholder - period
    pub xDummy3: TickType_t,
    /// Placeholder - timer ID
    pub pvDummy4: *mut c_void,
    /// Placeholder - callback
    pub pvDummy5: *mut c_void,
    /// Placeholder - trace number (conditional)
    #[cfg(feature = "trace-facility")]
    pub uxDummy6: UBaseType_t,
    /// Placeholder - status
    pub ucDummy7: u8,
}

// =============================================================================
// Scheduler Globals
// =============================================================================

/// Active timer list 1
static mut xActiveTimerList1: List_t = List_t::new();

/// Active timer list 2
static mut xActiveTimerList2: List_t = List_t::new();

/// Pointer to current active timer list
static mut pxCurrentTimerList: *mut List_t = ptr::null_mut();

/// Pointer to overflow timer list
static mut pxOverflowTimerList: *mut List_t = ptr::null_mut();

/// Queue for sending commands to timer task
static mut xTimerQueue: QueueHandle_t = ptr::null_mut();

/// Handle to the timer daemon task
static mut xTimerTaskHandle: TaskHandle_t = ptr::null_mut();

/// Last sampled time (for overflow detection)
static mut xLastTime: TickType_t = 0;

// =============================================================================
// Public API: Timer Creation
// =============================================================================

/// Create the timer service task
///
/// Called by the scheduler when configUSE_TIMERS == 1.
/// Creates the timer queue and daemon task.
///
/// # Returns
/// `pdPASS` if successful, `pdFAIL` otherwise
pub fn xTimerCreateTimerTask() -> BaseType_t {
    let mut xReturn: BaseType_t = pdFAIL;

    traceENTER_xTimerCreateTimerTask!();

    // Ensure timer infrastructure is created
    prvCheckForValidListAndQueue();

    unsafe {
        if xTimerQueue != ptr::null_mut() {
            // Create the timer task
            // [AMENDMENT] For static allocation, we would call
            // vApplicationGetTimerTaskMemory. For now, use dynamic.
            #[cfg(feature = "alloc")]
            {
                xReturn = xTaskCreate(
                    prvTimerTask,
                    configTIMER_SERVICE_TASK_NAME.as_ptr(),
                    configTIMER_TASK_STACK_DEPTH,
                    ptr::null_mut(),
                    configTIMER_TASK_PRIORITY,
                    &mut xTimerTaskHandle,
                );
            }

            #[cfg(not(feature = "alloc"))]
            {
                // Static allocation requires user to provide memory
                // via vApplicationGetTimerTaskMemory
                // TODO: Implement static allocation path
                xReturn = pdFAIL;
            }
        }
    }

    configASSERT(xReturn != pdFAIL);

    traceRETURN_xTimerCreateTimerTask!(xReturn);

    xReturn
}

/// Create a new software timer (dynamic allocation)
///
/// # Arguments
/// * `pcTimerName` - Text name for debugging
/// * `xTimerPeriodInTicks` - Timer period (must be > 0)
/// * `xAutoReload` - pdTRUE for periodic, pdFALSE for one-shot
/// * `pvTimerID` - User-supplied ID
/// * `pxCallbackFunction` - Function to call when timer expires
///
/// # Returns
/// Handle to the timer, or NULL on failure
#[cfg(feature = "alloc")]
pub fn xTimerCreate(
    pcTimerName: *const u8,
    xTimerPeriodInTicks: TickType_t,
    xAutoReload: BaseType_t,
    pvTimerID: *mut c_void,
    pxCallbackFunction: TimerCallbackFunction_t,
) -> TimerHandle_t {
    traceENTER_xTimerCreate!(pcTimerName, xTimerPeriodInTicks, xAutoReload, pvTimerID, pxCallbackFunction);

    let pxNewTimer: *mut Timer_t = unsafe { pvPortMalloc(core::mem::size_of::<Timer_t>()) as *mut Timer_t };

    if pxNewTimer != ptr::null_mut() {
        // Status is zero - not static, not active, auto-reload set below
        unsafe {
            (*pxNewTimer).ucStatus = 0x00;
        }
        prvInitialiseNewTimer(
            pcTimerName,
            xTimerPeriodInTicks,
            xAutoReload,
            pvTimerID,
            pxCallbackFunction,
            pxNewTimer,
        );
    }

    traceRETURN_xTimerCreate!(pxNewTimer);

    pxNewTimer as TimerHandle_t
}

/// Create a new software timer (static allocation)
///
/// # Arguments
/// * `pcTimerName` - Text name for debugging
/// * `xTimerPeriodInTicks` - Timer period (must be > 0)
/// * `xAutoReload` - pdTRUE for periodic, pdFALSE for one-shot
/// * `pvTimerID` - User-supplied ID
/// * `pxCallbackFunction` - Function to call when timer expires
/// * `pxTimerBuffer` - Pre-allocated StaticTimer_t buffer
///
/// # Returns
/// Handle to the timer, or NULL if pxTimerBuffer is NULL
pub fn xTimerCreateStatic(
    pcTimerName: *const u8,
    xTimerPeriodInTicks: TickType_t,
    xAutoReload: BaseType_t,
    pvTimerID: *mut c_void,
    pxCallbackFunction: TimerCallbackFunction_t,
    pxTimerBuffer: *mut StaticTimer_t,
) -> TimerHandle_t {
    traceENTER_xTimerCreateStatic!(pcTimerName, xTimerPeriodInTicks, xAutoReload, pvTimerID, pxCallbackFunction, pxTimerBuffer);

    // Verify StaticTimer_t is same size as Timer_t
    #[cfg(debug_assertions)]
    {
        let static_size = core::mem::size_of::<StaticTimer_t>();
        let timer_size = core::mem::size_of::<Timer_t>();
        configASSERT(static_size == timer_size);
    }

    configASSERT(pxTimerBuffer != ptr::null_mut());

    let pxNewTimer: *mut Timer_t = pxTimerBuffer as *mut Timer_t;

    if pxNewTimer != ptr::null_mut() {
        // Mark as statically allocated
        unsafe {
            (*pxNewTimer).ucStatus = tmrSTATUS_IS_STATICALLY_ALLOCATED;
        }
        prvInitialiseNewTimer(
            pcTimerName,
            xTimerPeriodInTicks,
            xAutoReload,
            pvTimerID,
            pxCallbackFunction,
            pxNewTimer,
        );
    }

    traceRETURN_xTimerCreateStatic!(pxNewTimer);

    pxNewTimer as TimerHandle_t
}

// =============================================================================
// Public API: Timer Control
// =============================================================================

/// Send a command to the timer daemon from a task
///
/// # Arguments
/// * `xTimer` - Timer handle
/// * `xCommandID` - Command to execute
/// * `xOptionalValue` - Value for command (e.g., new period)
/// * `pxHigherPriorityTaskWoken` - Set if a context switch is needed (unused for task)
/// * `xTicksToWait` - Block time if queue is full
///
/// # Returns
/// `pdPASS` if command was sent, `pdFAIL` otherwise
pub fn xTimerGenericCommandFromTask(
    xTimer: TimerHandle_t,
    xCommandID: BaseType_t,
    xOptionalValue: TickType_t,
    pxHigherPriorityTaskWoken: *mut BaseType_t,
    xTicksToWait: TickType_t,
) -> BaseType_t {
    let mut xReturn: BaseType_t = pdFAIL;

    traceENTER_xTimerGenericCommandFromTask!(xTimer, xCommandID, xOptionalValue, pxHigherPriorityTaskWoken, xTicksToWait);

    unsafe {
        if xTimerQueue != ptr::null_mut() && xTimer != ptr::null_mut() {
            let mut xMessage: DaemonTaskMessage_t = core::mem::zeroed();
            xMessage.xMessageID = xCommandID;
            xMessage.u.xTimerParameters.xMessageValue = xOptionalValue;
            xMessage.u.xTimerParameters.pxTimer = xTimer as *mut Timer_t;

            configASSERT(xCommandID < tmrFIRST_FROM_ISR_COMMAND);

            if xCommandID < tmrFIRST_FROM_ISR_COMMAND {
                let xTicksToWaitActual = if xTaskGetSchedulerState() == taskSCHEDULER_RUNNING {
                    xTicksToWait
                } else {
                    tmrNO_DELAY
                };

                xReturn = xQueueSendToBack(
                    xTimerQueue,
                    &xMessage as *const _ as *const c_void,
                    xTicksToWaitActual,
                );
            }

            traceTIMER_COMMAND_SEND!(xTimer, xCommandID, xOptionalValue, xReturn);
        }
    }

    traceRETURN_xTimerGenericCommandFromTask!(xReturn);

    xReturn
}

/// Send a command to the timer daemon from an ISR
///
/// # Arguments
/// * `xTimer` - Timer handle
/// * `xCommandID` - Command to execute
/// * `xOptionalValue` - Value for command
/// * `pxHigherPriorityTaskWoken` - Set to pdTRUE if context switch needed
/// * `xTicksToWait` - Ignored for ISR version
///
/// # Returns
/// `pdPASS` if command was sent, `pdFAIL` otherwise
pub fn xTimerGenericCommandFromISR(
    xTimer: TimerHandle_t,
    xCommandID: BaseType_t,
    xOptionalValue: TickType_t,
    pxHigherPriorityTaskWoken: *mut BaseType_t,
    _xTicksToWait: TickType_t,
) -> BaseType_t {
    let mut xReturn: BaseType_t = pdFAIL;

    traceENTER_xTimerGenericCommandFromISR!(xTimer, xCommandID, xOptionalValue, pxHigherPriorityTaskWoken, _xTicksToWait);

    unsafe {
        if xTimerQueue != ptr::null_mut() && xTimer != ptr::null_mut() {
            let mut xMessage: DaemonTaskMessage_t = core::mem::zeroed();
            xMessage.xMessageID = xCommandID;
            xMessage.u.xTimerParameters.xMessageValue = xOptionalValue;
            xMessage.u.xTimerParameters.pxTimer = xTimer as *mut Timer_t;

            configASSERT(xCommandID >= tmrFIRST_FROM_ISR_COMMAND);

            if xCommandID >= tmrFIRST_FROM_ISR_COMMAND {
                xReturn = xQueueSendToBackFromISR(
                    xTimerQueue,
                    &xMessage as *const _ as *const c_void,
                    pxHigherPriorityTaskWoken,
                );
            }

            traceTIMER_COMMAND_SEND!(xTimer, xCommandID, xOptionalValue, xReturn);
        }
    }

    traceRETURN_xTimerGenericCommandFromISR!(xReturn);

    xReturn
}

/// Generic timer command (dispatches to FromTask or FromISR based on command ID)
#[inline(always)]
pub fn xTimerGenericCommand(
    xTimer: TimerHandle_t,
    xCommandID: BaseType_t,
    xOptionalValue: TickType_t,
    pxHigherPriorityTaskWoken: *mut BaseType_t,
    xTicksToWait: TickType_t,
) -> BaseType_t {
    if xCommandID < tmrFIRST_FROM_ISR_COMMAND {
        xTimerGenericCommandFromTask(xTimer, xCommandID, xOptionalValue, pxHigherPriorityTaskWoken, xTicksToWait)
    } else {
        xTimerGenericCommandFromISR(xTimer, xCommandID, xOptionalValue, pxHigherPriorityTaskWoken, xTicksToWait)
    }
}

// =============================================================================
// Public API: Timer Query Functions
// =============================================================================

/// Get the timer daemon task handle
pub fn xTimerGetTimerDaemonTaskHandle() -> TaskHandle_t {
    traceENTER_xTimerGetTimerDaemonTaskHandle!();

    unsafe {
        configASSERT(xTimerTaskHandle != ptr::null_mut());
        traceRETURN_xTimerGetTimerDaemonTaskHandle!(xTimerTaskHandle);
        xTimerTaskHandle
    }
}

/// Get the timer period in ticks
pub fn xTimerGetPeriod(xTimer: TimerHandle_t) -> TickType_t {
    let pxTimer = xTimer as *mut Timer_t;

    traceENTER_xTimerGetPeriod!(xTimer);

    configASSERT(xTimer != ptr::null_mut());

    let xReturn = unsafe { (*pxTimer).xTimerPeriodInTicks };

    traceRETURN_xTimerGetPeriod!(xReturn);

    xReturn
}

/// Set timer reload mode
pub fn vTimerSetReloadMode(xTimer: TimerHandle_t, xAutoReload: BaseType_t) {
    let pxTimer = xTimer as *mut Timer_t;

    traceENTER_vTimerSetReloadMode!(xTimer, xAutoReload);

    configASSERT(xTimer != ptr::null_mut());

    taskENTER_CRITICAL();
    unsafe {
        if xAutoReload != pdFALSE {
            (*pxTimer).ucStatus |= tmrSTATUS_IS_AUTORELOAD;
        } else {
            (*pxTimer).ucStatus &= !tmrSTATUS_IS_AUTORELOAD;
        }
    }
    taskEXIT_CRITICAL();

    traceRETURN_vTimerSetReloadMode!();
}

/// Get timer reload mode
pub fn xTimerGetReloadMode(xTimer: TimerHandle_t) -> BaseType_t {
    let pxTimer = xTimer as *mut Timer_t;
    let xReturn: BaseType_t;

    traceENTER_xTimerGetReloadMode!(xTimer);

    configASSERT(xTimer != ptr::null_mut());

    taskENTER_CRITICAL();
    unsafe {
        if ((*pxTimer).ucStatus & tmrSTATUS_IS_AUTORELOAD) == 0 {
            xReturn = pdFALSE;
        } else {
            xReturn = pdTRUE;
        }
    }
    taskEXIT_CRITICAL();

    traceRETURN_xTimerGetReloadMode!(xReturn);

    xReturn
}

/// Get timer reload mode (UBaseType_t return variant)
pub fn uxTimerGetReloadMode(xTimer: TimerHandle_t) -> UBaseType_t {
    traceENTER_uxTimerGetReloadMode!(xTimer);

    let uxReturn = xTimerGetReloadMode(xTimer) as UBaseType_t;

    traceRETURN_uxTimerGetReloadMode!(uxReturn);

    uxReturn
}

/// Get the timer expiry time
pub fn xTimerGetExpiryTime(xTimer: TimerHandle_t) -> TickType_t {
    let pxTimer = xTimer as *mut Timer_t;

    traceENTER_xTimerGetExpiryTime!(xTimer);

    configASSERT(xTimer != ptr::null_mut());

    let xReturn = unsafe { listGET_LIST_ITEM_VALUE(&(*pxTimer).xTimerListItem) };

    traceRETURN_xTimerGetExpiryTime!(xReturn);

    xReturn
}

/// Get timer name
pub fn pcTimerGetName(xTimer: TimerHandle_t) -> *const u8 {
    let pxTimer = xTimer as *mut Timer_t;

    traceENTER_pcTimerGetName!(xTimer);

    configASSERT(xTimer != ptr::null_mut());

    let pcName = unsafe { (*pxTimer).pcTimerName };

    traceRETURN_pcTimerGetName!(pcName);

    pcName
}

/// Check if timer is active
pub fn xTimerIsTimerActive(xTimer: TimerHandle_t) -> BaseType_t {
    let pxTimer = xTimer as *mut Timer_t;
    let xReturn: BaseType_t;

    traceENTER_xTimerIsTimerActive!(xTimer);

    configASSERT(xTimer != ptr::null_mut());

    taskENTER_CRITICAL();
    unsafe {
        if ((*pxTimer).ucStatus & tmrSTATUS_IS_ACTIVE) == 0 {
            xReturn = pdFALSE;
        } else {
            xReturn = pdTRUE;
        }
    }
    taskEXIT_CRITICAL();

    traceRETURN_xTimerIsTimerActive!(xReturn);

    xReturn
}

/// Get timer ID
pub fn pvTimerGetTimerID(xTimer: TimerHandle_t) -> *mut c_void {
    let pxTimer = xTimer as *mut Timer_t;

    traceENTER_pvTimerGetTimerID!(xTimer);

    configASSERT(xTimer != ptr::null_mut());

    let pvReturn: *mut c_void;
    taskENTER_CRITICAL();
    unsafe {
        pvReturn = (*pxTimer).pvTimerID;
    }
    taskEXIT_CRITICAL();

    traceRETURN_pvTimerGetTimerID!(pvReturn);

    pvReturn
}

/// Set timer ID
pub fn vTimerSetTimerID(xTimer: TimerHandle_t, pvNewID: *mut c_void) {
    let pxTimer = xTimer as *mut Timer_t;

    traceENTER_vTimerSetTimerID!(xTimer, pvNewID);

    configASSERT(xTimer != ptr::null_mut());

    taskENTER_CRITICAL();
    unsafe {
        (*pxTimer).pvTimerID = pvNewID;
    }
    taskEXIT_CRITICAL();

    traceRETURN_vTimerSetTimerID!();
}

/// Get static buffer from a statically allocated timer
pub fn xTimerGetStaticBuffer(
    xTimer: TimerHandle_t,
    ppxTimerBuffer: *mut *mut StaticTimer_t,
) -> BaseType_t {
    let pxTimer = xTimer as *mut Timer_t;
    let xReturn: BaseType_t;

    traceENTER_xTimerGetStaticBuffer!(xTimer, ppxTimerBuffer);

    configASSERT(ppxTimerBuffer != ptr::null_mut());

    unsafe {
        if ((*pxTimer).ucStatus & tmrSTATUS_IS_STATICALLY_ALLOCATED) != 0 {
            *ppxTimerBuffer = pxTimer as *mut StaticTimer_t;
            xReturn = pdTRUE;
        } else {
            xReturn = pdFALSE;
        }
    }

    traceRETURN_xTimerGetStaticBuffer!(xReturn);

    xReturn
}

/// Reset timer module state (for scheduler restart)
pub fn vTimerResetState() {
    unsafe {
        xTimerQueue = ptr::null_mut();
        xTimerTaskHandle = ptr::null_mut();
    }
}

// =============================================================================
// Trace Facility Functions
// =============================================================================

/// Get timer number (for trace facility)
#[cfg(feature = "trace-facility")]
pub fn uxTimerGetTimerNumber(xTimer: TimerHandle_t) -> UBaseType_t {
    traceENTER_uxTimerGetTimerNumber!(xTimer);

    let uxReturn = unsafe { (*(xTimer as *mut Timer_t)).uxTimerNumber };

    traceRETURN_uxTimerGetTimerNumber!(uxReturn);

    uxReturn
}

/// Set timer number (for trace facility)
#[cfg(feature = "trace-facility")]
pub fn vTimerSetTimerNumber(xTimer: TimerHandle_t, uxTimerNumber: UBaseType_t) {
    traceENTER_vTimerSetTimerNumber!(xTimer, uxTimerNumber);

    unsafe {
        (*(xTimer as *mut Timer_t)).uxTimerNumber = uxTimerNumber;
    }

    traceRETURN_vTimerSetTimerNumber!();
}

// =============================================================================
// Pended Function Calls
// =============================================================================

/// Pend a function call from an ISR
///
/// Allows ISR to defer processing to the timer daemon task.
#[cfg(feature = "pend-function-call")]
pub fn xTimerPendFunctionCallFromISR(
    xFunctionToPend: PendedFunction_t,
    pvParameter1: *mut c_void,
    ulParameter2: u32,
    pxHigherPriorityTaskWoken: *mut BaseType_t,
) -> BaseType_t {
    traceENTER_xTimerPendFunctionCallFromISR!(xFunctionToPend, pvParameter1, ulParameter2, pxHigherPriorityTaskWoken);

    let mut xMessage: DaemonTaskMessage_t = unsafe { core::mem::zeroed() };
    xMessage.xMessageID = tmrCOMMAND_EXECUTE_CALLBACK_FROM_ISR;
    unsafe {
        xMessage.u.xCallbackParameters.pxCallbackFunction = xFunctionToPend;
        xMessage.u.xCallbackParameters.pvParameter1 = pvParameter1;
        xMessage.u.xCallbackParameters.ulParameter2 = ulParameter2;
    }

    let xReturn = unsafe {
        xQueueSendFromISR(
            xTimerQueue,
            &xMessage as *const _ as *const c_void,
            pxHigherPriorityTaskWoken,
        )
    };

    tracePEND_FUNC_CALL_FROM_ISR!(xFunctionToPend, pvParameter1, ulParameter2, xReturn);
    traceRETURN_xTimerPendFunctionCallFromISR!(xReturn);

    xReturn
}

/// Pend a function call from a task
#[cfg(feature = "pend-function-call")]
pub fn xTimerPendFunctionCall(
    xFunctionToPend: PendedFunction_t,
    pvParameter1: *mut c_void,
    ulParameter2: u32,
    xTicksToWait: TickType_t,
) -> BaseType_t {
    traceENTER_xTimerPendFunctionCall!(xFunctionToPend, pvParameter1, ulParameter2, xTicksToWait);

    unsafe {
        configASSERT(xTimerQueue != ptr::null_mut());
    }

    let mut xMessage: DaemonTaskMessage_t = unsafe { core::mem::zeroed() };
    xMessage.xMessageID = tmrCOMMAND_EXECUTE_CALLBACK;
    unsafe {
        xMessage.u.xCallbackParameters.pxCallbackFunction = xFunctionToPend;
        xMessage.u.xCallbackParameters.pvParameter1 = pvParameter1;
        xMessage.u.xCallbackParameters.ulParameter2 = ulParameter2;
    }

    let xReturn = unsafe {
        xQueueSendToBack(
            xTimerQueue,
            &xMessage as *const _ as *const c_void,
            xTicksToWait,
        )
    };

    tracePEND_FUNC_CALL!(xFunctionToPend, pvParameter1, ulParameter2, xReturn);
    traceRETURN_xTimerPendFunctionCall!(xReturn);

    xReturn
}

// =============================================================================
// Private Functions
// =============================================================================

/// Initialize a newly created timer
fn prvInitialiseNewTimer(
    pcTimerName: *const u8,
    xTimerPeriodInTicks: TickType_t,
    xAutoReload: BaseType_t,
    pvTimerID: *mut c_void,
    pxCallbackFunction: TimerCallbackFunction_t,
    pxNewTimer: *mut Timer_t,
) {
    // Period must be > 0
    configASSERT(xTimerPeriodInTicks > 0);

    // Ensure infrastructure is initialized
    prvCheckForValidListAndQueue();

    // Initialize timer fields
    unsafe {
        (*pxNewTimer).pcTimerName = pcTimerName;
        (*pxNewTimer).xTimerPeriodInTicks = xTimerPeriodInTicks;
        (*pxNewTimer).pvTimerID = pvTimerID;
        (*pxNewTimer).pxCallbackFunction = pxCallbackFunction;
        vListInitialiseItem(&mut (*pxNewTimer).xTimerListItem);

        if xAutoReload != pdFALSE {
            (*pxNewTimer).ucStatus |= tmrSTATUS_IS_AUTORELOAD;
        }
    }

    traceTIMER_CREATE!(pxNewTimer);
}

/// Ensure timer lists and queue are initialized
fn prvCheckForValidListAndQueue() {
    taskENTER_CRITICAL();

    unsafe {
        if xTimerQueue == ptr::null_mut() {
            vListInitialise(&mut xActiveTimerList1);
            vListInitialise(&mut xActiveTimerList2);
            pxCurrentTimerList = &mut xActiveTimerList1;
            pxOverflowTimerList = &mut xActiveTimerList2;

            // Create timer queue
            // [AMENDMENT] Using static queue if available
            xTimerQueue = xQueueCreate(
                configTIMER_QUEUE_LENGTH,
                core::mem::size_of::<DaemonTaskMessage_t>() as UBaseType_t,
            );

            // TODO: Queue registry support
        }
    }

    taskEXIT_CRITICAL();
}

/// Timer daemon task entry point
extern "C" fn prvTimerTask(pvParameters: *mut c_void) {
    let mut xNextExpireTime: TickType_t;
    let mut xListWasEmpty: BaseType_t = pdFALSE;

    // Unused parameter
    let _ = pvParameters;

    // Daemon startup hook could be called here
    // #[cfg(feature = "daemon-task-startup-hook")]
    // vApplicationDaemonTaskStartupHook();

    loop {
        // Get next timer expiry time
        xNextExpireTime = prvGetNextExpireTime(&mut xListWasEmpty);

        // Process timer or block
        prvProcessTimerOrBlockTask(xNextExpireTime, xListWasEmpty);

        // Process received commands
        prvProcessReceivedCommands();
    }
}

/// Get the next timer expiry time
fn prvGetNextExpireTime(pxListWasEmpty: *mut BaseType_t) -> TickType_t {
    let xNextExpireTime: TickType_t;

    unsafe {
        // Check if list is empty
        *pxListWasEmpty = listLIST_IS_EMPTY(pxCurrentTimerList);

        if *pxListWasEmpty == pdFALSE {
            xNextExpireTime = listGET_ITEM_VALUE_OF_HEAD_ENTRY(pxCurrentTimerList);
        } else {
            // Ensure task unblocks when tick count rolls over
            xNextExpireTime = 0;
        }
    }

    xNextExpireTime
}

/// Sample current time, checking for overflow
fn prvSampleTimeNow(pxTimerListsWereSwitched: *mut BaseType_t) -> TickType_t {
    let xTimeNow: TickType_t;

    xTimeNow = xTaskGetTickCount();

    unsafe {
        if xTimeNow < xLastTime {
            prvSwitchTimerLists();
            *pxTimerListsWereSwitched = pdTRUE;
        } else {
            *pxTimerListsWereSwitched = pdFALSE;
        }

        xLastTime = xTimeNow;
    }

    xTimeNow
}

/// Switch timer lists on tick counter overflow
fn prvSwitchTimerLists() {
    let mut xNextExpireTime: TickType_t;

    unsafe {
        // Process all timers in current list (they must have expired)
        while listLIST_IS_EMPTY(pxCurrentTimerList) == pdFALSE {
            xNextExpireTime = listGET_ITEM_VALUE_OF_HEAD_ENTRY(pxCurrentTimerList);

            // Process expired timer
            prvProcessExpiredTimer(xNextExpireTime, tmrMAX_TIME_BEFORE_OVERFLOW);
        }

        // Swap lists
        let pxTemp = pxCurrentTimerList;
        pxCurrentTimerList = pxOverflowTimerList;
        pxOverflowTimerList = pxTemp;
    }
}

/// Process a timer that has expired or block waiting
fn prvProcessTimerOrBlockTask(xNextExpireTime: TickType_t, xListWasEmpty: BaseType_t) {
    let mut xTimerListsWereSwitched: BaseType_t = pdFALSE;

    vTaskSuspendAll();

    unsafe {
        // Sample current time
        let xTimeNow = prvSampleTimeNow(&mut xTimerListsWereSwitched);

        if xTimerListsWereSwitched == pdFALSE {
            // Has the timer expired?
            if (xListWasEmpty == pdFALSE) && (xNextExpireTime <= xTimeNow) {
                xTaskResumeAll();
                prvProcessExpiredTimer(xNextExpireTime, xTimeNow);
            } else {
                // Block until timer expires or command received
                let mut xListWasEmptyNow = xListWasEmpty;
                if xListWasEmptyNow != pdFALSE {
                    // Check overflow list too
                    xListWasEmptyNow = listLIST_IS_EMPTY(pxOverflowTimerList);
                }

                vQueueWaitForMessageRestricted(
                    xTimerQueue,
                    xNextExpireTime.wrapping_sub(xTimeNow),
                    xListWasEmptyNow,
                );

                if xTaskResumeAll() == pdFALSE {
                    // Yield to allow higher priority task to run
                    portYIELD_WITHIN_API();
                }
            }
        } else {
            xTaskResumeAll();
        }
    }
}

/// Insert timer into the appropriate active list
fn prvInsertTimerInActiveList(
    pxTimer: *mut Timer_t,
    xNextExpiryTime: TickType_t,
    xTimeNow: TickType_t,
    xCommandTime: TickType_t,
) -> BaseType_t {
    let mut xProcessTimerNow: BaseType_t = pdFALSE;

    unsafe {
        listSET_LIST_ITEM_VALUE(&mut (*pxTimer).xTimerListItem, xNextExpiryTime);
        listSET_LIST_ITEM_OWNER(&mut (*pxTimer).xTimerListItem, pxTimer as *mut c_void);

        if xNextExpiryTime <= xTimeNow {
            // Has the expiry time already passed?
            if xTimeNow.wrapping_sub(xCommandTime) >= (*pxTimer).xTimerPeriodInTicks {
                // Timer has fully expired
                xProcessTimerNow = pdTRUE;
            } else {
                // Goes in overflow list
                vListInsert(pxOverflowTimerList, &mut (*pxTimer).xTimerListItem);
            }
        } else {
            if (xTimeNow < xCommandTime) && (xNextExpiryTime >= xCommandTime) {
                // Tick count overflowed since command but expiry hasn't
                xProcessTimerNow = pdTRUE;
            } else {
                // Goes in current list
                vListInsert(pxCurrentTimerList, &mut (*pxTimer).xTimerListItem);
            }
        }
    }

    xProcessTimerNow
}

/// Reload an auto-reload timer, calling callback for any backlogged expiries
fn prvReloadTimer(pxTimer: *mut Timer_t, mut xExpiredTime: TickType_t, xTimeNow: TickType_t) {
    unsafe {
        // Keep reloading until next expiry is in the future
        while prvInsertTimerInActiveList(
            pxTimer,
            xExpiredTime.wrapping_add((*pxTimer).xTimerPeriodInTicks),
            xTimeNow,
            xExpiredTime,
        ) != pdFALSE
        {
            // Advance expiry time
            xExpiredTime = xExpiredTime.wrapping_add((*pxTimer).xTimerPeriodInTicks);

            // Call callback
            traceTIMER_EXPIRED!(pxTimer);
            ((*pxTimer).pxCallbackFunction)(pxTimer as TimerHandle_t);
        }
    }
}

/// Process an expired timer
fn prvProcessExpiredTimer(xNextExpireTime: TickType_t, xTimeNow: TickType_t) {
    unsafe {
        // Get the timer at the head of the list
        let pxTimer = listGET_OWNER_OF_HEAD_ENTRY(pxCurrentTimerList) as *mut Timer_t;

        // Remove from active list
        uxListRemove(&mut (*pxTimer).xTimerListItem);

        // Handle auto-reload or one-shot
        if ((*pxTimer).ucStatus & tmrSTATUS_IS_AUTORELOAD) != 0 {
            prvReloadTimer(pxTimer, xNextExpireTime, xTimeNow);
        } else {
            (*pxTimer).ucStatus &= !tmrSTATUS_IS_ACTIVE;
        }

        // Call the callback
        traceTIMER_EXPIRED!(pxTimer);
        ((*pxTimer).pxCallbackFunction)(pxTimer as TimerHandle_t);
    }
}

/// Process commands received on the timer queue
fn prvProcessReceivedCommands() {
    let mut xMessage: DaemonTaskMessage_t = unsafe { core::mem::zeroed() };
    let mut pxTimer: *mut Timer_t;
    let mut xTimerListsWereSwitched: BaseType_t = pdFALSE;

    unsafe {
        while xQueueReceive(
            xTimerQueue,
            &mut xMessage as *mut _ as *mut c_void,
            tmrNO_DELAY,
        ) != pdFAIL
        {
            // Check for pended function callbacks (negative command IDs)
            #[cfg(feature = "pend-function-call")]
            {
                if xMessage.xMessageID < 0 {
                    let pxCallback = &xMessage.u.xCallbackParameters;
                    // Call the pended function
                    (pxCallback.pxCallbackFunction)(
                        pxCallback.pvParameter1,
                        pxCallback.ulParameter2,
                    );
                }
            }

            // Timer commands have non-negative IDs
            if xMessage.xMessageID >= 0 {
                pxTimer = xMessage.u.xTimerParameters.pxTimer;

                if pxTimer != ptr::null_mut() {
                    // Remove timer from any list it might be in
                    if listIS_CONTAINED_WITHIN(ptr::null_mut(), &mut (*pxTimer).xTimerListItem) == pdFALSE {
                        uxListRemove(&mut (*pxTimer).xTimerListItem);
                    }

                    traceTIMER_COMMAND_RECEIVED!(pxTimer, xMessage.xMessageID, xMessage.u.xTimerParameters.xMessageValue);

                    // Sample time now
                    let xTimeNow = prvSampleTimeNow(&mut xTimerListsWereSwitched);

                    match xMessage.xMessageID {
                        x if x == tmrCOMMAND_START
                            || x == tmrCOMMAND_START_FROM_ISR
                            || x == tmrCOMMAND_RESET
                            || x == tmrCOMMAND_RESET_FROM_ISR =>
                        {
                            // Start or reset timer
                            (*pxTimer).ucStatus |= tmrSTATUS_IS_ACTIVE;

                            let xNextExpiryTime = xMessage
                                .u
                                .xTimerParameters
                                .xMessageValue
                                .wrapping_add((*pxTimer).xTimerPeriodInTicks);

                            if prvInsertTimerInActiveList(
                                pxTimer,
                                xNextExpiryTime,
                                xTimeNow,
                                xMessage.u.xTimerParameters.xMessageValue,
                            ) != pdFALSE
                            {
                                // Timer already expired
                                if ((*pxTimer).ucStatus & tmrSTATUS_IS_AUTORELOAD) != 0 {
                                    prvReloadTimer(
                                        pxTimer,
                                        xNextExpiryTime,
                                        xTimeNow,
                                    );
                                } else {
                                    (*pxTimer).ucStatus &= !tmrSTATUS_IS_ACTIVE;
                                }

                                // Call callback
                                traceTIMER_EXPIRED!(pxTimer);
                                ((*pxTimer).pxCallbackFunction)(pxTimer as TimerHandle_t);
                            }
                        }

                        x if x == tmrCOMMAND_STOP || x == tmrCOMMAND_STOP_FROM_ISR => {
                            // Stop timer (already removed from list above)
                            (*pxTimer).ucStatus &= !tmrSTATUS_IS_ACTIVE;
                        }

                        x if x == tmrCOMMAND_CHANGE_PERIOD
                            || x == tmrCOMMAND_CHANGE_PERIOD_FROM_ISR =>
                        {
                            // Change period
                            (*pxTimer).ucStatus |= tmrSTATUS_IS_ACTIVE;
                            (*pxTimer).xTimerPeriodInTicks =
                                xMessage.u.xTimerParameters.xMessageValue;
                            configASSERT((*pxTimer).xTimerPeriodInTicks > 0);

                            // Insert with new period
                            prvInsertTimerInActiveList(
                                pxTimer,
                                xTimeNow.wrapping_add((*pxTimer).xTimerPeriodInTicks),
                                xTimeNow,
                                xTimeNow,
                            );
                        }

                        x if x == tmrCOMMAND_DELETE => {
                            // Delete timer
                            #[cfg(feature = "alloc")]
                            {
                                if ((*pxTimer).ucStatus & tmrSTATUS_IS_STATICALLY_ALLOCATED) == 0 {
                                    vPortFree(pxTimer as *mut c_void);
                                } else {
                                    (*pxTimer).ucStatus &= !tmrSTATUS_IS_ACTIVE;
                                }
                            }
                            #[cfg(not(feature = "alloc"))]
                            {
                                (*pxTimer).ucStatus &= !tmrSTATUS_IS_ACTIVE;
                            }
                        }

                        _ => {
                            // Unknown command - should not happen
                        }
                    }
                }
            }
        }
    }
}

// =============================================================================
// Convenience Macros (inline functions)
// =============================================================================

/// Start a timer
#[inline(always)]
pub fn xTimerStart(xTimer: TimerHandle_t, xTicksToWait: TickType_t) -> BaseType_t {
    xTimerGenericCommand(
        xTimer,
        tmrCOMMAND_START,
        xTaskGetTickCount(),
        ptr::null_mut(),
        xTicksToWait,
    )
}

/// Stop a timer
#[inline(always)]
pub fn xTimerStop(xTimer: TimerHandle_t, xTicksToWait: TickType_t) -> BaseType_t {
    xTimerGenericCommand(xTimer, tmrCOMMAND_STOP, 0, ptr::null_mut(), xTicksToWait)
}

/// Change timer period
#[inline(always)]
pub fn xTimerChangePeriod(
    xTimer: TimerHandle_t,
    xNewPeriod: TickType_t,
    xTicksToWait: TickType_t,
) -> BaseType_t {
    xTimerGenericCommand(
        xTimer,
        tmrCOMMAND_CHANGE_PERIOD,
        xNewPeriod,
        ptr::null_mut(),
        xTicksToWait,
    )
}

/// Delete a timer
#[inline(always)]
pub fn xTimerDelete(xTimer: TimerHandle_t, xTicksToWait: TickType_t) -> BaseType_t {
    xTimerGenericCommand(xTimer, tmrCOMMAND_DELETE, 0, ptr::null_mut(), xTicksToWait)
}

/// Reset a timer
#[inline(always)]
pub fn xTimerReset(xTimer: TimerHandle_t, xTicksToWait: TickType_t) -> BaseType_t {
    xTimerGenericCommand(
        xTimer,
        tmrCOMMAND_RESET,
        xTaskGetTickCount(),
        ptr::null_mut(),
        xTicksToWait,
    )
}

/// Start a timer from ISR
#[inline(always)]
pub fn xTimerStartFromISR(
    xTimer: TimerHandle_t,
    pxHigherPriorityTaskWoken: *mut BaseType_t,
) -> BaseType_t {
    xTimerGenericCommand(
        xTimer,
        tmrCOMMAND_START_FROM_ISR,
        xTaskGetTickCountFromISR(),
        pxHigherPriorityTaskWoken,
        0,
    )
}

/// Stop a timer from ISR
#[inline(always)]
pub fn xTimerStopFromISR(
    xTimer: TimerHandle_t,
    pxHigherPriorityTaskWoken: *mut BaseType_t,
) -> BaseType_t {
    xTimerGenericCommand(
        xTimer,
        tmrCOMMAND_STOP_FROM_ISR,
        0,
        pxHigherPriorityTaskWoken,
        0,
    )
}

/// Change timer period from ISR
#[inline(always)]
pub fn xTimerChangePeriodFromISR(
    xTimer: TimerHandle_t,
    xNewPeriod: TickType_t,
    pxHigherPriorityTaskWoken: *mut BaseType_t,
) -> BaseType_t {
    xTimerGenericCommand(
        xTimer,
        tmrCOMMAND_CHANGE_PERIOD_FROM_ISR,
        xNewPeriod,
        pxHigherPriorityTaskWoken,
        0,
    )
}

/// Reset a timer from ISR
#[inline(always)]
pub fn xTimerResetFromISR(
    xTimer: TimerHandle_t,
    pxHigherPriorityTaskWoken: *mut BaseType_t,
) -> BaseType_t {
    xTimerGenericCommand(
        xTimer,
        tmrCOMMAND_RESET_FROM_ISR,
        xTaskGetTickCountFromISR(),
        pxHigherPriorityTaskWoken,
        0,
    )
}
