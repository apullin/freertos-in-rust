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

//! FreeRTOS Queue Implementation
//!
//! This module provides queues, semaphores, and mutexes.
//! Items are queued by copy, not reference.
//!
//! ## Queue Types
//!
//! - Base queue - FIFO data queue
//! - Binary semaphore - For synchronization
//! - Counting semaphore - For resource counting
//! - Mutex - For mutual exclusion with priority inheritance
//! - Recursive mutex - Can be taken multiple times by same task

use crate::config::*;
use crate::kernel::list::*;
use crate::kernel::tasks::*;
use crate::memory::*;
use crate::port::*;
use crate::trace::*;
use crate::types::*;
use core::ffi::c_void;
use core::ptr;

// =============================================================================
// Queue Lock Constants
// =============================================================================

/// Queue is unlocked
const queueUNLOCKED: i8 = -1;

/// Queue is locked but unmodified
const queueLOCKED_UNMODIFIED: i8 = 0;

/// Maximum value for i8
const queueINT8_MAX: i8 = 127;

// =============================================================================
// Queue Position Constants (from queue.h)
// =============================================================================

/// Send to back of queue
pub const queueSEND_TO_BACK: BaseType_t = 0;

/// Send to front of queue
pub const queueSEND_TO_FRONT: BaseType_t = 1;

/// Overwrite the queue (for queues of length 1)
pub const queueOVERWRITE: BaseType_t = 2;

// =============================================================================
// Queue Type Constants (from queue.h)
// =============================================================================

/// Base queue type
pub const queueQUEUE_TYPE_BASE: u8 = 0;

/// Mutex type
pub const queueQUEUE_TYPE_MUTEX: u8 = 1;

/// Counting semaphore type
pub const queueQUEUE_TYPE_COUNTING_SEMAPHORE: u8 = 2;

/// Binary semaphore type
pub const queueQUEUE_TYPE_BINARY_SEMAPHORE: u8 = 3;

/// Recursive mutex type
pub const queueQUEUE_TYPE_RECURSIVE_MUTEX: u8 = 4;

/// Queue set type
pub const queueQUEUE_TYPE_SET: u8 = 5;

// =============================================================================
// Semaphore Constants
// =============================================================================

/// Semaphores have item size of 0
const queueSEMAPHORE_QUEUE_ITEM_LENGTH: UBaseType_t = 0;

/// Mutex give should not block
const queueMUTEX_GIVE_BLOCK_TIME: TickType_t = 0;

// =============================================================================
// Queue Pointers Structure (for queue mode)
// =============================================================================

/// Data required exclusively when structure is used as a queue
#[repr(C)]
#[derive(Clone, Copy)]
pub struct QueuePointers_t {
    /// Points to the byte at the end of the queue storage area
    pub pcTail: *mut i8,
    /// Points to the last place that a queued item was read from
    pub pcReadFrom: *mut i8,
}

// =============================================================================
// Semaphore Data Structure (for mutex mode)
// =============================================================================

/// Data required exclusively when structure is used as a semaphore/mutex
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SemaphoreData_t {
    /// The handle of the task that holds the mutex
    pub xMutexHolder: TaskHandle_t,
    /// Recursive call count for recursive mutexes
    pub uxRecursiveCallCount: UBaseType_t,
}

// =============================================================================
// Queue/Semaphore Union
// =============================================================================

/// Union of queue and semaphore data
///
/// [AMENDMENT] Using Copy-able raw types to avoid ManuallyDrop complexity.
/// Both variants contain only raw pointers and primitive types which are Copy.
#[repr(C)]
#[derive(Clone, Copy)]
pub union QueueUnion_t {
    /// Queue-specific data
    pub xQueue: QueuePointers_t,
    /// Semaphore/mutex-specific data
    pub xSemaphore: SemaphoreData_t,
}

// =============================================================================
// Queue Definition Structure
// =============================================================================

/*
 * Definition of the queue used by the scheduler.
 * Items are queued by copy, not reference.  See the following link for the
 * rationale: https://www.FreeRTOS.org/Embedded-RTOS-Queues.html
 */

/// The old naming convention is used to prevent breaking kernel aware debuggers.
#[repr(C)]
pub struct xQUEUE {
    /// Points to the beginning of the queue storage area.
    /// [AMENDMENT] Also used as uxQueueType - NULL means mutex
    pub pcHead: *mut i8,

    /// Points to the free next place in the storage area.
    pub pcWriteTo: *mut i8,

    /// Data required for queue or semaphore operation
    pub u: QueueUnion_t,

    /// List of tasks blocked waiting to post onto this queue
    pub xTasksWaitingToSend: List_t,

    /// List of tasks blocked waiting to read from this queue
    pub xTasksWaitingToReceive: List_t,

    /// The number of items currently in the queue
    pub uxMessagesWaiting: UBaseType_t,

    /// The length of the queue (number of items it will hold)
    pub uxLength: UBaseType_t,

    /// The size of each item in bytes
    pub uxItemSize: UBaseType_t,

    /// Stores the number of items received while locked
    pub cRxLock: i8,

    /// Stores the number of items transmitted while locked
    pub cTxLock: i8,

    /// Set to pdTRUE if statically allocated
    #[cfg(all(
        feature = "alloc", // configSUPPORT_DYNAMIC_ALLOCATION
    ))]
    pub ucStaticallyAllocated: u8,

    /// Pointer to the queue set this queue/semaphore belongs to (if any)
    #[cfg(feature = "queue-sets")]
    pub pxQueueSetContainer: *mut xQUEUE,

    // TODO: Trace facility support
    // #[cfg(feature = "trace-facility")]
    // pub uxQueueNumber: UBaseType_t,
    // #[cfg(feature = "trace-facility")]
    // pub ucQueueType: u8,
}

/// Typedef for Queue_t
pub type Queue_t = xQUEUE;

/// Queue handle type
pub type QueueHandle_t = *mut Queue_t;

/// Queue set handle type (a queue set is itself a queue)
#[cfg(feature = "queue-sets")]
pub type QueueSetHandle_t = *mut Queue_t;

/// Queue set member handle type (a queue or semaphore that belongs to a set)
#[cfg(feature = "queue-sets")]
pub type QueueSetMemberHandle_t = *mut Queue_t;

// =============================================================================
// Static Queue (for static allocation)
// =============================================================================

/// StaticQueue_t - same size as Queue_t for static allocation
/// [AMENDMENT] This must be the same size as Queue_t
#[repr(C)]
pub struct StaticQueue_t {
    _data: xQUEUE,
}

// =============================================================================
// Helper Macros as Functions
// =============================================================================

/// Coverage test delay (no-op)
#[inline(always)]
fn mtCOVERAGE_TEST_DELAY() {}

/// Coverage test marker (no-op)
#[inline(always)]
fn mtCOVERAGE_TEST_MARKER() {}

/// Yield if using preemption
#[inline(always)]
fn queueYIELD_IF_USING_PREEMPTION() {
    if configUSE_PREEMPTION != 0 {
        portYIELD_WITHIN_API();
    }
}

