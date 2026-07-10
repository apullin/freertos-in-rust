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
use core::ptr;
#[cfg(not(feature = "tick-64bit"))]
use core::sync::atomic::AtomicU32;
#[cfg(feature = "tick-64bit")]
use core::sync::atomic::AtomicU64;
use core::sync::atomic::Ordering;

use crate::config::*;
use crate::kernel::list::*;
use crate::kernel::tasks::*;
#[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
use crate::memory::{pvPortMalloc, vPortFree};
use crate::trace::*;
use crate::types::*;

// =============================================================================
// Event Bits Control Constants
// =============================================================================

/// Clear events on exit bit (stored in event list item value)
#[cfg(not(feature = "tick-64bit"))]
pub const eventCLEAR_EVENTS_ON_EXIT_BIT: EventBits_t = 0x0100_0000;

#[cfg(feature = "tick-64bit")]
pub const eventCLEAR_EVENTS_ON_EXIT_BIT: EventBits_t = 0x0100_0000_0000_0000;

/// Bit set to indicate task was unblocked due to bit set (not timeout)
#[cfg(not(feature = "tick-64bit"))]
pub const eventUNBLOCKED_DUE_TO_BIT_SET: EventBits_t = 0x0200_0000;

#[cfg(feature = "tick-64bit")]
pub const eventUNBLOCKED_DUE_TO_BIT_SET: EventBits_t = 0x0200_0000_0000_0000;

/// Wait for all bits (AND vs OR wait condition)
#[cfg(not(feature = "tick-64bit"))]
pub const eventWAIT_FOR_ALL_BITS: EventBits_t = 0x0400_0000;

#[cfg(feature = "tick-64bit")]
pub const eventWAIT_FOR_ALL_BITS: EventBits_t = 0x0400_0000_0000_0000;

/// Mask for control bytes (top 8 bits of EventBits_t)
#[cfg(not(feature = "tick-64bit"))]
pub const eventEVENT_BITS_CONTROL_BYTES: EventBits_t = 0xFF00_0000;

#[cfg(feature = "tick-64bit")]
pub const eventEVENT_BITS_CONTROL_BYTES: EventBits_t = 0xFF00_0000_0000_0000;

// =============================================================================
// Type Definitions
// =============================================================================

/// EventBits_t is the same as TickType_t
pub type EventBits_t = TickType_t;

/// Atomic storage with the same width and layout as [`EventBits_t`].
///
/// Only atomic loads and stores are required. Task-side read/modify/write
/// sequences remain serialized by the same scheduler/critical sections as the
/// pinned kernel, while interrupt-side observation no longer races those Rust
/// accesses.
#[cfg(not(feature = "tick-64bit"))]
pub type AtomicEventBits_t = AtomicU32;

#[cfg(feature = "tick-64bit")]
pub type AtomicEventBits_t = AtomicU64;

const _: () =
    assert!(core::mem::size_of::<AtomicEventBits_t>() == core::mem::size_of::<EventBits_t>());
const _: () =
    assert!(core::mem::align_of::<AtomicEventBits_t>() == core::mem::align_of::<EventBits_t>());

// =============================================================================
// Event Group Structure
// =============================================================================

/// Event Group Control Block
#[repr(C)]
pub struct EventGroupDef_t {
    /// The current event bits
    pub uxEventBits: AtomicEventBits_t,

    /// List of tasks waiting for bits
    pub xTasksWaitingForBits: List_t,

    /// Trace facility number
    #[cfg(feature = "trace-facility")]
    pub uxEventGroupNumber: UBaseType_t,

    /// Whether statically allocated
    #[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
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
    pub xDummy1: AtomicEventBits_t,
    /// Placeholder - list
    pub xDummy2: StaticList_t,
    /// Placeholder - trace number
    #[cfg(feature = "trace-facility")]
    pub uxDummy3: UBaseType_t,
    /// Placeholder - static flag
    #[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
    pub ucDummy4: u8,
}

impl StaticEventGroup_t {
    /// Create a zeroed StaticEventGroup_t for use in static allocation.
    ///
    /// # Example
    /// ```ignore
    /// static mut EVENT_GROUP: StaticEventGroup_t = StaticEventGroup_t::new();
    /// ```
    pub const fn new() -> Self {
        StaticEventGroup_t {
            xDummy1: AtomicEventBits_t::new(0),
            xDummy2: StaticList_t::new(),
            #[cfg(feature = "trace-facility")]
            uxDummy3: 0,
            #[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
            ucDummy4: 0,
        }
    }
}

/// Static list placeholder type
#[repr(C)]
pub struct StaticList_t {
    _data: List_t,
}

impl StaticList_t {
    /// Create a zeroed StaticList_t for use in static allocation.
    pub const fn new() -> Self {
        StaticList_t {
            _data: List_t::new(),
        }
    }
}

const _: () =
    assert!(core::mem::size_of::<StaticEventGroup_t>() == core::mem::size_of::<EventGroup_t>());
