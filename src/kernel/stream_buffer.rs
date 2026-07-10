/*
 * FreeRTOS Kernel <DEVELOPMENT BRANCH>
 * Copyright (C) 2021 Amazon.com, Inc. or its affiliates. All Rights Reserved.
 *
 * SPDX-License-Identifier: MIT
 *
 * [AMENDMENT] This is the Rust port of stream_buffer.c for FreeRTOS-in-Rust.
 */

//! Stream Buffer Implementation
//!
//! Stream buffers are used to send a continuous stream of data from one task
//! or interrupt to another. Their implementation is lightweight, making them
//! particularly suited for interrupt to task and core to core communication.
//!
//! **Important**: Stream buffers assume there is only ONE writer and ONE reader.
//! Multiple writers or readers require external serialization (e.g., critical sections).
//!
//! # Buffer Types
//!
//! - **Stream Buffer**: Continuous byte stream, reader gets whatever bytes are available
//! - **Message Buffer**: Discrete messages with length prefix, reader gets complete messages
//! - **Batching Buffer**: Like stream buffer but blocks until trigger level is reached
//!
//! # Usage
//!
//! ```ignore
//! // Create a stream buffer
//! let handle = xStreamBufferCreate(100, 1);
//!
//! // Send data
//! xStreamBufferSend(handle, data.as_ptr(), data.len(), portMAX_DELAY);
//!
//! // Receive data
//! let received = xStreamBufferReceive(handle, buffer.as_mut_ptr(), buffer.len(), portMAX_DELAY);
//! ```

use core::ffi::c_void;
use core::ptr;
use core::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

use crate::config::*;
use crate::kernel::tasks::*;
#[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
use crate::memory::{pvPortMalloc, vPortFree};
use crate::port::*;
use crate::types::*;

// =============================================================================
// Constants
// =============================================================================

/// Stream buffer type constant
pub const sbTYPE_STREAM_BUFFER: BaseType_t = 0;

/// Message buffer type constant
pub const sbTYPE_MESSAGE_BUFFER: BaseType_t = 1;

/// Batching buffer type constant
pub const sbTYPE_STREAM_BATCHING_BUFFER: BaseType_t = 2;

/// Bytes needed to store message length in message buffers
const sbBYTES_TO_STORE_MESSAGE_LENGTH: usize =
    core::mem::size_of::<configMESSAGE_BUFFER_LENGTH_TYPE>();

/// Flag: is a message buffer (holds discrete messages)
const sbFLAGS_IS_MESSAGE_BUFFER: u8 = 1;

/// Flag: was statically allocated
const sbFLAGS_IS_STATICALLY_ALLOCATED: u8 = 2;

/// Flag: is a batching buffer
const sbFLAGS_IS_BATCHING_BUFFER: u8 = 4;

/// Message buffer length type (configurable)
pub type configMESSAGE_BUFFER_LENGTH_TYPE = u32;

/// Default notification index
const tskDEFAULT_INDEX_TO_NOTIFY: UBaseType_t = 0;

// =============================================================================
// Stream Buffer Handle
// =============================================================================

/// Opaque handle to a stream buffer
pub type StreamBufferHandle_t = *mut c_void;

/// Callback function type for send/receive completion
pub type StreamBufferCallbackFunction_t = Option<
    extern "C" fn(
        xStreamBuffer: StreamBufferHandle_t,
        xIsInsideISR: BaseType_t,
        pxHigherPriorityTaskWoken: *mut BaseType_t,
    ),
>;

// =============================================================================
// Stream Buffer Structure
// =============================================================================

/// Internal stream buffer structure
#[repr(C)]
pub struct StreamBuffer_t {
    /// Index to the next item to read within the buffer
    ///
    // [AMENDMENT] This is atomic because the single reader and single writer
    // can be a task and an interrupt respectively. Acquire/release ordering
    // publishes the corresponding data-buffer copy.
    pub xTail: AtomicUsize,

    /// Index to the next item to write within the buffer
    pub xHead: AtomicUsize,

    /// The length of the buffer pointed to by pucBuffer
    pub xLength: usize,

    /// Number of bytes that must be in buffer before unblocking waiting task
    pub xTriggerLevelBytes: AtomicUsize,

    /// Handle of task waiting for data (or NULL)
    pub xTaskWaitingToReceive: AtomicPtr<c_void>,

    /// Handle of task waiting to send (or NULL)
    pub xTaskWaitingToSend: AtomicPtr<c_void>,

    /// Points to the buffer storage area
    pub pucBuffer: *mut u8,

    /// Flags for buffer type
    pub ucFlags: u8,

    /// Stream buffer number for tracing
    #[cfg(feature = "trace-facility")]
    pub uxStreamBufferNumber: UBaseType_t,

    /// Optional callback called after a successful send.
    pub pxSendCompletedCallback: StreamBufferCallbackFunction_t,

    /// Optional callback called after a successful receive.
    pub pxReceiveCompletedCallback: StreamBufferCallbackFunction_t,

    /// Notification index to use
    pub uxNotificationIndex: AtomicUsize,
}

/// Static stream buffer structure for user allocation
#[repr(transparent)]
pub struct StaticStreamBuffer_t {
    // [AMENDMENT] Preserve the size and alignment of `StreamBuffer_t` without
    // requiring an initialised internal value. A byte array is only
    // byte-aligned and cannot soundly be cast to the internal structure.
    _storage: core::mem::MaybeUninit<StreamBuffer_t>,
}

impl StaticStreamBuffer_t {
    pub const fn new() -> Self {
        StaticStreamBuffer_t {
            _storage: core::mem::MaybeUninit::uninit(),
        }
    }
}

/// Message buffers use the same control-block storage as stream buffers.
pub type StaticMessageBuffer_t = StaticStreamBuffer_t;

// =============================================================================
// Creation Functions
// =============================================================================

/// Create a stream buffer using dynamic allocation
///
/// # Arguments
///
/// * `xBufferSizeBytes` - Total buffer capacity in bytes
/// * `xTriggerLevelBytes` - Bytes needed before unblocking waiting reader
/// * `xStreamBufferType` - Type: sbTYPE_STREAM_BUFFER, sbTYPE_MESSAGE_BUFFER, or sbTYPE_STREAM_BATCHING_BUFFER
/// * `pxSendCompletedCallback` - Optional callback on send completion
/// * `pxReceiveCompletedCallback` - Optional callback on receive completion
///
/// # Returns
///
/// Handle to the created stream buffer, or NULL on failure
///
/// # Safety
///
/// Call only from task context while the selected allocator and port critical
/// sections are initialized and cannot be reset concurrently. The returned
/// raw handle must be given a single producer and a single consumer, and must
/// eventually be deleted exactly once after all access has ceased.
#[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
pub unsafe fn xStreamBufferGenericCreate(
    xBufferSizeBytes: usize,
    mut xTriggerLevelBytes: usize,
    xStreamBufferType: BaseType_t,
    pxSendCompletedCallback: StreamBufferCallbackFunction_t,
    pxReceiveCompletedCallback: StreamBufferCallbackFunction_t,
) -> StreamBufferHandle_t {
    let ucFlags: u8;

    if xBufferSizeBytes == 0
        || xTriggerLevelBytes > xBufferSizeBytes
        || (xStreamBufferType == sbTYPE_MESSAGE_BUFFER
            && xBufferSizeBytes <= sbBYTES_TO_STORE_MESSAGE_LENGTH)
    {
        return ptr::null_mut();
    }

    // Determine flags based on buffer type
    if xStreamBufferType == sbTYPE_MESSAGE_BUFFER {
        ucFlags = sbFLAGS_IS_MESSAGE_BUFFER;
        configASSERT(xBufferSizeBytes > sbBYTES_TO_STORE_MESSAGE_LENGTH);
    } else if xStreamBufferType == sbTYPE_STREAM_BATCHING_BUFFER {
        ucFlags = sbFLAGS_IS_BATCHING_BUFFER;
        configASSERT(xBufferSizeBytes > 0);
    } else {
        ucFlags = 0;
        configASSERT(xBufferSizeBytes > 0);
    }

    configASSERT(xTriggerLevelBytes <= xBufferSizeBytes);

    // Trigger level of 0 makes no sense
    if xTriggerLevelBytes == 0 {
        xTriggerLevelBytes = 1;
    }

    // Allocate structure and buffer together.  The additional byte is the
    // upstream sentinel that distinguishes full from empty.
    let Some(xBufferSizeBytes) = xBufferSizeBytes.checked_add(1) else {
        return ptr::null_mut();
    };
    let Some(total_size) = core::mem::size_of::<StreamBuffer_t>().checked_add(xBufferSizeBytes)
    else {
        return ptr::null_mut();
    };

    let pvAllocatedMemory = pvPortMalloc(total_size) as *mut u8;

    if !pvAllocatedMemory.is_null() {
        let pxStreamBuffer = pvAllocatedMemory as *mut StreamBuffer_t;
        let pucBuffer = pvAllocatedMemory.add(core::mem::size_of::<StreamBuffer_t>());

        prvInitialiseNewStreamBuffer(
            pxStreamBuffer,
            pucBuffer,
            xBufferSizeBytes,
            xTriggerLevelBytes,
            ucFlags,
            pxSendCompletedCallback,
            pxReceiveCompletedCallback,
        );

        pxStreamBuffer as StreamBufferHandle_t
    } else {
        ptr::null_mut()
    }
}

/// Create a stream buffer using static allocation
///
/// # Arguments
///
/// * `xBufferSizeBytes` - Size of pucStreamBufferStorageArea in bytes
/// * `xTriggerLevelBytes` - Bytes needed before unblocking waiting reader
/// * `xStreamBufferType` - Type of buffer
/// * `pucStreamBufferStorageArea` - User-provided buffer storage
/// * `pxStaticStreamBuffer` - User-provided structure storage
/// * `pxSendCompletedCallback` - Optional callback on send completion
/// * `pxReceiveCompletedCallback` - Optional callback on receive completion
///
/// # Returns
///
/// Handle to the created stream buffer, or NULL on failure
///
/// # Safety
///
/// `pucStreamBufferStorageArea` must be uniquely writable for
/// `xBufferSizeBytes` bytes. `pxStaticStreamBuffer` must be aligned, uniquely
/// writable, and stable until deletion and the end of every access. Call only
/// from task context; the resulting handle permits one producer and one
/// consumer.
pub unsafe fn xStreamBufferGenericCreateStatic(
    xBufferSizeBytes: usize,
    mut xTriggerLevelBytes: usize,
    xStreamBufferType: BaseType_t,
    pucStreamBufferStorageArea: *mut u8,
    pxStaticStreamBuffer: *mut StaticStreamBuffer_t,
    pxSendCompletedCallback: StreamBufferCallbackFunction_t,
    pxReceiveCompletedCallback: StreamBufferCallbackFunction_t,
) -> StreamBufferHandle_t {
    configASSERT(!pucStreamBufferStorageArea.is_null());
    configASSERT(!pxStaticStreamBuffer.is_null());
    configASSERT(xTriggerLevelBytes <= xBufferSizeBytes);

    if pucStreamBufferStorageArea.is_null()
        || pxStaticStreamBuffer.is_null()
        || xBufferSizeBytes == 0
        || xTriggerLevelBytes > xBufferSizeBytes
        || (xStreamBufferType == sbTYPE_MESSAGE_BUFFER
            && xBufferSizeBytes <= sbBYTES_TO_STORE_MESSAGE_LENGTH)
    {
        return ptr::null_mut();
    }

    // Trigger level of 0 makes no sense
    if xTriggerLevelBytes == 0 {
        xTriggerLevelBytes = 1;
    }

    let ucFlags: u8;
    if xStreamBufferType == sbTYPE_MESSAGE_BUFFER {
        ucFlags = sbFLAGS_IS_MESSAGE_BUFFER | sbFLAGS_IS_STATICALLY_ALLOCATED;
        configASSERT(xBufferSizeBytes > sbBYTES_TO_STORE_MESSAGE_LENGTH);
    } else if xStreamBufferType == sbTYPE_STREAM_BATCHING_BUFFER {
        ucFlags = sbFLAGS_IS_BATCHING_BUFFER | sbFLAGS_IS_STATICALLY_ALLOCATED;
        configASSERT(xBufferSizeBytes > 0);
    } else {
        ucFlags = sbFLAGS_IS_STATICALLY_ALLOCATED;
    }

    if !pucStreamBufferStorageArea.is_null() && !pxStaticStreamBuffer.is_null() {
        let pxStreamBuffer = pxStaticStreamBuffer as *mut StreamBuffer_t;

        prvInitialiseNewStreamBuffer(
            pxStreamBuffer,
            pucStreamBufferStorageArea,
            xBufferSizeBytes,
            xTriggerLevelBytes,
            ucFlags,
            pxSendCompletedCallback,
            pxReceiveCompletedCallback,
        );

        pxStreamBuffer as StreamBufferHandle_t
    } else {
        ptr::null_mut()
    }
}