/// Lock a queue
#[inline(always)]
unsafe fn prvLockQueue(pxQueue: *mut Queue_t) {
    taskENTER_CRITICAL();
    {
        if (*pxQueue).cRxLock == queueUNLOCKED {
            (*pxQueue).cRxLock = queueLOCKED_UNMODIFIED;
        }
        if (*pxQueue).cTxLock == queueUNLOCKED {
            (*pxQueue).cTxLock = queueLOCKED_UNMODIFIED;
        }
    }
    taskEXIT_CRITICAL();
}

// =============================================================================
// Queue Reset
// =============================================================================

/// Reset a queue to its initial state
///
/// # Safety
///
/// xQueue must be a valid queue handle
pub unsafe fn xQueueGenericReset(xQueue: QueueHandle_t, xNewQueue: BaseType_t) -> BaseType_t {
    let mut xReturn: BaseType_t = pdPASS;
    let pxQueue: *mut Queue_t = xQueue;

    traceENTER_xQueueGenericReset(pxQueue as *mut c_void, xNewQueue);

    configASSERT(!pxQueue.is_null());

    if !pxQueue.is_null()
        && (*pxQueue).uxLength >= 1
        && (usize::MAX / (*pxQueue).uxLength as usize) >= (*pxQueue).uxItemSize as usize
    {
        taskENTER_CRITICAL();
        {
            (*pxQueue).u.xQueue.pcTail = (*pxQueue)
                .pcHead
                .add(((*pxQueue).uxLength * (*pxQueue).uxItemSize) as usize);
            (*pxQueue).uxMessagesWaiting = 0;
            (*pxQueue).pcWriteTo = (*pxQueue).pcHead;
            (*pxQueue).u.xQueue.pcReadFrom = (*pxQueue)
                .pcHead
                .add((((*pxQueue).uxLength - 1) * (*pxQueue).uxItemSize) as usize);
            (*pxQueue).cRxLock = queueUNLOCKED;
            (*pxQueue).cTxLock = queueUNLOCKED;

            if xNewQueue == pdFALSE {
                /* If there are tasks blocked waiting to read from the queue, then
                 * the tasks will remain blocked as after this function exits the queue
                 * will still be empty.  If there are tasks blocked waiting to write to
                 * the queue, then one should be unblocked as after this function exits
                 * it will be possible to write to it. */
                if listLIST_IS_EMPTY(&(*pxQueue).xTasksWaitingToSend) == pdFALSE {
                    if xTaskRemoveFromEventList(&(*pxQueue).xTasksWaitingToSend) != pdFALSE {
                        queueYIELD_IF_USING_PREEMPTION();
                    } else {
                        mtCOVERAGE_TEST_MARKER();
                    }
                } else {
                    mtCOVERAGE_TEST_MARKER();
                }
            } else {
                /* Ensure the event queues start in the correct state. */
                vListInitialise(&mut (*pxQueue).xTasksWaitingToSend);
                vListInitialise(&mut (*pxQueue).xTasksWaitingToReceive);
            }
        }
        taskEXIT_CRITICAL();
    } else {
        xReturn = pdFAIL;
    }

    configASSERT(xReturn != pdFAIL);

    traceRETURN_xQueueGenericReset(xReturn);

    xReturn
}

// =============================================================================
// Queue Initialization
// =============================================================================

/// Initialize a new queue structure
unsafe fn prvInitialiseNewQueue(
    uxQueueLength: UBaseType_t,
    uxItemSize: UBaseType_t,
    pucQueueStorage: *mut u8,
    ucQueueType: u8,
    pxNewQueue: *mut Queue_t,
) {
    /* Remove compiler warnings about unused parameters */
    let _ = ucQueueType;

    if uxItemSize == 0 {
        /* No RAM was allocated for the queue storage area, but PC head cannot
         * be set to NULL because NULL is used as a key to say the queue is used as
         * a mutex.  Therefore just set pcHead to point to the queue as a benign
         * value that is known to be within the memory map. */
        (*pxNewQueue).pcHead = pxNewQueue as *mut i8;
    } else {
        /* Set the head to the start of the queue storage area. */
        (*pxNewQueue).pcHead = pucQueueStorage as *mut i8;
    }

    /* Initialise the queue members as described where the queue type is
     * defined. */
    (*pxNewQueue).uxLength = uxQueueLength;
    (*pxNewQueue).uxItemSize = uxItemSize;
    xQueueGenericReset(pxNewQueue, pdTRUE);

    // TODO: Trace facility
    // (*pxNewQueue).ucQueueType = ucQueueType;

    // Initialize queue set container pointer
    #[cfg(feature = "queue-sets")]
    {
        (*pxNewQueue).pxQueueSetContainer = ptr::null_mut();
    }

    traceQUEUE_CREATE(pxNewQueue as *mut c_void);
}

// =============================================================================
// Static Queue Creation
// =============================================================================

/// Create a queue using statically allocated memory
///
/// # Safety
///
/// pucQueueStorage must point to valid memory of size uxQueueLength * uxItemSize
/// pxStaticQueue must point to valid memory for StaticQueue_t
pub unsafe fn xQueueGenericCreateStatic(
    uxQueueLength: UBaseType_t,
    uxItemSize: UBaseType_t,
    pucQueueStorage: *mut u8,
    pxStaticQueue: *mut StaticQueue_t,
    ucQueueType: u8,
) -> QueueHandle_t {
    let mut pxNewQueue: *mut Queue_t = ptr::null_mut();

    traceENTER_xQueueGenericCreateStatic(
        uxQueueLength,
        uxItemSize,
        pucQueueStorage as *mut c_void,
        pxStaticQueue as *mut c_void,
        ucQueueType,
    );

    /* The StaticQueue_t structure and the queue storage area must be
     * supplied. */
    configASSERT(!pxStaticQueue.is_null());

    if uxQueueLength > 0
        && !pxStaticQueue.is_null()
        /* A queue storage area should be provided if the item size is not 0, and
         * should not be provided if the item size is 0. */
        && !((!pucQueueStorage.is_null()) && (uxItemSize == 0))
        && !((pucQueueStorage.is_null()) && (uxItemSize != 0))
    {
        /* The address of a statically allocated queue was passed in, use it. */
        pxNewQueue = pxStaticQueue as *mut Queue_t;

        #[cfg(feature = "alloc")]
        {
            /* Queues can be allocated either statically or dynamically, so
             * note this queue was allocated statically in case the queue is
             * later deleted. */
            (*pxNewQueue).ucStaticallyAllocated = pdTRUE as u8;
        }

        prvInitialiseNewQueue(
            uxQueueLength,
            uxItemSize,
            pucQueueStorage,
            ucQueueType,
            pxNewQueue,
        );
    } else {
        configASSERT(!pxNewQueue.is_null());
        mtCOVERAGE_TEST_MARKER();
    }

    traceRETURN_xQueueGenericCreateStatic(pxNewQueue as *mut c_void);

    pxNewQueue
}

// =============================================================================
// Dynamic Queue Creation
// =============================================================================