const _: () =
    assert!(core::mem::align_of::<StaticEventGroup_t>() == core::mem::align_of::<EventGroup_t>());
const _: () = assert!(
    core::mem::offset_of!(StaticEventGroup_t, xDummy2)
        == core::mem::offset_of!(EventGroup_t, xTasksWaitingForBits)
);
#[cfg(feature = "trace-facility")]
const _: () = assert!(
    core::mem::offset_of!(StaticEventGroup_t, uxDummy3)
        == core::mem::offset_of!(EventGroup_t, uxEventGroupNumber)
);
#[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
const _: () = assert!(
    core::mem::offset_of!(StaticEventGroup_t, ucDummy4)
        == core::mem::offset_of!(EventGroup_t, ucStaticallyAllocated)
);

// =============================================================================
// Helper Functions
// =============================================================================

/// Coverage test marker (no-op)
#[inline(always)]
fn mtCOVERAGE_TEST_MARKER() {}

/// Load the event word without adding ordering beyond the scheduler and port
/// synchronization already required by the raw API.
#[inline(always)]
unsafe fn prvLoadEventBits(pxEventBits: *const EventGroup_t) -> EventBits_t {
    unsafe { (*pxEventBits).uxEventBits.load(Ordering::Relaxed) }
}

/// Store the event word. Callers retain the pinned kernel's task-side
/// serialization around every read/modify/write sequence.
#[inline(always)]
unsafe fn prvStoreEventBits(pxEventBits: *mut EventGroup_t, uxBits: EventBits_t) {
    unsafe { (*pxEventBits).uxEventBits.store(uxBits, Ordering::Relaxed) };
}

// =============================================================================
// Event Group Creation
// =============================================================================

/// Create a dynamically allocated event group.
///
/// # Safety
///
/// Call only from task context while the selected FreeRTOS allocator and port
/// critical-section implementation are initialized. The allocator must not be
/// reset concurrently. A non-null returned handle must eventually be deleted
/// exactly once after all task and interrupt access has ceased.
#[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
pub unsafe fn xEventGroupCreate() -> EventGroupHandle_t {
    traceENTER_xEventGroupCreate();

    let pxEventBits: *mut EventGroup_t =
        unsafe { pvPortMalloc(core::mem::size_of::<EventGroup_t>()) as *mut EventGroup_t };

    if !pxEventBits.is_null() {
        unsafe {
            /* [AMENDMENT] Construct the allocated Rust value before taking
             * references to any of its fields. */
            ptr::write(
                pxEventBits,
                EventGroup_t {
                    uxEventBits: AtomicEventBits_t::new(0),
                    xTasksWaitingForBits: List_t::new(),
                    #[cfg(feature = "trace-facility")]
                    uxEventGroupNumber: 0,
                    ucStaticallyAllocated: pdFALSE as u8,
                },
            );
            vListInitialise(ptr::addr_of_mut!((*pxEventBits).xTasksWaitingForBits));
        }

        traceEVENT_GROUP_CREATE(pxEventBits as *mut c_void);
    } else {
        traceEVENT_GROUP_CREATE_FAILED();
    }

    traceRETURN_xEventGroupCreate(pxEventBits);

    pxEventBits as EventGroupHandle_t
}

