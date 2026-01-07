/*
 * FreeRTOS Kernel <DEVELOPMENT BRANCH>
 * Copyright (C) 2021 Amazon.com, Inc. or its affiliates. All Rights Reserved.
 *
 * SPDX-License-Identifier: MIT
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy of
 * this software and associated documentation files (the "Software"), to deal in
 * the Software without restriction, including without limitation the rights to
 * use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of
 * the Software, and to permit persons to whom the Software is furnished to do so,
 * subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in all
 * copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS
 * FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR
 * COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER
 * IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN
 * CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
 *
 * https://www.FreeRTOS.org
 * https://github.com/FreeRTOS
 *
 */

/*
 * This is the list implementation used by the scheduler.  While it is tailored
 * heavily for the schedulers needs, it is also available for use by
 * application code.
 *
 * list_ts can only store pointers to list_item_ts.  Each ListItem_t contains a
 * numeric value (xItemValue).  Most of the time the lists are sorted in
 * ascending item value order.
 *
 * Lists are created already containing one list item.  The value of this
 * item is the maximum possible that can be stored, it is therefore always at
 * the end of the list and acts as a marker.  The list member pxHead always
 * points to this marker - even though it is at the tail of the list.  This
 * is because the tail contains a wrap back pointer to the true head of
 * the list.
 *
 * In addition to it's value, each list item contains a pointer to the next
 * item in the list (pxNext), a pointer to the list it is in (pxContainer)
 * and a pointer back to the object that contains it.  These later two
 * pointers are included for efficiency of list manipulation.  There is
 * effectively a two way link between the object containing the list item and
 * the list item itself.
 *
 *
 * \page ListIntroduction List Implementation
 * \ingroup FreeRTOSIntro
 */

//! FreeRTOS List Implementation
//!
//! This module provides the doubly-linked list implementation used throughout
//! the FreeRTOS kernel for managing ready lists, delayed task lists, etc.
//!
//! ## Structures
//!
//! - [`ListItem_t`] - A list item that can be inserted into a list
//! - [`MiniListItem_t`] - Optimized list item for end markers (TODO)
//! - [`List_t`] - The list container
//!
//! ## Functions
//!
//! - [`vListInitialise`] - Initialize a list
//! - [`vListInitialiseItem`] - Initialize a list item
//! - [`vListInsert`] - Insert item in sorted order
//! - [`vListInsertEnd`] - Insert item at end (before index)
//! - [`uxListRemove`] - Remove an item from its list

use crate::trace::*;
use crate::types::*;

// =============================================================================
// List Data Integrity Check Macros
// =============================================================================

/*
 * Macros that can be used to place known values within the list structures,
 * then check that the known values do not get corrupted during the execution of
 * the application.   These may catch the list data structures being overwritten in
 * memory.  They will not catch data errors caused by incorrect configuration or
 * use of FreeRTOS.
 */

#[cfg(not(feature = "list-data-integrity-check"))]
mod integrity {
    use super::*;

    #[inline(always)]
    pub fn listSET_FIRST_LIST_ITEM_INTEGRITY_CHECK_VALUE(_pxItem: &mut ListItem_t) {}

    #[inline(always)]
    pub fn listSET_SECOND_LIST_ITEM_INTEGRITY_CHECK_VALUE(_pxItem: &mut ListItem_t) {}

    #[inline(always)]
    pub fn listSET_LIST_INTEGRITY_CHECK_1_VALUE(_pxList: &mut List_t) {}

    #[inline(always)]
    pub fn listSET_LIST_INTEGRITY_CHECK_2_VALUE(_pxList: &mut List_t) {}

    #[inline(always)]
    pub fn listTEST_LIST_ITEM_INTEGRITY(_pxItem: &ListItem_t) {}

    #[inline(always)]
    pub fn listTEST_LIST_INTEGRITY(_pxList: &List_t) {}
}

#[cfg(feature = "list-data-integrity-check")]
mod integrity {
    use super::*;

    #[inline(always)]
    pub fn listSET_FIRST_LIST_ITEM_INTEGRITY_CHECK_VALUE(pxItem: &mut ListItem_t) {
        pxItem.xListItemIntegrityValue1 = pdINTEGRITY_CHECK_VALUE;
    }

