/*
 * FreeRTOS Kernel <DEVELOPMENT BRANCH>
 * Copyright (C) 2021 Amazon.com, Inc. or its affiliates. All Rights Reserved.
 *
 * SPDX-License-Identifier: MIT
 *
 * [AMENDMENT] This module is the Rust port of event_groups.c.
 * It provides event group functionality for FreeRTOS.
 * Event groups allow tasks to wait on combinations of event bits.
 */

//! Event Groups Implementation
//!
//! This module provides event groups for FreeRTOS.
//! An event group is a collection of bits that tasks can wait on.
//!
//! ## Key Concepts
//!
//! - Each event group contains 24 usable bits (32-bit ticks) or 8 bits (16-bit)
//! - Tasks can wait for any or all of a set of bits to be set
//! - Bits can be set from tasks or ISRs
//! - Bits can be cleared automatically when a task unblocks
//!
//! ## API Functions
//!
//! - [`xEventGroupCreate`] / [`xEventGroupCreateStatic`] - Create an event group
//! - [`xEventGroupWaitBits`] - Wait for bits to be set
//! - [`xEventGroupSetBits`] - Set bits in the event group
//! - [`xEventGroupClearBits`] - Clear bits in the event group
//! - [`xEventGroupSync`] - Synchronization (rendezvous) point

#![allow(unused_variables)]
#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(dead_code)]
#![allow(static_mut_refs)]

use core::ffi::c_void;

use crate::config::*;
use crate::kernel::list::*;
use crate::kernel::tasks::*;
use crate::memory::*;
use crate::trace::*;
use crate::types::*;

// =============================================================================
// Event Bits Control Constants
// =============================================================================

/// Clear events on exit bit (stored in event list item value)
#[cfg(feature = "tick-16bit")]
pub const eventCLEAR_EVENTS_ON_EXIT_BIT: EventBits_t = 0x0100;

#[cfg(feature = "tick-32bit")]
pub const eventCLEAR_EVENTS_ON_EXIT_BIT: EventBits_t = 0x0100_0000;

#[cfg(feature = "tick-64bit")]
pub const eventCLEAR_EVENTS_ON_EXIT_BIT: EventBits_t = 0x0100_0000_0000_0000;

/// Bit set to indicate task was unblocked due to bit set (not timeout)
#[cfg(feature = "tick-16bit")]
pub const eventUNBLOCKED_DUE_TO_BIT_SET: EventBits_t = 0x0200;

#[cfg(feature = "tick-32bit")]
pub const eventUNBLOCKED_DUE_TO_BIT_SET: EventBits_t = 0x0200_0000;

#[cfg(feature = "tick-64bit")]
pub const eventUNBLOCKED_DUE_TO_BIT_SET: EventBits_t = 0x0200_0000_0000_0000;

/// Wait for all bits (AND vs OR wait condition)
#[cfg(feature = "tick-16bit")]
pub const eventWAIT_FOR_ALL_BITS: EventBits_t = 0x0400;

#[cfg(feature = "tick-32bit")]
pub const eventWAIT_FOR_ALL_BITS: EventBits_t = 0x0400_0000;

#[cfg(feature = "tick-64bit")]
pub const eventWAIT_FOR_ALL_BITS: EventBits_t = 0x0400_0000_0000_0000;

/// Mask for control bytes (top 8 bits of EventBits_t)
#[cfg(feature = "tick-16bit")]
pub const eventEVENT_BITS_CONTROL_BYTES: EventBits_t = 0xFF00;

#[cfg(feature = "tick-32bit")]
pub const eventEVENT_BITS_CONTROL_BYTES: EventBits_t = 0xFF00_0000;

#[cfg(feature = "tick-64bit")]
pub const eventEVENT_BITS_CONTROL_BYTES: EventBits_t = 0xFF00_0000_0000_0000;

// =============================================================================
// Type Definitions
// =============================================================================

/// EventBits_t is the same as TickType_t
pub type EventBits_t = TickType_t;

// =============================================================================
// Event Group Structure
// =============================================================================

/// Event Group Control Block
#[repr(C)]
pub struct EventGroupDef_t {
    /// The current event bits
    pub uxEventBits: EventBits_t,