/// Create an event group in caller-provided storage.
///
/// # Safety
///
/// `pxEventGroupBuffer` must be non-null, properly aligned, uniquely writable,
/// and remain at a stable address until the group is deleted and no handle to
/// it can be used again.
pub unsafe fn xEventGroupCreateStatic(
    pxEventGroupBuffer: *mut StaticEventGroup_t,
) -> EventGroupHandle_t {
    traceENTER_xEventGroupCreateStatic(pxEventGroupBuffer);

    if pxEventGroupBuffer.is_null() {
        traceEVENT_GROUP_CREATE_FAILED();
        traceRETURN_xEventGroupCreateStatic(ptr::null_mut());
        return ptr::null_mut();
    }

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
            /* [AMENDMENT] Begin the EventGroup_t lifetime in the caller's
             * layout-compatible static storage before accessing its fields. */
            ptr::write(
                pxEventBits,
                EventGroup_t {
                    uxEventBits: AtomicEventBits_t::new(0),
                    xTasksWaitingForBits: List_t::new(),
                    #[cfg(feature = "trace-facility")]
                    uxEventGroupNumber: 0,
                    #[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
                    ucStaticallyAllocated: pdTRUE as u8,
                },
            );
            vListInitialise(ptr::addr_of_mut!((*pxEventBits).xTasksWaitingForBits));
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

/// Wait for bits to be set in an event group.
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
///
/// # Safety
///
/// `xEventGroup` must identify a live event group for the entire call and must
/// not be deleted concurrently. This task-context API must not be called from
/// an interrupt handler.
pub unsafe fn xEventGroupWaitBits(
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

    traceENTER_xEventGroupWaitBits(
        xEventGroup,
        uxBitsToWaitFor,
        xClearOnExit,
        xWaitForAllBits,
        xTicksToWait,
    );

    configASSERT(!xEventGroup.is_null());
    configASSERT((uxBitsToWaitFor & eventEVENT_BITS_CONTROL_BYTES) == 0);
    configASSERT(uxBitsToWaitFor != 0);
    configASSERT(!(xTaskGetSchedulerState() == taskSCHEDULER_SUSPENDED && xTicksToWait != 0));

    /* SAFETY: this task-context operation owns the suspension until the
     * paired resume below and cannot return through the intervening block. */
    unsafe { vTaskSuspendAll() };

    unsafe {
        let uxCurrentEventBits = prvLoadEventBits(pxEventBits);

        xWaitConditionMet =
            prvTestWaitCondition(uxCurrentEventBits, uxBitsToWaitFor, xWaitForAllBits);

        if xWaitConditionMet != pdFALSE {
            uxReturn = uxCurrentEventBits;
            xTicksToWait = 0;

            if xClearOnExit != pdFALSE {
                prvStoreEventBits(pxEventBits, uxCurrentEventBits & !uxBitsToWaitFor);
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
                ptr::addr_of_mut!((*pxEventBits).xTasksWaitingForBits),
                uxBitsToWaitFor | uxControlBits,
                xTicksToWait,
            );

            uxReturn = 0;

            traceEVENT_GROUP_WAIT_BITS_BLOCK(
                xEventGroup as *mut c_void,
                uxBitsToWaitFor as UBaseType_t,
            );
        }
    }

    /* SAFETY: this consumes the scheduler suspension acquired above. */
    xAlreadyYielded = unsafe { xTaskResumeAll() };

    if xTicksToWait != 0 {
        if xAlreadyYielded == pdFALSE {
            portYIELD_WITHIN_API();
        }

        uxReturn = uxTaskResetEventItemValue();

        if (uxReturn & eventUNBLOCKED_DUE_TO_BIT_SET) == 0 {
            taskENTER_CRITICAL();
            unsafe {
                uxReturn = prvLoadEventBits(pxEventBits);

                if prvTestWaitCondition(uxReturn, uxBitsToWaitFor, xWaitForAllBits) != pdFALSE {
                    if xClearOnExit != pdFALSE {
                        prvStoreEventBits(pxEventBits, uxReturn & !uxBitsToWaitFor);
                    }
                }

                xTimeoutOccurred = pdTRUE;
            }
            taskEXIT_CRITICAL();
        }

        uxReturn &= !eventEVENT_BITS_CONTROL_BYTES;
    }

    traceEVENT_GROUP_WAIT_BITS_END(
        xEventGroup as *mut c_void,
        uxBitsToWaitFor as UBaseType_t,
        xTimeoutOccurred,
    );

    traceRETURN_xEventGroupWaitBits(uxReturn);

    uxReturn
}

/// Synchronization point (rendezvous).
///
/// Sets bits and then waits for all specified bits to be set.
///
/// # Safety
///
/// `xEventGroup` must identify a live event group for the entire call and must
/// not be deleted concurrently. This task-context API must not be called from
/// an interrupt handler.
pub unsafe fn xEventGroupSync(
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

    configASSERT(!xEventGroup.is_null());
    configASSERT((uxBitsToSet & eventEVENT_BITS_CONTROL_BYTES) == 0);
    configASSERT((uxBitsToWaitFor & eventEVENT_BITS_CONTROL_BYTES) == 0);
    configASSERT(uxBitsToWaitFor != 0);
    configASSERT(!(xTaskGetSchedulerState() == taskSCHEDULER_SUSPENDED && xTicksToWait != 0));

    /* SAFETY: this task-context operation owns the suspension until the
     * paired resume below and cannot return through the intervening block. */
    unsafe { vTaskSuspendAll() };

    unsafe {
        uxOriginalBitValue = prvLoadEventBits(pxEventBits);

        /* SAFETY: this function's contract establishes the same live event
         * group invariant for the nested operation. */
        let _ = xEventGroupSetBits(xEventGroup, uxBitsToSet);

        if ((uxOriginalBitValue | uxBitsToSet) & uxBitsToWaitFor) == uxBitsToWaitFor {
            uxReturn = uxOriginalBitValue | uxBitsToSet;
            let uxCurrentEventBits = prvLoadEventBits(pxEventBits);
            prvStoreEventBits(pxEventBits, uxCurrentEventBits & !uxBitsToWaitFor);
            xTicksToWait = 0;
        } else {
            if xTicksToWait != 0 {
                traceEVENT_GROUP_SYNC_BLOCK(
                    xEventGroup as *mut c_void,
                    uxBitsToSet as UBaseType_t,
                    uxBitsToWaitFor as UBaseType_t,
                );

                vTaskPlaceOnUnorderedEventList(
                    ptr::addr_of_mut!((*pxEventBits).xTasksWaitingForBits),
                    uxBitsToWaitFor | eventCLEAR_EVENTS_ON_EXIT_BIT | eventWAIT_FOR_ALL_BITS,
                    xTicksToWait,
                );

                uxReturn = 0;
            } else {
                uxReturn = prvLoadEventBits(pxEventBits);
                xTimeoutOccurred = pdTRUE;
            }
        }
    }

    /* SAFETY: this consumes the scheduler suspension acquired above. */
    xAlreadyYielded = unsafe { xTaskResumeAll() };

    if xTicksToWait != 0 {
        if xAlreadyYielded == pdFALSE {
            portYIELD_WITHIN_API();
        }

        uxReturn = uxTaskResetEventItemValue();

        if (uxReturn & eventUNBLOCKED_DUE_TO_BIT_SET) == 0 {
            taskENTER_CRITICAL();
            unsafe {
                uxReturn = prvLoadEventBits(pxEventBits);

                if (uxReturn & uxBitsToWaitFor) == uxBitsToWaitFor {
                    prvStoreEventBits(pxEventBits, uxReturn & !uxBitsToWaitFor);
                }
            }
            taskEXIT_CRITICAL();

            xTimeoutOccurred = pdTRUE;
        }

        uxReturn &= !eventEVENT_BITS_CONTROL_BYTES;
    }

    traceEVENT_GROUP_SYNC_END(
        xEventGroup as *mut c_void,
        uxBitsToSet as UBaseType_t,
        uxBitsToWaitFor as UBaseType_t,
        xTimeoutOccurred,
    );

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
///
/// # Safety
///
/// `xEventGroup` must identify a live event group through the operation and
/// must not be deleted concurrently. This task-context API must not be called
/// from an interrupt handler.
pub unsafe fn xEventGroupSetBits(
    xEventGroup: EventGroupHandle_t,
    uxBitsToSet: EventBits_t,
) -> EventBits_t {
    let pxEventBits = xEventGroup as *mut EventGroup_t;
    let mut uxBitsToClear: EventBits_t = 0;
    let mut xMatchFound: BaseType_t = pdFALSE;
    let uxReturnBits: EventBits_t;

    traceENTER_xEventGroupSetBits(xEventGroup, uxBitsToSet);

    configASSERT(!xEventGroup.is_null());
    configASSERT((uxBitsToSet & eventEVENT_BITS_CONTROL_BYTES) == 0);

    unsafe {
        /* [AMENDMENT] The wait list is self-referential. Traverse it through
         * raw pointers so inspecting it cannot retag the retained links. */
        let pxList = ptr::addr_of!((*pxEventBits).xTasksWaitingForBits);
        let pxListEnd = listGET_END_MARKER(pxList);

        vTaskSuspendAll();

        traceEVENT_GROUP_SET_BITS(xEventGroup as *mut c_void, uxBitsToSet as UBaseType_t);

        let mut pxListItem = listGET_HEAD_ENTRY(pxList);
        let mut uxCurrentEventBits = prvLoadEventBits(pxEventBits);

        uxCurrentEventBits |= uxBitsToSet;
        prvStoreEventBits(pxEventBits, uxCurrentEventBits);

        while pxListItem != pxListEnd as *mut ListItem_t {
            let pxNext = listGET_NEXT(pxListItem);
            let mut uxBitsWaitedFor = listGET_LIST_ITEM_VALUE(pxListItem);
            xMatchFound = pdFALSE;

            let uxControlBits = uxBitsWaitedFor & eventEVENT_BITS_CONTROL_BYTES;
            uxBitsWaitedFor &= !eventEVENT_BITS_CONTROL_BYTES;

            if (uxControlBits & eventWAIT_FOR_ALL_BITS) == 0 {
                // OR wait - any bit matches
                if (uxBitsWaitedFor & uxCurrentEventBits) != 0 {
                    xMatchFound = pdTRUE;
                }
            } else if (uxBitsWaitedFor & uxCurrentEventBits) == uxBitsWaitedFor {
                // AND wait - all bits match
                xMatchFound = pdTRUE;
            }

            if xMatchFound != pdFALSE {
                if (uxControlBits & eventCLEAR_EVENTS_ON_EXIT_BIT) != 0 {
                    uxBitsToClear |= uxBitsWaitedFor;
                }

                let _ = xTaskRemoveFromUnorderedEventList(
                    pxListItem,
                    uxCurrentEventBits | eventUNBLOCKED_DUE_TO_BIT_SET,
                );
            }

            pxListItem = pxNext;
        }

        uxCurrentEventBits &= !uxBitsToClear;
        prvStoreEventBits(pxEventBits, uxCurrentEventBits);

        uxReturnBits = uxCurrentEventBits;
    }

    /* SAFETY: this consumes the suspension acquired while scanning the event
     * group's wait list above. */
    let _ = unsafe { xTaskResumeAll() };

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
///
/// # Safety
///
/// `xEventGroup` must identify a live event group through the operation and
/// must not be deleted concurrently. This task-context API must not be called
/// from an interrupt handler.
pub unsafe fn xEventGroupClearBits(
    xEventGroup: EventGroupHandle_t,
    uxBitsToClear: EventBits_t,
) -> EventBits_t {
    let pxEventBits = xEventGroup as *mut EventGroup_t;
    let uxReturn: EventBits_t;

    traceENTER_xEventGroupClearBits(xEventGroup, uxBitsToClear);

    configASSERT(!xEventGroup.is_null());
    configASSERT((uxBitsToClear & eventEVENT_BITS_CONTROL_BYTES) == 0);

    taskENTER_CRITICAL();
    unsafe {
        traceEVENT_GROUP_CLEAR_BITS(xEventGroup as *mut c_void, uxBitsToClear as UBaseType_t);

        uxReturn = prvLoadEventBits(pxEventBits);
        prvStoreEventBits(pxEventBits, uxReturn & !uxBitsToClear);
    }
    taskEXIT_CRITICAL();

    traceRETURN_xEventGroupClearBits(uxReturn);

    uxReturn
}

/// Get bits from an event group (from ISR).
///
/// # Safety
///
/// `xEventGroup` must identify a live event group through the operation and
/// must not be deleted concurrently. The port's FromISR calling rules and
/// interrupt-priority restrictions must be satisfied.
pub unsafe fn xEventGroupGetBitsFromISR(xEventGroup: EventGroupHandle_t) -> EventBits_t {
    let pxEventBits = xEventGroup as *const EventGroup_t;
    let uxReturn: EventBits_t;

    traceENTER_xEventGroupGetBitsFromISR(xEventGroup);

    configASSERT(!xEventGroup.is_null());

    unsafe {
        let uxSavedInterruptStatus = taskENTER_CRITICAL_FROM_ISR();
        uxReturn = prvLoadEventBits(pxEventBits);
        taskEXIT_CRITICAL_FROM_ISR(uxSavedInterruptStatus);
    }

    traceRETURN_xEventGroupGetBitsFromISR(uxReturn);

    uxReturn
}

/// Get current bits (convenience function).
///
/// # Safety
///
/// `xEventGroup` must identify a live event group through the operation and
/// must not be deleted concurrently. This task-context API must not be called
/// from an interrupt handler.
#[inline(always)]
pub unsafe fn xEventGroupGetBits(xEventGroup: EventGroupHandle_t) -> EventBits_t {
    xEventGroupClearBits(xEventGroup, 0)
}

// =============================================================================
// Event Group Delete
// =============================================================================

/// Delete an event group.
///
/// # Safety
///
/// `xEventGroup` must identify a live event group and this must be the final
/// deletion request for it. No task or interrupt may start another operation
/// with this handle once deletion begins, and every copy of the handle must be
/// treated as invalid when this function returns.
pub unsafe fn vEventGroupDelete(xEventGroup: EventGroupHandle_t) {
    let pxEventBits = xEventGroup as *mut EventGroup_t;

    traceENTER_vEventGroupDelete(xEventGroup);

    configASSERT(!pxEventBits.is_null());

    unsafe {
        let pxTasksWaitingForBits = ptr::addr_of!((*pxEventBits).xTasksWaitingForBits);

        vTaskSuspendAll();

        traceEVENT_GROUP_DELETE(xEventGroup as *mut c_void);

        while listCURRENT_LIST_LENGTH(pxTasksWaitingForBits) > 0 {
            configASSERT(
                (*pxTasksWaitingForBits).xListEnd.pxNext
                    != ptr::addr_of!((*pxTasksWaitingForBits).xListEnd).cast_mut(),
            );

            let _ = xTaskRemoveFromUnorderedEventList(
                (*pxTasksWaitingForBits).xListEnd.pxNext,
                eventUNBLOCKED_DUE_TO_BIT_SET,
            );
        }
    }

    /* SAFETY: this consumes the suspension acquired while draining the event
     * group's waiter list above. */
    let _ = unsafe { xTaskResumeAll() };

    #[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
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

/// Get the static buffer used by a statically allocated event group.
///
/// # Safety
///
/// `xEventGroup` must identify a live event group through the call.
/// `ppxEventGroupBuffer` must be non-null, aligned, and valid for one pointer
/// write. The returned pointer remains governed by the creation-time storage
/// lifetime and must not be used to alias or move the live group.
pub unsafe fn xEventGroupGetStaticBuffer(
    xEventGroup: EventGroupHandle_t,
    ppxEventGroupBuffer: *mut *mut StaticEventGroup_t,
) -> BaseType_t {
    let pxEventBits = xEventGroup as *mut EventGroup_t;
    let xReturn: BaseType_t;

    traceENTER_xEventGroupGetStaticBuffer(xEventGroup, ppxEventGroupBuffer);

    configASSERT(!pxEventBits.is_null());
    configASSERT(!ppxEventGroupBuffer.is_null());

    #[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
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

    #[cfg(not(any(feature = "alloc", feature = "heap-4", feature = "heap-5")))]
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

/// Callback to set bits (used by `xEventGroupSetBitsFromISR`).
///
/// # Safety
///
/// `pvEventGroup` must identify a live event group and `ulBitsToSet` must not
/// contain event-group control bits. The callback must run in task context
/// (normally the timer daemon), not directly in an interrupt handler.
#[cfg(all(feature = "pend-function-call", feature = "timers"))]
pub unsafe extern "C" fn vEventGroupSetBitsCallback(
    pvEventGroup: *mut c_void,
    ulBitsToSet: crate::kernel::timers::PendedFunctionParameter_t,
) {
    traceENTER_vEventGroupSetBitsCallback(pvEventGroup, ulBitsToSet);

    let _ = xEventGroupSetBits(
        pvEventGroup as EventGroupHandle_t,
        ulBitsToSet as EventBits_t,
    );

    traceRETURN_vEventGroupSetBitsCallback();
}

/// Callback to clear bits (used by `xEventGroupClearBitsFromISR`).
///
/// # Safety
///
/// `pvEventGroup` must identify a live event group and `ulBitsToClear` must not
/// contain event-group control bits. The callback must run in task context
/// (normally the timer daemon), not directly in an interrupt handler.
#[cfg(all(feature = "pend-function-call", feature = "timers"))]
pub unsafe extern "C" fn vEventGroupClearBitsCallback(
    pvEventGroup: *mut c_void,
    ulBitsToClear: crate::kernel::timers::PendedFunctionParameter_t,
) {
    traceENTER_vEventGroupClearBitsCallback(pvEventGroup, ulBitsToClear);

    let _ = xEventGroupClearBits(
        pvEventGroup as EventGroupHandle_t,
        ulBitsToClear as EventBits_t,
    );

    traceRETURN_vEventGroupClearBitsCallback();
}

// =============================================================================
// ISR Functions (require timer pend function call support)
// =============================================================================

/// Set bits from ISR (requires `INCLUDE_xTimerPendFunctionCall`).
///
/// # Safety
///
/// `xEventGroup` must remain live until the timer daemon has executed the
/// deferred callback, not merely until this function returns. If non-null,
/// `pxHigherPriorityTaskWoken` must be aligned and valid for a `BaseType_t`
/// write. The port's FromISR priority restrictions must be satisfied.
#[cfg(all(feature = "pend-function-call", feature = "timers"))]
pub unsafe fn xEventGroupSetBitsFromISR(
    xEventGroup: EventGroupHandle_t,
    uxBitsToSet: EventBits_t,
    pxHigherPriorityTaskWoken: *mut BaseType_t,
) -> BaseType_t {
    use crate::kernel::timers::xTimerPendFunctionCallFromISR;

    traceENTER_xEventGroupSetBitsFromISR(xEventGroup, uxBitsToSet, pxHigherPriorityTaskWoken);

    if xEventGroup.is_null() || (uxBitsToSet & eventEVENT_BITS_CONTROL_BYTES) != 0 {
        traceRETURN_xEventGroupSetBitsFromISR(pdFAIL);
        return pdFAIL;
    }

    traceEVENT_GROUP_SET_BITS_FROM_ISR(xEventGroup as *mut c_void, uxBitsToSet as UBaseType_t);

    /* SAFETY: this API's contract keeps the group live through callback
     * execution and establishes the caller's FromISR pointer invariants. */
    let xReturn = unsafe {
        xTimerPendFunctionCallFromISR(
            vEventGroupSetBitsCallback,
            xEventGroup as *mut c_void,
            uxBitsToSet as crate::kernel::timers::PendedFunctionParameter_t,
            pxHigherPriorityTaskWoken,
        )
    };

    traceRETURN_xEventGroupSetBitsFromISR(xReturn);

    xReturn
}

/// Clear bits from ISR (requires `INCLUDE_xTimerPendFunctionCall`).
///
/// # Safety
///
/// `xEventGroup` must remain live until the timer daemon has executed the
/// deferred callback. The port's FromISR priority restrictions must be
/// satisfied.
#[cfg(all(feature = "pend-function-call", feature = "timers"))]
pub unsafe fn xEventGroupClearBitsFromISR(
    xEventGroup: EventGroupHandle_t,
    uxBitsToClear: EventBits_t,
) -> BaseType_t {
    use crate::kernel::timers::xTimerPendFunctionCallFromISR;

    traceENTER_xEventGroupClearBitsFromISR(xEventGroup, uxBitsToClear);

    if xEventGroup.is_null() || (uxBitsToClear & eventEVENT_BITS_CONTROL_BYTES) != 0 {
        traceRETURN_xEventGroupClearBitsFromISR(pdFAIL);
        return pdFAIL;
    }

    traceEVENT_GROUP_CLEAR_BITS_FROM_ISR(xEventGroup as *mut c_void, uxBitsToClear as UBaseType_t);

    /* SAFETY: this API's contract keeps the group live through callback
     * execution and establishes the caller's FromISR context invariant. */
    let xReturn = unsafe {
        xTimerPendFunctionCallFromISR(
            vEventGroupClearBitsCallback,
            xEventGroup as *mut c_void,
            uxBitsToClear as crate::kernel::timers::PendedFunctionParameter_t,
            ptr::null_mut(),
        )
    };

    traceRETURN_xEventGroupClearBitsFromISR(xReturn);

    xReturn
}

// =============================================================================
// Trace Facility Functions
// =============================================================================

/// Get event group number (for trace facility).
///
/// # Safety
///
/// A non-null `xEventGroup` must identify a live event group through the call.
#[cfg(feature = "trace-facility")]
pub unsafe fn uxEventGroupGetNumber(xEventGroup: EventGroupHandle_t) -> UBaseType_t {
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

/// Set event group number (for trace facility).
///
/// # Safety
///
/// `xEventGroup` must identify a live event group through the call and the
/// caller must serialize this write with other trace-number accesses.
#[cfg(feature = "trace-facility")]
pub unsafe fn vEventGroupSetNumber(
    xEventGroup: EventGroupHandle_t,
    uxEventGroupNumber: UBaseType_t,
) {
    traceENTER_vEventGroupSetNumber(xEventGroup, uxEventGroupNumber);

    configASSERT(!xEventGroup.is_null());

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
fn traceENTER_xEventGroupWaitBits(
    _xEventGroup: EventGroupHandle_t,
    _uxBitsToWaitFor: EventBits_t,
    _xClearOnExit: BaseType_t,
    _xWaitForAllBits: BaseType_t,
    _xTicksToWait: TickType_t,
) {
}
#[inline(always)]
fn traceRETURN_xEventGroupWaitBits(_uxReturn: EventBits_t) {}
#[inline(always)]
fn traceENTER_xEventGroupSync(
    _xEventGroup: EventGroupHandle_t,
    _uxBitsToSet: EventBits_t,
    _uxBitsToWaitFor: EventBits_t,
    _xTicksToWait: TickType_t,
) {
}
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
fn traceENTER_xEventGroupGetStaticBuffer(
    _xEventGroup: EventGroupHandle_t,
    _ppxEventGroupBuffer: *mut *mut StaticEventGroup_t,
) {
}
#[inline(always)]
fn traceRETURN_xEventGroupGetStaticBuffer(_xReturn: BaseType_t) {}
#[inline(always)]
#[cfg(all(feature = "pend-function-call", feature = "timers"))]
fn traceENTER_vEventGroupSetBitsCallback(
    _pvEventGroup: *mut c_void,
    _ulBitsToSet: crate::kernel::timers::PendedFunctionParameter_t,
) {
}
#[inline(always)]
#[cfg(all(feature = "pend-function-call", feature = "timers"))]
fn traceRETURN_vEventGroupSetBitsCallback() {}
#[inline(always)]
#[cfg(all(feature = "pend-function-call", feature = "timers"))]
fn traceENTER_vEventGroupClearBitsCallback(
    _pvEventGroup: *mut c_void,
    _ulBitsToClear: crate::kernel::timers::PendedFunctionParameter_t,
) {
}
#[inline(always)]
#[cfg(all(feature = "pend-function-call", feature = "timers"))]
fn traceRETURN_vEventGroupClearBitsCallback() {}
#[cfg(all(feature = "pend-function-call", feature = "timers"))]
#[inline(always)]
fn traceENTER_xEventGroupSetBitsFromISR(
    _xEventGroup: EventGroupHandle_t,
    _uxBitsToSet: EventBits_t,
    _pxHigherPriorityTaskWoken: *mut BaseType_t,
) {
}
#[cfg(all(feature = "pend-function-call", feature = "timers"))]
#[inline(always)]
fn traceRETURN_xEventGroupSetBitsFromISR(_xReturn: BaseType_t) {}
#[cfg(all(feature = "pend-function-call", feature = "timers"))]
#[inline(always)]
fn traceENTER_xEventGroupClearBitsFromISR(
    _xEventGroup: EventGroupHandle_t,
    _uxBitsToClear: EventBits_t,
) {
}
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
fn traceENTER_vEventGroupSetNumber(
    _xEventGroup: EventGroupHandle_t,
    _uxEventGroupNumber: UBaseType_t,
) {
}
#[cfg(feature = "trace-facility")]
#[inline(always)]
fn traceRETURN_vEventGroupSetNumber() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_event_group_storage_matches_internal_layout() {
        assert_eq!(
            core::mem::size_of::<AtomicEventBits_t>(),
            core::mem::size_of::<EventBits_t>()
        );
        assert_eq!(
            core::mem::align_of::<AtomicEventBits_t>(),
            core::mem::align_of::<EventBits_t>()
        );
        assert_eq!(
            core::mem::size_of::<StaticEventGroup_t>(),
            core::mem::size_of::<EventGroup_t>()
        );
        assert_eq!(
            core::mem::align_of::<StaticEventGroup_t>(),
            core::mem::align_of::<EventGroup_t>()
        );
    }

    #[cfg(feature = "port-test")]
    #[test]
    fn task_side_stores_and_isr_reads_share_one_atomic_event_word() {
        use core::sync::atomic::AtomicBool;

        const FIRST: EventBits_t = 0x0055_5555;
        const SECOND: EventBits_t = 0x00aa_aaaa;
        #[cfg(miri)]
        const TRANSFERS: usize = 500;
        #[cfg(not(miri))]
        const TRANSFERS: usize = 20_000;

        struct SendHandle(*mut EventGroup_t);

        impl SendHandle {
            fn into_raw(self) -> *mut EventGroup_t {
                self.0
            }
        }

        // SAFETY: the fixture leaks the event group for both execution
        // contexts and the only concurrent field is the atomic event word.
        unsafe impl Send for SendHandle {}

        crate::port::test_port_reset();
        let storage = std::boxed::Box::leak(std::boxed::Box::new(StaticEventGroup_t::new()));
        // SAFETY: leaked storage has a stable unique address for the test.
        let event_group = unsafe { xEventGroupCreateStatic(storage) };
        assert!(!event_group.is_null());
        // SAFETY: the group is live and this initializes the first observable
        // value before either host execution context starts sampling it.
        unsafe { prvStoreEventBits(event_group.cast(), FIRST) };

        let writer_handle = SendHandle(event_group.cast());
        let start = AtomicBool::new(false);
        crate::port::test_port_set_inside_interrupt(true);
        std::thread::scope(|scope| {
            let start_ref = &start;
            scope.spawn(move || {
                while !start_ref.load(Ordering::Acquire) {
                    core::hint::spin_loop();
                }

                let event_group = writer_handle.into_raw();
                for index in 0..TRANSFERS {
                    let bits = if index & 1 == 0 { SECOND } else { FIRST };
                    // SAFETY: this writer models the task-side serialized
                    // store, and the leaked event group remains live.
                    unsafe { prvStoreEventBits(event_group, bits) };
                }
            });

            start.store(true, Ordering::Release);
            for _ in 0..TRANSFERS {
                // SAFETY: the test port models a permitted ISR, while atomic
                // storage makes this read race-free with the task-side writer.
                let observed = unsafe { xEventGroupGetBitsFromISR(event_group) };
                assert!(observed == FIRST || observed == SECOND);
            }
        });
        crate::port::test_port_set_inside_interrupt(false);
    }

    #[cfg(all(
        feature = "port-test",
        feature = "pend-function-call",
        feature = "timers"
    ))]
    #[test]
    fn deferred_callbacks_preserve_all_usable_32_bit_event_bits() {
        let mut storage = StaticEventGroup_t::new();
        // SAFETY: storage is unique and stable for this entire sequence; the
        // callbacks run synchronously in port-test task context, and the handle
        // is never used after storage leaves scope.
        unsafe {
            let event_group = xEventGroupCreateStatic(&mut storage);
            assert!(!event_group.is_null());

            let highest_usable_bit = 0x0080_0000 as EventBits_t;
            vEventGroupSetBitsCallback(
                event_group,
                highest_usable_bit as crate::kernel::timers::PendedFunctionParameter_t,
            );
            assert_eq!(xEventGroupGetBits(event_group), highest_usable_bit);

            vEventGroupClearBitsCallback(
                event_group,
                highest_usable_bit as crate::kernel::timers::PendedFunctionParameter_t,
            );
            assert_eq!(xEventGroupGetBits(event_group), 0);
        }
    }

    #[cfg(all(
        feature = "port-test",
        feature = "pend-function-call",
        feature = "timers"
    ))]
    #[test]
    fn from_isr_calls_reject_reserved_control_bits_before_queueing() {
        crate::port::test_port_reset();
        let mut storage = StaticEventGroup_t::new();
        // SAFETY: the storage is uniquely owned and stable for this test.
        let event_group = unsafe { xEventGroupCreateStatic(&mut storage) };
        assert!(!event_group.is_null());

        crate::port::test_port_set_inside_interrupt(true);
        // SAFETY: the test port reports ISR context and the group is live.
        // Reserved bits are an explicitly checked input, not a safety
        // precondition, and neither rejected call reaches the timer queue.
        unsafe {
            assert_eq!(
                xEventGroupSetBitsFromISR(
                    event_group,
                    eventEVENT_BITS_CONTROL_BYTES,
                    ptr::null_mut(),
                ),
                pdFAIL,
            );
            assert_eq!(
                xEventGroupClearBitsFromISR(event_group, eventEVENT_BITS_CONTROL_BYTES),
                pdFAIL,
            );
        }
        crate::port::test_port_set_inside_interrupt(false);
    }
}