    #[inline(always)]
    pub fn listSET_SECOND_LIST_ITEM_INTEGRITY_CHECK_VALUE(pxItem: &mut ListItem_t) {
        pxItem.xListItemIntegrityValue2 = pdINTEGRITY_CHECK_VALUE;
    }

    #[inline(always)]
    pub fn listSET_LIST_INTEGRITY_CHECK_1_VALUE(pxList: &mut List_t) {
        pxList.xListIntegrityValue1 = pdINTEGRITY_CHECK_VALUE;
    }

    #[inline(always)]
    pub fn listSET_LIST_INTEGRITY_CHECK_2_VALUE(pxList: &mut List_t) {
        pxList.xListIntegrityValue2 = pdINTEGRITY_CHECK_VALUE;
    }

    #[inline(always)]
    pub fn listTEST_LIST_ITEM_INTEGRITY(pxItem: &ListItem_t) {
        configASSERT(
            pxItem.xListItemIntegrityValue1 == pdINTEGRITY_CHECK_VALUE
                && pxItem.xListItemIntegrityValue2 == pdINTEGRITY_CHECK_VALUE,
        );
    }

    #[inline(always)]
    pub fn listTEST_LIST_INTEGRITY(pxList: &List_t) {
        configASSERT(
            pxList.xListIntegrityValue1 == pdINTEGRITY_CHECK_VALUE
                && pxList.xListIntegrityValue2 == pdINTEGRITY_CHECK_VALUE,
        );
    }
}

use integrity::*;

// =============================================================================
// Coverage Test Macros (no-ops)
// =============================================================================

/// Coverage test delay marker (no-op)
#[inline(always)]
fn mtCOVERAGE_TEST_DELAY() {}

/// Coverage test marker (no-op)
#[inline(always)]
fn mtCOVERAGE_TEST_MARKER() {}

// =============================================================================
// List Item Structure
// =============================================================================

/*
 * Definition of the only type of object that a list can contain.
 */

/// List item structure
///
/// Each list item contains:
/// - `xItemValue` - The value used for sorting (e.g., wake time, priority)
/// - `pxNext` / `pxPrevious` - Doubly-linked list pointers
/// - `pvOwner` - Pointer back to the owning object (usually a TCB)
/// - `pxContainer` - Pointer to the list this item is in
#[repr(C)]
pub struct ListItem_t {
    /// Set to a known value if configUSE_LIST_DATA_INTEGRITY_CHECK_BYTES is set to 1.
    #[cfg(feature = "list-data-integrity-check")]
    pub xListItemIntegrityValue1: TickType_t,

    /// The value being listed. In most cases this is used to sort the list in ascending order.
    pub xItemValue: TickType_t,

    /// Pointer to the next ListItem_t in the list.
    pub pxNext: *mut ListItem_t,

    /// Pointer to the previous ListItem_t in the list.
    pub pxPrevious: *mut ListItem_t,

    /// Pointer to the object (normally a TCB) that contains the list item.
    /// There is therefore a two way link between the object containing the
    /// list item and the list item itself.
    pub pvOwner: *mut core::ffi::c_void,

    /// Pointer to the list in which this list item is placed (if any).
    pub pxContainer: *mut List_t,

    /// Set to a known value if configUSE_LIST_DATA_INTEGRITY_CHECK_BYTES is set to 1.
    #[cfg(feature = "list-data-integrity-check")]
    pub xListItemIntegrityValue2: TickType_t,
}

impl ListItem_t {
    /// Create a new uninitialized ListItem_t
    ///
    /// [AMENDMENT] Creates a zeroed list item. Use vListInitialiseItem for proper initialization.
    pub const fn new() -> Self {
        ListItem_t {
            #[cfg(feature = "list-data-integrity-check")]
            xListItemIntegrityValue1: 0,
            xItemValue: 0,
            pxNext: core::ptr::null_mut(),
            pxPrevious: core::ptr::null_mut(),
            pvOwner: core::ptr::null_mut(),
            pxContainer: core::ptr::null_mut(),
            #[cfg(feature = "list-data-integrity-check")]
            xListItemIntegrityValue2: 0,
        }
    }
}