    /// List of tasks waiting for bits
    pub xTasksWaitingForBits: List_t,

    /// Trace facility number
    #[cfg(feature = "trace-facility")]
    pub uxEventGroupNumber: UBaseType_t,

    /// Whether statically allocated
    #[cfg(any(feature = "alloc", feature = "heap-4"))]
    pub ucStaticallyAllocated: u8,
}

/// Type alias for EventGroupDef_t
pub type EventGroup_t = EventGroupDef_t;

// =============================================================================
// Static Event Group (for static allocation)
// =============================================================================

/// Static buffer for event group allocation
#[repr(C)]
pub struct StaticEventGroup_t {
    /// Placeholder - event bits
    pub xDummy1: EventBits_t,
    /// Placeholder - list
    pub xDummy2: StaticList_t,
    /// Placeholder - trace number
    #[cfg(feature = "trace-facility")]
    pub uxDummy3: UBaseType_t,
    /// Placeholder - static flag
    #[cfg(any(feature = "alloc", feature = "heap-4"))]
    pub ucDummy4: u8,
}

/// Static list placeholder type
#[repr(C)]
pub struct StaticList_t {
    _data: List_t,
}

// =============================================================================
// Helper Functions
// =============================================================================

/// Coverage test marker (no-op)
#[inline(always)]
fn mtCOVERAGE_TEST_MARKER() {}

// =============================================================================
// Event Group Creation
// =============================================================================

/// Create an event group (dynamic allocation)
#[cfg(any(feature = "alloc", feature = "heap-4"))]
pub fn xEventGroupCreate() -> EventGroupHandle_t {
    traceENTER_xEventGroupCreate();

    let pxEventBits: *mut EventGroup_t =
        unsafe { pvPortMalloc(core::mem::size_of::<EventGroup_t>()) as *mut EventGroup_t };

    if !pxEventBits.is_null() {
        unsafe {
            (*pxEventBits).uxEventBits = 0;
            vListInitialise(&mut (*pxEventBits).xTasksWaitingForBits);
            (*pxEventBits).ucStaticallyAllocated = pdFALSE as u8;
        }

        traceEVENT_GROUP_CREATE(pxEventBits as *mut c_void);
    } else {
        traceEVENT_GROUP_CREATE_FAILED();
    }

    traceRETURN_xEventGroupCreate(pxEventBits);

    pxEventBits as EventGroupHandle_t
}

/// Create an event group (static allocation)
pub fn xEventGroupCreateStatic(pxEventGroupBuffer: *mut StaticEventGroup_t) -> EventGroupHandle_t {
    traceENTER_xEventGroupCreateStatic(pxEventGroupBuffer);

    configASSERT(!pxEventGroupBuffer.is_null());

    // Verify StaticEventGroup_t is same size as EventGroup_t
    #[cfg(debug_assertions)]
    {
        let static_size = core::mem::size_of::<StaticEventGroup_t>();
        let event_group_size = core::mem::size_of::<EventGroup_t>();
        configASSERT(static_size == event_group_size);
    }

    let pxEventBits = pxEventGroupBuffer as *mut EventGroup_t;

    if !pxEventBits.is_null() {
        unsafe {
            (*pxEventBits).uxEventBits = 0;
            vListInitialise(&mut (*pxEventBits).xTasksWaitingForBits);

            #[cfg(any(feature = "alloc", feature = "heap-4"))]
            {
                (*pxEventBits).ucStaticallyAllocated = pdTRUE as u8;
            }
        }

        traceEVENT_GROUP_CREATE(pxEventBits as *mut c_void);
    } else {
        traceEVENT_GROUP_CREATE_FAILED();
    }

    traceRETURN_xEventGroupCreateStatic(pxEventBits);

    pxEventBits as EventGroupHandle_t
}

// =============================================================================
// Event Group Wait Functions
// =============================================================================