/// Convenience wrapper: create a stream buffer.
///
/// # Safety
///
/// The caller must uphold [`xStreamBufferGenericCreate`]'s task-context,
/// allocator, and size requirements.
#[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
#[inline(always)]
pub unsafe fn xStreamBufferCreate(
    xBufferSizeBytes: usize,
    xTriggerLevelBytes: usize,
) -> StreamBufferHandle_t {
    xStreamBufferGenericCreate(
        xBufferSizeBytes,
        xTriggerLevelBytes,
        sbTYPE_STREAM_BUFFER,
        None,
        None,
    )
}

/// Header-macro veneer: create a stream buffer with completion callbacks.
///
/// # Safety
///
/// The caller must uphold [`xStreamBufferGenericCreate`]'s task-context,
/// allocator, size, and callback-lifetime requirements.
#[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
#[inline(always)]
pub unsafe fn xStreamBufferCreateWithCallback(
    xBufferSizeBytes: usize,
    xTriggerLevelBytes: usize,
    pxSendCompletedCallback: StreamBufferCallbackFunction_t,
    pxReceiveCompletedCallback: StreamBufferCallbackFunction_t,
) -> StreamBufferHandle_t {
    xStreamBufferGenericCreate(
        xBufferSizeBytes,
        xTriggerLevelBytes,
        sbTYPE_STREAM_BUFFER,
        pxSendCompletedCallback,
        pxReceiveCompletedCallback,
    )
}

/// Convenience wrapper: create a message buffer.
///
/// # Safety
///
/// The caller must uphold [`xStreamBufferGenericCreate`]'s task-context,
/// allocator, and size requirements.
#[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
#[inline(always)]
pub unsafe fn xMessageBufferCreate(xBufferSizeBytes: usize) -> MessageBufferHandle_t {
    xStreamBufferGenericCreate(xBufferSizeBytes, 0, sbTYPE_MESSAGE_BUFFER, None, None)
}

/// Header-macro veneer: create a message buffer with completion callbacks.
///
/// # Safety
///
/// The caller must uphold [`xStreamBufferGenericCreate`]'s task-context,
/// allocator, size, and callback-lifetime requirements.
#[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
#[inline(always)]
pub unsafe fn xMessageBufferCreateWithCallback(
    xBufferSizeBytes: usize,
    pxSendCompletedCallback: StreamBufferCallbackFunction_t,
    pxReceiveCompletedCallback: StreamBufferCallbackFunction_t,
) -> MessageBufferHandle_t {
    xStreamBufferGenericCreate(
        xBufferSizeBytes,
        0,
        sbTYPE_MESSAGE_BUFFER,
        pxSendCompletedCallback,
        pxReceiveCompletedCallback,
    )
}

/// Convenience wrapper: create a batching buffer.
///
/// # Safety
///
/// The caller must uphold [`xStreamBufferGenericCreate`]'s task-context,
/// allocator, and size requirements.
#[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
#[inline(always)]
pub unsafe fn xStreamBatchingBufferCreate(
    xBufferSizeBytes: usize,
    xTriggerLevelBytes: usize,
) -> StreamBufferHandle_t {
    xStreamBufferGenericCreate(
        xBufferSizeBytes,
        xTriggerLevelBytes,
        sbTYPE_STREAM_BATCHING_BUFFER,
        None,
        None,
    )
}

/// Header-macro veneer: create a batching buffer with completion callbacks.
///
/// # Safety
///
/// The caller must uphold [`xStreamBufferGenericCreate`]'s task-context,
/// allocator, size, and callback-lifetime requirements.
#[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
#[inline(always)]
pub unsafe fn xStreamBatchingBufferCreateWithCallback(
    xBufferSizeBytes: usize,
    xTriggerLevelBytes: usize,
    pxSendCompletedCallback: StreamBufferCallbackFunction_t,
    pxReceiveCompletedCallback: StreamBufferCallbackFunction_t,
) -> StreamBufferHandle_t {
    xStreamBufferGenericCreate(
        xBufferSizeBytes,
        xTriggerLevelBytes,
        sbTYPE_STREAM_BATCHING_BUFFER,
        pxSendCompletedCallback,
        pxReceiveCompletedCallback,
    )
}

/// Header-macro veneer: create a stream buffer in caller-provided storage.
///
/// # Safety
///
/// The caller must uphold [`xStreamBufferGenericCreateStatic`]'s storage,
/// lifetime, size, task-context, and callback requirements.
#[inline(always)]
pub unsafe fn xStreamBufferCreateStatic(
    xBufferSizeBytes: usize,
    xTriggerLevelBytes: usize,
    pucStreamBufferStorageArea: *mut u8,
    pxStaticStreamBuffer: *mut StaticStreamBuffer_t,
) -> StreamBufferHandle_t {
    xStreamBufferGenericCreateStatic(
        xBufferSizeBytes,
        xTriggerLevelBytes,
        sbTYPE_STREAM_BUFFER,
        pucStreamBufferStorageArea,
        pxStaticStreamBuffer,
        None,
        None,
    )
}

/// Header-macro veneer: create a static stream buffer with callbacks.
///
/// # Safety
///
/// The caller must uphold [`xStreamBufferGenericCreateStatic`]'s storage,
/// lifetime, size, task-context, and callback requirements.
#[inline(always)]
pub unsafe fn xStreamBufferCreateStaticWithCallback(
    xBufferSizeBytes: usize,
    xTriggerLevelBytes: usize,
    pucStreamBufferStorageArea: *mut u8,
    pxStaticStreamBuffer: *mut StaticStreamBuffer_t,
    pxSendCompletedCallback: StreamBufferCallbackFunction_t,
    pxReceiveCompletedCallback: StreamBufferCallbackFunction_t,
) -> StreamBufferHandle_t {
    xStreamBufferGenericCreateStatic(
        xBufferSizeBytes,
        xTriggerLevelBytes,
        sbTYPE_STREAM_BUFFER,
        pucStreamBufferStorageArea,
        pxStaticStreamBuffer,
        pxSendCompletedCallback,
        pxReceiveCompletedCallback,
    )
}

/// Header-macro veneer: create a message buffer in caller-provided storage.
///
/// # Safety
///
/// The caller must uphold [`xStreamBufferGenericCreateStatic`]'s storage,
/// lifetime, size, and task-context requirements.
#[inline(always)]
pub unsafe fn xMessageBufferCreateStatic(
    xBufferSizeBytes: usize,
    pucMessageBufferStorageArea: *mut u8,
    pxStaticMessageBuffer: *mut StaticMessageBuffer_t,
) -> MessageBufferHandle_t {
    xStreamBufferGenericCreateStatic(
        xBufferSizeBytes,
        0,
        sbTYPE_MESSAGE_BUFFER,
        pucMessageBufferStorageArea,
        pxStaticMessageBuffer,
        None,
        None,
    )
}

/// Header-macro veneer: create a static message buffer with callbacks.
///
/// # Safety
///
/// The caller must uphold [`xStreamBufferGenericCreateStatic`]'s storage,
/// lifetime, size, task-context, and callback requirements.
#[inline(always)]
pub unsafe fn xMessageBufferCreateStaticWithCallback(
    xBufferSizeBytes: usize,
    pucMessageBufferStorageArea: *mut u8,
    pxStaticMessageBuffer: *mut StaticMessageBuffer_t,
    pxSendCompletedCallback: StreamBufferCallbackFunction_t,
    pxReceiveCompletedCallback: StreamBufferCallbackFunction_t,
) -> MessageBufferHandle_t {
    xStreamBufferGenericCreateStatic(
        xBufferSizeBytes,
        0,
        sbTYPE_MESSAGE_BUFFER,
        pucMessageBufferStorageArea,
        pxStaticMessageBuffer,
        pxSendCompletedCallback,
        pxReceiveCompletedCallback,
    )
}

/// Header-macro veneer: create a batching buffer in caller-provided storage.
///
/// # Safety
///
/// The caller must uphold [`xStreamBufferGenericCreateStatic`]'s storage,
/// lifetime, size, and task-context requirements.
#[inline(always)]
pub unsafe fn xStreamBatchingBufferCreateStatic(
    xBufferSizeBytes: usize,
    xTriggerLevelBytes: usize,
    pucStreamBufferStorageArea: *mut u8,
    pxStaticStreamBuffer: *mut StaticStreamBuffer_t,
) -> StreamBufferHandle_t {
    xStreamBufferGenericCreateStatic(
        xBufferSizeBytes,
        xTriggerLevelBytes,
        sbTYPE_STREAM_BATCHING_BUFFER,
        pucStreamBufferStorageArea,
        pxStaticStreamBuffer,
        None,
        None,
    )
}

/// Header-macro veneer: create a static batching buffer with callbacks.
///
/// # Safety
///
/// The caller must uphold [`xStreamBufferGenericCreateStatic`]'s storage,
/// lifetime, size, task-context, and callback requirements.
#[inline(always)]
pub unsafe fn xStreamBatchingBufferCreateStaticWithCallback(
    xBufferSizeBytes: usize,
    xTriggerLevelBytes: usize,
    pucStreamBufferStorageArea: *mut u8,
    pxStaticStreamBuffer: *mut StaticStreamBuffer_t,
    pxSendCompletedCallback: StreamBufferCallbackFunction_t,
    pxReceiveCompletedCallback: StreamBufferCallbackFunction_t,
) -> StreamBufferHandle_t {
    xStreamBufferGenericCreateStatic(
        xBufferSizeBytes,
        xTriggerLevelBytes,
        sbTYPE_STREAM_BATCHING_BUFFER,
        pucStreamBufferStorageArea,
        pxStaticStreamBuffer,
        pxSendCompletedCallback,
        pxReceiveCompletedCallback,
    )
}