/// Create a queue using dynamically allocated memory
///
/// # Safety
///
/// Requires the `alloc` feature for dynamic allocation
#[cfg(feature = "alloc")]
pub unsafe fn xQueueGenericCreate(
    uxQueueLength: UBaseType_t,
    uxItemSize: UBaseType_t,
    ucQueueType: u8,
) -> QueueHandle_t {
    let mut pxNewQueue: *mut Queue_t = ptr::null_mut();

    traceENTER_xQueueGenericCreate(uxQueueLength, uxItemSize, ucQueueType);

    if uxQueueLength > 0
        && (usize::MAX / uxQueueLength as usize) >= uxItemSize as usize
        && (usize::MAX - core::mem::size_of::<Queue_t>())
            >= (uxQueueLength as usize * uxItemSize as usize)
    {
        /* Allocate enough space to hold the maximum number of items that
         * can be in the queue at any time.  It is valid for uxItemSize to be
         * zero in the case the queue is used as a semaphore. */
        let xQueueSizeInBytes: usize = uxQueueLength as usize * uxItemSize as usize;

        pxNewQueue =
            pvPortMalloc(core::mem::size_of::<Queue_t>() + xQueueSizeInBytes) as *mut Queue_t;

        if !pxNewQueue.is_null() {
            /* Jump past the queue structure to find the location of the queue
             * storage area. */
            let pucQueueStorage: *mut u8 =
                (pxNewQueue as *mut u8).add(core::mem::size_of::<Queue_t>());

            /* Queues can be created either statically or dynamically, so
             * note this task was created dynamically in case it is later
             * deleted. */
            (*pxNewQueue).ucStaticallyAllocated = pdFALSE as u8;

            prvInitialiseNewQueue(
                uxQueueLength,
                uxItemSize,
                pucQueueStorage,
                ucQueueType,
                pxNewQueue,
            );
        } else {
            traceQUEUE_CREATE_FAILED(ucQueueType);
            mtCOVERAGE_TEST_MARKER();
        }
    } else {
        configASSERT(!pxNewQueue.is_null());
        mtCOVERAGE_TEST_MARKER();
    }

    traceRETURN_xQueueGenericCreate(pxNewQueue as *mut c_void);

    pxNewQueue
}

// =============================================================================
// Convenience Macros as Functions
// =============================================================================

/// Create a queue (wrapper for xQueueGenericCreate)
#[cfg(feature = "alloc")]
#[inline(always)]
pub unsafe fn xQueueCreate(uxQueueLength: UBaseType_t, uxItemSize: UBaseType_t) -> QueueHandle_t {
    xQueueGenericCreate(uxQueueLength, uxItemSize, queueQUEUE_TYPE_BASE)
}

/// Create a queue using static memory (wrapper for xQueueGenericCreateStatic)
#[inline(always)]
pub unsafe fn xQueueCreateStatic(
    uxQueueLength: UBaseType_t,
    uxItemSize: UBaseType_t,
    pucQueueStorage: *mut u8,
    pxQueueBuffer: *mut StaticQueue_t,
) -> QueueHandle_t {
    xQueueGenericCreateStatic(
        uxQueueLength,
        uxItemSize,
        pucQueueStorage,
        pxQueueBuffer,
        queueQUEUE_TYPE_BASE,
    )
}

// =============================================================================
// Copy Data To/From Queue
// =============================================================================

/// Copy an item into the queue
///
/// Returns pdTRUE if a higher priority task was woken (for mutex case)
unsafe fn prvCopyDataToQueue(
    pxQueue: *mut Queue_t,
    pvItemToQueue: *const c_void,
    xPosition: BaseType_t,
) -> BaseType_t {
    let mut xReturn: BaseType_t = pdFALSE;
    let uxMessagesWaiting: UBaseType_t = (*pxQueue).uxMessagesWaiting;

    if (*pxQueue).uxItemSize == 0 {
        /* This is a mutex - handle priority inheritance */
        #[cfg(feature = "alloc")] // configUSE_MUTEXES
        {
            if (*pxQueue).pcHead.is_null() {
                /* Queue is being used as a mutex */
                xReturn = xTaskPriorityDisinherit((*pxQueue).u.xSemaphore.xMutexHolder);
                (*pxQueue).u.xSemaphore.xMutexHolder = ptr::null_mut();
            } else {
                mtCOVERAGE_TEST_MARKER();
            }
        }
    } else if xPosition == queueSEND_TO_BACK {
        ptr::copy_nonoverlapping(
            pvItemToQueue as *const u8,
            (*pxQueue).pcWriteTo as *mut u8,
            (*pxQueue).uxItemSize as usize,
        );
        (*pxQueue).pcWriteTo = (*pxQueue).pcWriteTo.add((*pxQueue).uxItemSize as usize);
        if (*pxQueue).pcWriteTo >= (*pxQueue).u.xQueue.pcTail {
            (*pxQueue).pcWriteTo = (*pxQueue).pcHead;
        } else {
            mtCOVERAGE_TEST_MARKER();
        }
    } else {
        ptr::copy_nonoverlapping(
            pvItemToQueue as *const u8,
            (*pxQueue).u.xQueue.pcReadFrom as *mut u8,
            (*pxQueue).uxItemSize as usize,
        );
        (*pxQueue).u.xQueue.pcReadFrom = (*pxQueue)
            .u
            .xQueue
            .pcReadFrom
            .sub((*pxQueue).uxItemSize as usize);
        if (*pxQueue).u.xQueue.pcReadFrom < (*pxQueue).pcHead {
            (*pxQueue).u.xQueue.pcReadFrom = (*pxQueue)
                .u
                .xQueue
                .pcTail
                .sub((*pxQueue).uxItemSize as usize);
        } else {
            mtCOVERAGE_TEST_MARKER();
        }

        if xPosition == queueOVERWRITE {
            if uxMessagesWaiting > 0 {
                /* An item is not being added but overwritten, so subtract
                 * one from the recorded number of items in the queue so when
                 * one is added again below the number of recorded items remains
                 * correct. */
                (*pxQueue).uxMessagesWaiting = uxMessagesWaiting - 1;
            } else {
                mtCOVERAGE_TEST_MARKER();
            }
        } else {
            mtCOVERAGE_TEST_MARKER();
        }
    }

    (*pxQueue).uxMessagesWaiting = (*pxQueue).uxMessagesWaiting + 1;

    xReturn
}

/// Copy an item out of a queue
unsafe fn prvCopyDataFromQueue(pxQueue: *mut Queue_t, pvBuffer: *mut c_void) {
    if (*pxQueue).uxItemSize != 0 {
        (*pxQueue).u.xQueue.pcReadFrom = (*pxQueue)
            .u
            .xQueue
            .pcReadFrom
            .add((*pxQueue).uxItemSize as usize);
        if (*pxQueue).u.xQueue.pcReadFrom >= (*pxQueue).u.xQueue.pcTail {
            (*pxQueue).u.xQueue.pcReadFrom = (*pxQueue).pcHead;
        } else {
            mtCOVERAGE_TEST_MARKER();
        }
        ptr::copy_nonoverlapping(
            (*pxQueue).u.xQueue.pcReadFrom as *const u8,
            pvBuffer as *mut u8,
            (*pxQueue).uxItemSize as usize,
        );
    }
}