/// Wait for bits to be set in an event group
///
/// # Arguments
/// * `xEventGroup` - Event group handle
/// * `uxBitsToWaitFor` - Bits to wait for
/// * `xClearOnExit` - Clear bits when unblocking
/// * `xWaitForAllBits` - Wait for all bits (AND) or any bit (OR)
/// * `xTicksToWait` - Block time
///
/// # Returns
/// The event bits value when unblocked (or timed out)
pub fn xEventGroupWaitBits(
    xEventGroup: EventGroupHandle_t,
    uxBitsToWaitFor: EventBits_t,
    xClearOnExit: BaseType_t,
    xWaitForAllBits: BaseType_t,
    mut xTicksToWait: TickType_t,
) -> EventBits_t {
    let pxEventBits = xEventGroup as *mut EventGroup_t;
    let mut uxReturn: EventBits_t;
    let mut uxControlBits: EventBits_t = 0;
    let xWaitConditionMet: BaseType_t;
    let xAlreadyYielded: BaseType_t;
    let mut xTimeoutOccurred: BaseType_t = pdFALSE;

    traceENTER_xEventGroupWaitBits(xEventGroup, uxBitsToWaitFor, xClearOnExit, xWaitForAllBits, xTicksToWait);

    configASSERT(!xEventGroup.is_null());
    configASSERT((uxBitsToWaitFor & eventEVENT_BITS_CONTROL_BYTES) == 0);
    configASSERT(uxBitsToWaitFor != 0);

    vTaskSuspendAll();

    unsafe {
        let uxCurrentEventBits = (*pxEventBits).uxEventBits;

        xWaitConditionMet = prvTestWaitCondition(uxCurrentEventBits, uxBitsToWaitFor, xWaitForAllBits);

        if xWaitConditionMet != pdFALSE {
            uxReturn = uxCurrentEventBits;
            xTicksToWait = 0;

            if xClearOnExit != pdFALSE {
                (*pxEventBits).uxEventBits &= !uxBitsToWaitFor;
            }
        } else if xTicksToWait == 0 {
            uxReturn = uxCurrentEventBits;
            xTimeoutOccurred = pdTRUE;
        } else {
            if xClearOnExit != pdFALSE {
                uxControlBits |= eventCLEAR_EVENTS_ON_EXIT_BIT;
            }
            if xWaitForAllBits != pdFALSE {
                uxControlBits |= eventWAIT_FOR_ALL_BITS;
            }

            vTaskPlaceOnUnorderedEventList(
                &mut (*pxEventBits).xTasksWaitingForBits,
                uxBitsToWaitFor | uxControlBits,
                xTicksToWait,
            );

            uxReturn = 0;

            traceEVENT_GROUP_WAIT_BITS_BLOCK(xEventGroup as *mut c_void, uxBitsToWaitFor as UBaseType_t);
        }
    }

    xAlreadyYielded = xTaskResumeAll();

    if xTicksToWait != 0 {
        if xAlreadyYielded == pdFALSE {
            portYIELD_WITHIN_API();
        }

        uxReturn = uxTaskResetEventItemValue();

        if (uxReturn & eventUNBLOCKED_DUE_TO_BIT_SET) == 0 {
            taskENTER_CRITICAL();
            unsafe {
                uxReturn = (*pxEventBits).uxEventBits;

                if prvTestWaitCondition(uxReturn, uxBitsToWaitFor, xWaitForAllBits) != pdFALSE {
                    if xClearOnExit != pdFALSE {
                        (*pxEventBits).uxEventBits &= !uxBitsToWaitFor;
                    }
                }

                xTimeoutOccurred = pdTRUE;
            }
            taskEXIT_CRITICAL();
        }

        uxReturn &= !eventEVENT_BITS_CONTROL_BYTES;
    }

    traceEVENT_GROUP_WAIT_BITS_END(xEventGroup as *mut c_void, uxBitsToWaitFor as UBaseType_t, xTimeoutOccurred);

    traceRETURN_xEventGroupWaitBits(uxReturn);

    uxReturn
}