// =============================================================================
// Delete Function
// =============================================================================

/// Delete a stream buffer
///
/// Frees memory if dynamically allocated, otherwise zeros the structure.
///
/// # Safety
///
/// `xStreamBuffer` must identify a live buffer created by this module. This
/// must be its sole deletion, after all task/ISR operations and outstanding
/// references have ceased; caller-provided static storage must remain writable.
pub unsafe fn vStreamBufferDelete(xStreamBuffer: StreamBufferHandle_t) {
    configASSERT(!xStreamBuffer.is_null());

    let pxStreamBuffer = xStreamBuffer as *mut StreamBuffer_t;

    if ((*pxStreamBuffer).ucFlags & sbFLAGS_IS_STATICALLY_ALLOCATED) == 0 {
        // Both structure and buffer were allocated by pvPortMalloc together.
        #[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
        vPortFree(pxStreamBuffer as *mut c_void);

        /* With dynamic allocation disabled, a non-static object cannot have
         * been created. Match upstream's deliberately-failing assertion so a
         * corrupted allocation flag is diagnosed instead of freed. */
        #[cfg(not(any(feature = "alloc", feature = "heap-4", feature = "heap-5")))]
        configASSERT(xStreamBuffer as usize == usize::MAX);
    } else {
        // Static allocation - just zero it out
        ptr::write_bytes(pxStreamBuffer, 0, 1);
    }
}

// =============================================================================
// Reset Functions
// =============================================================================

/// Reset a stream buffer to empty state
///
/// Can only reset if no tasks are blocked on it.
///
/// # Returns
///
/// pdPASS if reset, pdFAIL if tasks are blocked
///
/// # Safety
///
/// `xStreamBuffer` must identify a live buffer that remains allocated for the
/// call. Call only from task context with initialized kernel state, while no
/// producer or consumer can access the buffer and no task is blocked on it.
pub unsafe fn xStreamBufferReset(xStreamBuffer: StreamBufferHandle_t) -> BaseType_t {
    configASSERT(!xStreamBuffer.is_null());

    let pxStreamBuffer = xStreamBuffer as *mut StreamBuffer_t;
    let mut xReturn: BaseType_t = pdFAIL;

    portENTER_CRITICAL();
    {
        if (*pxStreamBuffer)
            .xTaskWaitingToReceive
            .load(Ordering::Acquire)
            .is_null()
            && (*pxStreamBuffer)
                .xTaskWaitingToSend
                .load(Ordering::Acquire)
                .is_null()
        {
            let pxSendCompletedCallback = (*pxStreamBuffer).pxSendCompletedCallback;
            let pxReceiveCompletedCallback = (*pxStreamBuffer).pxReceiveCompletedCallback;
            #[cfg(feature = "trace-facility")]
            let uxStreamBufferNumber = (*pxStreamBuffer).uxStreamBufferNumber;
            prvInitialiseNewStreamBuffer(
                pxStreamBuffer,
                (*pxStreamBuffer).pucBuffer,
                (*pxStreamBuffer).xLength,
                (*pxStreamBuffer).xTriggerLevelBytes.load(Ordering::Relaxed),
                (*pxStreamBuffer).ucFlags,
                pxSendCompletedCallback,
                pxReceiveCompletedCallback,
            );
            #[cfg(feature = "trace-facility")]
            {
                (*pxStreamBuffer).uxStreamBufferNumber = uxStreamBufferNumber;
            }
            xReturn = pdPASS;
        }
    }
    portEXIT_CRITICAL();

    xReturn
}

/// Reset a stream buffer from ISR
///
/// # Safety
///
/// `xStreamBuffer` must identify a live buffer that remains allocated for the
/// call. The interrupt must be permitted to call FreeRTOS APIs, no task may be
/// blocked on the buffer, and no other execution context may access it during
/// the reset.
pub unsafe fn xStreamBufferResetFromISR(xStreamBuffer: StreamBufferHandle_t) -> BaseType_t {
    configASSERT(!xStreamBuffer.is_null());

    let pxStreamBuffer = xStreamBuffer as *mut StreamBuffer_t;
    let mut xReturn: BaseType_t = pdFAIL;

    let uxSavedInterruptStatus = portSET_INTERRUPT_MASK_FROM_ISR();
    {
        if (*pxStreamBuffer)
            .xTaskWaitingToReceive
            .load(Ordering::Acquire)
            .is_null()
            && (*pxStreamBuffer)
                .xTaskWaitingToSend
                .load(Ordering::Acquire)
                .is_null()
        {
            let pxSendCompletedCallback = (*pxStreamBuffer).pxSendCompletedCallback;
            let pxReceiveCompletedCallback = (*pxStreamBuffer).pxReceiveCompletedCallback;
            #[cfg(feature = "trace-facility")]
            let uxStreamBufferNumber = (*pxStreamBuffer).uxStreamBufferNumber;
            prvInitialiseNewStreamBuffer(
                pxStreamBuffer,
                (*pxStreamBuffer).pucBuffer,
                (*pxStreamBuffer).xLength,
                (*pxStreamBuffer).xTriggerLevelBytes.load(Ordering::Relaxed),
                (*pxStreamBuffer).ucFlags,
                pxSendCompletedCallback,
                pxReceiveCompletedCallback,
            );
            #[cfg(feature = "trace-facility")]
            {
                (*pxStreamBuffer).uxStreamBufferNumber = uxStreamBufferNumber;
            }
            xReturn = pdPASS;
        }
    }
    portCLEAR_INTERRUPT_MASK_FROM_ISR(uxSavedInterruptStatus);

    xReturn
}

// =============================================================================
// Query Functions
// =============================================================================

/// Get number of bytes available to read
///
/// # Safety
///
/// `xStreamBuffer` must identify a live buffer that cannot be reset or deleted
/// during the call.
pub unsafe fn xStreamBufferBytesAvailable(xStreamBuffer: StreamBufferHandle_t) -> usize {
    configASSERT(!xStreamBuffer.is_null());
    let pxStreamBuffer = xStreamBuffer as *const StreamBuffer_t;
    prvBytesInBuffer(pxStreamBuffer)
}

/// Get number of bytes of free space
///
/// # Safety
///
/// `xStreamBuffer` must identify a live buffer that cannot be reset or deleted
/// during the call.
pub unsafe fn xStreamBufferSpacesAvailable(xStreamBuffer: StreamBufferHandle_t) -> usize {
    configASSERT(!xStreamBuffer.is_null());
    let pxStreamBuffer = xStreamBuffer as *const StreamBuffer_t;

    // A stale tail only under-reports space, which is safe for the writer.
    // Acquire pairs with the reader's release-store after copying data out.
    let xTail = (*pxStreamBuffer).xTail.load(Ordering::Acquire);
    let xHead = (*pxStreamBuffer).xHead.load(Ordering::Relaxed);
    let mut xSpace = (*pxStreamBuffer).xLength + xTail;
    xSpace -= xHead;

    xSpace -= 1;

    if xSpace >= (*pxStreamBuffer).xLength {
        xSpace -= (*pxStreamBuffer).xLength;
    }

    xSpace
}

/// Check if stream buffer is empty
///
/// # Safety
///
/// `xStreamBuffer` must identify a live buffer that cannot be reset or deleted
/// during the call.
pub unsafe fn xStreamBufferIsEmpty(xStreamBuffer: StreamBufferHandle_t) -> BaseType_t {
    configASSERT(!xStreamBuffer.is_null());
    let pxStreamBuffer = xStreamBuffer as *const StreamBuffer_t;

    if (*pxStreamBuffer).xHead.load(Ordering::Acquire)
        == (*pxStreamBuffer).xTail.load(Ordering::Acquire)
    {
        pdTRUE
    } else {
        pdFALSE
    }
}

/// Check if stream buffer is full
///
/// # Safety
///
/// `xStreamBuffer` must identify a live buffer that cannot be reset or deleted
/// during the call.
pub unsafe fn xStreamBufferIsFull(xStreamBuffer: StreamBufferHandle_t) -> BaseType_t {
    configASSERT(!xStreamBuffer.is_null());
    let pxStreamBuffer = xStreamBuffer as *const StreamBuffer_t;

    let xBytesToStoreMessageLength = if ((*pxStreamBuffer).ucFlags & sbFLAGS_IS_MESSAGE_BUFFER) != 0
    {
        sbBYTES_TO_STORE_MESSAGE_LENGTH
    } else {
        0
    };

    if xStreamBufferSpacesAvailable(xStreamBuffer) <= xBytesToStoreMessageLength {
        pdTRUE
    } else {
        pdFALSE
    }
}

/// Set the trigger level
///
/// # Safety
///
/// `xStreamBuffer` must identify a live buffer that cannot be reset or deleted
/// during the call. Call from task context and synchronize this update with
/// operations that can block waiting for the buffer's trigger level.
pub unsafe fn xStreamBufferSetTriggerLevel(
    xStreamBuffer: StreamBufferHandle_t,
    mut xTriggerLevel: usize,
) -> BaseType_t {
    configASSERT(!xStreamBuffer.is_null());
    let pxStreamBuffer = xStreamBuffer as *mut StreamBuffer_t;

    if xTriggerLevel == 0 {
        xTriggerLevel = 1;
    }

    if xTriggerLevel < (*pxStreamBuffer).xLength {
        (*pxStreamBuffer)
            .xTriggerLevelBytes
            .store(xTriggerLevel, Ordering::Relaxed);
        pdPASS
    } else {
        pdFALSE
    }
}

/// Get the next message length (for message buffers)
///
/// # Safety
///
/// `xStreamBuffer` must identify a live buffer that cannot be reset or deleted
/// during the call. The caller must be the buffer's sole consumer, with no
/// concurrent receive operation advancing the read position.
pub unsafe fn xStreamBufferNextMessageLengthBytes(xStreamBuffer: StreamBufferHandle_t) -> usize {
    configASSERT(!xStreamBuffer.is_null());
    let pxStreamBuffer = xStreamBuffer as *mut StreamBuffer_t;

    if ((*pxStreamBuffer).ucFlags & sbFLAGS_IS_MESSAGE_BUFFER) != 0 {
        let xBytesAvailable = prvBytesInBuffer(pxStreamBuffer);

        if xBytesAvailable > sbBYTES_TO_STORE_MESSAGE_LENGTH {
            let mut xTempReturn: configMESSAGE_BUFFER_LENGTH_TYPE = 0;
            prvReadBytesFromBuffer(
                pxStreamBuffer,
                &mut xTempReturn as *mut _ as *mut u8,
                sbBYTES_TO_STORE_MESSAGE_LENGTH,
                (*pxStreamBuffer).xTail.load(Ordering::Acquire),
            );
            xTempReturn as usize
        } else {
            /* A message length prefix is published atomically with its
             * payload, so a non-zero partial prefix indicates corruption. */
            configASSERT(xBytesAvailable == 0);
            0
        }
    } else {
        0
    }
}

// =============================================================================
// Send Functions
// =============================================================================

