/*
 * FreeRTOS Kernel <DEVELOPMENT BRANCH>
 * Copyright (C) 2021 Amazon.com, Inc. or its affiliates. All Rights Reserved.
 *
 * SPDX-License-Identifier: MIT
 *
 * [AMENDMENT] This is the Rust port of stream_buffer.c for FreeRusTOS.
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

use crate::config::*;
use crate::kernel::tasks::*;
use crate::port::*;
use crate::types::*;

#[cfg(any(feature = "alloc", feature = "heap-4"))]
extern crate alloc;
#[cfg(any(feature = "alloc", feature = "heap-4"))]
use alloc::alloc::{alloc, dealloc, Layout};

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
const sbBYTES_TO_STORE_MESSAGE_LENGTH: usize = core::mem::size_of::<configMESSAGE_BUFFER_LENGTH_TYPE>();

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
    pub xTail: usize,

    /// Index to the next item to write within the buffer
    pub xHead: usize,

    /// The length of the buffer pointed to by pucBuffer
    pub xLength: usize,

    /// Number of bytes that must be in buffer before unblocking waiting task
    pub xTriggerLevelBytes: usize,

    /// Handle of task waiting for data (or NULL)
    pub xTaskWaitingToReceive: TaskHandle_t,

    /// Handle of task waiting to send (or NULL)
    pub xTaskWaitingToSend: TaskHandle_t,

    /// Points to the buffer storage area
    pub pucBuffer: *mut u8,

    /// Flags for buffer type
    pub ucFlags: u8,

    /// Stream buffer number for tracing
    #[cfg(feature = "trace-facility")]
    pub uxStreamBufferNumber: UBaseType_t,

    /// Notification index to use
    pub uxNotificationIndex: UBaseType_t,
}

/// Static stream buffer structure for user allocation
#[repr(C)]
pub struct StaticStreamBuffer_t {
    _reserved: [u8; core::mem::size_of::<StreamBuffer_t>()],
}

impl StaticStreamBuffer_t {
    pub const fn new() -> Self {
        StaticStreamBuffer_t {
            _reserved: [0u8; core::mem::size_of::<StreamBuffer_t>()],
        }
    }
}

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
#[cfg(any(feature = "alloc", feature = "heap-4"))]
pub unsafe fn xStreamBufferGenericCreate(
    xBufferSizeBytes: usize,
    mut xTriggerLevelBytes: usize,
    xStreamBufferType: BaseType_t,
    pxSendCompletedCallback: StreamBufferCallbackFunction_t,
    pxReceiveCompletedCallback: StreamBufferCallbackFunction_t,
) -> StreamBufferHandle_t {
    let ucFlags: u8;

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

    // Allocate structure and buffer together
    // Add 1 to buffer size for implementation quirk (makes reported free space correct)
    let xBufferSizeBytes = xBufferSizeBytes + 1;
    let total_size = core::mem::size_of::<StreamBuffer_t>() + xBufferSizeBytes;

    let layout = Layout::from_size_align(total_size, core::mem::align_of::<StreamBuffer_t>())
        .expect("Invalid layout");
    let pvAllocatedMemory = alloc(layout);

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

/// Convenience wrapper: create a stream buffer
#[cfg(any(feature = "alloc", feature = "heap-4"))]
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

/// Convenience wrapper: create a message buffer
#[cfg(any(feature = "alloc", feature = "heap-4"))]
#[inline(always)]
pub unsafe fn xMessageBufferCreate(xBufferSizeBytes: usize) -> StreamBufferHandle_t {
    xStreamBufferGenericCreate(xBufferSizeBytes, 1, sbTYPE_MESSAGE_BUFFER, None, None)
}

/// Convenience wrapper: create a batching buffer
#[cfg(any(feature = "alloc", feature = "heap-4"))]
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

// =============================================================================
// Delete Function
// =============================================================================

/// Delete a stream buffer
///
/// Frees memory if dynamically allocated, otherwise zeros the structure.
pub unsafe fn vStreamBufferDelete(xStreamBuffer: StreamBufferHandle_t) {
    configASSERT(!xStreamBuffer.is_null());

    let pxStreamBuffer = xStreamBuffer as *mut StreamBuffer_t;

    if ((*pxStreamBuffer).ucFlags & sbFLAGS_IS_STATICALLY_ALLOCATED) == 0 {
        #[cfg(any(feature = "alloc", feature = "heap-4"))]
        {
            // Both structure and buffer were allocated together
            let total_size = core::mem::size_of::<StreamBuffer_t>() + (*pxStreamBuffer).xLength;
            let layout =
                Layout::from_size_align(total_size, core::mem::align_of::<StreamBuffer_t>())
                    .expect("Invalid layout");
            dealloc(pxStreamBuffer as *mut u8, layout);
        }
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
pub unsafe fn xStreamBufferReset(xStreamBuffer: StreamBufferHandle_t) -> BaseType_t {
    configASSERT(!xStreamBuffer.is_null());

    let pxStreamBuffer = xStreamBuffer as *mut StreamBuffer_t;
    let mut xReturn: BaseType_t = pdFAIL;

    portENTER_CRITICAL();
    {
        if (*pxStreamBuffer).xTaskWaitingToReceive.is_null()
            && (*pxStreamBuffer).xTaskWaitingToSend.is_null()
        {
            prvInitialiseNewStreamBuffer(
                pxStreamBuffer,
                (*pxStreamBuffer).pucBuffer,
                (*pxStreamBuffer).xLength,
                (*pxStreamBuffer).xTriggerLevelBytes,
                (*pxStreamBuffer).ucFlags,
                None,
                None,
            );
            xReturn = pdPASS;
        }
    }
    portEXIT_CRITICAL();

    xReturn
}

/// Reset a stream buffer from ISR
pub unsafe fn xStreamBufferResetFromISR(xStreamBuffer: StreamBufferHandle_t) -> BaseType_t {
    configASSERT(!xStreamBuffer.is_null());

    let pxStreamBuffer = xStreamBuffer as *mut StreamBuffer_t;
    let mut xReturn: BaseType_t = pdFAIL;

    let uxSavedInterruptStatus = portSET_INTERRUPT_MASK_FROM_ISR();
    {
        if (*pxStreamBuffer).xTaskWaitingToReceive.is_null()
            && (*pxStreamBuffer).xTaskWaitingToSend.is_null()
        {
            prvInitialiseNewStreamBuffer(
                pxStreamBuffer,
                (*pxStreamBuffer).pucBuffer,
                (*pxStreamBuffer).xLength,
                (*pxStreamBuffer).xTriggerLevelBytes,
                (*pxStreamBuffer).ucFlags,
                None,
                None,
            );
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
pub unsafe fn xStreamBufferBytesAvailable(xStreamBuffer: StreamBufferHandle_t) -> usize {
    configASSERT(!xStreamBuffer.is_null());
    let pxStreamBuffer = xStreamBuffer as *const StreamBuffer_t;
    prvBytesInBuffer(pxStreamBuffer)
}

/// Get number of bytes of free space
pub unsafe fn xStreamBufferSpacesAvailable(xStreamBuffer: StreamBufferHandle_t) -> usize {
    configASSERT(!xStreamBuffer.is_null());
    let pxStreamBuffer = xStreamBuffer as *const StreamBuffer_t;

    // Read tail and head atomically (retry if tail changes)
    let mut xSpace: usize;
    let mut xOriginalTail: usize;

    loop {
        xOriginalTail = (*pxStreamBuffer).xTail;
        xSpace = (*pxStreamBuffer).xLength + (*pxStreamBuffer).xTail;
        xSpace -= (*pxStreamBuffer).xHead;

        if xOriginalTail == (*pxStreamBuffer).xTail {
            break;
        }
    }

    xSpace -= 1;

    if xSpace >= (*pxStreamBuffer).xLength {
        xSpace -= (*pxStreamBuffer).xLength;
    }

    xSpace
}

/// Check if stream buffer is empty
pub unsafe fn xStreamBufferIsEmpty(xStreamBuffer: StreamBufferHandle_t) -> BaseType_t {
    configASSERT(!xStreamBuffer.is_null());
    let pxStreamBuffer = xStreamBuffer as *const StreamBuffer_t;

    if (*pxStreamBuffer).xHead == (*pxStreamBuffer).xTail {
        pdTRUE
    } else {
        pdFALSE
    }
}

/// Check if stream buffer is full
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
        (*pxStreamBuffer).xTriggerLevelBytes = xTriggerLevel;
        pdPASS
    } else {
        pdFALSE
    }
}

/// Get the next message length (for message buffers)
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
                (*pxStreamBuffer).xTail,
            );
            xTempReturn as usize
        } else {
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
        xRequiredSpace += sbBYTES_TO_STORE_MESSAGE_LENGTH;

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
        vTaskSetTimeOutState(&mut xTimeOut);

        loop {
            portENTER_CRITICAL();
            {
                xSpace = xStreamBufferSpacesAvailable(xStreamBuffer);

                if xSpace < xRequiredSpace {
                    // Record that we're waiting
                    configASSERT((*pxStreamBuffer).xTaskWaitingToSend.is_null());
                    (*pxStreamBuffer).xTaskWaitingToSend = xTaskGetCurrentTaskHandle();
                } else {
                    portEXIT_CRITICAL();
                    break;
                }
            }
            portEXIT_CRITICAL();

            // Wait for notification
            xTaskNotifyWait(0, 0, ptr::null_mut(), xTicksToWait);
            (*pxStreamBuffer).xTaskWaitingToSend = ptr::null_mut();

            if xTaskCheckForTimeOut(&mut xTimeOut, &mut xTicksToWait) != pdFALSE {
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
        if prvBytesInBuffer(pxStreamBuffer) >= (*pxStreamBuffer).xTriggerLevelBytes {
            sbSEND_COMPLETED(pxStreamBuffer);
        }
    }

    xReturn
}

/// Send bytes to a stream buffer from ISR
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
        xRequiredSpace += sbBYTES_TO_STORE_MESSAGE_LENGTH;
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
        if prvBytesInBuffer(pxStreamBuffer) >= (*pxStreamBuffer).xTriggerLevelBytes {
            sbSEND_COMPLETE_FROM_ISR(pxStreamBuffer, pxHigherPriorityTaskWoken);
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
    let xBytesToStoreMessageLength =
        if ((*pxStreamBuffer).ucFlags & sbFLAGS_IS_MESSAGE_BUFFER) != 0 {
            sbBYTES_TO_STORE_MESSAGE_LENGTH
        } else if ((*pxStreamBuffer).ucFlags & sbFLAGS_IS_BATCHING_BUFFER) != 0 {
            (*pxStreamBuffer).xTriggerLevelBytes
        } else {
            0
        };

    if xTicksToWait != 0 {
        portENTER_CRITICAL();
        {
            xBytesAvailable = prvBytesInBuffer(pxStreamBuffer);

            if xBytesAvailable <= xBytesToStoreMessageLength {
                // Record that we're waiting
                configASSERT((*pxStreamBuffer).xTaskWaitingToReceive.is_null());
                (*pxStreamBuffer).xTaskWaitingToReceive = xTaskGetCurrentTaskHandle();
            }
        }
        portEXIT_CRITICAL();

        if xBytesAvailable <= xBytesToStoreMessageLength {
            // Wait for notification
            xTaskNotifyWait(0, 0, ptr::null_mut(), xTicksToWait);
            (*pxStreamBuffer).xTaskWaitingToReceive = ptr::null_mut();

            // Recheck after blocking
            xBytesAvailable = prvBytesInBuffer(pxStreamBuffer);
        }
    } else {
        xBytesAvailable = prvBytesInBuffer(pxStreamBuffer);
    }

    if xBytesAvailable > xBytesToStoreMessageLength {
        xReceivedLength =
            prvReadMessageFromBuffer(pxStreamBuffer, pvRxData, xBufferLengthBytes, xBytesAvailable);

        if xReceivedLength != 0 {
            sbRECEIVE_COMPLETED(pxStreamBuffer);
        }
    }

    xReceivedLength
}

/// Receive bytes from a stream buffer from ISR
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

    let xBytesToStoreMessageLength =
        if ((*pxStreamBuffer).ucFlags & sbFLAGS_IS_MESSAGE_BUFFER) != 0 {
            sbBYTES_TO_STORE_MESSAGE_LENGTH
        } else {
            0
        };

    let xBytesAvailable = prvBytesInBuffer(pxStreamBuffer);

    if xBytesAvailable > xBytesToStoreMessageLength {
        xReceivedLength =
            prvReadMessageFromBuffer(pxStreamBuffer, pvRxData, xBufferLengthBytes, xBytesAvailable);

        if xReceivedLength != 0 {
            sbRECEIVE_COMPLETED_FROM_ISR(pxStreamBuffer, pxHigherPriorityTaskWoken);
        }
    }

    xReceivedLength
}

// =============================================================================
// Notification Index Functions
// =============================================================================

/// Get the notification index used by this stream buffer
pub unsafe fn uxStreamBufferGetStreamBufferNotificationIndex(
    xStreamBuffer: StreamBufferHandle_t,
) -> UBaseType_t {
    configASSERT(!xStreamBuffer.is_null());
    let pxStreamBuffer = xStreamBuffer as *const StreamBuffer_t;
    (*pxStreamBuffer).uxNotificationIndex
}

/// Set the notification index used by this stream buffer
pub unsafe fn vStreamBufferSetStreamBufferNotificationIndex(
    xStreamBuffer: StreamBufferHandle_t,
    uxNotificationIndex: UBaseType_t,
) {
    configASSERT(!xStreamBuffer.is_null());
    let pxStreamBuffer = xStreamBuffer as *mut StreamBuffer_t;

    // Should not change while tasks are waiting
    configASSERT((*pxStreamBuffer).xTaskWaitingToReceive.is_null());
    configASSERT((*pxStreamBuffer).xTaskWaitingToSend.is_null());

    (*pxStreamBuffer).uxNotificationIndex = uxNotificationIndex;
}

// =============================================================================
// ISR Notification Functions
// =============================================================================

/// Notify that send completed (from ISR)
pub unsafe fn xStreamBufferSendCompletedFromISR(
    xStreamBuffer: StreamBufferHandle_t,
    pxHigherPriorityTaskWoken: *mut BaseType_t,
) -> BaseType_t {
    configASSERT(!xStreamBuffer.is_null());
    let pxStreamBuffer = xStreamBuffer as *mut StreamBuffer_t;
    let mut xReturn: BaseType_t = pdFALSE;

    let uxSavedInterruptStatus = portSET_INTERRUPT_MASK_FROM_ISR();
    {
        if !(*pxStreamBuffer).xTaskWaitingToReceive.is_null() {
            xTaskNotifyFromISR(
                (*pxStreamBuffer).xTaskWaitingToReceive,
                0,
                eNoAction as i32,
                pxHigherPriorityTaskWoken,
            );
            (*pxStreamBuffer).xTaskWaitingToReceive = ptr::null_mut();
            xReturn = pdTRUE;
        }
    }
    portCLEAR_INTERRUPT_MASK_FROM_ISR(uxSavedInterruptStatus);

    xReturn
}

/// Notify that receive completed (from ISR)
pub unsafe fn xStreamBufferReceiveCompletedFromISR(
    xStreamBuffer: StreamBufferHandle_t,
    pxHigherPriorityTaskWoken: *mut BaseType_t,
) -> BaseType_t {
    configASSERT(!xStreamBuffer.is_null());
    let pxStreamBuffer = xStreamBuffer as *mut StreamBuffer_t;
    let mut xReturn: BaseType_t = pdFALSE;

    let uxSavedInterruptStatus = portSET_INTERRUPT_MASK_FROM_ISR();
    {
        if !(*pxStreamBuffer).xTaskWaitingToSend.is_null() {
            xTaskNotifyFromISR(
                (*pxStreamBuffer).xTaskWaitingToSend,
                0,
                eNoAction as i32,
                pxHigherPriorityTaskWoken,
            );
            (*pxStreamBuffer).xTaskWaitingToSend = ptr::null_mut();
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
    _pxSendCompletedCallback: StreamBufferCallbackFunction_t,
    _pxReceiveCompletedCallback: StreamBufferCallbackFunction_t,
) {
    // Zero the structure
    ptr::write_bytes(pxStreamBuffer, 0, 1);

    (*pxStreamBuffer).pucBuffer = pucBuffer;
    (*pxStreamBuffer).xLength = xBufferSizeBytes;
    (*pxStreamBuffer).xTriggerLevelBytes = xTriggerLevelBytes;
    (*pxStreamBuffer).ucFlags = ucFlags;
    (*pxStreamBuffer).uxNotificationIndex = tskDEFAULT_INDEX_TO_NOTIFY;
}

/// Calculate bytes in the buffer
unsafe fn prvBytesInBuffer(pxStreamBuffer: *const StreamBuffer_t) -> usize {
    let mut xCount = (*pxStreamBuffer).xLength + (*pxStreamBuffer).xHead;
    xCount -= (*pxStreamBuffer).xTail;

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
    let mut xNextHead = (*pxStreamBuffer).xHead;

    if ((*pxStreamBuffer).ucFlags & sbFLAGS_IS_MESSAGE_BUFFER) != 0 {
        // Message buffer - write length prefix first
        if xSpace >= xRequiredSpace {
            let xMessageLength = xDataLengthBytes as configMESSAGE_BUFFER_LENGTH_TYPE;
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
        (*pxStreamBuffer).xHead = prvWriteBytesToBuffer(
            pxStreamBuffer,
            pvTxData as *const u8,
            xDataLengthBytes,
            xNextHead,
        );
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
    let mut xNextTail = (*pxStreamBuffer).xTail;
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
        (*pxStreamBuffer).xTail =
            prvReadBytesFromBuffer(pxStreamBuffer, pvRxData as *mut u8, xCount, xNextTail);
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
    if !(*pxStreamBuffer).xTaskWaitingToReceive.is_null() {
        xTaskNotify(
            (*pxStreamBuffer).xTaskWaitingToReceive,
            0,
            eNoAction as i32,
        );
        (*pxStreamBuffer).xTaskWaitingToReceive = ptr::null_mut();
    }
    xTaskResumeAll();
}

/// Send completed notification from ISR
#[inline(always)]
unsafe fn sbSEND_COMPLETE_FROM_ISR(
    pxStreamBuffer: *mut StreamBuffer_t,
    pxHigherPriorityTaskWoken: *mut BaseType_t,
) {
    let uxSavedInterruptStatus = portSET_INTERRUPT_MASK_FROM_ISR();
    if !(*pxStreamBuffer).xTaskWaitingToReceive.is_null() {
        xTaskNotifyFromISR(
            (*pxStreamBuffer).xTaskWaitingToReceive,
            0,
            eNoAction as i32,
            pxHigherPriorityTaskWoken,
        );
        (*pxStreamBuffer).xTaskWaitingToReceive = ptr::null_mut();
    }
    portCLEAR_INTERRUPT_MASK_FROM_ISR(uxSavedInterruptStatus);
}

/// Receive completed notification
#[inline(always)]
unsafe fn sbRECEIVE_COMPLETED(pxStreamBuffer: *mut StreamBuffer_t) {
    vTaskSuspendAll();
    if !(*pxStreamBuffer).xTaskWaitingToSend.is_null() {
        xTaskNotify((*pxStreamBuffer).xTaskWaitingToSend, 0, eNoAction as i32);
        (*pxStreamBuffer).xTaskWaitingToSend = ptr::null_mut();
    }
    xTaskResumeAll();
}

/// Receive completed notification from ISR
#[inline(always)]
unsafe fn sbRECEIVE_COMPLETED_FROM_ISR(
    pxStreamBuffer: *mut StreamBuffer_t,
    pxHigherPriorityTaskWoken: *mut BaseType_t,
) {
    let uxSavedInterruptStatus = portSET_INTERRUPT_MASK_FROM_ISR();
    if !(*pxStreamBuffer).xTaskWaitingToSend.is_null() {
        xTaskNotifyFromISR(
            (*pxStreamBuffer).xTaskWaitingToSend,
            0,
            eNoAction as i32,
            pxHigherPriorityTaskWoken,
        );
        (*pxStreamBuffer).xTaskWaitingToSend = ptr::null_mut();
    }
    portCLEAR_INTERRUPT_MASK_FROM_ISR(uxSavedInterruptStatus);
}

/// Notification action: no action (just wake task)
const eNoAction: u32 = 0;