/// Synchronization point (rendezvous)
///
/// Sets bits and then waits for all specified bits to be set.
pub fn xEventGroupSync(
    xEventGroup: EventGroupHandle_t,
    uxBitsToSet: EventBits_t,
    uxBitsToWaitFor: EventBits_t,
    mut xTicksToWait: TickType_t,
) -> EventBits_t {
    let pxEventBits = xEventGroup as *mut EventGroup_t;
    let uxOriginalBitValue: EventBits_t;
    let mut uxReturn: EventBits_t;
    let xAlreadyYielded: BaseType_t;
    let mut xTimeoutOccurred: BaseType_t = pdFALSE;

    traceENTER_xEventGroupSync(xEventGroup, uxBitsToSet, uxBitsToWaitFor, xTicksToWait);

    configASSERT((uxBitsToWaitFor & eventEVENT_BITS_CONTROL_BYTES) == 0);
    configASSERT(uxBitsToWaitFor != 0);

    vTaskSuspendAll();

    unsafe {
        uxOriginalBitValue = (*pxEventBits).uxEventBits;

        let _ = xEventGroupSetBits(xEventGroup, uxBitsToSet);

        if ((uxOriginalBitValue | uxBitsToSet) & uxBitsToWaitFor) == uxBitsToWaitFor {
            uxReturn = uxOriginalBitValue | uxBitsToSet;
            (*pxEventBits).uxEventBits &= !uxBitsToWaitFor;
            xTicksToWait = 0;
        } else {
            if xTicksToWait != 0 {
                traceEVENT_GROUP_SYNC_BLOCK(xEventGroup as *mut c_void, uxBitsToSet as UBaseType_t, uxBitsToWaitFor as UBaseType_t);

                vTaskPlaceOnUnorderedEventList(
                    &mut (*pxEventBits).xTasksWaitingForBits,
                    uxBitsToWaitFor | eventCLEAR_EVENTS_ON_EXIT_BIT | eventWAIT_FOR_ALL_BITS,
                    xTicksToWait,
                );

                uxReturn = 0;
            } else {
                uxReturn = (*pxEventBits).uxEventBits;
                xTimeoutOccurred = pdTRUE;
            }
        }
    }

    xAlreadyYielded = xTaskResumeAll();

    if xTicksToWait != 0 {
        if xAlreadyYielded == pdFALSE {
            portYIELD_WITHIN_API();
        }

        uxReturn = uxTaskResetEventItemValue();

        if (uxReturn & eventUNBLOCKED_DUE_TO_BIT_SET) == 0 {
            taskENTER_CRITICAL();
            unsafe {
                uxReturn = (*pxEventBits).uxEventBits;

                if (uxReturn & uxBitsToWaitFor) == uxBitsToWaitFor {
                    (*pxEventBits).uxEventBits &= !uxBitsToWaitFor;
                }
            }
            taskEXIT_CRITICAL();

            xTimeoutOccurred = pdTRUE;
        }

        uxReturn &= !eventEVENT_BITS_CONTROL_BYTES;
    }

    traceEVENT_GROUP_SYNC_END(xEventGroup as *mut c_void, uxBitsToSet as UBaseType_t, uxBitsToWaitFor as UBaseType_t, xTimeoutOccurred);

    traceRETURN_xEventGroupSync(uxReturn);

    uxReturn
}

// =============================================================================
// Event Group Set/Clear Functions
// =============================================================================