/// Send bytes to a stream buffer
///
/// # Arguments
///
/// * `xStreamBuffer` - Handle to stream buffer
/// * `pvTxData` - Pointer to data to send
/// * `xDataLengthBytes` - Number of bytes to send
/// * `xTicksToWait` - Timeout in ticks
///
/// # Returns
///
/// Number of bytes actually sent
///
/// # Safety
///
/// `xStreamBuffer` must identify a live buffer that cannot be reset or deleted
/// during the call, and this caller must be its sole producer. `pvTxData` must
/// be non-null and readable for `xDataLengthBytes` bytes without overlapping
/// the stream buffer's storage. Call only from task context with initialized
/// scheduler and port state; any configured callback must be valid to invoke.
pub unsafe fn xStreamBufferSend(
    xStreamBuffer: StreamBufferHandle_t,
    pvTxData: *const c_void,
    xDataLengthBytes: usize,
    mut xTicksToWait: TickType_t,
) -> usize {
    configASSERT(!pvTxData.is_null());
    configASSERT(!xStreamBuffer.is_null());

    let pxStreamBuffer = xStreamBuffer as *mut StreamBuffer_t;
    let mut xSpace: usize = 0;
    let mut xRequiredSpace = xDataLengthBytes;
    let xMaxReportedSpace = (*pxStreamBuffer).xLength - 1;

    // For message buffers, need space for length prefix too
    if ((*pxStreamBuffer).ucFlags & sbFLAGS_IS_MESSAGE_BUFFER) != 0 {
        let Some(required_space) = xRequiredSpace.checked_add(sbBYTES_TO_STORE_MESSAGE_LENGTH)
        else {
            return 0;
        };
        xRequiredSpace = required_space;

        // Message won't fit even in empty buffer
        if xRequiredSpace > xMaxReportedSpace {
            xTicksToWait = 0;
        }
    } else {
        // For stream buffers, cap at max possible
        if xRequiredSpace > xMaxReportedSpace {
            xRequiredSpace = xMaxReportedSpace;
        }
    }

    if xTicksToWait != 0 {
        let mut xTimeOut = TimeOut_t::default();
        // SAFETY: xTimeOut is an initialized, uniquely owned local and this
        // send API runs in task context.
        unsafe { vTaskSetTimeOutState(&mut xTimeOut) };

        loop {
            portENTER_CRITICAL();
            {
                xSpace = xStreamBufferSpacesAvailable(xStreamBuffer);

                if xSpace < xRequiredSpace {
                    let uxNotificationIndex = (*pxStreamBuffer)
                        .uxNotificationIndex
                        .load(Ordering::Relaxed)
                        as UBaseType_t;

                    // Clear a stale notification before publishing the waiter,
                    // matching xTaskNotifyStateClearIndexed in upstream.
                    let _ = xTaskNotifyStateClearIndexed(ptr::null_mut(), uxNotificationIndex);

                    // Record that we're waiting
                    configASSERT(
                        (*pxStreamBuffer)
                            .xTaskWaitingToSend
                            .load(Ordering::Acquire)
                            .is_null(),
                    );
                    (*pxStreamBuffer)
                        .xTaskWaitingToSend
                        .store(xTaskGetCurrentTaskHandle(), Ordering::Release);
                } else {
                    portEXIT_CRITICAL();
                    break;
                }
            }
            portEXIT_CRITICAL();

            // Wait for notification
            let uxNotificationIndex = (*pxStreamBuffer)
                .uxNotificationIndex
                .load(Ordering::Relaxed) as UBaseType_t;
            xTaskNotifyWaitIndexed(uxNotificationIndex, 0, 0, ptr::null_mut(), xTicksToWait);
            (*pxStreamBuffer)
                .xTaskWaitingToSend
                .store(ptr::null_mut(), Ordering::Release);

            // SAFETY: both timeout values remain initialized and uniquely
            // owned throughout this task-context loop.
            if unsafe { xTaskCheckForTimeOut(&mut xTimeOut, &mut xTicksToWait) } != pdFALSE {
                break;
            }
        }
    }

    if xSpace == 0 {
        xSpace = xStreamBufferSpacesAvailable(xStreamBuffer);
    }

    let xReturn = prvWriteMessageToBuffer(
        pxStreamBuffer,
        pvTxData,
        xDataLengthBytes,
        xSpace,
        xRequiredSpace,
    );

    if xReturn > 0 {
        // Notify waiting receiver if trigger level reached
        if prvBytesInBuffer(pxStreamBuffer)
            >= (*pxStreamBuffer).xTriggerLevelBytes.load(Ordering::Relaxed)
        {
            prvSEND_COMPLETED(pxStreamBuffer);
        }
    }

    xReturn
}

/// Send bytes to a stream buffer from ISR
///
/// # Safety
///
/// `xStreamBuffer` must identify a live buffer that cannot be reset or deleted
/// during the call, and the interrupt-side caller must be its sole producer.
/// `pvTxData` must be non-null and readable for `xDataLengthBytes` bytes without
/// overlapping the stream buffer's storage. The interrupt must be permitted to
/// call FreeRTOS APIs, a non-null `pxHigherPriorityTaskWoken` must be uniquely
/// writable, and any configured callback must be valid to invoke from an ISR.
pub unsafe fn xStreamBufferSendFromISR(
    xStreamBuffer: StreamBufferHandle_t,
    pvTxData: *const c_void,
    xDataLengthBytes: usize,
    pxHigherPriorityTaskWoken: *mut BaseType_t,
) -> usize {
    configASSERT(!pvTxData.is_null());
    configASSERT(!xStreamBuffer.is_null());

    let pxStreamBuffer = xStreamBuffer as *mut StreamBuffer_t;
    let mut xRequiredSpace = xDataLengthBytes;

    if ((*pxStreamBuffer).ucFlags & sbFLAGS_IS_MESSAGE_BUFFER) != 0 {
        let Some(required_space) = xRequiredSpace.checked_add(sbBYTES_TO_STORE_MESSAGE_LENGTH)
        else {
            return 0;
        };
        xRequiredSpace = required_space;
    }

    let xSpace = xStreamBufferSpacesAvailable(xStreamBuffer);
    let xReturn = prvWriteMessageToBuffer(
        pxStreamBuffer,
        pvTxData,
        xDataLengthBytes,
        xSpace,
        xRequiredSpace,
    );

    if xReturn > 0 {
        if prvBytesInBuffer(pxStreamBuffer)
            >= (*pxStreamBuffer).xTriggerLevelBytes.load(Ordering::Relaxed)
        {
            prvSEND_COMPLETE_FROM_ISR(pxStreamBuffer, pxHigherPriorityTaskWoken);
        }
    }

    xReturn
}

// =============================================================================
// Receive Functions
// =============================================================================

/// Receive bytes from a stream buffer
///
/// # Arguments
///
/// * `xStreamBuffer` - Handle to stream buffer
/// * `pvRxData` - Buffer to receive into
/// * `xBufferLengthBytes` - Size of receive buffer
/// * `xTicksToWait` - Timeout in ticks
///
/// # Returns
///
/// Number of bytes received
///
/// # Safety
///
/// `xStreamBuffer` must identify a live buffer that cannot be reset or deleted
/// during the call, and this caller must be its sole consumer. `pvRxData` must
/// be non-null and writable for `xBufferLengthBytes` bytes without overlapping
/// the stream buffer's storage. Call only from task context with initialized
/// scheduler and port state; any configured callback must be valid to invoke.
pub unsafe fn xStreamBufferReceive(
    xStreamBuffer: StreamBufferHandle_t,
    pvRxData: *mut c_void,
    xBufferLengthBytes: usize,
    xTicksToWait: TickType_t,
) -> usize {
    configASSERT(!pvRxData.is_null());
    configASSERT(!xStreamBuffer.is_null());

    let pxStreamBuffer = xStreamBuffer as *mut StreamBuffer_t;
    let mut xReceivedLength: usize = 0;
    let mut xBytesAvailable: usize;

    // Determine minimum bytes before we'll unblock
    let xBytesToStoreMessageLength = if ((*pxStreamBuffer).ucFlags & sbFLAGS_IS_MESSAGE_BUFFER) != 0
    {
        sbBYTES_TO_STORE_MESSAGE_LENGTH
    } else if ((*pxStreamBuffer).ucFlags & sbFLAGS_IS_BATCHING_BUFFER) != 0 {
        (*pxStreamBuffer).xTriggerLevelBytes.load(Ordering::Relaxed)
    } else {
        0
    };

    if xTicksToWait != 0 {
        portENTER_CRITICAL();
        {
            xBytesAvailable = prvBytesInBuffer(pxStreamBuffer);

            if xBytesAvailable <= xBytesToStoreMessageLength {
                let uxNotificationIndex = (*pxStreamBuffer)
                    .uxNotificationIndex
                    .load(Ordering::Relaxed)
                    as UBaseType_t;

                let _ = xTaskNotifyStateClearIndexed(ptr::null_mut(), uxNotificationIndex);

                // Record that we're waiting
                configASSERT(
                    (*pxStreamBuffer)
                        .xTaskWaitingToReceive
                        .load(Ordering::Acquire)
                        .is_null(),
                );
                (*pxStreamBuffer)
                    .xTaskWaitingToReceive
                    .store(xTaskGetCurrentTaskHandle(), Ordering::Release);
            }
        }
        portEXIT_CRITICAL();

        if xBytesAvailable <= xBytesToStoreMessageLength {
            // Wait for notification
            let uxNotificationIndex = (*pxStreamBuffer)
                .uxNotificationIndex
                .load(Ordering::Relaxed) as UBaseType_t;
            xTaskNotifyWaitIndexed(uxNotificationIndex, 0, 0, ptr::null_mut(), xTicksToWait);
            (*pxStreamBuffer)
                .xTaskWaitingToReceive
                .store(ptr::null_mut(), Ordering::Release);

            // Recheck after blocking
            xBytesAvailable = prvBytesInBuffer(pxStreamBuffer);
        }
    } else {
        xBytesAvailable = prvBytesInBuffer(pxStreamBuffer);
    }

    if xBytesAvailable > xBytesToStoreMessageLength {
        xReceivedLength = prvReadMessageFromBuffer(
            pxStreamBuffer,
            pvRxData,
            xBufferLengthBytes,
            xBytesAvailable,
        );

        if xReceivedLength != 0 {
            prvRECEIVE_COMPLETED(pxStreamBuffer);
        }
    }

    xReceivedLength
}

/// Receive bytes from a stream buffer from ISR
///
/// # Safety
///
/// `xStreamBuffer` must identify a live buffer that cannot be reset or deleted
/// during the call, and the interrupt-side caller must be its sole consumer.
/// `pvRxData` must be non-null and writable for `xBufferLengthBytes` bytes
/// without overlapping the stream buffer's storage. The interrupt must be
/// permitted to call FreeRTOS APIs, a non-null `pxHigherPriorityTaskWoken` must
/// be uniquely writable, and any callback must be valid to invoke from an ISR.
pub unsafe fn xStreamBufferReceiveFromISR(
    xStreamBuffer: StreamBufferHandle_t,
    pvRxData: *mut c_void,
    xBufferLengthBytes: usize,
    pxHigherPriorityTaskWoken: *mut BaseType_t,
) -> usize {
    configASSERT(!pvRxData.is_null());
    configASSERT(!xStreamBuffer.is_null());

    let pxStreamBuffer = xStreamBuffer as *mut StreamBuffer_t;
    let mut xReceivedLength: usize = 0;

    let xBytesToStoreMessageLength = if ((*pxStreamBuffer).ucFlags & sbFLAGS_IS_MESSAGE_BUFFER) != 0
    {
        sbBYTES_TO_STORE_MESSAGE_LENGTH
    } else {
        0
    };

    let xBytesAvailable = prvBytesInBuffer(pxStreamBuffer);

    if xBytesAvailable > xBytesToStoreMessageLength {
        xReceivedLength = prvReadMessageFromBuffer(
            pxStreamBuffer,
            pvRxData,
            xBufferLengthBytes,
            xBytesAvailable,
        );

        if xReceivedLength != 0 {
            prvRECEIVE_COMPLETED_FROM_ISR(pxStreamBuffer, pxHigherPriorityTaskWoken);
        }
    }

    xReceivedLength
}