// [AMENDMENT] TODO: MiniListItem_t support
// For now, MiniListItem_t is the same as ListItem_t (configUSE_MINI_LIST_ITEM = 0)
/// Mini list item (currently same as ListItem_t)
///
/// [AMENDMENT] TODO: Implement proper MiniListItem_t optimization.
/// When configUSE_MINI_LIST_ITEM is enabled, this would be a smaller struct
/// without pvOwner and pxContainer fields.
pub type MiniListItem_t = ListItem_t;

/// Static list item type for static allocation
/// [AMENDMENT] Same size as ListItem_t for static allocation compatibility
pub type StaticListItem_t = ListItem_t;

// =============================================================================
// List Structure
// =============================================================================

/*
 * Definition of the type of queue used by the scheduler.
 */

/// List structure
///
/// The list contains:
/// - `uxNumberOfItems` - Number of items in the list
/// - `pxIndex` - Used to walk through the list
/// - `xListEnd` - Marker item always at the end
#[repr(C)]
pub struct List_t {
    /// Set to a known value if configUSE_LIST_DATA_INTEGRITY_CHECK_BYTES is set to 1.
    #[cfg(feature = "list-data-integrity-check")]
    pub xListIntegrityValue1: TickType_t,

    /// The number of items in the list.
    pub uxNumberOfItems: UBaseType_t,

    /// Used to walk through the list. Points to the last item returned by a call
    /// to listGET_OWNER_OF_NEXT_ENTRY().
    pub pxIndex: *mut ListItem_t,

    /// List item that contains the maximum possible item value meaning it is
    /// always at the end of the list and is therefore used as a marker.
    pub xListEnd: MiniListItem_t,

    /// Set to a known value if configUSE_LIST_DATA_INTEGRITY_CHECK_BYTES is set to 1.
    #[cfg(feature = "list-data-integrity-check")]
    pub xListIntegrityValue2: TickType_t,
}

impl List_t {
    /// Create a new uninitialized List_t
    ///
    /// [AMENDMENT] Creates a zeroed list. Use vListInitialise for proper initialization.
    pub const fn new() -> Self {
        List_t {
            #[cfg(feature = "list-data-integrity-check")]
            xListIntegrityValue1: 0,
            uxNumberOfItems: 0,
            pxIndex: core::ptr::null_mut(),
            xListEnd: ListItem_t::new(),
            #[cfg(feature = "list-data-integrity-check")]
            xListIntegrityValue2: 0,
        }
    }
}

// =============================================================================
// List Access Macros (as inline functions)
// =============================================================================

/*
 * Access macro to set the owner of a list item.  The owner of a list item
 * is the object (usually a TCB) that contains the list item.
 *
 * \page listSET_LIST_ITEM_OWNER listSET_LIST_ITEM_OWNER
 * \ingroup LinkedList
 */

/// Set the owner of a list item
#[inline(always)]
pub unsafe fn listSET_LIST_ITEM_OWNER(
    pxListItem: *mut ListItem_t,
    pxOwner: *mut core::ffi::c_void,
) {
    (*pxListItem).pvOwner = pxOwner;
}

/*
 * Access macro to get the owner of a list item.  The owner of a list item
 * is the object (usually a TCB) that contains the list item.
 *
 * \page listGET_LIST_ITEM_OWNER listSET_LIST_ITEM_OWNER
 * \ingroup LinkedList
 */

/// Get the owner of a list item
#[inline(always)]
pub unsafe fn listGET_LIST_ITEM_OWNER(pxListItem: *const ListItem_t) -> *mut core::ffi::c_void {
    (*pxListItem).pvOwner
}

/*
 * Access macro to set the value of the list item.  In most cases the value is
 * used to sort the list in ascending order.
 *
 * \page listSET_LIST_ITEM_VALUE listSET_LIST_ITEM_VALUE
 * \ingroup LinkedList
 */

/// Set the value of a list item
#[inline(always)]
pub unsafe fn listSET_LIST_ITEM_VALUE(pxListItem: *mut ListItem_t, xValue: TickType_t) {
    (*pxListItem).xItemValue = xValue;
}