// =============================================================================
// Queue Empty/Full Checks
// =============================================================================

/// Check if queue is empty
unsafe fn prvIsQueueEmpty(pxQueue: *const Queue_t) -> BaseType_t {
    let xReturn: BaseType_t;

    taskENTER_CRITICAL();
    {
        if (*pxQueue).uxMessagesWaiting == 0 {
            xReturn = pdTRUE;
        } else {
            xReturn = pdFALSE;
        }
    }
    taskEXIT_CRITICAL();

    xReturn
}

/// Check if queue is full
unsafe fn prvIsQueueFull(pxQueue: *const Queue_t) -> BaseType_t {
    let xReturn: BaseType_t;

    taskENTER_CRITICAL();
    {
        if (*pxQueue).uxMessagesWaiting == (*pxQueue).uxLength {
            xReturn = pdTRUE;
        } else {
            xReturn = pdFALSE;
        }
    }
    taskEXIT_CRITICAL();

    xReturn
}

// =============================================================================
// Queue Send (Generic)
// =============================================================================

/// Send an item to a queue
///
/// # Safety
///
/// xQueue must be a valid queue handle
/// pvItemToQueue must point to valid data of the queue's item size
pub unsafe fn xQueueGenericSend(
    xQueue: QueueHandle_t,
    pvItemToQueue: *const c_void,
    xTicksToWait: TickType_t,
    xCopyPosition: BaseType_t,
) -> BaseType_t {
    let mut xEntryTimeSet: BaseType_t = pdFALSE;
    let xYieldRequired: BaseType_t;
    let mut xTimeOut = TimeOut_t::new();
    let pxQueue: *mut Queue_t = xQueue;

    traceENTER_xQueueGenericSend(
        pxQueue as *mut c_void,
        pvItemToQueue,
        xTicksToWait,
        xCopyPosition,
    );

    configASSERT(!pxQueue.is_null());
    configASSERT(!(pvItemToQueue.is_null() && (*pxQueue).uxItemSize != 0));
    configASSERT(!(xCopyPosition == queueOVERWRITE && (*pxQueue).uxLength != 1));

    loop {
        taskENTER_CRITICAL();
        {
            /* Is there room on the queue now?  The running task must be the
             * highest priority task wanting to access the queue. */
            if (*pxQueue).uxMessagesWaiting < (*pxQueue).uxLength || xCopyPosition == queueOVERWRITE
            {
                traceQUEUE_SEND(pxQueue as *mut c_void);

                // Queue set support: track previous message count for overwrite detection
                #[cfg(feature = "queue-sets")]
                let uxPreviousMessagesWaiting = (*pxQueue).uxMessagesWaiting;

                xYieldRequired = prvCopyDataToQueue(pxQueue, pvItemToQueue, xCopyPosition);

                // Queue set handling: if this queue is a member of a set, notify the set
                #[cfg(feature = "queue-sets")]
                {
                    if !(*pxQueue).pxQueueSetContainer.is_null() {
                        if xCopyPosition == queueOVERWRITE && uxPreviousMessagesWaiting != 0 {
                            // Do not notify the queue set as an existing item was
                            // overwritten, so the number of items hasn't changed.
                            mtCOVERAGE_TEST_MARKER();
                        } else if prvNotifyQueueSetContainer(pxQueue) != pdFALSE {
                            // The queue is a member of a queue set, and posting to
                            // the queue set caused a higher priority task to unblock.
                            queueYIELD_IF_USING_PREEMPTION();
                        }
                    } else {
                        // Not a member of a queue set - normal unblock logic
                        if listLIST_IS_EMPTY(&(*pxQueue).xTasksWaitingToReceive) == pdFALSE {
                            if xTaskRemoveFromEventList(&(*pxQueue).xTasksWaitingToReceive)
                                != pdFALSE
                            {
                                queueYIELD_IF_USING_PREEMPTION();
                            }
                        } else if xYieldRequired != pdFALSE {
                            queueYIELD_IF_USING_PREEMPTION();
                        }
                    }
                }

                // Without queue sets, use simpler logic
                #[cfg(not(feature = "queue-sets"))]
                {
                    /* If there was a task waiting for data to arrive on the
                     * queue then unblock it now. */
                    if listLIST_IS_EMPTY(&(*pxQueue).xTasksWaitingToReceive) == pdFALSE {
                        if xTaskRemoveFromEventList(&(*pxQueue).xTasksWaitingToReceive) != pdFALSE {
                            /* The unblocked task has a priority higher than
                             * our own so yield immediately. */
                            queueYIELD_IF_USING_PREEMPTION();
                        } else {
                            mtCOVERAGE_TEST_MARKER();
                        }
                    } else if xYieldRequired != pdFALSE {
                        /* This path is a special case for mutex priority inheritance */
                        queueYIELD_IF_USING_PREEMPTION();
                    } else {
                        mtCOVERAGE_TEST_MARKER();
                    }
                }

                taskEXIT_CRITICAL();

                traceRETURN_xQueueGenericSend(pdPASS);

                return pdPASS;
            } else {
                if xTicksToWait == 0 {
                    /* The queue was full and no block time is specified */
                    taskEXIT_CRITICAL();

                    traceQUEUE_SEND_FAILED(pxQueue as *mut c_void);
                    traceRETURN_xQueueGenericSend(errQUEUE_FULL);

                    return errQUEUE_FULL;
                } else if xEntryTimeSet == pdFALSE {
                    /* Queue is full and a block time was specified */
                    vTaskSetTimeOutState(&mut xTimeOut);
                    xEntryTimeSet = pdTRUE;
                } else {
                    mtCOVERAGE_TEST_MARKER();
                }
            }
        }
        taskEXIT_CRITICAL();

        /* Interrupts and other tasks can send/receive now */
        vTaskSuspendAll();
        prvLockQueue(pxQueue);

        /* Check timeout */
        if xTaskCheckForTimeOut(&mut xTimeOut, &mut (xTicksToWait as TickType_t)) == pdFALSE {
            if prvIsQueueFull(pxQueue) != pdFALSE {
                traceBLOCKING_ON_QUEUE_SEND(pxQueue as *mut c_void);
                vTaskPlaceOnEventList(&mut (*pxQueue).xTasksWaitingToSend, xTicksToWait);
                prvUnlockQueue(pxQueue);

                if xTaskResumeAll() == pdFALSE {
                    portYIELD_WITHIN_API();
                }
            } else {
                /* Queue not full, try again */
                prvUnlockQueue(pxQueue);
                xTaskResumeAll();
            }
        } else {
            /* Timeout expired */
            prvUnlockQueue(pxQueue);
            xTaskResumeAll();

            traceQUEUE_SEND_FAILED(pxQueue as *mut c_void);
            traceRETURN_xQueueGenericSend(errQUEUE_FULL);

            return errQUEUE_FULL;
        }
    }
}