// =============================================================================
// Notification Index Functions
// =============================================================================

/// Get the notification index used by this stream buffer
///
/// # Safety
///
/// `xStreamBuffer` must identify a live buffer that cannot be reset or deleted
/// during the call.
pub unsafe fn uxStreamBufferGetStreamBufferNotificationIndex(
    xStreamBuffer: StreamBufferHandle_t,
) -> UBaseType_t {
    configASSERT(!xStreamBuffer.is_null());
    let pxStreamBuffer = xStreamBuffer as *const StreamBuffer_t;
    (*pxStreamBuffer)
        .uxNotificationIndex
        .load(Ordering::Relaxed) as UBaseType_t
}

/// Set the notification index used by this stream buffer
///
/// # Safety
///
/// `xStreamBuffer` must identify a live buffer that cannot be reset or deleted
/// during the call, and `uxNotificationIndex` must select an existing task
/// notification slot. Call from task context while no task is waiting on, or
/// can concurrently begin an operation on, this buffer.
pub unsafe fn vStreamBufferSetStreamBufferNotificationIndex(
    xStreamBuffer: StreamBufferHandle_t,
    uxNotificationIndex: UBaseType_t,
) {
    configASSERT(!xStreamBuffer.is_null());
    let pxStreamBuffer = xStreamBuffer as *mut StreamBuffer_t;

    // Should not change while tasks are waiting
    configASSERT(
        (*pxStreamBuffer)
            .xTaskWaitingToReceive
            .load(Ordering::Acquire)
            .is_null(),
    );
    configASSERT(
        (*pxStreamBuffer)
            .xTaskWaitingToSend
            .load(Ordering::Acquire)
            .is_null(),
    );
    configASSERT((uxNotificationIndex as usize) < configTASK_NOTIFICATION_ARRAY_ENTRIES);

    if (uxNotificationIndex as usize) < configTASK_NOTIFICATION_ARRAY_ENTRIES {
        (*pxStreamBuffer)
            .uxNotificationIndex
            .store(uxNotificationIndex as usize, Ordering::Relaxed);
    }
}

// =============================================================================
// ISR Notification Functions
// =============================================================================

/// Notify that send completed (from ISR)
///
/// # Safety
///
/// `xStreamBuffer` must identify a live buffer that cannot be reset or deleted
/// during the call. The interrupt must be permitted to call FreeRTOS APIs, and
/// a non-null `pxHigherPriorityTaskWoken` must be uniquely writable.
pub unsafe fn xStreamBufferSendCompletedFromISR(
    xStreamBuffer: StreamBufferHandle_t,
    pxHigherPriorityTaskWoken: *mut BaseType_t,
) -> BaseType_t {
    configASSERT(!xStreamBuffer.is_null());
    let pxStreamBuffer = xStreamBuffer as *mut StreamBuffer_t;
    let mut xReturn: BaseType_t = pdFALSE;

    let uxSavedInterruptStatus = portSET_INTERRUPT_MASK_FROM_ISR();
    {
        let xTaskWaitingToReceive = prvTakeWaitingTask(&(*pxStreamBuffer).xTaskWaitingToReceive);
        if !xTaskWaitingToReceive.is_null() {
            xTaskNotifyIndexedFromISR(
                xTaskWaitingToReceive,
                (*pxStreamBuffer)
                    .uxNotificationIndex
                    .load(Ordering::Relaxed) as UBaseType_t,
                0,
                eNoAction as i32,
                pxHigherPriorityTaskWoken,
            );
            xReturn = pdTRUE;
        }
    }
    portCLEAR_INTERRUPT_MASK_FROM_ISR(uxSavedInterruptStatus);

    xReturn
}

/// Notify that receive completed (from ISR)
///
/// # Safety
///
/// `xStreamBuffer` must identify a live buffer that cannot be reset or deleted
/// during the call. The interrupt must be permitted to call FreeRTOS APIs, and
/// a non-null `pxHigherPriorityTaskWoken` must be uniquely writable.
pub unsafe fn xStreamBufferReceiveCompletedFromISR(
    xStreamBuffer: StreamBufferHandle_t,
    pxHigherPriorityTaskWoken: *mut BaseType_t,
) -> BaseType_t {
    configASSERT(!xStreamBuffer.is_null());
    let pxStreamBuffer = xStreamBuffer as *mut StreamBuffer_t;
    let mut xReturn: BaseType_t = pdFALSE;

    let uxSavedInterruptStatus = portSET_INTERRUPT_MASK_FROM_ISR();
    {
        let xTaskWaitingToSend = prvTakeWaitingTask(&(*pxStreamBuffer).xTaskWaitingToSend);
        if !xTaskWaitingToSend.is_null() {
            xTaskNotifyIndexedFromISR(
                xTaskWaitingToSend,
                (*pxStreamBuffer)
                    .uxNotificationIndex
                    .load(Ordering::Relaxed) as UBaseType_t,
                0,
                eNoAction as i32,
                pxHigherPriorityTaskWoken,
            );
            xReturn = pdTRUE;
        }
    }
    portCLEAR_INTERRUPT_MASK_FROM_ISR(uxSavedInterruptStatus);

    xReturn
}

// =============================================================================
// Private Helper Functions
// =============================================================================

/// Initialize a new stream buffer
unsafe fn prvInitialiseNewStreamBuffer(
    pxStreamBuffer: *mut StreamBuffer_t,
    pucBuffer: *mut u8,
    xBufferSizeBytes: usize,
    xTriggerLevelBytes: usize,
    ucFlags: u8,
    pxSendCompletedCallback: StreamBufferCallbackFunction_t,
    pxReceiveCompletedCallback: StreamBufferCallbackFunction_t,
) {
    /* [AMENDMENT] Construct atomic members as atomics rather than relying on
     * byte-wise zeroing. */
    ptr::write(
        pxStreamBuffer,
        StreamBuffer_t {
            xTail: AtomicUsize::new(0),
            xHead: AtomicUsize::new(0),
            xLength: xBufferSizeBytes,
            xTriggerLevelBytes: AtomicUsize::new(xTriggerLevelBytes),
            xTaskWaitingToReceive: AtomicPtr::new(ptr::null_mut()),
            xTaskWaitingToSend: AtomicPtr::new(ptr::null_mut()),
            pucBuffer,
            ucFlags,
            #[cfg(feature = "trace-facility")]
            uxStreamBufferNumber: 0,
            pxSendCompletedCallback,
            pxReceiveCompletedCallback,
            uxNotificationIndex: AtomicUsize::new(tskDEFAULT_INDEX_TO_NOTIFY as usize),
        },
    );
}

/// Load and clear a waiting-task slot from within the surrounding FreeRTOS
/// scheduler/interrupt exclusion region.
///
// [AMENDMENT] Cortex-M0 provides atomic pointer loads/stores but no atomic
// exchange. The surrounding critical section or scheduler exclusion preserves
// the upstream serialization, while atomic accesses keep task/ISR aliasing
// valid in Rust.
#[inline(always)]
fn prvTakeWaitingTask(pxWaitingTask: &AtomicPtr<c_void>) -> TaskHandle_t {
    let xTask = pxWaitingTask.load(Ordering::Acquire);
    if !xTask.is_null() {
        pxWaitingTask.store(ptr::null_mut(), Ordering::Release);
    }
    xTask
}

/// Calculate bytes in the buffer
unsafe fn prvBytesInBuffer(pxStreamBuffer: *const StreamBuffer_t) -> usize {
    // Acquire publishes bytes copied by the writer before its head update.
    let xHead = (*pxStreamBuffer).xHead.load(Ordering::Acquire);
    let xTail = (*pxStreamBuffer).xTail.load(Ordering::Acquire);
    let mut xCount = (*pxStreamBuffer).xLength + xHead;
    xCount -= xTail;

    if xCount >= (*pxStreamBuffer).xLength {
        xCount -= (*pxStreamBuffer).xLength;
    }

    xCount
}

/// Write bytes to buffer (returns new head position)
unsafe fn prvWriteBytesToBuffer(
    pxStreamBuffer: *mut StreamBuffer_t,
    pucData: *const u8,
    xCount: usize,
    mut xHead: usize,
) -> usize {
    configASSERT(xCount > 0);

    // Calculate bytes writable before wrap
    let xFirstLength = core::cmp::min((*pxStreamBuffer).xLength - xHead, xCount);

    // Write first chunk
    ptr::copy_nonoverlapping(
        pucData,
        (*pxStreamBuffer).pucBuffer.add(xHead),
        xFirstLength,
    );

    // Write remaining after wrap
    if xCount > xFirstLength {
        ptr::copy_nonoverlapping(
            pucData.add(xFirstLength),
            (*pxStreamBuffer).pucBuffer,
            xCount - xFirstLength,
        );
    }

    xHead += xCount;
    if xHead >= (*pxStreamBuffer).xLength {
        xHead -= (*pxStreamBuffer).xLength;
    }

    xHead
}

/// Read bytes from buffer (returns new tail position)
unsafe fn prvReadBytesFromBuffer(
    pxStreamBuffer: *mut StreamBuffer_t,
    pucData: *mut u8,
    xCount: usize,
    mut xTail: usize,
) -> usize {
    configASSERT(xCount > 0);

    // Calculate bytes readable before wrap
    let xFirstLength = core::cmp::min((*pxStreamBuffer).xLength - xTail, xCount);

    // Read first chunk
    ptr::copy_nonoverlapping(
        (*pxStreamBuffer).pucBuffer.add(xTail),
        pucData,
        xFirstLength,
    );

    // Read remaining after wrap
    if xCount > xFirstLength {
        ptr::copy_nonoverlapping(
            (*pxStreamBuffer).pucBuffer,
            pucData.add(xFirstLength),
            xCount - xFirstLength,
        );
    }

    xTail += xCount;
    if xTail >= (*pxStreamBuffer).xLength {
        xTail -= (*pxStreamBuffer).xLength;
    }

    xTail
}

/// Write a message to the buffer
unsafe fn prvWriteMessageToBuffer(
    pxStreamBuffer: *mut StreamBuffer_t,
    pvTxData: *const c_void,
    mut xDataLengthBytes: usize,
    xSpace: usize,
    xRequiredSpace: usize,
) -> usize {
    let mut xNextHead = (*pxStreamBuffer).xHead.load(Ordering::Relaxed);

    if ((*pxStreamBuffer).ucFlags & sbFLAGS_IS_MESSAGE_BUFFER) != 0 {
        // Message buffer - write length prefix first
        if xSpace >= xRequiredSpace {
            let Ok(xMessageLength) = configMESSAGE_BUFFER_LENGTH_TYPE::try_from(xDataLengthBytes)
            else {
                return 0;
            };
            xNextHead = prvWriteBytesToBuffer(
                pxStreamBuffer,
                &xMessageLength as *const _ as *const u8,
                sbBYTES_TO_STORE_MESSAGE_LENGTH,
                xNextHead,
            );
        } else {
            // Not enough space
            xDataLengthBytes = 0;
        }
    } else {
        // Stream buffer - write as many bytes as possible
        xDataLengthBytes = core::cmp::min(xDataLengthBytes, xSpace);
    }

    if xDataLengthBytes != 0 {
        let xHead = prvWriteBytesToBuffer(
            pxStreamBuffer,
            pvTxData as *const u8,
            xDataLengthBytes,
            xNextHead,
        );
        // Publish the copied bytes to the reader.
        (*pxStreamBuffer).xHead.store(xHead, Ordering::Release);
    }

    xDataLengthBytes
}