/*
 * Access macro to retrieve the value of the list item.  The value can
 * represent anything - for example the priority of a task, or the time at
 * which a task should be unblocked.
 *
 * \page listGET_LIST_ITEM_VALUE listGET_LIST_ITEM_VALUE
 * \ingroup LinkedList
 */

/// Get the value of a list item
#[inline(always)]
pub unsafe fn listGET_LIST_ITEM_VALUE(pxListItem: *const ListItem_t) -> TickType_t {
    (*pxListItem).xItemValue
}

/*
 * Access macro to retrieve the value of the list item at the head of a given
 * list.
 *
 * \page listGET_LIST_ITEM_VALUE listGET_LIST_ITEM_VALUE
 * \ingroup LinkedList
 */

/// Get the value of the head entry
#[inline(always)]
pub unsafe fn listGET_ITEM_VALUE_OF_HEAD_ENTRY(pxList: *const List_t) -> TickType_t {
    (*(*pxList).xListEnd.pxNext).xItemValue
}

/*
 * Return the list item at the head of the list.
 *
 * \page listGET_HEAD_ENTRY listGET_HEAD_ENTRY
 * \ingroup LinkedList
 */

/// Get the head entry of a list
#[inline(always)]
pub unsafe fn listGET_HEAD_ENTRY(pxList: *const List_t) -> *mut ListItem_t {
    (*pxList).xListEnd.pxNext
}

/*
 * Return the next list item.
 *
 * \page listGET_NEXT listGET_NEXT
 * \ingroup LinkedList
 */

/// Get the next item in the list
#[inline(always)]
pub unsafe fn listGET_NEXT(pxListItem: *const ListItem_t) -> *mut ListItem_t {
    (*pxListItem).pxNext
}

/*
 * Return the list item that marks the end of the list
 *
 * \page listGET_END_MARKER listGET_END_MARKER
 * \ingroup LinkedList
 */

/// Get the end marker of a list
#[inline(always)]
pub unsafe fn listGET_END_MARKER(pxList: *const List_t) -> *const ListItem_t {
    &(*pxList).xListEnd as *const MiniListItem_t as *const ListItem_t
}

/*
 * Access macro to determine if a list contains any items.  The macro will
 * only have the value true if the list is empty.
 *
 * \page listLIST_IS_EMPTY listLIST_IS_EMPTY
 * \ingroup LinkedList
 */

/// Check if a list is empty
#[inline(always)]
pub unsafe fn listLIST_IS_EMPTY(pxList: *const List_t) -> BaseType_t {
    if (*pxList).uxNumberOfItems == 0 {
        pdTRUE
    } else {
        pdFALSE
    }
}

/*
 * Access macro to return the number of items in the list.
 */

/// Get the current length of a list
#[inline(always)]
pub unsafe fn listCURRENT_LIST_LENGTH(pxList: *const List_t) -> UBaseType_t {
    (*pxList).uxNumberOfItems
}

/*
 * Access function to obtain the owner of the first entry in a list.  Lists
 * are normally sorted in ascending item value order.
 *
 * \page listGET_OWNER_OF_HEAD_ENTRY listGET_OWNER_OF_HEAD_ENTRY
 * \ingroup LinkedList
 */

/// Get the owner of the head entry
#[inline(always)]
pub unsafe fn listGET_OWNER_OF_HEAD_ENTRY(pxList: *const List_t) -> *mut core::ffi::c_void {
    (*(*pxList).xListEnd.pxNext).pvOwner
}

/*
 * Check to see if a list item is within a list.  The list item maintains a
 * "container" pointer that points to the list it is in.  All this macro does
 * is check to see if the container and the list match.
 *
 * @param pxList The list we want to know if the list item is within.
 * @param pxListItem The list item we want to know if is in the list.
 * @return pdTRUE if the list item is in the list, otherwise pdFALSE.
 */

/// Check if a list item is contained within a list
#[inline(always)]
pub unsafe fn listIS_CONTAINED_WITHIN(
    pxList: *const List_t,
    pxListItem: *const ListItem_t,
) -> BaseType_t {
    if (*pxListItem).pxContainer == pxList as *mut List_t {
        pdTRUE
    } else {
        pdFALSE
    }
}