// =============================================================================
// Queue Receive
// =============================================================================

/// Receive an item from a queue
///
/// # Safety
///
/// xQueue must be a valid queue handle
/// pvBuffer must point to valid memory of at least the queue's item size
pub unsafe fn xQueueReceive(
    xQueue: QueueHandle_t,
    pvBuffer: *mut c_void,
    xTicksToWait: TickType_t,
) -> BaseType_t {
    let mut xEntryTimeSet: BaseType_t = pdFALSE;
    let mut xTimeOut = TimeOut_t::new();
    let pxQueue: *mut Queue_t = xQueue;

    traceENTER_xQueueReceive(pxQueue as *mut c_void, pvBuffer, xTicksToWait);

    configASSERT(!pxQueue.is_null());
    configASSERT(!(pvBuffer.is_null() && (*pxQueue).uxItemSize != 0));

    loop {
        taskENTER_CRITICAL();
        {
            let uxMessagesWaiting = (*pxQueue).uxMessagesWaiting;

            if uxMessagesWaiting > 0 {
                /* Data available */
                prvCopyDataFromQueue(pxQueue, pvBuffer);
                traceQUEUE_RECEIVE(pxQueue as *mut c_void);
                (*pxQueue).uxMessagesWaiting = uxMessagesWaiting - 1;

                /* If there was a task waiting to send, unblock it */
                if listLIST_IS_EMPTY(&(*pxQueue).xTasksWaitingToSend) == pdFALSE {
                    if xTaskRemoveFromEventList(&(*pxQueue).xTasksWaitingToSend) != pdFALSE {
                        queueYIELD_IF_USING_PREEMPTION();
                    } else {
                        mtCOVERAGE_TEST_MARKER();
                    }
                } else {
                    mtCOVERAGE_TEST_MARKER();
                }

                taskEXIT_CRITICAL();
                traceRETURN_xQueueReceive(pdPASS);
                return pdPASS;
            } else {
                if xTicksToWait == 0 {
                    /* Queue empty and no block time */
                    taskEXIT_CRITICAL();
                    traceQUEUE_RECEIVE_FAILED(pxQueue as *mut c_void);
                    traceRETURN_xQueueReceive(errQUEUE_EMPTY);
                    return errQUEUE_EMPTY;
                } else if xEntryTimeSet == pdFALSE {
                    vTaskSetTimeOutState(&mut xTimeOut);
                    xEntryTimeSet = pdTRUE;
                } else {
                    mtCOVERAGE_TEST_MARKER();
                }
            }
        }
        taskEXIT_CRITICAL();

        vTaskSuspendAll();
        prvLockQueue(pxQueue);

        if xTaskCheckForTimeOut(&mut xTimeOut, &mut (xTicksToWait as TickType_t)) == pdFALSE {
            if prvIsQueueEmpty(pxQueue) != pdFALSE {
                traceBLOCKING_ON_QUEUE_RECEIVE(pxQueue as *mut c_void);
                vTaskPlaceOnEventList(&mut (*pxQueue).xTasksWaitingToReceive, xTicksToWait);
                prvUnlockQueue(pxQueue);

                if xTaskResumeAll() == pdFALSE {
                    portYIELD_WITHIN_API();
                }
            } else {
                prvUnlockQueue(pxQueue);
                xTaskResumeAll();
            }
        } else {
            prvUnlockQueue(pxQueue);
            xTaskResumeAll();

            traceQUEUE_RECEIVE_FAILED(pxQueue as *mut c_void);
            traceRETURN_xQueueReceive(errQUEUE_EMPTY);
            return errQUEUE_EMPTY;
        }
    }
}

// =============================================================================
// Queue Unlock
// =============================================================================

/// Unlock a previously locked queue
unsafe fn prvUnlockQueue(pxQueue: *mut Queue_t) {
    /* THIS FUNCTION MUST BE CALLED WITH THE SCHEDULER SUSPENDED. */

    taskENTER_CRITICAL();
    {
        let mut cTxLock: i8 = (*pxQueue).cTxLock;

        /* See if data was added while locked */
        while cTxLock > queueLOCKED_UNMODIFIED {
            /* Data was posted while locked - unblock waiting tasks */
            // TODO: Full implementation with task unblocking
            cTxLock -= 1;
        }

        (*pxQueue).cTxLock = queueUNLOCKED;
    }
    taskEXIT_CRITICAL();

    taskENTER_CRITICAL();
    {
        let mut cRxLock: i8 = (*pxQueue).cRxLock;

        while cRxLock > queueLOCKED_UNMODIFIED {
            // TODO: Full implementation
            cRxLock -= 1;
        }

        (*pxQueue).cRxLock = queueUNLOCKED;
    }
    taskEXIT_CRITICAL();
}

// =============================================================================
// Queue Utility Functions
// =============================================================================

/// Get the number of messages waiting in a queue
pub unsafe fn uxQueueMessagesWaiting(xQueue: QueueHandle_t) -> UBaseType_t {
    let uxReturn: UBaseType_t;

    configASSERT(!xQueue.is_null());

    taskENTER_CRITICAL();
    {
        uxReturn = (*xQueue).uxMessagesWaiting;
    }
    taskEXIT_CRITICAL();

    uxReturn
}

/// Get the number of free spaces in a queue
pub unsafe fn uxQueueSpacesAvailable(xQueue: QueueHandle_t) -> UBaseType_t {
    let uxReturn: UBaseType_t;

    configASSERT(!xQueue.is_null());

    taskENTER_CRITICAL();
    {
        uxReturn = (*xQueue).uxLength - (*xQueue).uxMessagesWaiting;
    }
    taskEXIT_CRITICAL();

    uxReturn
}

// =============================================================================
// Additional Trace Functions (stubs)
// =============================================================================

#[inline(always)]
fn traceENTER_xQueueGenericReset(_pxQueue: *mut c_void, _xNewQueue: BaseType_t) {}

#[inline(always)]
fn traceRETURN_xQueueGenericReset(_xReturn: BaseType_t) {}

#[inline(always)]
fn traceENTER_xQueueGenericCreateStatic(
    _uxQueueLength: UBaseType_t,
    _uxItemSize: UBaseType_t,
    _pucQueueStorage: *mut c_void,
    _pxStaticQueue: *mut c_void,
    _ucQueueType: u8,
) {
}

#[inline(always)]
fn traceRETURN_xQueueGenericCreateStatic(_pxNewQueue: *mut c_void) {}

#[inline(always)]
fn traceENTER_xQueueGenericCreate(
    _uxQueueLength: UBaseType_t,
    _uxItemSize: UBaseType_t,
    _ucQueueType: u8,
) {
}

#[inline(always)]
fn traceRETURN_xQueueGenericCreate(_pxNewQueue: *mut c_void) {}

#[inline(always)]
fn traceENTER_xQueueGenericSend(
    _pxQueue: *mut c_void,
    _pvItemToQueue: *const c_void,
    _xTicksToWait: TickType_t,
    _xCopyPosition: BaseType_t,
) {
}