/// Read a message from the buffer
unsafe fn prvReadMessageFromBuffer(
    pxStreamBuffer: *mut StreamBuffer_t,
    pvRxData: *mut c_void,
    xBufferLengthBytes: usize,
    mut xBytesAvailable: usize,
) -> usize {
    let mut xNextTail = (*pxStreamBuffer).xTail.load(Ordering::Relaxed);
    let xNextMessageLength: usize;

    if ((*pxStreamBuffer).ucFlags & sbFLAGS_IS_MESSAGE_BUFFER) != 0 {
        // Read the message length
        let mut xTempMessageLength: configMESSAGE_BUFFER_LENGTH_TYPE = 0;
        xNextTail = prvReadBytesFromBuffer(
            pxStreamBuffer,
            &mut xTempMessageLength as *mut _ as *mut u8,
            sbBYTES_TO_STORE_MESSAGE_LENGTH,
            xNextTail,
        );
        xNextMessageLength = xTempMessageLength as usize;
        xBytesAvailable -= sbBYTES_TO_STORE_MESSAGE_LENGTH;

        // Check user buffer is large enough
        if xNextMessageLength > xBufferLengthBytes {
            // User buffer too small - don't read
            return 0;
        }
    } else {
        // Stream buffer - read as many as possible
        xNextMessageLength = xBufferLengthBytes;
    }

    let xCount = core::cmp::min(xNextMessageLength, xBytesAvailable);

    if xCount != 0 {
        let xTail = prvReadBytesFromBuffer(pxStreamBuffer, pvRxData as *mut u8, xCount, xNextTail);
        // Publish completion of the read before the writer reuses the space.
        (*pxStreamBuffer).xTail.store(xTail, Ordering::Release);
    }

    xCount
}

// =============================================================================
// Notification Macros as Functions
// =============================================================================

/// Send completed notification
#[inline(always)]
unsafe fn sbSEND_COMPLETED(pxStreamBuffer: *mut StreamBuffer_t) {
    vTaskSuspendAll();
    let xTaskWaitingToReceive = prvTakeWaitingTask(&(*pxStreamBuffer).xTaskWaitingToReceive);
    if !xTaskWaitingToReceive.is_null() {
        xTaskNotifyIndexed(
            xTaskWaitingToReceive,
            (*pxStreamBuffer)
                .uxNotificationIndex
                .load(Ordering::Relaxed) as UBaseType_t,
            0,
            eNoAction as i32,
        );
    }
    xTaskResumeAll();
}

/// Invoke an instance callback, falling back to the standard task notification.
#[inline(always)]
unsafe fn prvSEND_COMPLETED(pxStreamBuffer: *mut StreamBuffer_t) {
    if let Some(pxCallback) = (*pxStreamBuffer).pxSendCompletedCallback {
        pxCallback(
            pxStreamBuffer as StreamBufferHandle_t,
            pdFALSE,
            ptr::null_mut(),
        );
    } else {
        sbSEND_COMPLETED(pxStreamBuffer);
    }
}

/// Send completed notification from ISR
#[inline(always)]
unsafe fn sbSEND_COMPLETE_FROM_ISR(
    pxStreamBuffer: *mut StreamBuffer_t,
    pxHigherPriorityTaskWoken: *mut BaseType_t,
) {
    let uxSavedInterruptStatus = portSET_INTERRUPT_MASK_FROM_ISR();
    let xTaskWaitingToReceive = prvTakeWaitingTask(&(*pxStreamBuffer).xTaskWaitingToReceive);
    if !xTaskWaitingToReceive.is_null() {
        xTaskNotifyIndexedFromISR(
            xTaskWaitingToReceive,
            (*pxStreamBuffer)
                .uxNotificationIndex
                .load(Ordering::Relaxed) as UBaseType_t,
            0,
            eNoAction as i32,
            pxHigherPriorityTaskWoken,
        );
    }
    portCLEAR_INTERRUPT_MASK_FROM_ISR(uxSavedInterruptStatus);
}

/// Invoke an instance callback from ISR, falling back to task notification.
#[inline(always)]
unsafe fn prvSEND_COMPLETE_FROM_ISR(
    pxStreamBuffer: *mut StreamBuffer_t,
    pxHigherPriorityTaskWoken: *mut BaseType_t,
) {
    if let Some(pxCallback) = (*pxStreamBuffer).pxSendCompletedCallback {
        pxCallback(
            pxStreamBuffer as StreamBufferHandle_t,
            pdTRUE,
            pxHigherPriorityTaskWoken,
        );
    } else {
        sbSEND_COMPLETE_FROM_ISR(pxStreamBuffer, pxHigherPriorityTaskWoken);
    }
}

/// Receive completed notification
#[inline(always)]
unsafe fn sbRECEIVE_COMPLETED(pxStreamBuffer: *mut StreamBuffer_t) {
    vTaskSuspendAll();
    let xTaskWaitingToSend = prvTakeWaitingTask(&(*pxStreamBuffer).xTaskWaitingToSend);
    if !xTaskWaitingToSend.is_null() {
        xTaskNotifyIndexed(
            xTaskWaitingToSend,
            (*pxStreamBuffer)
                .uxNotificationIndex
                .load(Ordering::Relaxed) as UBaseType_t,
            0,
            eNoAction as i32,
        );
    }
    xTaskResumeAll();
}

/// Invoke an instance callback, falling back to the standard task notification.
#[inline(always)]
unsafe fn prvRECEIVE_COMPLETED(pxStreamBuffer: *mut StreamBuffer_t) {
    if let Some(pxCallback) = (*pxStreamBuffer).pxReceiveCompletedCallback {
        pxCallback(
            pxStreamBuffer as StreamBufferHandle_t,
            pdFALSE,
            ptr::null_mut(),
        );
    } else {
        sbRECEIVE_COMPLETED(pxStreamBuffer);
    }
}

/// Receive completed notification from ISR
#[inline(always)]
unsafe fn sbRECEIVE_COMPLETED_FROM_ISR(
    pxStreamBuffer: *mut StreamBuffer_t,
    pxHigherPriorityTaskWoken: *mut BaseType_t,
) {
    let uxSavedInterruptStatus = portSET_INTERRUPT_MASK_FROM_ISR();
    let xTaskWaitingToSend = prvTakeWaitingTask(&(*pxStreamBuffer).xTaskWaitingToSend);
    if !xTaskWaitingToSend.is_null() {
        xTaskNotifyIndexedFromISR(
            xTaskWaitingToSend,
            (*pxStreamBuffer)
                .uxNotificationIndex
                .load(Ordering::Relaxed) as UBaseType_t,
            0,
            eNoAction as i32,
            pxHigherPriorityTaskWoken,
        );
    }
    portCLEAR_INTERRUPT_MASK_FROM_ISR(uxSavedInterruptStatus);
}

/// Invoke an instance callback from ISR, falling back to task notification.
#[inline(always)]
unsafe fn prvRECEIVE_COMPLETED_FROM_ISR(
    pxStreamBuffer: *mut StreamBuffer_t,
    pxHigherPriorityTaskWoken: *mut BaseType_t,
) {
    if let Some(pxCallback) = (*pxStreamBuffer).pxReceiveCompletedCallback {
        pxCallback(
            pxStreamBuffer as StreamBufferHandle_t,
            pdTRUE,
            pxHigherPriorityTaskWoken,
        );
    } else {
        sbRECEIVE_COMPLETED_FROM_ISR(pxStreamBuffer, pxHigherPriorityTaskWoken);
    }
}

/// Notification action: no action (just wake task)
const eNoAction: u32 = 0;

// =============================================================================
// Static Buffer Query Function
// =============================================================================

/// Get pointers to the static buffers used by a statically allocated stream buffer
///
/// Returns pdTRUE if the stream buffer was statically allocated, pdFALSE otherwise.
/// If statically allocated, the pointers to the storage area and static structure
/// are returned via the output parameters.
///
/// # Safety
///
/// The caller must ensure the output pointers are valid.
pub unsafe fn xStreamBufferGetStaticBuffers(
    xStreamBuffer: StreamBufferHandle_t,
    ppucStreamBufferStorageArea: *mut *mut u8,
    ppxStaticStreamBuffer: *mut *mut StaticStreamBuffer_t,
) -> BaseType_t {
    configASSERT(!xStreamBuffer.is_null());
    configASSERT(!ppucStreamBufferStorageArea.is_null());
    configASSERT(!ppxStaticStreamBuffer.is_null());

    let pxStreamBuffer = xStreamBuffer as *mut StreamBuffer_t;
    let xReturn: BaseType_t;

    if ((*pxStreamBuffer).ucFlags & sbFLAGS_IS_STATICALLY_ALLOCATED) != 0 {
        xReturn = pdTRUE;

        // Return the storage area pointer
        *ppucStreamBufferStorageArea = (*pxStreamBuffer).pucBuffer;

        // Return the static structure pointer (same as the handle for static allocation)
        *ppxStaticStreamBuffer = pxStreamBuffer as *mut StaticStreamBuffer_t;
    } else {
        xReturn = pdFALSE;
    }

    xReturn
}

// =============================================================================
// Message Buffer Header-Macro Veneers
// =============================================================================

/// Return the caller-provided storage behind a static message buffer.
///
/// # Safety
///
/// The caller must uphold [`xStreamBufferGetStaticBuffers`]'s live-handle and
/// output-pointer requirements.
#[inline(always)]
pub unsafe fn xMessageBufferGetStaticBuffers(
    xMessageBuffer: MessageBufferHandle_t,
    ppucMessageBufferStorageArea: *mut *mut u8,
    ppxStaticMessageBuffer: *mut *mut StaticMessageBuffer_t,
) -> BaseType_t {
    xStreamBufferGetStaticBuffers(
        xMessageBuffer,
        ppucMessageBufferStorageArea,
        ppxStaticMessageBuffer,
    )
}

/// Send one complete message from task context.
///
/// # Safety
///
/// The caller must uphold [`xStreamBufferSend`]'s live-handle, source-buffer,
/// single-writer, and task-context requirements.
#[inline(always)]
pub unsafe fn xMessageBufferSend(
    xMessageBuffer: MessageBufferHandle_t,
    pvTxData: *const c_void,
    xDataLengthBytes: usize,
    xTicksToWait: TickType_t,
) -> usize {
    xStreamBufferSend(xMessageBuffer, pvTxData, xDataLengthBytes, xTicksToWait)
}