/// Set bits in an event group
///
/// # Arguments
/// * `xEventGroup` - Event group handle
/// * `uxBitsToSet` - Bits to set
///
/// # Returns
/// The event bits value after setting
#[allow(unused_assignments)] // C pattern: initialize to fail
pub fn xEventGroupSetBits(xEventGroup: EventGroupHandle_t, uxBitsToSet: EventBits_t) -> EventBits_t {
    let pxEventBits = xEventGroup as *mut EventGroup_t;
    let mut uxBitsToClear: EventBits_t = 0;
    let mut xMatchFound: BaseType_t = pdFALSE;
    let uxReturnBits: EventBits_t;

    traceENTER_xEventGroupSetBits(xEventGroup, uxBitsToSet);

    configASSERT(!xEventGroup.is_null());
    configASSERT((uxBitsToSet & eventEVENT_BITS_CONTROL_BYTES) == 0);

    unsafe {
        let pxList = &(*pxEventBits).xTasksWaitingForBits;
        let pxListEnd = listGET_END_MARKER(pxList);

        vTaskSuspendAll();

        traceEVENT_GROUP_SET_BITS(xEventGroup as *mut c_void, uxBitsToSet as UBaseType_t);

        let mut pxListItem = listGET_HEAD_ENTRY(pxList);

        (*pxEventBits).uxEventBits |= uxBitsToSet;

        while pxListItem != pxListEnd as *mut ListItem_t {
            let pxNext = listGET_NEXT(pxListItem);
            let mut uxBitsWaitedFor = listGET_LIST_ITEM_VALUE(pxListItem);
            xMatchFound = pdFALSE;

            let uxControlBits = uxBitsWaitedFor & eventEVENT_BITS_CONTROL_BYTES;
            uxBitsWaitedFor &= !eventEVENT_BITS_CONTROL_BYTES;

            if (uxControlBits & eventWAIT_FOR_ALL_BITS) == 0 {
                // OR wait - any bit matches
                if (uxBitsWaitedFor & (*pxEventBits).uxEventBits) != 0 {
                    xMatchFound = pdTRUE;
                }
            } else if (uxBitsWaitedFor & (*pxEventBits).uxEventBits) == uxBitsWaitedFor {
                // AND wait - all bits match
                xMatchFound = pdTRUE;
            }

            if xMatchFound != pdFALSE {
                if (uxControlBits & eventCLEAR_EVENTS_ON_EXIT_BIT) != 0 {
                    uxBitsToClear |= uxBitsWaitedFor;
                }

                let _ = xTaskRemoveFromUnorderedEventList(
                    pxListItem,
                    (*pxEventBits).uxEventBits | eventUNBLOCKED_DUE_TO_BIT_SET,
                );
            }

            pxListItem = pxNext;
        }

        (*pxEventBits).uxEventBits &= !uxBitsToClear;

        uxReturnBits = (*pxEventBits).uxEventBits;
    }

    let _ = xTaskResumeAll();

    traceRETURN_xEventGroupSetBits(uxReturnBits);

    uxReturnBits
}

/// Clear bits in an event group
///
/// # Arguments
/// * `xEventGroup` - Event group handle
/// * `uxBitsToClear` - Bits to clear
///
/// # Returns
/// The event bits value before clearing
pub fn xEventGroupClearBits(xEventGroup: EventGroupHandle_t, uxBitsToClear: EventBits_t) -> EventBits_t {
    let pxEventBits = xEventGroup as *mut EventGroup_t;
    let uxReturn: EventBits_t;

    traceENTER_xEventGroupClearBits(xEventGroup, uxBitsToClear);

    configASSERT(!xEventGroup.is_null());
    configASSERT((uxBitsToClear & eventEVENT_BITS_CONTROL_BYTES) == 0);

    taskENTER_CRITICAL();
    unsafe {
        traceEVENT_GROUP_CLEAR_BITS(xEventGroup as *mut c_void, uxBitsToClear as UBaseType_t);

        uxReturn = (*pxEventBits).uxEventBits;
        (*pxEventBits).uxEventBits &= !uxBitsToClear;
    }
    taskEXIT_CRITICAL();

    traceRETURN_xEventGroupClearBits(uxReturn);

    uxReturn
}

/// Get bits from an event group (from ISR)
pub fn xEventGroupGetBitsFromISR(xEventGroup: EventGroupHandle_t) -> EventBits_t {
    let pxEventBits = xEventGroup as *const EventGroup_t;
    let uxReturn: EventBits_t;

    traceENTER_xEventGroupGetBitsFromISR(xEventGroup);

    unsafe {
        let uxSavedInterruptStatus = taskENTER_CRITICAL_FROM_ISR();
        uxReturn = (*pxEventBits).uxEventBits;
        taskEXIT_CRITICAL_FROM_ISR(uxSavedInterruptStatus);
    }

    traceRETURN_xEventGroupGetBitsFromISR(uxReturn);

    uxReturn
}

/// Get current bits (convenience function)
#[inline(always)]
pub fn xEventGroupGetBits(xEventGroup: EventGroupHandle_t) -> EventBits_t {
    xEventGroupClearBits(xEventGroup, 0)
}

// =============================================================================
// Event Group Delete
// =============================================================================

