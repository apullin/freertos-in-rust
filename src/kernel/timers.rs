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
#[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
use crate::memory::{pvPortMalloc, vPortFree};
use crate::types::*;

// Import trace macros (exported at crate root via #[macro_export])
use crate::{
    traceENTER_pcTimerGetName, traceENTER_pvTimerGetTimerID, traceENTER_uxTimerGetReloadMode,
    traceENTER_vTimerSetReloadMode, traceENTER_vTimerSetTimerID, traceENTER_xTimerCreateStatic,
    traceENTER_xTimerCreateTimerTask, traceENTER_xTimerGenericCommandFromISR,
    traceENTER_xTimerGenericCommandFromTask, traceENTER_xTimerGetExpiryTime,
    traceENTER_xTimerGetPeriod, traceENTER_xTimerGetReloadMode, traceENTER_xTimerGetStaticBuffer,
    traceENTER_xTimerGetTimerDaemonTaskHandle, traceENTER_xTimerIsTimerActive,
    traceRETURN_pcTimerGetName, traceRETURN_pvTimerGetTimerID, traceRETURN_uxTimerGetReloadMode,
    traceRETURN_vTimerSetReloadMode, traceRETURN_vTimerSetTimerID, traceRETURN_xTimerCreateStatic,
    traceRETURN_xTimerCreateTimerTask, traceRETURN_xTimerGenericCommandFromISR,
    traceRETURN_xTimerGenericCommandFromTask, traceRETURN_xTimerGetExpiryTime,
    traceRETURN_xTimerGetPeriod, traceRETURN_xTimerGetReloadMode,
    traceRETURN_xTimerGetStaticBuffer, traceRETURN_xTimerGetTimerDaemonTaskHandle,
    traceRETURN_xTimerIsTimerActive, traceTIMER_COMMAND_RECEIVED, traceTIMER_COMMAND_SEND,
    traceTIMER_CREATE, traceTIMER_EXPIRED,
};

#[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
use crate::{traceENTER_xTimerCreate, traceRETURN_xTimerCreate};

#[cfg(feature = "trace-facility")]
use crate::{
    traceENTER_uxTimerGetTimerNumber, traceENTER_vTimerSetTimerNumber,
    traceRETURN_uxTimerGetTimerNumber, traceRETURN_vTimerSetTimerNumber,
};

#[cfg(feature = "pend-function-call")]
use crate::{
    traceENTER_xTimerPendFunctionCall, traceENTER_xTimerPendFunctionCallFromISR,
    tracePEND_FUNC_CALL, tracePEND_FUNC_CALL_FROM_ISR, traceRETURN_xTimerPendFunctionCall,
    traceRETURN_xTimerPendFunctionCallFromISR,
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

/// Timer callback function prototype.
///
/// The daemon supplies a valid, live handle for the duration of the callback.
/// The callback is unsafe to invoke directly because arbitrary callers cannot
/// establish that invariant.
pub type TimerCallbackFunction_t = unsafe extern "C" fn(TimerHandle_t);

/// Pended function callback prototype
/// For xTimerPendFunctionCall
///
// [AMENDMENT] Upstream stores the second parameter as `uint32_t`, which
// truncates 64-bit event-group masks. Widen it with the tick configuration so
// all advertised event bits survive a deferred FromISR call.
#[cfg(feature = "tick-64bit")]
pub type PendedFunctionParameter_t = u64;
#[cfg(not(feature = "tick-64bit"))]
pub type PendedFunctionParameter_t = u32;
/// The callback may carry safety preconditions for its opaque pointer. The
/// timer daemon therefore invokes it through an unsafe function pointer rather
/// than pretending that arbitrary deferred data is always valid.
pub type PendedFunction_t = unsafe extern "C" fn(*mut c_void, PendedFunctionParameter_t);

const _: () = assert!(
    core::mem::size_of::<PendedFunctionParameter_t>() >= core::mem::size_of::<TickType_t>()
);

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
    pub ulParameter2: PendedFunctionParameter_t,
}