/// Send one complete message from interrupt context.
///
/// # Safety
///
/// The caller must uphold [`xStreamBufferSendFromISR`]'s live-handle,
/// source-buffer, single-writer, wake-pointer, and ISR-priority requirements.
#[inline(always)]
pub unsafe fn xMessageBufferSendFromISR(
    xMessageBuffer: MessageBufferHandle_t,
    pvTxData: *const c_void,
    xDataLengthBytes: usize,
    pxHigherPriorityTaskWoken: *mut BaseType_t,
) -> usize {
    xStreamBufferSendFromISR(
        xMessageBuffer,
        pvTxData,
        xDataLengthBytes,
        pxHigherPriorityTaskWoken,
    )
}

/// Receive one complete message in task context.
///
/// # Safety
///
/// The caller must uphold [`xStreamBufferReceive`]'s live-handle,
/// destination-buffer, single-reader, and task-context requirements.
#[inline(always)]
pub unsafe fn xMessageBufferReceive(
    xMessageBuffer: MessageBufferHandle_t,
    pvRxData: *mut c_void,
    xBufferLengthBytes: usize,
    xTicksToWait: TickType_t,
) -> usize {
    xStreamBufferReceive(xMessageBuffer, pvRxData, xBufferLengthBytes, xTicksToWait)
}

/// Receive one complete message from interrupt context.
///
/// # Safety
///
/// The caller must uphold [`xStreamBufferReceiveFromISR`]'s live-handle,
/// destination-buffer, single-reader, wake-pointer, and ISR requirements.
#[inline(always)]
pub unsafe fn xMessageBufferReceiveFromISR(
    xMessageBuffer: MessageBufferHandle_t,
    pvRxData: *mut c_void,
    xBufferLengthBytes: usize,
    pxHigherPriorityTaskWoken: *mut BaseType_t,
) -> usize {
    xStreamBufferReceiveFromISR(
        xMessageBuffer,
        pvRxData,
        xBufferLengthBytes,
        pxHigherPriorityTaskWoken,
    )
}

/// Delete a message buffer.
///
/// # Safety
///
/// The caller must uphold [`vStreamBufferDelete`]'s exclusive final-access and
/// allocation-origin requirements.
#[inline(always)]
pub unsafe fn vMessageBufferDelete(xMessageBuffer: MessageBufferHandle_t) {
    vStreamBufferDelete(xMessageBuffer)
}

/// Return whether a message buffer is full.
///
/// # Safety
///
/// The handle must identify a live message buffer through the call.
#[inline(always)]
pub unsafe fn xMessageBufferIsFull(xMessageBuffer: MessageBufferHandle_t) -> BaseType_t {
    xStreamBufferIsFull(xMessageBuffer)
}

/// Return whether a message buffer is empty.
///
/// # Safety
///
/// The handle must identify a live message buffer through the call.
#[inline(always)]
pub unsafe fn xMessageBufferIsEmpty(xMessageBuffer: MessageBufferHandle_t) -> BaseType_t {
    xStreamBufferIsEmpty(xMessageBuffer)
}

/// Reset a message buffer from task context.
///
/// # Safety
///
/// The caller must uphold [`xStreamBufferReset`]'s live-handle, exclusion, and
/// task-context requirements.
#[inline(always)]
pub unsafe fn xMessageBufferReset(xMessageBuffer: MessageBufferHandle_t) -> BaseType_t {
    xStreamBufferReset(xMessageBuffer)
}

/// Reset a message buffer from interrupt context.
///
/// # Safety
///
/// The caller must uphold [`xStreamBufferResetFromISR`]'s live-handle,
/// exclusion, and ISR-priority requirements.
#[inline(always)]
pub unsafe fn xMessageBufferResetFromISR(xMessageBuffer: MessageBufferHandle_t) -> BaseType_t {
    xStreamBufferResetFromISR(xMessageBuffer)
}

/// Return the free space in a message buffer.
///
/// # Safety
///
/// The handle must identify a live message buffer through the call.
#[inline(always)]
pub unsafe fn xMessageBufferSpaceAvailable(xMessageBuffer: MessageBufferHandle_t) -> usize {
    xStreamBufferSpacesAvailable(xMessageBuffer)
}

/// Compatibility spelling of [`xMessageBufferSpaceAvailable`].
///
/// # Safety
///
/// The handle must identify a live message buffer through the call.
#[inline(always)]
pub unsafe fn xMessageBufferSpacesAvailable(xMessageBuffer: MessageBufferHandle_t) -> usize {
    xStreamBufferSpacesAvailable(xMessageBuffer)
}

/// Return the length of the next complete message.
///
/// # Safety
///
/// The handle must identify a live, single-reader message buffer through the
/// call.
#[inline(always)]
pub unsafe fn xMessageBufferNextLengthBytes(xMessageBuffer: MessageBufferHandle_t) -> usize {
    xStreamBufferNextMessageLengthBytes(xMessageBuffer)
}

/// Run message-buffer send completion processing from an interrupt.
///
/// # Safety
///
/// The caller must uphold [`xStreamBufferSendCompletedFromISR`]'s live-handle,
/// wake-pointer, and ISR-priority requirements.
#[inline(always)]
pub unsafe fn xMessageBufferSendCompletedFromISR(
    xMessageBuffer: MessageBufferHandle_t,
    pxHigherPriorityTaskWoken: *mut BaseType_t,
) -> BaseType_t {
    xStreamBufferSendCompletedFromISR(xMessageBuffer, pxHigherPriorityTaskWoken)
}

/// Run message-buffer receive completion processing from an interrupt.
///
/// # Safety
///
/// The caller must uphold [`xStreamBufferReceiveCompletedFromISR`]'s
/// live-handle, wake-pointer, and ISR-priority requirements.
#[inline(always)]
pub unsafe fn xMessageBufferReceiveCompletedFromISR(
    xMessageBuffer: MessageBufferHandle_t,
    pxHigherPriorityTaskWoken: *mut BaseType_t,
) -> BaseType_t {
    xStreamBufferReceiveCompletedFromISR(xMessageBuffer, pxHigherPriorityTaskWoken)
}

// =============================================================================
// Trace Facility Functions
// =============================================================================

/// Get the stream buffer number (for trace facility)
///
/// Returns the number assigned to this stream buffer, which can be used
/// for trace and debugging purposes.
///
/// # Safety
///
/// `xStreamBuffer` must identify a live buffer that cannot be reset or deleted
/// during the call. Access to its trace number must not race a setter.
#[cfg(feature = "trace-facility")]
pub unsafe fn uxStreamBufferGetStreamBufferNumber(
    xStreamBuffer: StreamBufferHandle_t,
) -> UBaseType_t {
    configASSERT(!xStreamBuffer.is_null());
    let pxStreamBuffer = xStreamBuffer as *const StreamBuffer_t;
    (*pxStreamBuffer).uxStreamBufferNumber
}

/// Set the stream buffer number (for trace facility)
///
/// Assigns a number to this stream buffer for trace and debugging purposes.
///
/// # Safety
///
/// `xStreamBuffer` must identify a live buffer that cannot be reset or deleted
/// during the call. The caller must have exclusive access to its trace number.
#[cfg(feature = "trace-facility")]
pub unsafe fn vStreamBufferSetStreamBufferNumber(
    xStreamBuffer: StreamBufferHandle_t,
    uxStreamBufferNumber: UBaseType_t,
) {
    configASSERT(!xStreamBuffer.is_null());
    let pxStreamBuffer = xStreamBuffer as *mut StreamBuffer_t;
    (*pxStreamBuffer).uxStreamBufferNumber = uxStreamBufferNumber;
}