/// Delete an event group
pub fn vEventGroupDelete(xEventGroup: EventGroupHandle_t) {
    let pxEventBits = xEventGroup as *mut EventGroup_t;

    traceENTER_vEventGroupDelete(xEventGroup);

    configASSERT(!pxEventBits.is_null());

    unsafe {
        let pxTasksWaitingForBits = &(*pxEventBits).xTasksWaitingForBits;

        vTaskSuspendAll();

        traceEVENT_GROUP_DELETE(xEventGroup as *mut c_void);

        while listCURRENT_LIST_LENGTH(pxTasksWaitingForBits) > 0 {
            configASSERT((*pxTasksWaitingForBits).xListEnd.pxNext != &(*pxTasksWaitingForBits).xListEnd as *const _ as *mut _);

            let _ = xTaskRemoveFromUnorderedEventList(
                (*pxTasksWaitingForBits).xListEnd.pxNext,
                eventUNBLOCKED_DUE_TO_BIT_SET,
            );
        }
    }

    let _ = xTaskResumeAll();

    #[cfg(any(feature = "alloc", feature = "heap-4"))]
    unsafe {
        if (*pxEventBits).ucStaticallyAllocated == pdFALSE as u8 {
            vPortFree(pxEventBits as *mut c_void);
        }
    }

    traceRETURN_vEventGroupDelete();
}

// =============================================================================
// Static Buffer Query
// =============================================================================

/// Get the static buffer used by a statically allocated event group
pub fn xEventGroupGetStaticBuffer(
    xEventGroup: EventGroupHandle_t,
    ppxEventGroupBuffer: *mut *mut StaticEventGroup_t,
) -> BaseType_t {
    let pxEventBits = xEventGroup as *mut EventGroup_t;
    let xReturn: BaseType_t;

    traceENTER_xEventGroupGetStaticBuffer(xEventGroup, ppxEventGroupBuffer);

    configASSERT(!pxEventBits.is_null());
    configASSERT(!ppxEventGroupBuffer.is_null());

    #[cfg(any(feature = "alloc", feature = "heap-4"))]
    {
        xReturn = unsafe {
            if (*pxEventBits).ucStaticallyAllocated == pdTRUE as u8 {
                *ppxEventGroupBuffer = pxEventBits as *mut StaticEventGroup_t;
                pdTRUE
            } else {
                pdFALSE
            }
        };
    }

    #[cfg(not(any(feature = "alloc", feature = "heap-4")))]
    {
        xReturn = unsafe {
            *ppxEventGroupBuffer = pxEventBits as *mut StaticEventGroup_t;
            pdTRUE
        };
    }

    traceRETURN_xEventGroupGetStaticBuffer(xReturn);

    xReturn
}

// =============================================================================
// Callback Functions (for ISR support via timer pend)
// =============================================================================

/// Callback to set bits (used by xEventGroupSetBitsFromISR)
pub extern "C" fn vEventGroupSetBitsCallback(pvEventGroup: *mut c_void, ulBitsToSet: u32) {
    traceENTER_vEventGroupSetBitsCallback(pvEventGroup, ulBitsToSet);

    let _ = xEventGroupSetBits(pvEventGroup as EventGroupHandle_t, ulBitsToSet as EventBits_t);

    traceRETURN_vEventGroupSetBitsCallback();
}

/// Callback to clear bits (used by xEventGroupClearBitsFromISR)
pub extern "C" fn vEventGroupClearBitsCallback(pvEventGroup: *mut c_void, ulBitsToClear: u32) {
    traceENTER_vEventGroupClearBitsCallback(pvEventGroup, ulBitsToClear);

    let _ = xEventGroupClearBits(pvEventGroup as EventGroupHandle_t, ulBitsToClear as EventBits_t);

    traceRETURN_vEventGroupClearBitsCallback();
}

// =============================================================================
// ISR Functions (require timer pend function call support)
// =============================================================================

/// Set bits from ISR (requires INCLUDE_xTimerPendFunctionCall)
#[cfg(all(feature = "pend-function-call", feature = "timers"))]
pub fn xEventGroupSetBitsFromISR(
    xEventGroup: EventGroupHandle_t,
    uxBitsToSet: EventBits_t,
    pxHigherPriorityTaskWoken: *mut BaseType_t,
) -> BaseType_t {
    use crate::kernel::timers::xTimerPendFunctionCallFromISR;

    traceENTER_xEventGroupSetBitsFromISR(xEventGroup, uxBitsToSet, pxHigherPriorityTaskWoken);

    traceEVENT_GROUP_SET_BITS_FROM_ISR(xEventGroup as *mut c_void, uxBitsToSet as UBaseType_t);

    let xReturn = unsafe {
        xTimerPendFunctionCallFromISR(
            vEventGroupSetBitsCallback,
            xEventGroup as *mut c_void,
            uxBitsToSet as u32,
            pxHigherPriorityTaskWoken,
        )
    };

    traceRETURN_xEventGroupSetBitsFromISR(xReturn);

    xReturn
}