/*
 * Return the list a list item is contained within (referenced from).
 *
 * @param pxListItem The list item being queried.
 * @return A pointer to the List_t object that references the pxListItem
 */

/// Get the container of a list item
#[inline(always)]
pub unsafe fn listLIST_ITEM_CONTAINER(pxListItem: *const ListItem_t) -> *mut List_t {
    (*pxListItem).pxContainer
}

/*
 * This provides a crude means of knowing if a list has been initialised, as
 * pxList->xListEnd.xItemValue is set to portMAX_DELAY by the vListInitialise()
 * function.
 */

/// Check if a list has been initialized
#[inline(always)]
pub unsafe fn listLIST_IS_INITIALISED(pxList: *const List_t) -> BaseType_t {
    if (*pxList).xListEnd.xItemValue == portMAX_DELAY {
        pdTRUE
    } else {
        pdFALSE
    }
}

// =============================================================================
// listGET_OWNER_OF_NEXT_ENTRY macro
// =============================================================================

/*
 * Access function to obtain the owner of the next entry in a list.
 *
 * The list member pxIndex is used to walk through a list.  Calling
 * listGET_OWNER_OF_NEXT_ENTRY increments pxIndex to the next item in the list
 * and returns that entry's pxOwner parameter.  Using multiple calls to this
 * function it is therefore possible to move through every item contained in
 * a list.
 *
 * The pxOwner parameter of a list item is a pointer to the object that owns
 * the list item.  In the scheduler this is normally a task control block.
 * The pxOwner parameter effectively creates a two way link between the list
 * item and its owner.
 *
 * @param pxTCB pxTCB is set to the address of the owner of the next list item.
 * @param pxList The list from which the next item owner is to be returned.
 *
 * \page listGET_OWNER_OF_NEXT_ENTRY listGET_OWNER_OF_NEXT_ENTRY
 * \ingroup LinkedList
 */

/// Get owner of next entry and advance the index
///
/// [AMENDMENT] This is a macro in C that modifies pxTCB. In Rust, we return
/// the owner and let the caller assign it.
#[inline(always)]
pub unsafe fn listGET_OWNER_OF_NEXT_ENTRY(pxList: *mut List_t) -> *mut core::ffi::c_void {
    let pxConstList = pxList;
    /* Increment the index to the next item and return the item, ensuring */
    /* we don't return the marker used at the end of the list.  */
    (*pxConstList).pxIndex = (*(*pxConstList).pxIndex).pxNext;
    if (*pxConstList).pxIndex as *const ListItem_t == listGET_END_MARKER(pxConstList) {
        (*pxConstList).pxIndex = (*(*pxConstList).xListEnd.pxNext).pxNext;
        // [AMENDMENT] Fixed: should be xListEnd.pxNext, not xListEnd.pxNext.pxNext
        (*pxConstList).pxIndex = (*pxConstList).xListEnd.pxNext;
    }
    (*(*pxConstList).pxIndex).pvOwner
}

// =============================================================================
// PUBLIC LIST API documented in list.h
// =============================================================================

/*-----------------------------------------------------------
 * PUBLIC LIST API documented in list.h
 *----------------------------------------------------------*/