/// Union for timer message payload
#[repr(C)]
#[derive(Clone, Copy)]
pub union DaemonTaskMessageUnion {
    /// Timer command parameters
    pub xTimerParameters: TimerParameter_t,
    /// Callback parameters (for pended functions)
    #[cfg(feature = "pend-function-call")]
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

/// Opaque static buffer for timer allocation.
#[repr(transparent)]
pub struct StaticTimer_t {
    // [AMENDMENT] `MaybeUninit<Timer_t>` preserves both size and alignment
    // without requiring callers to manufacture a valid internal timer.
    _storage: core::mem::MaybeUninit<Timer_t>,
}

impl StaticTimer_t {
    pub const fn new() -> Self {
        Self {
            _storage: core::mem::MaybeUninit::uninit(),
        }
    }
}

const _: () = assert!(core::mem::size_of::<StaticTimer_t>() == core::mem::size_of::<Timer_t>());
const _: () = assert!(core::mem::align_of::<StaticTimer_t>() == core::mem::align_of::<Timer_t>());

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

/// Application-provided storage for a statically allocated timer daemon task.
static mut pxTimerTaskTCBBuffer: *mut StaticTask_t = ptr::null_mut();
static mut pxTimerTaskStackBuffer: *mut StackType_t = ptr::null_mut();
static mut uxTimerTaskStackSize: configSTACK_DEPTH_TYPE = 0;

/// Static timer command queue, matching upstream when static allocation is
/// supported.
static mut xStaticTimerQueue: StaticQueue_t = StaticQueue_t::new();
const TIMER_QUEUE_STORAGE_SIZE: usize =
    configTIMER_QUEUE_LENGTH as usize * core::mem::size_of::<DaemonTaskMessage_t>();
static mut ucStaticTimerQueueStorage: [u8; TIMER_QUEUE_STORAGE_SIZE] =
    [0; TIMER_QUEUE_STORAGE_SIZE];

/// Last sampled time (for overflow detection)
static mut xLastTime: TickType_t = 0;

/// Register storage used to create the timer daemon without an allocator.
///
/// This is the Rust equivalent of supplying
/// `vApplicationGetTimerTaskMemory()`. It must be called before the scheduler
/// is started and the storage must remain valid for the scheduler's lifetime.
///
/// # Safety
///
/// The TCB and stack must be uniquely owned, correctly aligned, and the stack
/// must contain at least `uxStackSize` words.
pub unsafe fn vTimerSetTimerTaskMemory(
    pxTaskTCBBuffer: *mut StaticTask_t,
    pxTaskStackBuffer: *mut StackType_t,
    uxStackSize: configSTACK_DEPTH_TYPE,
) -> BaseType_t {
    if pxTaskTCBBuffer.is_null() || pxTaskStackBuffer.is_null() || uxStackSize == 0 {
        return pdFAIL;
    }

    taskENTER_CRITICAL();
    let xReturn = if xTimerTaskHandle.is_null() {
        pxTimerTaskTCBBuffer = pxTaskTCBBuffer;
        pxTimerTaskStackBuffer = pxTaskStackBuffer;
        uxTimerTaskStackSize = uxStackSize;
        pdPASS
    } else {
        pdFAIL
    };
    taskEXIT_CRITICAL();

    xReturn
}

// =============================================================================
// Public API: Timer Creation
// =============================================================================

/// Create the timer service task.
///
/// Called by the scheduler when configUSE_TIMERS == 1.
/// Creates the timer queue and daemon task.
///
/// # Returns
/// `pdPASS` if successful, `pdFAIL` otherwise
///
/// # Safety
///
/// Call only from the scheduler's exclusive pre-start initialization path,
/// after any static timer-task memory has been registered. No timer daemon may
/// already exist and no task, interrupt, scheduler restart, or timer API may
/// concurrently mutate the task lists or timer module state.
pub unsafe fn xTimerCreateTimerTask() -> BaseType_t {
    let mut xReturn: BaseType_t = pdFAIL;

    traceENTER_xTimerCreateTimerTask!();

    // Ensure timer infrastructure is created
    prvCheckForValidListAndQueue();

    unsafe {
        if !xTimerQueue.is_null() {
            if !pxTimerTaskTCBBuffer.is_null()
                && !pxTimerTaskStackBuffer.is_null()
                && uxTimerTaskStackSize > 0
            {
                xTimerTaskHandle = xTaskCreateStatic(
                    prvTimerTask,
                    configTIMER_SERVICE_TASK_NAME.as_ptr(),
                    uxTimerTaskStackSize,
                    ptr::null_mut(),
                    configTIMER_TASK_PRIORITY,
                    pxTimerTaskStackBuffer,
                    pxTimerTaskTCBBuffer,
                );

                if !xTimerTaskHandle.is_null() {
                    xReturn = pdPASS;
                }
            } else {
                #[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
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

                #[cfg(not(any(feature = "alloc", feature = "heap-4", feature = "heap-5")))]
                {
                    xReturn = pdFAIL;
                }
            }
        }
    }

    traceRETURN_xTimerCreateTimerTask!(xReturn);

    xReturn
}

/// Create a new software timer (dynamic allocation).
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
///
/// # Safety
///
/// If non-null, `pcTimerName` must identify a NUL-terminated byte string that
/// remains readable until the timer has been deleted and all queued timer
/// commands have completed. `pvTimerID` is opaque to the kernel, but any data
/// the callback derives from it must remain valid for every possible callback.
/// The callback must obey timer-daemon context restrictions.
#[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
pub unsafe fn xTimerCreate(
    pcTimerName: *const u8,
    xTimerPeriodInTicks: TickType_t,
    xAutoReload: BaseType_t,
    pvTimerID: *mut c_void,
    pxCallbackFunction: TimerCallbackFunction_t,
) -> TimerHandle_t {
    traceENTER_xTimerCreate!(
        pcTimerName,
        xTimerPeriodInTicks,
        xAutoReload,
        pvTimerID,
        pxCallbackFunction
    );

    if xTimerPeriodInTicks == 0 {
        traceRETURN_xTimerCreate!(ptr::null_mut::<Timer_t>());
        return ptr::null_mut();
    }

    let pxNewTimer: *mut Timer_t =
        unsafe { pvPortMalloc(core::mem::size_of::<Timer_t>()) as *mut Timer_t };

    if pxNewTimer != ptr::null_mut() {
        prvInitialiseNewTimer(
            pcTimerName,
            xTimerPeriodInTicks,
            xAutoReload,
            pvTimerID,
            pxCallbackFunction,
            pxNewTimer,
            0,
        );
    }

    traceRETURN_xTimerCreate!(pxNewTimer);

    pxNewTimer as TimerHandle_t
}

/// Create a new software timer (static allocation).
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
///
/// # Safety
///
/// `pxTimerBuffer` must be non-null, properly aligned, uniquely writable, and
/// remain at a stable address until deletion has been processed by the timer
/// daemon and no queued command can refer to it. If non-null, `pcTimerName`
/// must be a NUL-terminated byte string readable for the same lifetime.
/// `pvTimerID` is opaque to the kernel, but callback-owned data reached through
/// it must remain valid for every callback invocation.
pub unsafe fn xTimerCreateStatic(
    pcTimerName: *const u8,
    xTimerPeriodInTicks: TickType_t,
    xAutoReload: BaseType_t,
    pvTimerID: *mut c_void,
    pxCallbackFunction: TimerCallbackFunction_t,
    pxTimerBuffer: *mut StaticTimer_t,
) -> TimerHandle_t {
    traceENTER_xTimerCreateStatic!(
        pcTimerName,
        xTimerPeriodInTicks,
        xAutoReload,
        pvTimerID,
        pxCallbackFunction,
        pxTimerBuffer
    );

    if pxTimerBuffer.is_null() || xTimerPeriodInTicks == 0 {
        traceRETURN_xTimerCreateStatic!(ptr::null_mut::<Timer_t>());
        return ptr::null_mut();
    }

    // Verify StaticTimer_t is same size as Timer_t
    #[cfg(debug_assertions)]
    {
        let static_size = core::mem::size_of::<StaticTimer_t>();
        let timer_size = core::mem::size_of::<Timer_t>();
        configASSERT(static_size == timer_size);
    }

    let pxNewTimer: *mut Timer_t = pxTimerBuffer as *mut Timer_t;

    if pxNewTimer != ptr::null_mut() {
        prvInitialiseNewTimer(
            pcTimerName,
            xTimerPeriodInTicks,
            xAutoReload,
            pvTimerID,
            pxCallbackFunction,
            pxNewTimer,
            tmrSTATUS_IS_STATICALLY_ALLOCATED,
        );
    }

    traceRETURN_xTimerCreateStatic!(pxNewTimer);

    pxNewTimer as TimerHandle_t
}

// =============================================================================
// Public API: Timer Control
// =============================================================================

/// Send a command to the timer daemon from a task.
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
///
/// # Safety
///
/// `xTimer` must identify a live timer until the daemon consumes this command.
/// On a successful delete command, no caller may subsequently use the handle.
/// This task-context API must not be called from an interrupt handler. Unknown
/// command IDs and a zero change-period value are rejected with `pdFAIL`.
pub unsafe fn xTimerGenericCommandFromTask(
    xTimer: TimerHandle_t,
    xCommandID: BaseType_t,
    xOptionalValue: TickType_t,
    pxHigherPriorityTaskWoken: *mut BaseType_t,
    xTicksToWait: TickType_t,
) -> BaseType_t {
    let mut xReturn: BaseType_t = pdFAIL;

    traceENTER_xTimerGenericCommandFromTask!(
        xTimer,
        xCommandID,
        xOptionalValue,
        pxHigherPriorityTaskWoken,
        xTicksToWait
    );

    unsafe {
        let xCommandIsValid = xCommandID == tmrCOMMAND_START_DONT_TRACE
            || xCommandID == tmrCOMMAND_START
            || xCommandID == tmrCOMMAND_RESET
            || xCommandID == tmrCOMMAND_STOP
            || xCommandID == tmrCOMMAND_CHANGE_PERIOD
            || xCommandID == tmrCOMMAND_DELETE;

        if xTimerQueue != ptr::null_mut() && xTimer != ptr::null_mut() && xCommandIsValid {
            let mut xMessage: DaemonTaskMessage_t = core::mem::zeroed();
            xMessage.xMessageID = xCommandID;
            xMessage.u.xTimerParameters.xMessageValue = xOptionalValue;
            xMessage.u.xTimerParameters.pxTimer = xTimer as *mut Timer_t;

            let xPeriodIsValid = xCommandID != tmrCOMMAND_CHANGE_PERIOD || xOptionalValue != 0;

            if xPeriodIsValid {
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

/// Send a command to the timer daemon from an ISR.
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
///
/// # Safety
///
/// `xTimer` must identify a live timer until the daemon consumes this command.
/// If non-null, `pxHigherPriorityTaskWoken` must be aligned and writable for
/// this call. The port's FromISR priority restrictions must be satisfied.
/// Unknown command IDs and a zero change-period value return `pdFAIL`.
pub unsafe fn xTimerGenericCommandFromISR(
    xTimer: TimerHandle_t,
    xCommandID: BaseType_t,
    xOptionalValue: TickType_t,
    pxHigherPriorityTaskWoken: *mut BaseType_t,
    _xTicksToWait: TickType_t,
) -> BaseType_t {
    let mut xReturn: BaseType_t = pdFAIL;

    traceENTER_xTimerGenericCommandFromISR!(
        xTimer,
        xCommandID,
        xOptionalValue,
        pxHigherPriorityTaskWoken,
        _xTicksToWait
    );

    unsafe {
        let xCommandIsValid = xCommandID == tmrCOMMAND_START_FROM_ISR
            || xCommandID == tmrCOMMAND_RESET_FROM_ISR
            || xCommandID == tmrCOMMAND_STOP_FROM_ISR
            || xCommandID == tmrCOMMAND_CHANGE_PERIOD_FROM_ISR;

        if xTimerQueue != ptr::null_mut() && xTimer != ptr::null_mut() && xCommandIsValid {
            let mut xMessage: DaemonTaskMessage_t = core::mem::zeroed();
            xMessage.xMessageID = xCommandID;
            xMessage.u.xTimerParameters.xMessageValue = xOptionalValue;
            xMessage.u.xTimerParameters.pxTimer = xTimer as *mut Timer_t;

            let xPeriodIsValid =
                xCommandID != tmrCOMMAND_CHANGE_PERIOD_FROM_ISR || xOptionalValue != 0;

            if xPeriodIsValid {
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

/// Generic timer command (dispatches by command ID).
///
/// # Safety
///
/// `xTimer` must identify a live timer until the daemon consumes the command.
/// The caller's task or interrupt context must match the command-ID family. If
/// non-null and a FromISR command is used,
/// `pxHigherPriorityTaskWoken` must be aligned and writable for the call.
/// Unknown command IDs are rejected with `pdFAIL`.
#[inline(always)]
pub unsafe fn xTimerGenericCommand(
    xTimer: TimerHandle_t,
    xCommandID: BaseType_t,
    xOptionalValue: TickType_t,
    pxHigherPriorityTaskWoken: *mut BaseType_t,
    xTicksToWait: TickType_t,
) -> BaseType_t {
    if xCommandID < tmrFIRST_FROM_ISR_COMMAND {
        xTimerGenericCommandFromTask(
            xTimer,
            xCommandID,
            xOptionalValue,
            pxHigherPriorityTaskWoken,
            xTicksToWait,
        )
    } else {
        xTimerGenericCommandFromISR(
            xTimer,
            xCommandID,
            xOptionalValue,
            pxHigherPriorityTaskWoken,
            xTicksToWait,
        )
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

/// Get the timer period in ticks.
///
/// # Safety
///
/// `xTimer` must identify a live timer through the call and must not be deleted
/// concurrently.
pub unsafe fn xTimerGetPeriod(xTimer: TimerHandle_t) -> TickType_t {
    let pxTimer = xTimer as *mut Timer_t;

    traceENTER_xTimerGetPeriod!(xTimer);

    configASSERT(xTimer != ptr::null_mut());

    let xReturn = unsafe { (*pxTimer).xTimerPeriodInTicks };

    traceRETURN_xTimerGetPeriod!(xReturn);

    xReturn
}

/// Set timer reload mode.
///
/// # Safety
///
/// `xTimer` must identify a live timer through the call and must not be deleted
/// concurrently. This task-context API must not be called from an interrupt.
pub unsafe fn vTimerSetReloadMode(xTimer: TimerHandle_t, xAutoReload: BaseType_t) {
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

/// Get timer reload mode.
///
/// # Safety
///
/// `xTimer` must identify a live timer through the call and must not be deleted
/// concurrently. This task-context API must not be called from an interrupt.
pub unsafe fn xTimerGetReloadMode(xTimer: TimerHandle_t) -> BaseType_t {
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

/// Get timer reload mode (`UBaseType_t` return variant).
///
/// # Safety
///
/// `xTimer` must identify a live timer through the call and must not be deleted
/// concurrently. This task-context API must not be called from an interrupt.
pub unsafe fn uxTimerGetReloadMode(xTimer: TimerHandle_t) -> UBaseType_t {
    traceENTER_uxTimerGetReloadMode!(xTimer);

    let uxReturn = xTimerGetReloadMode(xTimer) as UBaseType_t;

    traceRETURN_uxTimerGetReloadMode!(uxReturn);

    uxReturn
}

/// Get the timer expiry time.
///
/// # Safety
///
/// `xTimer` must identify a live timer through the call and must not be deleted
/// concurrently.
pub unsafe fn xTimerGetExpiryTime(xTimer: TimerHandle_t) -> TickType_t {
    let pxTimer = xTimer as *mut Timer_t;

    traceENTER_xTimerGetExpiryTime!(xTimer);

    configASSERT(xTimer != ptr::null_mut());

    let xReturn = unsafe { listGET_LIST_ITEM_VALUE(ptr::addr_of!((*pxTimer).xTimerListItem)) };

    traceRETURN_xTimerGetExpiryTime!(xReturn);

    xReturn
}

/// Get the stored timer-name pointer.
///
/// # Safety
///
/// `xTimer` must identify a live timer through the call. The returned pointer
/// is only as valid as the name storage promised to `xTimerCreate` or
/// `xTimerCreateStatic`; the kernel does not copy or validate the string.
pub unsafe fn pcTimerGetName(xTimer: TimerHandle_t) -> *const u8 {
    let pxTimer = xTimer as *mut Timer_t;

    traceENTER_pcTimerGetName!(xTimer);

    configASSERT(xTimer != ptr::null_mut());

    let pcName = unsafe { (*pxTimer).pcTimerName };

    traceRETURN_pcTimerGetName!(pcName);

    pcName
}

/// Check if a timer is active.
///
/// # Safety
///
/// `xTimer` must identify a live timer through the call and must not be deleted
/// concurrently. This task-context API must not be called from an interrupt.
pub unsafe fn xTimerIsTimerActive(xTimer: TimerHandle_t) -> BaseType_t {
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

/// Get the opaque timer ID.
///
/// # Safety
///
/// `xTimer` must identify a live timer through the call and must not be deleted
/// concurrently. The returned pointer is not validated or dereferenced by the
/// kernel.
pub unsafe fn pvTimerGetTimerID(xTimer: TimerHandle_t) -> *mut c_void {
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

/// Set the opaque timer ID.
///
/// # Safety
///
/// `xTimer` must identify a live timer through the call and must not be deleted
/// concurrently. The kernel stores but does not validate `pvNewID`; any future
/// callback that dereferences it requires the pointed-to data to remain valid.
pub unsafe fn vTimerSetTimerID(xTimer: TimerHandle_t, pvNewID: *mut c_void) {
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

/// Get static storage from a statically allocated timer.
///
/// # Safety
///
/// `xTimer` must identify a live timer through the call. `ppxTimerBuffer` must
/// be non-null, aligned, and valid for one pointer write. The returned pointer
/// must not be used to move, alias mutably, or overwrite the live timer.
pub unsafe fn xTimerGetStaticBuffer(
    xTimer: TimerHandle_t,
    ppxTimerBuffer: *mut *mut StaticTimer_t,
) -> BaseType_t {
    let pxTimer = xTimer as *mut Timer_t;
    let xReturn: BaseType_t;

    traceENTER_xTimerGetStaticBuffer!(xTimer, ppxTimerBuffer);

    configASSERT(xTimer != ptr::null_mut());
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

/// Reset timer module state for a scheduler restart.
///
/// # Safety
///
/// The scheduler and timer daemon must be stopped, no timer API call may be in
/// progress, and no queued command or pended callback may remain. Existing
/// timer handles must not subsequently be used with the reset module state.
pub unsafe fn vTimerResetState() {
    unsafe {
        xTimerQueue = ptr::null_mut();
        xTimerTaskHandle = ptr::null_mut();
    }
}

// =============================================================================
// Trace Facility Functions
// =============================================================================

/// Get timer number (for trace facility).
///
/// # Safety
///
/// `xTimer` must identify a live timer through the call and must not be deleted
/// concurrently.
#[cfg(feature = "trace-facility")]
pub unsafe fn uxTimerGetTimerNumber(xTimer: TimerHandle_t) -> UBaseType_t {
    traceENTER_uxTimerGetTimerNumber!(xTimer);

    let uxReturn = unsafe { (*(xTimer as *mut Timer_t)).uxTimerNumber };

    traceRETURN_uxTimerGetTimerNumber!(uxReturn);

    uxReturn
}

/// Set timer number (for trace facility).
///
/// # Safety
///
/// `xTimer` must identify a live timer through the call and the caller must
/// serialize this write with other trace-number accesses.
#[cfg(feature = "trace-facility")]
pub unsafe fn vTimerSetTimerNumber(xTimer: TimerHandle_t, uxTimerNumber: UBaseType_t) {
    traceENTER_vTimerSetTimerNumber!(xTimer, uxTimerNumber);

    unsafe {
        (*(xTimer as *mut Timer_t)).uxTimerNumber = uxTimerNumber;
    }

    traceRETURN_vTimerSetTimerNumber!();
}

// =============================================================================
// Pended Function Calls
// =============================================================================

/// Pend a function call from an ISR.
///
/// Allows ISR to defer processing to the timer daemon task.
///
/// # Safety
///
/// The caller must uphold `xFunctionToPend`'s safety requirements when the
/// timer daemon eventually invokes it. In particular, data reachable through
/// `pvParameter1` must remain valid until then. If non-null,
/// `pxHigherPriorityTaskWoken` must be aligned and writable for this call. The
/// port's FromISR priority restrictions must be satisfied.
#[cfg(feature = "pend-function-call")]
pub unsafe fn xTimerPendFunctionCallFromISR(
    xFunctionToPend: PendedFunction_t,
    pvParameter1: *mut c_void,
    ulParameter2: PendedFunctionParameter_t,
    pxHigherPriorityTaskWoken: *mut BaseType_t,
) -> BaseType_t {
    traceENTER_xTimerPendFunctionCallFromISR!(
        xFunctionToPend,
        pvParameter1,
        ulParameter2,
        pxHigherPriorityTaskWoken
    );

    if unsafe { xTimerQueue.is_null() } {
        traceRETURN_xTimerPendFunctionCallFromISR!(pdFAIL);
        return pdFAIL;
    }

    let mut xMessage: DaemonTaskMessage_t = unsafe { core::mem::zeroed() };
    xMessage.xMessageID = tmrCOMMAND_EXECUTE_CALLBACK_FROM_ISR;
    xMessage.u.xCallbackParameters.pxCallbackFunction = xFunctionToPend;
    xMessage.u.xCallbackParameters.pvParameter1 = pvParameter1;
    xMessage.u.xCallbackParameters.ulParameter2 = ulParameter2;

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

/// Pend a function call from a task.
///
/// # Safety
///
/// The caller must uphold `xFunctionToPend`'s safety requirements when the
/// timer daemon eventually invokes it. In particular, data reachable through
/// `pvParameter1` must remain valid until then. This task-context API must not
/// be called from an interrupt handler.
#[cfg(feature = "pend-function-call")]
pub unsafe fn xTimerPendFunctionCall(
    xFunctionToPend: PendedFunction_t,
    pvParameter1: *mut c_void,
    ulParameter2: PendedFunctionParameter_t,
    xTicksToWait: TickType_t,
) -> BaseType_t {
    traceENTER_xTimerPendFunctionCall!(xFunctionToPend, pvParameter1, ulParameter2, xTicksToWait);

    unsafe {
        configASSERT(xTimerQueue != ptr::null_mut());
        if xTimerQueue.is_null() {
            traceRETURN_xTimerPendFunctionCall!(pdFAIL);
            return pdFAIL;
        }
    }

    let mut xMessage: DaemonTaskMessage_t = unsafe { core::mem::zeroed() };
    xMessage.xMessageID = tmrCOMMAND_EXECUTE_CALLBACK;
    xMessage.u.xCallbackParameters.pxCallbackFunction = xFunctionToPend;
    xMessage.u.xCallbackParameters.pvParameter1 = pvParameter1;
    xMessage.u.xCallbackParameters.ulParameter2 = ulParameter2;

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
    mut ucStatus: u8,
) {
    // Period must be > 0
    configASSERT(xTimerPeriodInTicks > 0);

    // Ensure infrastructure is initialized
    prvCheckForValidListAndQueue();

    if xAutoReload != pdFALSE {
        ucStatus |= tmrSTATUS_IS_AUTORELOAD;
    }

    /* [AMENDMENT] Construct the complete Rust value in one write. Allocator
     * and `MaybeUninit` storage do not initially contain a valid `Timer_t`, so
     * taking references to individual uninitialised fields would be invalid. */
    unsafe {
        ptr::write(
            pxNewTimer,
            Timer_t {
                pcTimerName,
                xTimerListItem: ListItem_t::new(),
                xTimerPeriodInTicks,
                pvTimerID,
                pxCallbackFunction,
                #[cfg(feature = "trace-facility")]
                uxTimerNumber: 0,
                ucStatus,
            },
        );
        vListInitialiseItem(ptr::addr_of_mut!((*pxNewTimer).xTimerListItem));
    }

    traceTIMER_CREATE!(pxNewTimer);
}

/// Ensure timer lists and queue are initialized
fn prvCheckForValidListAndQueue() {
    taskENTER_CRITICAL();

    unsafe {
        if xTimerQueue == ptr::null_mut() {
            vListInitialise(ptr::addr_of_mut!(xActiveTimerList1));
            vListInitialise(ptr::addr_of_mut!(xActiveTimerList2));
            /* [AMENDMENT] These lists are now self-referential. Retain their
             * addresses directly as raw pointers instead of creating fresh
             * exclusive references to the complete list values. */
            pxCurrentTimerList = ptr::addr_of_mut!(xActiveTimerList1);
            pxOverflowTimerList = ptr::addr_of_mut!(xActiveTimerList2);

            /* The timer queue is static whenever static allocation is
             * supported, matching upstream and allowing allocator-free timer
             * configurations to compile and start. */
            xTimerQueue = xQueueCreateStatic(
                configTIMER_QUEUE_LENGTH,
                core::mem::size_of::<DaemonTaskMessage_t>() as UBaseType_t,
                ucStaticTimerQueueStorage.as_mut_ptr(),
                &mut xStaticTimerQueue,
            );

            #[cfg(feature = "queue-registry")]
            if !xTimerQueue.is_null() {
                /* SAFETY: xTimerQueue names the just-created live static
                 * queue, and this static C string has program lifetime. */
                vQueueAddToRegistry(xTimerQueue, b"TmrQ\0".as_ptr());
            }
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

    // Safety: this function runs only in the live timer daemon task and every
    // control-flow branch below resumes the same suspension before returning.
    unsafe { vTaskSuspendAll() };

    unsafe {
        // Sample current time
        let xTimeNow = prvSampleTimeNow(&mut xTimerListsWereSwitched);

        if xTimerListsWereSwitched == pdFALSE {
            // Has the timer expired?
            if (xListWasEmpty == pdFALSE) && (xNextExpireTime <= xTimeNow) {
                /* SAFETY: this consumes the timer daemon's suspension
                 * acquired at function entry. */
                xTaskResumeAll();
                prvProcessExpiredTimer(xNextExpireTime, xTimeNow);
            } else {
                // Block until timer expires or command received
                let mut xListWasEmptyNow = xListWasEmpty;
                if xListWasEmptyNow != pdFALSE {
                    // Check overflow list too
                    xListWasEmptyNow = listLIST_IS_EMPTY(pxOverflowTimerList);
                }

                /* When both timer lists are empty the daemon waits
                 * indefinitely for a command. Upstream performs this
                 * conversion in vTaskPlaceOnEventListRestricted; pass the
                 * effective value explicitly as a defensive boundary here. */
                let xTicksToWait = if xListWasEmptyNow != pdFALSE {
                    portMAX_DELAY
                } else {
                    xNextExpireTime.wrapping_sub(xTimeNow)
                };

                vQueueWaitForMessageRestricted(xTimerQueue, xTicksToWait, xListWasEmptyNow);

                /* SAFETY: this consumes the timer daemon's suspension after
                 * atomically arranging its queue wait. */
                if xTaskResumeAll() == pdFALSE {
                    // Yield to allow higher priority task to run
                    portYIELD_WITHIN_API();
                }
            }
        } else {
            /* SAFETY: this consumes the timer daemon's suspension acquired at
             * function entry after the tick-overflow list switch. */
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
        listSET_LIST_ITEM_VALUE(
            ptr::addr_of_mut!((*pxTimer).xTimerListItem),
            xNextExpiryTime,
        );
        listSET_LIST_ITEM_OWNER(
            ptr::addr_of_mut!((*pxTimer).xTimerListItem),
            pxTimer as *mut c_void,
        );

        if xNextExpiryTime <= xTimeNow {
            // Has the expiry time already passed?
            if xTimeNow.wrapping_sub(xCommandTime) >= (*pxTimer).xTimerPeriodInTicks {
                // Timer has fully expired
                xProcessTimerNow = pdTRUE;
            } else {
                // Goes in overflow list
                vListInsert(
                    pxOverflowTimerList,
                    ptr::addr_of_mut!((*pxTimer).xTimerListItem),
                );
            }
        } else {
            if (xTimeNow < xCommandTime) && (xNextExpiryTime >= xCommandTime) {
                // Tick count overflowed since command but expiry hasn't
                xProcessTimerNow = pdTRUE;
            } else {
                // Goes in current list
                vListInsert(
                    pxCurrentTimerList,
                    ptr::addr_of_mut!((*pxTimer).xTimerListItem),
                );
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
        uxListRemove(ptr::addr_of_mut!((*pxTimer).xTimerListItem));

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
                    if listIS_CONTAINED_WITHIN(
                        ptr::null_mut(),
                        ptr::addr_of!((*pxTimer).xTimerListItem),
                    ) == pdFALSE
                    {
                        uxListRemove(ptr::addr_of_mut!((*pxTimer).xTimerListItem));
                    }

                    traceTIMER_COMMAND_RECEIVED!(
                        pxTimer,
                        xMessage.xMessageID,
                        xMessage.u.xTimerParameters.xMessageValue
                    );

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
                                    prvReloadTimer(pxTimer, xNextExpiryTime, xTimeNow);
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
                            #[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
                            {
                                if ((*pxTimer).ucStatus & tmrSTATUS_IS_STATICALLY_ALLOCATED) == 0 {
                                    vPortFree(pxTimer as *mut c_void);
                                } else {
                                    (*pxTimer).ucStatus &= !tmrSTATUS_IS_ACTIVE;
                                }
                            }
                            #[cfg(not(any(
                                feature = "alloc",
                                feature = "heap-4",
                                feature = "heap-5"
                            )))]
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

/// Start a timer.
///
/// # Safety
///
/// `xTimer` must identify a live timer until the timer daemon consumes the
/// command. This task-context API must not be called from an interrupt.
#[inline(always)]
pub unsafe fn xTimerStart(xTimer: TimerHandle_t, xTicksToWait: TickType_t) -> BaseType_t {
    xTimerGenericCommand(
        xTimer,
        tmrCOMMAND_START,
        xTaskGetTickCount(),
        ptr::null_mut(),
        xTicksToWait,
    )
}

/// Stop a timer.
///
/// # Safety
///
/// `xTimer` must identify a live timer until the timer daemon consumes the
/// command. This task-context API must not be called from an interrupt.
#[inline(always)]
pub unsafe fn xTimerStop(xTimer: TimerHandle_t, xTicksToWait: TickType_t) -> BaseType_t {
    xTimerGenericCommand(xTimer, tmrCOMMAND_STOP, 0, ptr::null_mut(), xTicksToWait)
}

/// Change timer period.
///
/// # Safety
///
/// `xTimer` must identify a live timer until the timer daemon consumes the
/// command. This task-context API must not be called from an interrupt.
#[inline(always)]
pub unsafe fn xTimerChangePeriod(
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

/// Queue deletion of a timer.
///
/// # Safety
///
/// `xTimer` must identify a live timer until the daemon consumes the delete
/// command. If the command is queued successfully, every handle copy must be
/// retired immediately: the daemon may free a dynamically allocated timer at
/// any later scheduling point. Static storage must not be reused until command
/// processing is known to have completed.
#[inline(always)]
pub unsafe fn xTimerDelete(xTimer: TimerHandle_t, xTicksToWait: TickType_t) -> BaseType_t {
    xTimerGenericCommand(xTimer, tmrCOMMAND_DELETE, 0, ptr::null_mut(), xTicksToWait)
}

/// Reset a timer.
///
/// # Safety
///
/// `xTimer` must identify a live timer until the timer daemon consumes the
/// command. This task-context API must not be called from an interrupt.
#[inline(always)]
pub unsafe fn xTimerReset(xTimer: TimerHandle_t, xTicksToWait: TickType_t) -> BaseType_t {
    xTimerGenericCommand(
        xTimer,
        tmrCOMMAND_RESET,
        xTaskGetTickCount(),
        ptr::null_mut(),
        xTicksToWait,
    )
}

/// Start a timer from an ISR.
///
/// # Safety
///
/// `xTimer` must remain live until the daemon consumes the command. If
/// non-null, `pxHigherPriorityTaskWoken` must be aligned and writable for the
/// call. The port's FromISR priority restrictions must be satisfied.
#[inline(always)]
pub unsafe fn xTimerStartFromISR(
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

/// Stop a timer from an ISR.
///
/// # Safety
///
/// `xTimer` must remain live until the daemon consumes the command. If
/// non-null, `pxHigherPriorityTaskWoken` must be aligned and writable for the
/// call. The port's FromISR priority restrictions must be satisfied.
#[inline(always)]
pub unsafe fn xTimerStopFromISR(
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

/// Change a timer period from an ISR.
///
/// # Safety
///
/// `xTimer` must remain live until the daemon consumes the command. If
/// non-null, `pxHigherPriorityTaskWoken` must be aligned and writable for the
/// call. The port's FromISR priority restrictions must be satisfied.
#[inline(always)]
pub unsafe fn xTimerChangePeriodFromISR(
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

/// Reset a timer from an ISR.
///
/// # Safety
///
/// `xTimer` must remain live until the daemon consumes the command. If
/// non-null, `pxHigherPriorityTaskWoken` must be aligned and writable for the
/// call. The port's FromISR priority restrictions must be satisfied.
#[inline(always)]
pub unsafe fn xTimerResetFromISR(
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

#[cfg(test)]
mod tests {
    use super::*;

    extern "C" fn callback(_xTimer: TimerHandle_t) {}

    #[cfg(feature = "pend-function-call")]
    extern "C" fn pended_callback(
        _pvParameter1: *mut c_void,
        _ulParameter2: PendedFunctionParameter_t,
    ) {
    }

    #[test]
    fn static_timer_storage_matches_internal_layout() {
        assert_eq!(
            core::mem::size_of::<StaticTimer_t>(),
            core::mem::size_of::<Timer_t>()
        );
        assert_eq!(
            core::mem::align_of::<StaticTimer_t>(),
            core::mem::align_of::<Timer_t>()
        );
    }

    #[test]
    fn zero_period_static_timer_is_rejected_before_initialisation() {
        let mut storage = StaticTimer_t::new();
        // SAFETY: the name and uniquely borrowed storage outlive this test;
        // the zero period rejects creation before the storage is initialized.
        let timer = unsafe {
            xTimerCreateStatic(
                b"zero\0".as_ptr(),
                0,
                pdTRUE,
                ptr::null_mut(),
                callback,
                &mut storage,
            )
        };
        assert!(timer.is_null());
    }

    #[cfg(feature = "pend-function-call")]
    #[test]
    fn pended_call_fails_cleanly_before_timer_queue_initialisation() {
        // SAFETY: no daemon or queued callback exists in this isolated test.
        unsafe {
            vTimerResetState();
            assert_eq!(
                xTimerPendFunctionCallFromISR(
                    pended_callback,
                    ptr::null_mut(),
                    PendedFunctionParameter_t::MAX,
                    ptr::null_mut(),
                ),
                pdFAIL,
            );
        }
    }

    #[cfg(feature = "port-test")]
    #[test]
    fn zero_period_change_is_rejected_without_mutating_timer() {
        crate::port::test_port_reset();
        // SAFETY: no daemon or queued callback exists in this isolated test.
        unsafe { vTimerResetState() };

        let mut storage = StaticTimer_t::new();
        // SAFETY: the static name and uniquely borrowed timer storage remain
        // live for all operations below.
        let timer = unsafe {
            xTimerCreateStatic(
                b"period\0".as_ptr(),
                10,
                pdFALSE,
                ptr::null_mut(),
                callback,
                &mut storage,
            )
        };
        assert!(!timer.is_null());
        // SAFETY: `timer` still names the live static timer above.
        unsafe {
            assert_eq!(xTimerChangePeriod(timer, 0, 0), pdFAIL);
            assert_eq!(xTimerGetPeriod(timer), 10);
        }

        // SAFETY: the failed command queued nothing and no daemon is running.
        unsafe { vTimerResetState() };
    }

    #[cfg(feature = "port-test")]
    #[test]
    fn generic_commands_reject_unknown_ids_before_queueing() {
        crate::port::test_port_reset();
        // SAFETY: no daemon or queued callback exists in this isolated test.
        unsafe { vTimerResetState() };

        let mut storage = StaticTimer_t::new();
        // SAFETY: the static name and unique storage remain live throughout
        // the test, and the callback has no external data requirements.
        let timer = unsafe {
            xTimerCreateStatic(
                b"command\0".as_ptr(),
                10,
                pdFALSE,
                ptr::null_mut(),
                callback,
                &mut storage,
            )
        };
        assert!(!timer.is_null());

        // SAFETY: `timer` is live and this is task context. Invalid command
        // values are an explicitly checked input, not a safety precondition.
        assert_eq!(
            unsafe { xTimerGenericCommandFromTask(timer, BaseType_t::MIN, 0, ptr::null_mut(), 0) },
            pdFAIL,
        );

        crate::port::test_port_set_inside_interrupt(true);
        // SAFETY: the test port now reports ISR context, `timer` remains live,
        // and the null wake pointer is accepted. The invalid ID is checked.
        let isr_result =
            unsafe { xTimerGenericCommandFromISR(timer, BaseType_t::MAX, 0, ptr::null_mut(), 0) };
        crate::port::test_port_set_inside_interrupt(false);
        assert_eq!(isr_result, pdFAIL);

        // SAFETY: neither rejected command entered the queue and no daemon is
        // running, so module state is quiescent.
        unsafe { vTimerResetState() };
    }

    #[cfg(feature = "port-test")]
    #[test]
    fn obsolete_start_dont_trace_command_is_ignored_by_daemon() {
        crate::port::test_port_reset();
        // SAFETY: no daemon or queued callback exists in this isolated test.
        unsafe { vTimerResetState() };

        let mut storage = StaticTimer_t::new();
        // SAFETY: the static name and unique storage remain live until after
        // the queued command is processed synchronously below.
        let timer = unsafe {
            xTimerCreateStatic(
                b"obsolete\0".as_ptr(),
                10,
                pdFALSE,
                ptr::null_mut(),
                callback,
                &mut storage,
            )
        };
        assert!(!timer.is_null());

        // The pinned header still defines command zero, and the generic macro
        // can enqueue it, but the pinned daemon intentionally has no matching
        // switch case. Preserve that observable no-op behavior.
        assert_eq!(
            unsafe {
                xTimerGenericCommandFromTask(
                    timer,
                    tmrCOMMAND_START_DONT_TRACE,
                    0,
                    ptr::null_mut(),
                    0,
                )
            },
            pdPASS,
        );
        prvProcessReceivedCommands();
        // SAFETY: the command was consumed and the static timer remains live.
        assert_eq!(unsafe { xTimerIsTimerActive(timer) }, pdFALSE);

        // SAFETY: the queue is empty, the timer is inactive, and no daemon is
        // running in this isolated test.
        unsafe { vTimerResetState() };
    }

    #[cfg(all(feature = "port-test", feature = "queue-registry"))]
    #[test]
    fn timer_queue_uses_the_pinned_registry_name() {
        crate::port::test_port_reset();
        // SAFETY: no daemon or queued callback exists in this isolated test.
        unsafe { vTimerResetState() };

        let mut storage = StaticTimer_t::new();
        // SAFETY: the static name and unique storage remain live throughout
        // this test; creation initializes the timer queue.
        let timer = unsafe {
            xTimerCreateStatic(
                b"registry\0".as_ptr(),
                10,
                pdFALSE,
                ptr::null_mut(),
                callback,
                &mut storage,
            )
        };
        assert!(!timer.is_null());

        // SAFETY: the timer queue is the live static queue initialized above.
        let name = unsafe { pcQueueGetName(xTimerQueue) };
        assert!(!name.is_null());
        // SAFETY: the registered name is the program-lifetime `TmrQ` literal.
        assert_eq!(unsafe { core::slice::from_raw_parts(name, 4) }, b"TmrQ");

        // Keep the global registry clean for other serial kernel tests.
        // SAFETY: xTimerQueue is still the live static queue above.
        unsafe {
            vQueueUnregisterQueue(xTimerQueue);
            vTimerResetState();
        }
    }
}