#[inline(always)]
fn traceRETURN_xQueueGenericSend(_xReturn: BaseType_t) {}

#[inline(always)]
fn traceENTER_xQueueReceive(
    _pxQueue: *mut c_void,
    _pvBuffer: *mut c_void,
    _xTicksToWait: TickType_t,
) {
}

#[inline(always)]
fn traceRETURN_xQueueReceive(_xReturn: BaseType_t) {}

// =============================================================================
// Queue Wrapper Functions
// =============================================================================

/// Send an item to the back of a queue (wrapper for xQueueGenericSend)
#[inline(always)]
pub unsafe fn xQueueSendToBack(
    xQueue: QueueHandle_t,
    pvItemToQueue: *const c_void,
    xTicksToWait: TickType_t,
) -> BaseType_t {
    xQueueGenericSend(xQueue, pvItemToQueue, xTicksToWait, queueSEND_TO_BACK)
}

/// Send an item to the front of a queue (wrapper for xQueueGenericSend)
#[inline(always)]
pub unsafe fn xQueueSendToFront(
    xQueue: QueueHandle_t,
    pvItemToQueue: *const c_void,
    xTicksToWait: TickType_t,
) -> BaseType_t {
    xQueueGenericSend(xQueue, pvItemToQueue, xTicksToWait, queueSEND_TO_FRONT)
}

/// Send an item to a queue (same as xQueueSendToBack)
#[inline(always)]
pub unsafe fn xQueueSend(
    xQueue: QueueHandle_t,
    pvItemToQueue: *const c_void,
    xTicksToWait: TickType_t,
) -> BaseType_t {
    xQueueSendToBack(xQueue, pvItemToQueue, xTicksToWait)
}

/// Send an item to the back of a queue from an ISR
///
/// This is a simplified version that doesn't handle all the
/// complexity of the full implementation.
#[inline(always)]
pub unsafe fn xQueueSendToBackFromISR(
    xQueue: QueueHandle_t,
    pvItemToQueue: *const c_void,
    pxHigherPriorityTaskWoken: *mut BaseType_t,
) -> BaseType_t {
    xQueueGenericSendFromISR(xQueue, pvItemToQueue, pxHigherPriorityTaskWoken, queueSEND_TO_BACK)
}

/// Send an item to a queue from an ISR (same as xQueueSendToBackFromISR)
#[inline(always)]
pub unsafe fn xQueueSendFromISR(
    xQueue: QueueHandle_t,
    pvItemToQueue: *const c_void,
    pxHigherPriorityTaskWoken: *mut BaseType_t,
) -> BaseType_t {
    xQueueSendToBackFromISR(xQueue, pvItemToQueue, pxHigherPriorityTaskWoken)
}

/// Generic send to queue from ISR
///
/// [AMENDMENT] Simplified implementation - full version would handle
/// queue locking and unblocking waiting tasks.
#[allow(unused_assignments)] // C pattern: initialize to fail
pub unsafe fn xQueueGenericSendFromISR(
    xQueue: QueueHandle_t,
    pvItemToQueue: *const c_void,
    pxHigherPriorityTaskWoken: *mut BaseType_t,
    xCopyPosition: BaseType_t,
) -> BaseType_t {
    let mut xReturn: BaseType_t = pdFAIL;
    let pxQueue = xQueue as *mut Queue_t;

    configASSERT(!pxQueue.is_null());
    configASSERT(!(!pvItemToQueue.is_null() && (*pxQueue).uxItemSize == 0));

    // Critical section for ISR - using interrupt mask
    let uxSavedInterruptStatus = portSET_INTERRUPT_MASK_FROM_ISR();
    {
        if (*pxQueue).uxMessagesWaiting < (*pxQueue).uxLength {
            let cTxLock = (*pxQueue).cTxLock;

            traceQUEUE_SEND_FROM_ISR(pxQueue as *mut c_void);

            // Copy data to queue
            prvCopyDataToQueue(pxQueue, pvItemToQueue, xCopyPosition);

            // Queue set handling for ISR context
            #[cfg(feature = "queue-sets")]
            {
                if !(*pxQueue).pxQueueSetContainer.is_null() {
                    if prvNotifyQueueSetContainer(pxQueue) != pdFALSE {
                        if !pxHigherPriorityTaskWoken.is_null() {
                            *pxHigherPriorityTaskWoken = pdTRUE;
                        }
                    }
                } else {
                    // Not a member of a queue set - normal unblock logic
                    if cTxLock == queueUNLOCKED {
                        if listLIST_IS_EMPTY(&(*pxQueue).xTasksWaitingToReceive) == pdFALSE {
                            if xTaskRemoveFromEventList(&(*pxQueue).xTasksWaitingToReceive)
                                != pdFALSE
                            {
                                if !pxHigherPriorityTaskWoken.is_null() {
                                    *pxHigherPriorityTaskWoken = pdTRUE;
                                }
                            }
                        }
                    } else {
                        configASSERT(cTxLock != queueINT8_MAX);
                        (*pxQueue).cTxLock = cTxLock + 1;
                    }
                }
            }

            // Without queue sets, use simpler logic
            #[cfg(not(feature = "queue-sets"))]
            {
                // If queue was locked, increment lock count instead of waking tasks
                if cTxLock == queueUNLOCKED {
                    // Queue not locked - can unblock waiting task
                    if listLIST_IS_EMPTY(&(*pxQueue).xTasksWaitingToReceive) == pdFALSE {
                        if xTaskRemoveFromEventList(&(*pxQueue).xTasksWaitingToReceive) != pdFALSE {
                            if !pxHigherPriorityTaskWoken.is_null() {
                                *pxHigherPriorityTaskWoken = pdTRUE;
                            }
                        }
                    }
                } else {
                    // Queue is locked - increment tx lock count
                    configASSERT(cTxLock != queueINT8_MAX);
                    (*pxQueue).cTxLock = cTxLock + 1;
                }
            }

            xReturn = pdPASS;
        } else {
            traceQUEUE_SEND_FROM_ISR_FAILED(pxQueue as *mut c_void);
            xReturn = errQUEUE_FULL;
        }
    }
    portCLEAR_INTERRUPT_MASK_FROM_ISR(uxSavedInterruptStatus);

    xReturn
}

/// Wait for a message to arrive on a queue with restricted wake
///
/// This function is used by the timer task to wait for timer commands.
/// It places the calling task on the queue's receive wait list.
///
/// [AMENDMENT] This is a simplified version. The full version handles
/// complex interactions with the delayed task list.
pub unsafe fn vQueueWaitForMessageRestricted(
    xQueue: QueueHandle_t,
    xTicksToWait: TickType_t,
    xWaitIndefinitely: BaseType_t,
) {
    let pxQueue = xQueue as *mut Queue_t;

    // Lock the queue
    prvLockQueue(pxQueue);

    // Check if queue is still empty
    if (*pxQueue).uxMessagesWaiting == 0 {
        // Place ourselves on the waiting list
        // The actual blocking is handled by the caller (vTaskSuspendAll was already called)
        vTaskPlaceOnEventListRestricted(
            &mut (*pxQueue).xTasksWaitingToReceive,
            xTicksToWait,
            xWaitIndefinitely,
        );
    }

    // Unlock the queue
    prvUnlockQueue(pxQueue);
}