/// Initialize a list
///
/// Must be called before a list is used! This initialises all the members
/// of the list structure and inserts the xListEnd item into the list as a
/// marker to the back of the list.
///
/// # Safety
///
/// `pxList` must point to valid, allocated memory for a `List_t`.
pub unsafe fn vListInitialise(pxList: *mut List_t) {
    traceENTER_vListInitialise(pxList);

    /* The list structure contains a list item which is used to mark the
     * end of the list.  To initialise the list the list end is inserted
     * as the only list entry. */
    (*pxList).pxIndex = &mut (*pxList).xListEnd as *mut MiniListItem_t as *mut ListItem_t;

    listSET_FIRST_LIST_ITEM_INTEGRITY_CHECK_VALUE(&mut (*pxList).xListEnd);

    /* The list end value is the highest possible value in the list to
     * ensure it remains at the end of the list. */
    (*pxList).xListEnd.xItemValue = portMAX_DELAY;

    /* The list end next and previous pointers point to itself so we know
     * when the list is empty. */
    (*pxList).xListEnd.pxNext = &mut (*pxList).xListEnd as *mut MiniListItem_t as *mut ListItem_t;
    (*pxList).xListEnd.pxPrevious =
        &mut (*pxList).xListEnd as *mut MiniListItem_t as *mut ListItem_t;

    /* Initialize the remaining fields of xListEnd when it is a proper ListItem_t */
    // [AMENDMENT] configUSE_MINI_LIST_ITEM == 0, so we always initialize these
    (*pxList).xListEnd.pvOwner = core::ptr::null_mut();
    (*pxList).xListEnd.pxContainer = core::ptr::null_mut();
    listSET_SECOND_LIST_ITEM_INTEGRITY_CHECK_VALUE(&mut (*pxList).xListEnd);

    (*pxList).uxNumberOfItems = 0;

    /* Write known values into the list if
     * configUSE_LIST_DATA_INTEGRITY_CHECK_BYTES is set to 1. */
    listSET_LIST_INTEGRITY_CHECK_1_VALUE(&mut *pxList);
    listSET_LIST_INTEGRITY_CHECK_2_VALUE(&mut *pxList);

    traceRETURN_vListInitialise();
}
/*-----------------------------------------------------------*/

/// Initialize a list item
///
/// Must be called before a list item is used. This sets the list container to
/// null so the item does not think that it is already contained in a list.
///
/// # Safety
///
/// `pxItem` must point to valid, allocated memory for a `ListItem_t`.
pub unsafe fn vListInitialiseItem(pxItem: *mut ListItem_t) {
    traceENTER_vListInitialiseItem(pxItem);

    /* Make sure the list item is not recorded as being on a list. */
    (*pxItem).pxContainer = core::ptr::null_mut();

    /* Write known values into the list item if
     * configUSE_LIST_DATA_INTEGRITY_CHECK_BYTES is set to 1. */
    listSET_FIRST_LIST_ITEM_INTEGRITY_CHECK_VALUE(&mut *pxItem);
    listSET_SECOND_LIST_ITEM_INTEGRITY_CHECK_VALUE(&mut *pxItem);

    traceRETURN_vListInitialiseItem();
}
/*-----------------------------------------------------------*/

/// Insert a list item at the end of the list
///
/// Insert a list item into a list. The item will be inserted in a position
/// such that it will be the last item within the list returned by multiple
/// calls to listGET_OWNER_OF_NEXT_ENTRY.
///
/// # Safety
///
/// Both pointers must be valid. `pxNewListItem` must not already be in a list.
pub unsafe fn vListInsertEnd(pxList: *mut List_t, pxNewListItem: *mut ListItem_t) {
    let pxIndex: *mut ListItem_t = (*pxList).pxIndex;

    traceENTER_vListInsertEnd(pxList, pxNewListItem);

    /* Only effective when configASSERT() is also defined, these tests may catch
     * the list data structures being overwritten in memory.  They will not catch
     * data errors caused by incorrect configuration or use of FreeRTOS. */
    listTEST_LIST_INTEGRITY(&*pxList);
    listTEST_LIST_ITEM_INTEGRITY(&*pxNewListItem);

    /* Insert a new list item into pxList, but rather than sort the list,
     * makes the new list item the last item to be removed by a call to
     * listGET_OWNER_OF_NEXT_ENTRY(). */
    (*pxNewListItem).pxNext = pxIndex;
    (*pxNewListItem).pxPrevious = (*pxIndex).pxPrevious;

    /* Only used during decision coverage testing. */
    mtCOVERAGE_TEST_DELAY();

    (*(*pxIndex).pxPrevious).pxNext = pxNewListItem;
    (*pxIndex).pxPrevious = pxNewListItem;

    /* Remember which list the item is in. */
    (*pxNewListItem).pxContainer = pxList;

    (*pxList).uxNumberOfItems = (*pxList).uxNumberOfItems + 1;

    traceRETURN_vListInsertEnd();
}
/*-----------------------------------------------------------*/

