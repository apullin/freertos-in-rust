//! Safe StreamBuffer wrapper
//!
//! Provides a safe wrapper around FreeRTOS stream buffers.
//! Stream buffers are optimized for transferring a continuous stream of bytes
//! from one task (or ISR) to another.
//!
//! **Important**: Stream buffers assume a single writer and single reader.
//! Multiple writers or readers require external synchronization.

use core::ffi::c_void;
use core::mem::{size_of, MaybeUninit};

use crate::kernel::stream_buffer::{
    sbTYPE_STREAM_BUFFER, xStreamBufferBytesAvailable, xStreamBufferGenericCreateStatic,
    xStreamBufferIsEmpty, xStreamBufferIsFull, xStreamBufferReceive, xStreamBufferReset,
    xStreamBufferSend, xStreamBufferSpacesAvailable, StaticStreamBuffer_t, StreamBufferHandle_t,
};
#[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
use crate::kernel::stream_buffer::xStreamBufferCreate;
use crate::types::*;

/// A byte stream buffer for efficient task-to-task data transfer.
///
/// Stream buffers provide a lightweight mechanism for transferring
/// arbitrary byte sequences between tasks. Unlike queues which transfer
/// discrete items, stream buffers handle continuous byte streams.
///
/// # Single Producer / Single Consumer
///
/// Stream buffers are designed for one writer and one reader. If you need
/// multiple writers or readers, protect access with a mutex.
///
/// # Trigger Level
///
/// The trigger level determines how many bytes must be in the buffer
/// before a blocked reader is unblocked. A trigger level of 1 means
/// the reader wakes as soon as any data is available.
pub struct StreamBuffer {
    handle: StreamBufferHandle_t,
}

// Safety: StreamBuffer handles can be shared between tasks
// (though only one should write and one should read)
unsafe impl Sync for StreamBuffer {}
unsafe impl Send for StreamBuffer {}

impl StreamBuffer {
    /// Creates a new stream buffer.
    ///
    /// # Arguments
    ///
    /// * `size` - Buffer capacity in bytes
    /// * `trigger_level` - Minimum bytes before reader unblocks (1 = immediate)
    ///
    /// Returns `None` if creation failed (e.g., out of memory).
    #[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
    pub fn new(size: usize, trigger_level: usize) -> Option<Self> {
        let handle = unsafe { xStreamBufferCreate(size, trigger_level) };
        if handle.is_null() {
            None
        } else {
            Some(Self { handle })
        }
    }

    /// Creates a stream buffer using static storage.
    ///
    /// # Arguments
    ///
    /// * `storage` - Static byte buffer for data
    /// * `trigger_level` - Minimum bytes before reader unblocks (1 = immediate)
    /// * `stream_buffer` - Static control structure
    ///
    /// # Example
    ///
    /// ```ignore
    /// use freertos_in_rust::sync::StreamBuffer;
    /// use freertos_in_rust::kernel::stream_buffer::StaticStreamBuffer_t;
    ///
    /// static mut STORAGE: [u8; 128] = [0; 128];
    /// static mut SB_BUF: StaticStreamBuffer_t = StaticStreamBuffer_t::new();
    ///
    /// let stream = StreamBuffer::new_static(
    ///     unsafe { &mut STORAGE },
    ///     1,  // trigger level
    ///     unsafe { &mut SB_BUF },
    /// ).expect("Failed to create stream buffer");
    /// ```
    pub fn new_static(
        storage: &'static mut [u8],
        trigger_level: usize,
        stream_buffer: &'static mut StaticStreamBuffer_t,
    ) -> Option<Self> {
        let handle = unsafe {
            xStreamBufferGenericCreateStatic(
                storage.len(),
                trigger_level,
                sbTYPE_STREAM_BUFFER,
                storage.as_mut_ptr(),
                stream_buffer as *mut StaticStreamBuffer_t,
                None,
                None,
            )
        };
        if handle.is_null() {
            None
        } else {
            Some(Self { handle })
        }
    }

    // =========================================================================
    // Send
    // =========================================================================

