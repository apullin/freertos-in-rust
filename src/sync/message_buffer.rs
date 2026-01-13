//! Safe MessageBuffer wrapper
//!
//! Provides a safe wrapper around FreeRTOS message buffers.
//! Message buffers transfer discrete, variable-length messages
//! (unlike stream buffers which transfer continuous byte streams).
//!
//! **Important**: Message buffers assume a single writer and single reader.
//! Multiple writers or readers require external synchronization.

use core::ffi::c_void;
use core::mem::{size_of, MaybeUninit};

use crate::kernel::stream_buffer::{
    sbTYPE_MESSAGE_BUFFER, xStreamBufferBytesAvailable, xStreamBufferGenericCreate,
    xStreamBufferIsEmpty, xStreamBufferIsFull, xStreamBufferNextMessageLengthBytes,
    xStreamBufferReceive, xStreamBufferReset, xStreamBufferSend, xStreamBufferSpacesAvailable,
    StreamBufferHandle_t,
};
use crate::types::*;

/// A message buffer for transferring discrete, variable-length messages.
///
/// Unlike `StreamBuffer` which handles continuous byte streams, `MessageBuffer`
/// transfers complete messages. Each message is stored with a length prefix,
/// and receivers always get complete messages (never partial).
///
/// # Single Producer / Single Consumer
///
/// Message buffers are designed for one writer and one reader. If you need
/// multiple writers or readers, protect access with a mutex.
///
/// # Message Size
///
/// Each message has a 4-byte length overhead. The buffer size must be large
/// enough to hold at least one complete message plus its length prefix.
pub struct MessageBuffer {
    handle: StreamBufferHandle_t,
}

// Safety: MessageBuffer handles can be shared between tasks
// (though only one should write and one should read)
unsafe impl Sync for MessageBuffer {}
unsafe impl Send for MessageBuffer {}

impl MessageBuffer {
    /// Creates a new message buffer.
    ///
    /// # Arguments
    ///
    /// * `size` - Buffer capacity in bytes (must fit at least one message + 4 bytes overhead)
    ///
    /// Returns `None` if creation failed (e.g., out of memory).
    #[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
    pub fn new(size: usize) -> Option<Self> {
        let handle =
            unsafe { xStreamBufferGenericCreate(size, 1, sbTYPE_MESSAGE_BUFFER, None, None) };
        if handle.is_null() {
            None
        } else {
            Some(Self { handle })
        }
    }

    // =========================================================================
    // Send
    // =========================================================================

    /// Sends a message to the buffer, blocking until space is available.
    ///
    /// Returns `true` if the message was sent, `false` if it couldn't fit
    /// (message larger than buffer capacity).
    pub fn send(&self, data: &[u8]) -> bool {
        self.send_timeout(data, portMAX_DELAY)
    }

    /// Attempts to send a message without blocking.
    ///
    /// Returns `true` if sent, `false` if no space or message too large.
    pub fn try_send(&self, data: &[u8]) -> bool {
        self.send_timeout(data, 0)
    }

    /// Sends a message with a timeout.
    ///
    /// Returns `true` if the message was sent within the timeout.
    /// Returns `false` if timeout elapsed or message too large for buffer.
    pub fn send_timeout(&self, data: &[u8], ticks: TickType_t) -> bool {
        if data.is_empty() {
            return false;
        }
        let sent = unsafe {
            xStreamBufferSend(
                self.handle,
                data.as_ptr() as *const c_void,
                data.len(),
                ticks,
            )
        };
        // For message buffers, it's all-or-nothing
        sent == data.len()
    }

    // =========================================================================
    // Typed Send
    // =========================================================================

    /// Sends a typed value as a message, blocking until space is available.
    ///
    /// The value is sent as its raw bytes representation.
    pub fn send_val<T: Copy>(&self, value: &T) -> bool {
        self.send_val_timeout(value, portMAX_DELAY)
    }

    /// Attempts to send a typed value without blocking.
    pub fn try_send_val<T: Copy>(&self, value: &T) -> bool {
        self.send_val_timeout(value, 0)
    }