/// Insert a list item in sorted order
///
/// Insert a list item into a list. The item will be inserted into the list in
/// a position determined by its item value (ascending item value order).
///
/// # Safety
///
/// Both pointers must be valid. `pxNewListItem` must not already be in a list.
pub unsafe fn vListInsert(pxList: *mut List_t, pxNewListItem: *mut ListItem_t) {
    let mut pxIterator: *mut ListItem_t;
    let xValueOfInsertion: TickType_t = (*pxNewListItem).xItemValue;

    traceENTER_vListInsert(pxList, pxNewListItem);

    /* Only effective when configASSERT() is also defined, these tests may catch
     * the list data structures being overwritten in memory.  They will not catch
     * data errors caused by incorrect configuration or use of FreeRTOS. */
    listTEST_LIST_INTEGRITY(&*pxList);
    listTEST_LIST_ITEM_INTEGRITY(&*pxNewListItem);

    /* Insert the new list item into the list, sorted in xItemValue order.
     *
     * If the list already contains a list item with the same item value then the
     * new list item should be placed after it.  This ensures that TCBs which are
     * stored in ready lists (all of which have the same xItemValue value) get a
     * share of the CPU.  However, if the xItemValue is the same as the back marker
     * the iteration loop below will not end.  Therefore the value is checked
     * first, and the algorithm slightly modified if necessary. */
    if xValueOfInsertion == portMAX_DELAY {
        pxIterator = (*pxList).xListEnd.pxPrevious;
    } else {
        /* *** NOTE ***********************************************************
         *  If you find your application is crashing here then likely causes are
         *  listed below.  In addition see https://www.freertos.org/Why-FreeRTOS/FAQs for
         *  more tips, and ensure configASSERT() is defined!
         *  https://www.FreeRTOS.org/a00110.html#configASSERT
         *
         *   1) Stack overflow -
         *      see https://www.FreeRTOS.org/Stacks-and-stack-overflow-checking.html
         *   2) Incorrect interrupt priority assignment, especially on Cortex-M
         *      parts where numerically high priority values denote low actual
         *      interrupt priorities, which can seem counter intuitive.  See
         *      https://www.FreeRTOS.org/RTOS-Cortex-M3-M4.html and the definition
         *      of configMAX_SYSCALL_INTERRUPT_PRIORITY on
         *      https://www.FreeRTOS.org/a00110.html
         *   3) Calling an API function from within a critical section or when
         *      the scheduler is suspended, or calling an API function that does
         *      not end in "FromISR" from an interrupt.
         *   4) Using a queue or semaphore before it has been initialised or
         *      before the scheduler has been started (are interrupts firing
         *      before vTaskStartScheduler() has been called?).
         *   5) If the FreeRTOS port supports interrupt nesting then ensure that
         *      the priority of the tick interrupt is at or below
         *      configMAX_SYSCALL_INTERRUPT_PRIORITY.
         **********************************************************************/

        pxIterator = &mut (*pxList).xListEnd as *mut MiniListItem_t as *mut ListItem_t;
        while (*(*pxIterator).pxNext).xItemValue <= xValueOfInsertion {
            pxIterator = (*pxIterator).pxNext;
            /* There is nothing to do here, just iterating to the wanted
             * insertion position.
             * IF YOU FIND YOUR CODE STUCK HERE, SEE THE NOTE JUST ABOVE.
             */
        }
    }

    (*pxNewListItem).pxNext = (*pxIterator).pxNext;
    (*(*pxNewListItem).pxNext).pxPrevious = pxNewListItem;
    (*pxNewListItem).pxPrevious = pxIterator;
    (*pxIterator).pxNext = pxNewListItem;

    /* Remember which list the item is in.  This allows fast removal of the
     * item later. */
    (*pxNewListItem).pxContainer = pxList;

    (*pxList).uxNumberOfItems = (*pxList).uxNumberOfItems + 1;

    traceRETURN_vListInsert();
}
/*-----------------------------------------------------------*/