    /// Sends bytes to the stream buffer, blocking until space is available.
    ///
    /// Returns the number of bytes actually sent. With infinite timeout,
    /// this will send all bytes unless the buffer is smaller than the data.
    pub fn send(&self, data: &[u8]) -> usize {
        self.send_timeout(data, portMAX_DELAY)
    }

    /// Attempts to send bytes without blocking.
    ///
    /// Returns the number of bytes sent (may be less than `data.len()`
    /// if the buffer doesn't have enough space).
    pub fn try_send(&self, data: &[u8]) -> usize {
        self.send_timeout(data, 0)
    }

    /// Sends bytes with a timeout.
    ///
    /// Returns the number of bytes sent within the timeout period.
    pub fn send_timeout(&self, data: &[u8], ticks: TickType_t) -> usize {
        if data.is_empty() {
            return 0;
        }
        unsafe {
            xStreamBufferSend(
                self.handle,
                data.as_ptr() as *const c_void,
                data.len(),
                ticks,
            )
        }
    }

    // =========================================================================
    // Receive
    // =========================================================================

    /// Receives bytes from the stream buffer, blocking until data is available.
    ///
    /// Returns the number of bytes received. Will block until at least
    /// `trigger_level` bytes are available (as set during creation).
    pub fn receive(&self, buf: &mut [u8]) -> usize {
        self.receive_timeout(buf, portMAX_DELAY)
    }

    /// Attempts to receive bytes without blocking.
    ///
    /// Returns the number of bytes received (0 if buffer is empty).
    pub fn try_receive(&self, buf: &mut [u8]) -> usize {
        self.receive_timeout(buf, 0)
    }

    /// Receives bytes with a timeout.
    ///
    /// Returns the number of bytes received within the timeout period.
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
    // Typed Send
    // =========================================================================

    /// Sends a typed value to the stream, blocking until space is available.
    ///
    /// Returns `true` if all bytes were sent, `false` otherwise.
    ///
    /// # Note
    ///
    /// Stream buffers may send partial data if the buffer is too small.
    /// This method only returns `true` if ALL bytes of the value were sent.
    pub fn send_val<T: Copy>(&self, value: &T) -> bool {
        self.send_val_timeout(value, portMAX_DELAY)
    }

    /// Attempts to send a typed value without blocking.
    pub fn try_send_val<T: Copy>(&self, value: &T) -> bool {
        self.send_val_timeout(value, 0)
    }

    /// Sends a typed value with a timeout.
    ///
    /// Returns `true` if all bytes were sent within the timeout.
    pub fn send_val_timeout<T: Copy>(&self, value: &T, ticks: TickType_t) -> bool {
        let data =
            unsafe { core::slice::from_raw_parts(value as *const T as *const u8, size_of::<T>()) };
        self.send_timeout(data, ticks) == size_of::<T>()
    }

    // =========================================================================
    // Typed Receive
    // =========================================================================

    /// Receives a typed value from the stream, blocking until data is available.
    ///
    /// Returns `Some(value)` if exactly `size_of::<T>()` bytes were received,
    /// `None` otherwise.
    ///
    /// # Note
    ///
    /// Stream buffers may receive partial data. This method only returns
    /// `Some` if ALL bytes of the type were received.
    pub fn receive_val<T: Copy>(&self) -> Option<T> {
        self.receive_val_timeout(portMAX_DELAY)
    }

    /// Attempts to receive a typed value without blocking.
    pub fn try_receive_val<T: Copy>(&self) -> Option<T> {
        self.receive_val_timeout(0)
    }

    /// Receives a typed value with a timeout.
    ///
    /// Returns `Some(value)` if exactly `size_of::<T>()` bytes were received
    /// within the timeout, `None` otherwise.
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

    /// Returns the number of bytes available to read.
    pub fn available(&self) -> usize {
        unsafe { xStreamBufferBytesAvailable(self.handle) }
    }

    /// Returns the number of bytes of free space.
    pub fn spaces(&self) -> usize {
        unsafe { xStreamBufferSpacesAvailable(self.handle) }
    }

    /// Returns `true` if the buffer is empty.
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