// =============================================================================
// Mutex and Semaphore Functions
// =============================================================================

/// Create a mutex using dynamic allocation
///
/// Mutexes support priority inheritance - if a high priority task blocks
/// on a mutex held by a low priority task, the low priority task inherits
/// the high priority until it releases the mutex.
#[cfg(all(feature = "alloc", feature = "use-mutexes"))]
pub unsafe fn xQueueCreateMutex(ucQueueType: u8) -> QueueHandle_t {
    let xNewQueue = xQueueGenericCreate(1, 0, ucQueueType);

    if !xNewQueue.is_null() {
        let pxQueue = xNewQueue as *mut Queue_t;

        // Initialize mutex-specific fields
        (*pxQueue).u.xSemaphore.xMutexHolder = ptr::null_mut();
        (*pxQueue).u.xSemaphore.uxRecursiveCallCount = 0;

        // Set pcHead to NULL to indicate this is a mutex
        (*pxQueue).pcHead = ptr::null_mut();

        // Give the mutex initially (it starts available)
        xQueueGenericSend(xNewQueue, ptr::null(), 0, queueSEND_TO_BACK);
    }

    xNewQueue
}

/// Take a semaphore (or mutex)
///
/// This wraps xQueueReceive with mutex-specific handling for priority inheritance.
#[inline(always)]
pub unsafe fn xQueueSemaphoreTake(
    xQueue: QueueHandle_t,
    xTicksToWait: TickType_t,
) -> BaseType_t {
    let pxQueue = xQueue as *mut Queue_t;

    // For mutexes (pcHead == NULL), we need to handle priority inheritance
    #[cfg(feature = "use-mutexes")]
    {
        if (*pxQueue).pcHead.is_null() {
            // This is a mutex - use the special mutex take logic
            return xQueueTakeMutexRecursive(xQueue, xTicksToWait);
        }
    }

    // For regular semaphores, just receive from queue
    xQueueReceive(xQueue, ptr::null_mut(), xTicksToWait)
}

/// Take a mutex with priority inheritance support
#[cfg(feature = "use-mutexes")]
unsafe fn xQueueTakeMutexRecursive(
    xMutex: QueueHandle_t,
    xTicksToWait: TickType_t,
) -> BaseType_t {
    use crate::kernel::tasks::{xTaskGetCurrentTaskHandle, pvTaskIncrementMutexHeldCount};

    let pxQueue = xMutex as *mut Queue_t;
    let xCurrentTaskHandle = xTaskGetCurrentTaskHandle();

    // Check if we already hold this mutex
    if (*pxQueue).u.xSemaphore.xMutexHolder == xCurrentTaskHandle {
        // Already hold it, increment count
        (*pxQueue).u.xSemaphore.uxRecursiveCallCount += 1;
        return pdPASS;
    }

    // Try to take the mutex
    let xReturn = xQueueReceive(xMutex, ptr::null_mut(), xTicksToWait);

    if xReturn == pdPASS {
        // We got it - record ownership
        (*pxQueue).u.xSemaphore.xMutexHolder = xCurrentTaskHandle;
        (*pxQueue).u.xSemaphore.uxRecursiveCallCount = 1;
        pvTaskIncrementMutexHeldCount();
    }

    xReturn
}

// =============================================================================
// Queue Sets (if enabled)
// =============================================================================

/// Notify a queue set container that a member has data.
///
/// This is called from within xQueueGenericSend and xQueueGenericSendFromISR
/// when data is posted to a queue that is a member of a queue set. The queue
/// set is notified by posting the handle of the queue to the queue set.
///
/// # Safety
/// - Must be called from within a critical section
/// - pxQueue must be a valid queue handle
#[cfg(feature = "queue-sets")]
unsafe fn prvNotifyQueueSetContainer(pxQueue: *const Queue_t) -> BaseType_t {
    let pxQueueSetContainer = (*pxQueue).pxQueueSetContainer;
    let mut xReturn: BaseType_t = pdFALSE;

    // This function must be called from a critical section.
    configASSERT(!pxQueueSetContainer.is_null());
    configASSERT((*pxQueueSetContainer).uxMessagesWaiting < (*pxQueueSetContainer).uxLength);

    if (*pxQueueSetContainer).uxMessagesWaiting < (*pxQueueSetContainer).uxLength {
        let cTxLock = (*pxQueueSetContainer).cTxLock;

        traceQUEUE_SET_SEND(pxQueueSetContainer as *mut c_void);

        // The data copied is the handle of the queue that contains data.
        // A queue set is a queue whose items are queue handles (pointers).
        xReturn = prvCopyDataToQueue(
            pxQueueSetContainer,
            &pxQueue as *const *const Queue_t as *const c_void,
            queueSEND_TO_BACK,
        );

        if cTxLock == queueUNLOCKED {
            if listLIST_IS_EMPTY(&(*pxQueueSetContainer).xTasksWaitingToReceive) == pdFALSE {
                if xTaskRemoveFromEventList(&(*pxQueueSetContainer).xTasksWaitingToReceive) != pdFALSE
                {
                    // The task waiting has a higher priority.
                    xReturn = pdTRUE;
                }
            }
        } else {
            // Queue set is locked, increment tx lock count.
            if cTxLock < queueINT8_MAX {
                (*pxQueueSetContainer).cTxLock = cTxLock + 1;
            }
        }
    }

    xReturn
}

/// Create a queue set.
///
/// A queue set is a collection of queues and/or semaphores. A task can block
/// on a queue set to wait for data to become available on any of the queues
/// or semaphores in the set.
///
/// # Parameters
/// - uxEventQueueLength: The maximum number of events that can be queued at once.
///   This should be the sum of the lengths of all queues in the set plus the
///   maximum count of all semaphores in the set.
///
/// # Returns
/// - A handle to the queue set on success
/// - null on failure
///
/// # Safety
/// - Requires heap allocation (alloc feature)
#[cfg(all(feature = "queue-sets", feature = "alloc"))]
pub unsafe fn xQueueCreateSet(uxEventQueueLength: UBaseType_t) -> QueueSetHandle_t {
    // A queue set is just a queue where each item is a pointer to a queue/semaphore.
    // [AMENDMENT] In C, this uses sizeof(Queue_t *). In Rust, we use size_of::<*mut Queue_t>().
    let pxQueue = xQueueGenericCreate(
        uxEventQueueLength,
        core::mem::size_of::<*mut Queue_t>() as UBaseType_t,
        queueQUEUE_TYPE_SET,
    );

    pxQueue
}