/// Remove a list item from its list
///
/// Remove an item from a list. The list item has a pointer to the list that
/// it is in, so only the list item need be passed into the function.
///
/// # Returns
///
/// The number of items that remain in the list after the list item has
/// been removed.
///
/// # Safety
///
/// `pxItemToRemove` must be in a valid list.
pub unsafe fn uxListRemove(pxItemToRemove: *mut ListItem_t) -> UBaseType_t {
    /* The list item knows which list it is in.  Obtain the list from the list
     * item. */
    let pxList: *mut List_t = (*pxItemToRemove).pxContainer;

    traceENTER_uxListRemove(pxItemToRemove);

    (*(*pxItemToRemove).pxNext).pxPrevious = (*pxItemToRemove).pxPrevious;
    (*(*pxItemToRemove).pxPrevious).pxNext = (*pxItemToRemove).pxNext;

    /* Only used during decision coverage testing. */
    mtCOVERAGE_TEST_DELAY();

    /* Make sure the index is left pointing to a valid item. */
    if (*pxList).pxIndex == pxItemToRemove {
        (*pxList).pxIndex = (*pxItemToRemove).pxPrevious;
    } else {
        mtCOVERAGE_TEST_MARKER();
    }

    (*pxItemToRemove).pxContainer = core::ptr::null_mut();
    (*pxList).uxNumberOfItems = (*pxList).uxNumberOfItems - 1;

    traceRETURN_uxListRemove((*pxList).uxNumberOfItems);

    (*pxList).uxNumberOfItems
}
/*-----------------------------------------------------------*/

// =============================================================================
// Inline macro equivalents (listREMOVE_ITEM, listINSERT_END)
// =============================================================================

/// Inline version of uxListRemove that doesn't return a value
///
/// Version of uxListRemove() that does not return a value. Provided as a slight
/// optimisation for xTaskIncrementTick() by being inline.
///
/// # Safety
///
/// `pxItemToRemove` must be in a valid list.
#[inline(always)]
pub unsafe fn listREMOVE_ITEM(pxItemToRemove: *mut ListItem_t) {
    /* The list item knows which list it is in.  Obtain the list from the list
     * item. */
    let pxList: *mut List_t = (*pxItemToRemove).pxContainer;

    (*(*pxItemToRemove).pxNext).pxPrevious = (*pxItemToRemove).pxPrevious;
    (*(*pxItemToRemove).pxPrevious).pxNext = (*pxItemToRemove).pxNext;

    /* Make sure the index is left pointing to a valid item. */
    if (*pxList).pxIndex == pxItemToRemove {
        (*pxList).pxIndex = (*pxItemToRemove).pxPrevious;
    }

    (*pxItemToRemove).pxContainer = core::ptr::null_mut();
    (*pxList).uxNumberOfItems = (*pxList).uxNumberOfItems - 1;
}

/// Inline version of vListInsertEnd
///
/// Inline version of vListInsertEnd() to provide slight optimisation for
/// xTaskIncrementTick().
///
/// # Safety
///
/// Both pointers must be valid. `pxNewListItem` must not already be in a list.
#[inline(always)]
pub unsafe fn listINSERT_END(pxList: *mut List_t, pxNewListItem: *mut ListItem_t) {
    let pxIndex: *mut ListItem_t = (*pxList).pxIndex;

    /* Only effective when configASSERT() is also defined, these tests may catch
     * the list data structures being overwritten in memory.  They will not catch
     * data errors caused by incorrect configuration or use of FreeRTOS. */
    listTEST_LIST_INTEGRITY(&*pxList);
    listTEST_LIST_ITEM_INTEGRITY(&*pxNewListItem);

    /* Insert a new list item into ( pxList ), but rather than sort the list,
     * makes the new list item the last item to be removed by a call to
     * listGET_OWNER_OF_NEXT_ENTRY(). */
    (*pxNewListItem).pxNext = pxIndex;
    (*pxNewListItem).pxPrevious = (*pxIndex).pxPrevious;

    (*(*pxIndex).pxPrevious).pxNext = pxNewListItem;
    (*pxIndex).pxPrevious = pxNewListItem;

    /* Remember which list the item is in. */
    (*pxNewListItem).pxContainer = pxList;

    (*pxList).uxNumberOfItems = (*pxList).uxNumberOfItems + 1;
}