/// Clear bits from ISR (requires INCLUDE_xTimerPendFunctionCall)
#[cfg(all(feature = "pend-function-call", feature = "timers"))]
pub fn xEventGroupClearBitsFromISR(
    xEventGroup: EventGroupHandle_t,
    uxBitsToClear: EventBits_t,
) -> BaseType_t {
    use crate::kernel::timers::xTimerPendFunctionCallFromISR;

    traceENTER_xEventGroupClearBitsFromISR(xEventGroup, uxBitsToClear);

    traceEVENT_GROUP_CLEAR_BITS_FROM_ISR(xEventGroup as *mut c_void, uxBitsToClear as UBaseType_t);

    let xReturn = unsafe {
        xTimerPendFunctionCallFromISR(
            vEventGroupClearBitsCallback,
            xEventGroup as *mut c_void,
            uxBitsToClear as u32,
            ptr::null_mut(),
        )
    };

    traceRETURN_xEventGroupClearBitsFromISR(xReturn);

    xReturn
}

// =============================================================================
// Trace Facility Functions
// =============================================================================

/// Get event group number (for trace facility)
#[cfg(feature = "trace-facility")]
pub fn uxEventGroupGetNumber(xEventGroup: EventGroupHandle_t) -> UBaseType_t {
    let xReturn: UBaseType_t;

    traceENTER_uxEventGroupGetNumber(xEventGroup);

    if xEventGroup.is_null() {
        xReturn = 0;
    } else {
        let pxEventBits = xEventGroup as *const EventGroup_t;
        xReturn = unsafe { (*pxEventBits).uxEventGroupNumber };
    }

    traceRETURN_uxEventGroupGetNumber(xReturn);

    xReturn
}

/// Set event group number (for trace facility)
#[cfg(feature = "trace-facility")]
pub fn vEventGroupSetNumber(xEventGroup: EventGroupHandle_t, uxEventGroupNumber: UBaseType_t) {
    traceENTER_vEventGroupSetNumber(xEventGroup, uxEventGroupNumber);

    unsafe {
        (*(xEventGroup as *mut EventGroup_t)).uxEventGroupNumber = uxEventGroupNumber;
    }

    traceRETURN_vEventGroupSetNumber();
}

// =============================================================================
// Private Functions
// =============================================================================

/// Test if wait condition is met
fn prvTestWaitCondition(
    uxCurrentEventBits: EventBits_t,
    uxBitsToWaitFor: EventBits_t,
    xWaitForAllBits: BaseType_t,
) -> BaseType_t {
    let mut xWaitConditionMet: BaseType_t = pdFALSE;

    if xWaitForAllBits == pdFALSE {
        // OR condition - any bit set is a match
        if (uxCurrentEventBits & uxBitsToWaitFor) != 0 {
            xWaitConditionMet = pdTRUE;
        }
    } else {
        // AND condition - all bits must be set
        if (uxCurrentEventBits & uxBitsToWaitFor) == uxBitsToWaitFor {
            xWaitConditionMet = pdTRUE;
        }
    }

    xWaitConditionMet
}

// =============================================================================
// Trace Stubs
// =============================================================================