/// Get the stream buffer type (for trace facility)
///
/// Returns non-zero if this is a message buffer, zero if it's a stream buffer.
///
/// # Safety
///
/// `xStreamBuffer` must identify a live buffer that cannot be reset or deleted
/// during the call.
#[cfg(feature = "trace-facility")]
pub unsafe fn ucStreamBufferGetStreamBufferType(xStreamBuffer: StreamBufferHandle_t) -> u8 {
    configASSERT(!xStreamBuffer.is_null());
    let pxStreamBuffer = xStreamBuffer as *const StreamBuffer_t;
    (*pxStreamBuffer).ucFlags & sbFLAGS_IS_MESSAGE_BUFFER
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::sync::atomic::{AtomicUsize, Ordering};

    static SEND_CALLBACKS: AtomicUsize = AtomicUsize::new(0);
    static RECEIVE_CALLBACKS: AtomicUsize = AtomicUsize::new(0);
    static ISR_CALLBACKS: AtomicUsize = AtomicUsize::new(0);

    extern "C" fn send_callback(
        _xStreamBuffer: StreamBufferHandle_t,
        xIsInsideISR: BaseType_t,
        _pxHigherPriorityTaskWoken: *mut BaseType_t,
    ) {
        SEND_CALLBACKS.fetch_add(1, Ordering::Relaxed);
        if xIsInsideISR != pdFALSE {
            ISR_CALLBACKS.fetch_add(1, Ordering::Relaxed);
        }
    }

    extern "C" fn receive_callback(
        _xStreamBuffer: StreamBufferHandle_t,
        xIsInsideISR: BaseType_t,
        _pxHigherPriorityTaskWoken: *mut BaseType_t,
    ) {
        RECEIVE_CALLBACKS.fetch_add(1, Ordering::Relaxed);
        if xIsInsideISR != pdFALSE {
            ISR_CALLBACKS.fetch_add(1, Ordering::Relaxed);
        }
    }

    extern "C" fn no_op_callback(
        _xStreamBuffer: StreamBufferHandle_t,
        _xIsInsideISR: BaseType_t,
        _pxHigherPriorityTaskWoken: *mut BaseType_t,
    ) {
    }

    #[test]
    fn static_stream_buffer_storage_matches_internal_layout() {
        assert_eq!(
            core::mem::size_of::<StaticStreamBuffer_t>(),
            core::mem::size_of::<StreamBuffer_t>()
        );
        assert_eq!(
            core::mem::align_of::<StaticStreamBuffer_t>(),
            core::mem::align_of::<StreamBuffer_t>()
        );
    }

    #[cfg(feature = "port-test")]
    #[test]
    fn callbacks_survive_reset_and_distinguish_isr_context() {
        crate::port::test_port_reset();
        SEND_CALLBACKS.store(0, Ordering::Relaxed);
        RECEIVE_CALLBACKS.store(0, Ordering::Relaxed);
        ISR_CALLBACKS.store(0, Ordering::Relaxed);

        let mut storage = [0u8; 17];
        let mut control = StaticStreamBuffer_t::new();
        let handle = unsafe {
            xStreamBufferGenericCreateStatic(
                storage.len(),
                1,
                sbTYPE_STREAM_BUFFER,
                storage.as_mut_ptr(),
                &mut control,
                Some(send_callback),
                Some(receive_callback),
            )
        };
        assert!(!handle.is_null());

        #[cfg(feature = "trace-facility")]
        unsafe {
            vStreamBufferSetStreamBufferNumber(handle, 123);
        }

        let sent = [1u8, 2, 3];
        let mut received = [0u8; 3];
        assert_eq!(
            unsafe { xStreamBufferSend(handle, sent.as_ptr().cast(), sent.len(), 0) },
            sent.len()
        );
        assert_eq!(
            unsafe {
                xStreamBufferReceive(handle, received.as_mut_ptr().cast(), received.len(), 0)
            },
            received.len()
        );
        assert_eq!(received, sent);
        assert_eq!(unsafe { xStreamBufferReset(handle) }, pdPASS);

        #[cfg(feature = "trace-facility")]
        assert_eq!(unsafe { uxStreamBufferGetStreamBufferNumber(handle) }, 123);

        let mut higher_priority_task_woken = pdFALSE;
        assert_eq!(
            unsafe {
                xStreamBufferSendFromISR(
                    handle,
                    sent.as_ptr().cast(),
                    sent.len(),
                    &mut higher_priority_task_woken,
                )
            },
            sent.len()
        );
        assert_eq!(
            unsafe {
                xStreamBufferReceiveFromISR(
                    handle,
                    received.as_mut_ptr().cast(),
                    received.len(),
                    &mut higher_priority_task_woken,
                )
            },
            received.len()
        );
        assert_eq!(unsafe { xStreamBufferResetFromISR(handle) }, pdPASS);
        assert_eq!(
            unsafe { xStreamBufferSend(handle, sent.as_ptr().cast(), sent.len(), 0) },
            sent.len()
        );
        assert_eq!(
            unsafe {
                xStreamBufferReceive(handle, received.as_mut_ptr().cast(), received.len(), 0)
            },
            received.len()
        );

        #[cfg(feature = "trace-facility")]
        assert_eq!(unsafe { uxStreamBufferGetStreamBufferNumber(handle) }, 123);

        assert_eq!(SEND_CALLBACKS.load(Ordering::Relaxed), 3);
        assert_eq!(RECEIVE_CALLBACKS.load(Ordering::Relaxed), 3);
        assert_eq!(ISR_CALLBACKS.load(Ordering::Relaxed), 2);
    }

    #[cfg(feature = "port-test")]
    #[test]
    fn header_macro_veneers_preserve_buffer_types_and_message_boundaries() {
        crate::port::test_port_reset();

        let mut stream_storage = [0u8; 8];
        let mut stream_control = StaticStreamBuffer_t::new();
        let stream = unsafe {
            xStreamBufferCreateStatic(
                stream_storage.len(),
                2,
                stream_storage.as_mut_ptr(),
                &mut stream_control,
            )
        };
        assert!(!stream.is_null());
        assert_eq!(
            unsafe { (*(stream as *mut StreamBuffer_t)).ucFlags },
            sbFLAGS_IS_STATICALLY_ALLOCATED,
        );

        let mut batching_storage = [0u8; 8];
        let mut batching_control = StaticStreamBuffer_t::new();
        let batching = unsafe {
            xStreamBatchingBufferCreateStaticWithCallback(
                batching_storage.len(),
                2,
                batching_storage.as_mut_ptr(),
                &mut batching_control,
                Some(no_op_callback),
                Some(no_op_callback),
            )
        };
        assert!(!batching.is_null());
        assert_eq!(
            unsafe { (*(batching as *mut StreamBuffer_t)).ucFlags },
            sbFLAGS_IS_STATICALLY_ALLOCATED | sbFLAGS_IS_BATCHING_BUFFER,
        );

        let mut message_storage = [0u8; 24];
        let mut message_control = StaticMessageBuffer_t::new();
        let message_storage_ptr = message_storage.as_mut_ptr();
        let message_control_ptr = core::ptr::addr_of_mut!(message_control);
        let message = unsafe {
            xMessageBufferCreateStaticWithCallback(
                message_storage.len(),
                message_storage_ptr,
                message_control_ptr,
                Some(no_op_callback),
                Some(no_op_callback),
            )
        };
        assert!(!message.is_null());
        assert_eq!(
            unsafe { (*(message as *mut StreamBuffer_t)).ucFlags },
            sbFLAGS_IS_STATICALLY_ALLOCATED | sbFLAGS_IS_MESSAGE_BUFFER,
        );

        let payload = [1u8, 2, 3];
        assert_eq!(
            unsafe { xMessageBufferSend(message, payload.as_ptr().cast(), payload.len(), 0) },
            payload.len(),
        );
        assert_eq!(
            unsafe { xMessageBufferNextLengthBytes(message) },
            payload.len(),
        );
        assert_eq!(unsafe { xMessageBufferIsEmpty(message) }, pdFALSE);
        assert_eq!(unsafe { xMessageBufferSpaceAvailable(message) }, unsafe {
            xMessageBufferSpacesAvailable(message)
        },);

        let mut received = [0u8; 3];
        assert_eq!(
            unsafe {
                xMessageBufferReceive(message, received.as_mut_ptr().cast(), received.len(), 0)
            },
            received.len(),
        );
        assert_eq!(received, payload);

        let mut returned_storage = ptr::null_mut();
        let mut returned_control = ptr::null_mut();
        assert_eq!(
            unsafe {
                xMessageBufferGetStaticBuffers(
                    message,
                    &mut returned_storage,
                    &mut returned_control,
                )
            },
            pdTRUE,
        );
        assert_eq!(returned_storage, message_storage_ptr);
        assert!(core::ptr::eq(returned_control, message_control_ptr));

        crate::port::test_port_set_inside_interrupt(true);
        let mut higher_priority_task_woken = pdFALSE;
        assert_eq!(
            unsafe {
                xMessageBufferSendFromISR(
                    message,
                    payload.as_ptr().cast(),
                    payload.len(),
                    &mut higher_priority_task_woken,
                )
            },
            payload.len(),
        );
        assert_eq!(
            unsafe {
                xMessageBufferReceiveFromISR(
                    message,
                    received.as_mut_ptr().cast(),
                    received.len(),
                    &mut higher_priority_task_woken,
                )
            },
            received.len(),
        );
        assert_eq!(
            unsafe { xMessageBufferSendCompletedFromISR(message, &mut higher_priority_task_woken) },
            pdFALSE,
        );
        assert_eq!(
            unsafe {
                xMessageBufferReceiveCompletedFromISR(message, &mut higher_priority_task_woken)
            },
            pdFALSE,
        );
        assert_eq!(unsafe { xMessageBufferResetFromISR(message) }, pdPASS);
        crate::port::test_port_set_inside_interrupt(false);

        unsafe {
            assert_eq!(xMessageBufferReset(message), pdPASS);
            assert_eq!(xMessageBufferIsFull(message), pdFALSE);
            vMessageBufferDelete(message);
        }
    }

    #[cfg(feature = "port-test")]
    #[test]
    #[should_panic(expected = "FreeRTOS assertion failed")]
    fn next_message_length_rejects_a_partial_published_prefix() {
        let mut storage = [0u8; 16];
        let mut control = StaticStreamBuffer_t::new();
        // SAFETY: both buffers are unique and remain live for the test.
        let handle = unsafe {
            xStreamBufferGenericCreateStatic(
                storage.len(),
                1,
                sbTYPE_MESSAGE_BUFFER,
                storage.as_mut_ptr(),
                &mut control,
                None,
                None,
            )
        };
        assert!(!handle.is_null());

        // Model the impossible/corrupt state diagnosed by upstream: exactly a
        // length prefix is visible without any payload byte.
        let stream = handle as *mut StreamBuffer_t;
        unsafe {
            (*stream)
                .xHead
                .store(sbBYTES_TO_STORE_MESSAGE_LENGTH, Ordering::Release);
            let _ = xStreamBufferNextMessageLengthBytes(handle);
        }
    }

    #[cfg(all(
        feature = "port-test",
        not(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))
    ))]
    #[test]
    #[should_panic(expected = "FreeRTOS assertion failed")]
    fn static_only_delete_rejects_a_corrupt_dynamic_allocation_flag() {
        let mut storage = [0u8; 8];
        let mut control = StaticStreamBuffer_t::new();
        // SAFETY: both buffers are unique and remain live for the test.
        let handle = unsafe {
            xStreamBufferGenericCreateStatic(
                storage.len(),
                1,
                sbTYPE_STREAM_BUFFER,
                storage.as_mut_ptr(),
                &mut control,
                None,
                None,
            )
        };
        assert!(!handle.is_null());

        // Simulate structure corruption: this configuration cannot create a
        // dynamically allocated object, so upstream asserts on deletion.
        unsafe {
            (*(handle as *mut StreamBuffer_t)).ucFlags &= !sbFLAGS_IS_STATICALLY_ALLOCATED;
            vStreamBufferDelete(handle);
        }
    }

    #[cfg(all(feature = "port-test", feature = "heap-4"))]
    #[test]
    fn dynamic_buffer_uses_and_returns_freertos_heap_storage() {
        use crate::memory::{pvPortMalloc, vPortFree, vPortHeapResetState, xPortGetFreeHeapSize};

        // SAFETY: this isolated test has no live heap allocations at reset.
        unsafe { vPortHeapResetState() };
        unsafe {
            let probe = pvPortMalloc(1);
            assert!(!probe.is_null());
            vPortFree(probe);
            let initial_free = xPortGetFreeHeapSize();

            let handle = xStreamBufferCreate(127, 1);
            assert!(!handle.is_null());
            assert!(xPortGetFreeHeapSize() < initial_free);
            assert_eq!(handle as usize % crate::port::portBYTE_ALIGNMENT, 0,);

            vStreamBufferDelete(handle);
            assert_eq!(xPortGetFreeHeapSize(), initial_free);
        }
        // SAFETY: the stream buffer and probe were both deleted above.
        unsafe { vPortHeapResetState() };
    }

    #[cfg(feature = "port-test")]
    #[test]
    fn atomic_spsc_handoff_preserves_bytes_under_host_threads() {
        #[cfg(miri)]
        const TRANSFERS: usize = 500;
        #[cfg(not(miri))]
        const TRANSFERS: usize = 50_000;

        struct SendHandle(StreamBufferHandle_t);

        impl SendHandle {
            fn into_raw(self) -> StreamBufferHandle_t {
                self.0
            }
        }

        // SAFETY: the fixture leaks the buffer for both threads' lifetimes and
        // assigns exactly one sender and one receiver, which is the concurrency
        // contract implemented by the stream buffer's atomic indices.
        unsafe impl Send for SendHandle {}

        let storage = std::boxed::Box::leak(std::boxed::Box::new([0u8; 257]));
        let control = std::boxed::Box::leak(std::boxed::Box::new(StaticStreamBuffer_t::new()));
        let handle = unsafe {
            xStreamBufferGenericCreateStatic(
                storage.len(),
                1,
                sbTYPE_STREAM_BUFFER,
                storage.as_mut_ptr(),
                control,
                Some(no_op_callback),
                Some(no_op_callback),
            )
        };
        assert!(!handle.is_null());

        let sender_handle = SendHandle(handle);
        let sender = std::thread::spawn(move || {
            let handle = sender_handle.into_raw();
            let mut index = 0usize;
            while index < TRANSFERS {
                let byte = (index % 251) as u8;
                if unsafe { xStreamBufferSend(handle, (&byte as *const u8).cast(), 1, 0) } == 1 {
                    index += 1;
                } else {
                    std::thread::yield_now();
                }
            }
        });

        let receiver_handle = SendHandle(handle);
        let receiver = std::thread::spawn(move || {
            let handle = receiver_handle.into_raw();
            let mut index = 0usize;
            while index < TRANSFERS {
                let mut byte = 0u8;
                if unsafe { xStreamBufferReceive(handle, (&mut byte as *mut u8).cast(), 1, 0) } == 1
                {
                    assert_eq!(byte, (index % 251) as u8);
                    index += 1;
                } else {
                    std::thread::yield_now();
                }
            }
        });

        sender.join().unwrap();
        receiver.join().unwrap();
        assert_eq!(unsafe { xStreamBufferBytesAvailable(handle) }, 0);
    }
}