    /// Sends a typed value with a timeout.
    pub fn send_val_timeout<T: Copy>(&self, value: &T, ticks: TickType_t) -> bool {
        let data =
            unsafe { core::slice::from_raw_parts(value as *const T as *const u8, size_of::<T>()) };
        self.send_timeout(data, ticks)
    }

    // =========================================================================
    // Receive
    // =========================================================================

    /// Receives a message from the buffer, blocking until one is available.
    ///
    /// Returns the number of bytes received, or 0 if the provided buffer
    /// is too small for the message (message remains in buffer).
    pub fn receive(&self, buf: &mut [u8]) -> usize {
        self.receive_timeout(buf, portMAX_DELAY)
    }

    /// Attempts to receive a message without blocking.
    ///
    /// Returns the number of bytes received, or 0 if no message available
    /// or buffer too small.
    pub fn try_receive(&self, buf: &mut [u8]) -> usize {
        self.receive_timeout(buf, 0)
    }

    /// Receives a message with a timeout.
    ///
    /// Returns the number of bytes received, or 0 if timeout elapsed
    /// or buffer too small for the waiting message.
    pub fn receive_timeout(&self, buf: &mut [u8], ticks: TickType_t) -> usize {
        if buf.is_empty() {
            return 0;
        }
        unsafe {
            xStreamBufferReceive(
                self.handle,
                buf.as_mut_ptr() as *mut c_void,
                buf.len(),
                ticks,
            )
        }
    }

    // =========================================================================
    // Typed Receive
    // =========================================================================

    /// Receives a message as a typed value, blocking until one is available.
    ///
    /// Returns `Some(value)` if a message of exactly `size_of::<T>()` bytes
    /// was received, `None` otherwise.
    pub fn receive_val<T: Copy>(&self) -> Option<T> {
        self.receive_val_timeout(portMAX_DELAY)
    }

    /// Attempts to receive a typed value without blocking.
    pub fn try_receive_val<T: Copy>(&self) -> Option<T> {
        self.receive_val_timeout(0)
    }

    /// Receives a typed value with a timeout.
    ///
    /// Returns `Some(value)` if a message of exactly `size_of::<T>()` bytes
    /// was received within the timeout, `None` otherwise.
    pub fn receive_val_timeout<T: Copy>(&self, ticks: TickType_t) -> Option<T> {
        let mut value = MaybeUninit::<T>::uninit();
        let received = unsafe {
            xStreamBufferReceive(
                self.handle,
                value.as_mut_ptr() as *mut c_void,
                size_of::<T>(),
                ticks,
            )
        };
        if received == size_of::<T>() {
            Some(unsafe { value.assume_init() })
        } else {
            None
        }
    }

    // =========================================================================
    // Query
    // =========================================================================

    /// Returns the size of the next message in bytes, or 0 if empty.
    ///
    /// Use this to check if your receive buffer is large enough before
    /// calling `receive()`.
    pub fn next_message_size(&self) -> usize {
        unsafe { xStreamBufferNextMessageLengthBytes(self.handle) }
    }

    /// Returns the total bytes used (including message length prefixes).
    pub fn bytes_used(&self) -> usize {
        unsafe { xStreamBufferBytesAvailable(self.handle) }
    }

    /// Returns the number of bytes of free space.
    pub fn spaces(&self) -> usize {
        unsafe { xStreamBufferSpacesAvailable(self.handle) }
    }

    /// Returns `true` if the buffer is empty (no messages).
    pub fn is_empty(&self) -> bool {
        unsafe { xStreamBufferIsEmpty(self.handle) != pdFALSE }
    }

    /// Returns `true` if the buffer is full.
    pub fn is_full(&self) -> bool {
        unsafe { xStreamBufferIsFull(self.handle) != pdFALSE }
    }

    // =========================================================================
    // Control
    // =========================================================================

    /// Resets the buffer to empty state.
    ///
    /// Returns `true` if reset succeeded, `false` if tasks are blocked
    /// on the buffer (cannot reset while tasks are waiting).
    pub fn reset(&self) -> bool {
        unsafe { xStreamBufferReset(self.handle) == pdPASS }
    }

    /// Returns the raw FreeRTOS handle for interop.
    pub unsafe fn raw_handle(&self) -> StreamBufferHandle_t {
        self.handle
    }
}