/// Create a queue set using statically allocated memory.
///
/// # Parameters
/// - uxEventQueueLength: The maximum number of events that can be queued at once.
/// - pucQueueStorage: Pointer to storage for the queue data. Must be at least
///   uxEventQueueLength * sizeof(pointer) bytes.
/// - pxStaticQueue: Pointer to a StaticQueue_t structure for the queue state.
///
/// # Returns
/// - A handle to the queue set on success
/// - null on failure
///
/// # Safety
/// - pucQueueStorage must point to valid memory of sufficient size
/// - pxStaticQueue must point to valid memory for StaticQueue_t
#[cfg(feature = "queue-sets")]
pub unsafe fn xQueueCreateSetStatic(
    uxEventQueueLength: UBaseType_t,
    pucQueueStorage: *mut u8,
    pxStaticQueue: *mut StaticQueue_t,
) -> QueueSetHandle_t {
    // [AMENDMENT] In C, this uses sizeof(Queue_t *). In Rust, we use size_of::<*mut Queue_t>().
    let pxQueue = xQueueGenericCreateStatic(
        uxEventQueueLength,
        core::mem::size_of::<*mut Queue_t>() as UBaseType_t,
        pucQueueStorage,
        pxStaticQueue,
        queueQUEUE_TYPE_SET,
    );

    pxQueue
}

/// Add a queue or semaphore to a queue set.
///
/// A queue or semaphore can only be a member of one queue set at a time.
/// The queue/semaphore must be empty when added to a set.
///
/// # Parameters
/// - xQueueOrSemaphore: Handle of the queue or semaphore to add
/// - xQueueSet: Handle of the queue set to add it to
///
/// # Returns
/// - pdPASS if successfully added
/// - pdFAIL if already a member of a set or not empty
///
/// # Safety
/// - Both handles must be valid
#[cfg(feature = "queue-sets")]
pub unsafe fn xQueueAddToSet(
    xQueueOrSemaphore: QueueSetMemberHandle_t,
    xQueueSet: QueueSetHandle_t,
) -> BaseType_t {
    let xReturn: BaseType_t;

    taskENTER_CRITICAL();
    {
        if !(*xQueueOrSemaphore).pxQueueSetContainer.is_null() {
            // Cannot add a queue/semaphore to more than one queue set.
            xReturn = pdFAIL;
        } else if (*xQueueOrSemaphore).uxMessagesWaiting != 0 {
            // Cannot add a queue/semaphore to a queue set if there are already
            // items in the queue/semaphore.
            xReturn = pdFAIL;
        } else {
            (*xQueueOrSemaphore).pxQueueSetContainer = xQueueSet;
            xReturn = pdPASS;
        }
    }
    taskEXIT_CRITICAL();

    xReturn
}

/// Remove a queue or semaphore from a queue set.
///
/// The queue/semaphore must be empty when removed from a set.
///
/// # Parameters
/// - xQueueOrSemaphore: Handle of the queue or semaphore to remove
/// - xQueueSet: Handle of the queue set it belongs to
///
/// # Returns
/// - pdPASS if successfully removed
/// - pdFAIL if not a member of the specified set or not empty
///
/// # Safety
/// - Both handles must be valid
#[cfg(feature = "queue-sets")]
pub unsafe fn xQueueRemoveFromSet(
    xQueueOrSemaphore: QueueSetMemberHandle_t,
    xQueueSet: QueueSetHandle_t,
) -> BaseType_t {
    let xReturn: BaseType_t;

    if (*xQueueOrSemaphore).pxQueueSetContainer != xQueueSet {
        // The queue was not a member of the set.
        xReturn = pdFAIL;
    } else if (*xQueueOrSemaphore).uxMessagesWaiting != 0 {
        // It is dangerous to remove a queue from a set when the queue is
        // not empty because the queue set will still hold pending events.
        xReturn = pdFAIL;
    } else {
        taskENTER_CRITICAL();
        {
            // The queue is no longer contained in the set.
            (*xQueueOrSemaphore).pxQueueSetContainer = ptr::null_mut();
        }
        taskEXIT_CRITICAL();
        xReturn = pdPASS;
    }

    xReturn
}

/// Select a queue or semaphore from a queue set that has data available.
///
/// Blocks until a member of the set has data available or the timeout expires.
///
/// # Parameters
/// - xQueueSet: Handle of the queue set to select from
/// - xTicksToWait: Maximum time to wait for data to become available
///
/// # Returns
/// - Handle of a queue/semaphore with available data, or null if timeout
///
/// # Safety
/// - xQueueSet must be a valid queue set handle
#[cfg(feature = "queue-sets")]
pub unsafe fn xQueueSelectFromSet(
    xQueueSet: QueueSetHandle_t,
    xTicksToWait: TickType_t,
) -> QueueSetMemberHandle_t {
    let mut xReturn: QueueSetMemberHandle_t = ptr::null_mut();

    // A queue set is a queue containing pointers to other queues.
    // Receiving from a queue set gives us the handle of a queue with data.
    xQueueReceive(
        xQueueSet,
        &mut xReturn as *mut QueueSetMemberHandle_t as *mut c_void,
        xTicksToWait,
    );

    xReturn
}

/// Select a queue or semaphore from a queue set from ISR context.
///
/// Non-blocking version for use in interrupt service routines.
///
/// # Parameters
/// - xQueueSet: Handle of the queue set to select from
///
/// # Returns
/// - Handle of a queue/semaphore with available data, or null if none available
///
/// # Safety
/// - xQueueSet must be a valid queue set handle
/// - Must be called from an ISR or with interrupts disabled
#[cfg(feature = "queue-sets")]
pub unsafe fn xQueueSelectFromSetFromISR(xQueueSet: QueueSetHandle_t) -> QueueSetMemberHandle_t {
    let mut xReturn: QueueSetMemberHandle_t = ptr::null_mut();
    let pxQueue = xQueueSet as *mut Queue_t;

    // ISR-safe receive: try to get an item from the queue set without blocking
    let uxSavedInterruptStatus = portSET_INTERRUPT_MASK_FROM_ISR();
    {
        if (*pxQueue).uxMessagesWaiting > 0 {
            // There's data available - copy it out
            let pcReadFrom = (*pxQueue).u.xQueue.pcReadFrom;
            let pcNextReadFrom = pcReadFrom.add((*pxQueue).uxItemSize as usize);

            // Wrap if needed
            let pcActualRead = if pcNextReadFrom >= (*pxQueue).u.xQueue.pcTail {
                (*pxQueue).pcHead
            } else {
                pcNextReadFrom
            };
            (*pxQueue).u.xQueue.pcReadFrom = pcActualRead;

            // Copy the queue handle
            ptr::copy_nonoverlapping(
                pcActualRead as *const QueueSetMemberHandle_t,
                &mut xReturn,
                1,
            );

            (*pxQueue).uxMessagesWaiting -= 1;
        }
    }
    portCLEAR_INTERRUPT_MASK_FROM_ISR(uxSavedInterruptStatus);

    xReturn
}

/// Trace hook for queue set send (no-op by default)
#[cfg(feature = "queue-sets")]
#[inline(always)]
fn traceQUEUE_SET_SEND(_pxQueue: *mut c_void) {}