#[inline(always)]
fn traceENTER_xEventGroupCreate() {}
#[inline(always)]
fn traceRETURN_xEventGroupCreate(_pxEventBits: *mut EventGroup_t) {}
#[inline(always)]
fn traceENTER_xEventGroupCreateStatic(_pxEventGroupBuffer: *mut StaticEventGroup_t) {}
#[inline(always)]
fn traceRETURN_xEventGroupCreateStatic(_pxEventBits: *mut EventGroup_t) {}
#[inline(always)]
fn traceENTER_xEventGroupWaitBits(_xEventGroup: EventGroupHandle_t, _uxBitsToWaitFor: EventBits_t, _xClearOnExit: BaseType_t, _xWaitForAllBits: BaseType_t, _xTicksToWait: TickType_t) {}
#[inline(always)]
fn traceRETURN_xEventGroupWaitBits(_uxReturn: EventBits_t) {}
#[inline(always)]
fn traceENTER_xEventGroupSync(_xEventGroup: EventGroupHandle_t, _uxBitsToSet: EventBits_t, _uxBitsToWaitFor: EventBits_t, _xTicksToWait: TickType_t) {}
#[inline(always)]
fn traceRETURN_xEventGroupSync(_uxReturn: EventBits_t) {}
#[inline(always)]
fn traceENTER_xEventGroupSetBits(_xEventGroup: EventGroupHandle_t, _uxBitsToSet: EventBits_t) {}
#[inline(always)]
fn traceRETURN_xEventGroupSetBits(_uxReturnBits: EventBits_t) {}
#[inline(always)]
fn traceENTER_xEventGroupClearBits(_xEventGroup: EventGroupHandle_t, _uxBitsToClear: EventBits_t) {}
#[inline(always)]
fn traceRETURN_xEventGroupClearBits(_uxReturn: EventBits_t) {}
#[inline(always)]
fn traceENTER_xEventGroupGetBitsFromISR(_xEventGroup: EventGroupHandle_t) {}
#[inline(always)]
fn traceRETURN_xEventGroupGetBitsFromISR(_uxReturn: EventBits_t) {}
#[inline(always)]
fn traceENTER_vEventGroupDelete(_xEventGroup: EventGroupHandle_t) {}
#[inline(always)]
fn traceRETURN_vEventGroupDelete() {}
#[inline(always)]
fn traceENTER_xEventGroupGetStaticBuffer(_xEventGroup: EventGroupHandle_t, _ppxEventGroupBuffer: *mut *mut StaticEventGroup_t) {}
#[inline(always)]
fn traceRETURN_xEventGroupGetStaticBuffer(_xReturn: BaseType_t) {}
#[inline(always)]
fn traceENTER_vEventGroupSetBitsCallback(_pvEventGroup: *mut c_void, _ulBitsToSet: u32) {}
#[inline(always)]
fn traceRETURN_vEventGroupSetBitsCallback() {}
#[inline(always)]
fn traceENTER_vEventGroupClearBitsCallback(_pvEventGroup: *mut c_void, _ulBitsToClear: u32) {}
#[inline(always)]
fn traceRETURN_vEventGroupClearBitsCallback() {}
#[cfg(all(feature = "pend-function-call", feature = "timers"))]
#[inline(always)]
fn traceENTER_xEventGroupSetBitsFromISR(_xEventGroup: EventGroupHandle_t, _uxBitsToSet: EventBits_t, _pxHigherPriorityTaskWoken: *mut BaseType_t) {}
#[cfg(all(feature = "pend-function-call", feature = "timers"))]
#[inline(always)]
fn traceRETURN_xEventGroupSetBitsFromISR(_xReturn: BaseType_t) {}
#[cfg(all(feature = "pend-function-call", feature = "timers"))]
#[inline(always)]
fn traceENTER_xEventGroupClearBitsFromISR(_xEventGroup: EventGroupHandle_t, _uxBitsToClear: EventBits_t) {}
#[cfg(all(feature = "pend-function-call", feature = "timers"))]
#[inline(always)]
fn traceRETURN_xEventGroupClearBitsFromISR(_xReturn: BaseType_t) {}
#[cfg(feature = "trace-facility")]
#[inline(always)]
fn traceENTER_uxEventGroupGetNumber(_xEventGroup: EventGroupHandle_t) {}
#[cfg(feature = "trace-facility")]
#[inline(always)]
fn traceRETURN_uxEventGroupGetNumber(_xReturn: UBaseType_t) {}
#[cfg(feature = "trace-facility")]
#[inline(always)]
fn traceENTER_vEventGroupSetNumber(_xEventGroup: EventGroupHandle_t, _uxEventGroupNumber: UBaseType_t) {}
#[cfg(feature = "trace-facility")]
#[inline(always)]
fn traceRETURN_vEventGroupSetNumber() {}
