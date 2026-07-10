//! Safe byte-oriented stream-buffer wrapper.
//!
//! A FreeRTOS stream buffer has exactly one producer and one consumer.  The
//! [`StreamBuffer::split`] API turns that runtime rule into two distinct Rust
//! capabilities.  Neither the owner nor either endpoint is `Sync`, so safe
//! code cannot accidentally create multiple concurrent producers or
//! consumers.
//!
//! This wrapper deliberately transfers bytes only.  Reinterpreting an
//! arbitrary `T: Copy` as bytes is not sound: `Copy` types can contain padding,
//! references, or invalid bit patterns, and a stream buffer can perform a
//! partial transfer.  Serialize typed values into a byte representation before
//! sending them.

use core::cell::Cell;
use core::ffi::c_void;
use core::marker::PhantomData;

#[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
use crate::kernel::stream_buffer::xStreamBufferCreate;
use crate::kernel::stream_buffer::{
    sbTYPE_STREAM_BUFFER, vStreamBufferDelete, xStreamBufferBytesAvailable,
    xStreamBufferGenericCreateStatic, xStreamBufferIsEmpty, xStreamBufferIsFull,
    xStreamBufferReceive, xStreamBufferReceiveFromISR, xStreamBufferReset,
    xStreamBufferResetFromISR, xStreamBufferSend, xStreamBufferSendFromISR,
    xStreamBufferSpacesAvailable, StaticStreamBuffer_t, StreamBufferHandle_t,
};
use crate::sync::task::TaskContext;
use crate::sync::{assert_can_block, assert_task_context};
use crate::types::*;

/// An owned, single-producer/single-consumer byte stream.
///
/// Use the methods on this type when one task owns both sides.  To transfer
/// bytes between two execution contexts, call [`Self::split`] and move the
/// resulting endpoints to the producer and consumer respectively.
///
/// The wrapper is `Send` but intentionally not `Sync`.
///
/// ```compile_fail
/// use freertos_in_rust::sync::StreamBuffer;
///
/// fn assert_sync<T: Sync>() {}
/// assert_sync::<StreamBuffer>();
/// ```
///
/// ```compile_fail
/// use freertos_in_rust::sync::StreamBuffer;
///
/// fn cannot_split_twice(buffer: &mut StreamBuffer) {
///     let first = buffer.split();
///     let second = buffer.split();
///     drop((first, second));
/// }
/// ```
///
/// ```compile_fail
/// use freertos_in_rust::sync::StreamBuffer;
///
/// fn arbitrary_values_are_not_bytes(buffer: &mut StreamBuffer) {
///     buffer.send_val(&true);
/// }
/// ```
pub struct StreamBuffer {
    handle: StreamBufferHandle_t,
    capacity: usize,
    _not_sync: PhantomData<Cell<()>>,
}

// SAFETY: Moving the unique owner to another task does not access the buffer.
// Concurrent access is available only through the partitioned endpoints below.
unsafe impl Send for StreamBuffer {}

/// The sole producer endpoint of a [`StreamBuffer`].
///
/// This endpoint is `Send` but intentionally neither `Clone` nor `Sync`.
pub struct StreamBufferSender<'a> {
    handle: StreamBufferHandle_t,
    capacity: usize,
    _owner: PhantomData<&'a mut StreamBuffer>,
    _not_sync: PhantomData<Cell<()>>,
}

// SAFETY: A sender is a unique capability, and FreeRTOS permits its single
// producer to move between tasks. It cannot be shared by safe Rust code.
unsafe impl Send for StreamBufferSender<'_> {}

/// The sole consumer endpoint of a [`StreamBuffer`].
///
/// This endpoint is `Send` but intentionally neither `Clone` nor `Sync`.
pub struct StreamBufferReceiver<'a> {
    handle: StreamBufferHandle_t,
    capacity: usize,
    _owner: PhantomData<&'a mut StreamBuffer>,
    _not_sync: PhantomData<Cell<()>>,
}

// SAFETY: A receiver is a unique capability, and FreeRTOS permits its single
// consumer to move between tasks. It cannot be shared by safe Rust code.
unsafe impl Send for StreamBufferReceiver<'_> {}

impl StreamBuffer {
    /// Creates a dynamically allocated stream buffer.
    ///
    /// `size` is the usable byte capacity. `trigger_level` must be in
    /// `1..=size`. Returns `None` for an invalid size, arithmetic overflow, or
    /// allocation failure.
    ///
    /// This is a task-context operation; do not allocate a buffer in an ISR.
    #[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
    pub fn new(_context: &TaskContext, size: usize, trigger_level: usize) -> Option<Self> {
        assert_task_context("StreamBuffer::new");
        if size == 0 || trigger_level == 0 || trigger_level > size || size.checked_add(1).is_none()
        {
            return None;
        }

        let handle = unsafe { xStreamBufferCreate(size, trigger_level) };
        Self::from_created_handle(handle, size)
    }

    /// Creates a stream buffer in caller-provided static storage.
    ///
    /// FreeRTOS reserves one byte of static storage to distinguish full from
    /// empty, so the usable capacity is `storage.len() - 1`. The storage must
    /// therefore contain at least two bytes, and `trigger_level` must be in
    /// `1..storage.len()`.
    pub fn new_static(
        _context: &TaskContext,
        storage: &'static mut [u8],
        trigger_level: usize,
        stream_buffer: &'static mut StaticStreamBuffer_t,
    ) -> Option<Self> {
        assert_task_context("StreamBuffer::new_static");
        let capacity = storage.len().checked_sub(1)?;
        if capacity == 0 || trigger_level == 0 || trigger_level > capacity {
            return None;
        }

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
        Self::from_created_handle(handle, capacity)
    }

    fn from_created_handle(handle: StreamBufferHandle_t, capacity: usize) -> Option<Self> {
        if handle.is_null() {
            None
        } else {
            Some(Self {
                handle,
                capacity,
                _not_sync: PhantomData,
            })
        }
    }

    /// Splits the stream into its unique producer and consumer capabilities.
    ///
    /// The owner remains borrowed until both endpoints are dropped. This also
    /// prevents resetting or deleting a buffer while a safe endpoint exists.
    pub fn split(&mut self) -> (StreamBufferSender<'_>, StreamBufferReceiver<'_>) {
        (
            StreamBufferSender {
                handle: self.handle,
                capacity: self.capacity,
                _owner: PhantomData,
                _not_sync: PhantomData,
            },
            StreamBufferReceiver {
                handle: self.handle,
                capacity: self.capacity,
                _owner: PhantomData,
                _not_sync: PhantomData,
            },
        )
    }

    /// Returns the usable byte capacity.
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// Sends bytes from task context, waiting indefinitely for space.
    ///
    /// A stream transfer can be partial when `data` is larger than the buffer.
    /// This method must not be called from interrupt context.
    pub fn send(&mut self, _context: &TaskContext, data: &[u8]) -> usize {
        send_task(self.handle, data, portMAX_DELAY)
    }

    /// Attempts a nonblocking send from task context.
    ///
    /// This method still uses the task-context FreeRTOS API. Use
    /// [`Self::send_from_isr`] in an interrupt handler.
    pub fn try_send(&mut self, _context: &TaskContext, data: &[u8]) -> usize {
        send_task(self.handle, data, 0)
    }

    /// Sends bytes from task context, waiting for at most `ticks` ticks.
    ///
    /// Returns the number of bytes copied. This method must not be called from
    /// interrupt context.
    pub fn send_timeout(
        &mut self,
        _context: &TaskContext,
        data: &[u8],
        ticks: TickType_t,
    ) -> usize {
        send_task(self.handle, data, ticks)
    }

    /// Performs a nonblocking send from interrupt context.
    ///
    /// Returns `(bytes_sent, higher_priority_task_woken)`. If the second value
    /// is true, request a context switch using the active port's ISR-yield
    /// mechanism before returning from the interrupt.
    ///
    /// # Safety
    ///
    /// The caller must be executing in an ISR at a priority from which
    /// FreeRTOS system calls are permitted. This value must be the interrupt's
    /// sole producer; it must not race a task-context producer.
    pub unsafe fn send_from_isr(&mut self, data: &[u8]) -> (usize, bool) {
        unsafe { send_isr(self.handle, data) }
    }

    /// Receives bytes in task context, waiting indefinitely for data.
    ///
    /// This method must not be called from interrupt context.
    pub fn receive(&mut self, _context: &TaskContext, output: &mut [u8]) -> usize {
        receive_task(self.handle, output, portMAX_DELAY)
    }

    /// Attempts a nonblocking receive from task context.
    ///
    /// This method still uses the task-context FreeRTOS API. Use
    /// [`Self::receive_from_isr`] in an interrupt handler.
    pub fn try_receive(&mut self, _context: &TaskContext, output: &mut [u8]) -> usize {
        receive_task(self.handle, output, 0)
    }

    /// Receives bytes in task context, waiting for at most `ticks` ticks.
    ///
    /// This method must not be called from interrupt context.
    pub fn receive_timeout(
        &mut self,
        _context: &TaskContext,
        output: &mut [u8],
        ticks: TickType_t,
    ) -> usize {
        receive_task(self.handle, output, ticks)
    }

    /// Performs a nonblocking receive from interrupt context.
    ///
    /// Returns `(bytes_received, higher_priority_task_woken)`.
    ///
    /// # Safety
    ///
    /// The caller must be executing in an ISR at a priority from which
    /// FreeRTOS system calls are permitted. This value must be the interrupt's
    /// sole consumer; it must not race a task-context consumer.
    pub unsafe fn receive_from_isr(&mut self, output: &mut [u8]) -> (usize, bool) {
        unsafe { receive_isr(self.handle, output) }
    }

    /// Returns the number of bytes available to read.
    pub fn available(&self, _context: &TaskContext) -> usize {
        assert_task_context("StreamBuffer::available");
        available(self.handle)
    }

    /// Returns the number of bytes of free space.
    pub fn spaces(&self, _context: &TaskContext) -> usize {
        assert_task_context("StreamBuffer::spaces");
        spaces(self.handle)
    }

    /// Returns `true` if the buffer is empty.
    pub fn is_empty(&self, _context: &TaskContext) -> bool {
        assert_task_context("StreamBuffer::is_empty");
        is_empty(self.handle)
    }

    /// Returns `true` if the buffer is full.
    pub fn is_full(&self, _context: &TaskContext) -> bool {
        assert_task_context("StreamBuffer::is_full");
        is_full(self.handle)
    }

    /// Resets the buffer from task context.
    ///
    /// Returns `false` if a task is blocked on the buffer. Safe endpoints
    /// cannot exist while this exclusive borrow is active.
    pub fn reset(&mut self, _context: &TaskContext) -> bool {
        assert_task_context("StreamBuffer::reset");
        unsafe { xStreamBufferReset(self.handle) == pdPASS }
    }

    /// Resets the buffer from interrupt context.
    ///
    /// # Safety
    ///
    /// The caller must be in an ISR at a FreeRTOS-compatible priority, and no
    /// operation may be in progress through an unsafe copy of the raw handle.
    pub unsafe fn reset_from_isr(&mut self) -> bool {
        unsafe { xStreamBufferResetFromISR(self.handle) == pdPASS }
    }

    /// Deletes the underlying FreeRTOS object and consumes the owner.
    ///
    /// Dynamic buffers are freed; static control storage is cleared. Buffers
    /// are not implicitly deleted on `Drop`, because an arbitrary Rust drop
    /// site cannot prove it is a valid FreeRTOS task context.
    ///
    /// # Safety
    ///
    /// Call this only from task context when no task or ISR is using or blocked
    /// on the buffer, including through copies obtained from [`Self::raw_handle`].
    pub unsafe fn delete(self, _context: &TaskContext) {
        unsafe { vStreamBufferDelete(self.handle) };
    }

    /// Returns the raw FreeRTOS handle for low-level interoperation.
    ///
    /// # Safety
    ///
    /// The caller must preserve this wrapper's unique-producer,
    /// unique-consumer, lifetime, and execution-context invariants.
    pub unsafe fn raw_handle(&self) -> StreamBufferHandle_t {
        self.handle
    }
}

impl StreamBufferSender<'_> {
    /// Returns the usable byte capacity.
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// Sends bytes from task context, waiting indefinitely for space.
    pub fn send(&mut self, _context: &TaskContext, data: &[u8]) -> usize {
        send_task(self.handle, data, portMAX_DELAY)
    }

    /// Attempts a nonblocking send from task context.
    pub fn try_send(&mut self, _context: &TaskContext, data: &[u8]) -> usize {
        send_task(self.handle, data, 0)
    }

    /// Sends bytes from task context, waiting for at most `ticks` ticks.
    pub fn send_timeout(
        &mut self,
        _context: &TaskContext,
        data: &[u8],
        ticks: TickType_t,
    ) -> usize {
        send_task(self.handle, data, ticks)
    }

    /// Performs a nonblocking send from interrupt context.
    ///
    /// Returns `(bytes_sent, higher_priority_task_woken)`.
    ///
    /// # Safety
    ///
    /// The caller must be in an ISR at a FreeRTOS-compatible priority. This
    /// endpoint must be the ISR's sole producer and must not also be used by a
    /// task-context producer.
    pub unsafe fn send_from_isr(&mut self, data: &[u8]) -> (usize, bool) {
        unsafe { send_isr(self.handle, data) }
    }

    /// Returns the number of bytes of free space.
    pub fn spaces(&self, _context: &TaskContext) -> usize {
        assert_task_context("StreamBufferSender::spaces");
        spaces(self.handle)
    }

    /// Returns `true` if the buffer is full.
    pub fn is_full(&self, _context: &TaskContext) -> bool {
        assert_task_context("StreamBufferSender::is_full");
        is_full(self.handle)
    }
}

impl StreamBufferReceiver<'_> {
    /// Returns the usable byte capacity.
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// Receives bytes in task context, waiting indefinitely for data.
    pub fn receive(&mut self, _context: &TaskContext, output: &mut [u8]) -> usize {
        receive_task(self.handle, output, portMAX_DELAY)
    }

    /// Attempts a nonblocking receive from task context.
    pub fn try_receive(&mut self, _context: &TaskContext, output: &mut [u8]) -> usize {
        receive_task(self.handle, output, 0)
    }

    /// Receives bytes in task context, waiting for at most `ticks` ticks.
    pub fn receive_timeout(
        &mut self,
        _context: &TaskContext,
        output: &mut [u8],
        ticks: TickType_t,
    ) -> usize {
        receive_task(self.handle, output, ticks)
    }

    /// Performs a nonblocking receive from interrupt context.
    ///
    /// Returns `(bytes_received, higher_priority_task_woken)`.
    ///
    /// # Safety
    ///
    /// The caller must be in an ISR at a FreeRTOS-compatible priority. This
    /// endpoint must be the ISR's sole consumer and must not also be used by a
    /// task-context consumer.
    pub unsafe fn receive_from_isr(&mut self, output: &mut [u8]) -> (usize, bool) {
        unsafe { receive_isr(self.handle, output) }
    }

    /// Returns the number of bytes available to read.
    pub fn available(&self, _context: &TaskContext) -> usize {
        assert_task_context("StreamBufferReceiver::available");
        available(self.handle)
    }

    /// Returns `true` if the buffer is empty.
    pub fn is_empty(&self, _context: &TaskContext) -> bool {
        assert_task_context("StreamBufferReceiver::is_empty");
        is_empty(self.handle)
    }
}

fn send_task(handle: StreamBufferHandle_t, data: &[u8], ticks: TickType_t) -> usize {
    assert_can_block("StreamBuffer::send", ticks);
    if data.is_empty() {
        return 0;
    }
    unsafe { xStreamBufferSend(handle, data.as_ptr().cast::<c_void>(), data.len(), ticks) }
}

fn receive_task(handle: StreamBufferHandle_t, output: &mut [u8], ticks: TickType_t) -> usize {
    assert_can_block("StreamBuffer::receive", ticks);
    if output.is_empty() {
        return 0;
    }
    unsafe {
        xStreamBufferReceive(
            handle,
            output.as_mut_ptr().cast::<c_void>(),
            output.len(),
            ticks,
        )
    }
}

unsafe fn send_isr(handle: StreamBufferHandle_t, data: &[u8]) -> (usize, bool) {
    if data.is_empty() {
        return (0, false);
    }
    let mut higher_priority_task_woken = pdFALSE;
    let sent = unsafe {
        xStreamBufferSendFromISR(
            handle,
            data.as_ptr().cast::<c_void>(),
            data.len(),
            &mut higher_priority_task_woken,
        )
    };
    (sent, higher_priority_task_woken != pdFALSE)
}

unsafe fn receive_isr(handle: StreamBufferHandle_t, output: &mut [u8]) -> (usize, bool) {
    if output.is_empty() {
        return (0, false);
    }
    let mut higher_priority_task_woken = pdFALSE;
    let received = unsafe {
        xStreamBufferReceiveFromISR(
            handle,
            output.as_mut_ptr().cast::<c_void>(),
            output.len(),
            &mut higher_priority_task_woken,
        )
    };
    (received, higher_priority_task_woken != pdFALSE)
}

fn available(handle: StreamBufferHandle_t) -> usize {
    unsafe { xStreamBufferBytesAvailable(handle) }
}

fn spaces(handle: StreamBufferHandle_t) -> usize {
    unsafe { xStreamBufferSpacesAvailable(handle) }
}

fn is_empty(handle: StreamBufferHandle_t) -> bool {
    unsafe { xStreamBufferIsEmpty(handle) != pdFALSE }
}

fn is_full(handle: StreamBufferHandle_t) -> bool {
    unsafe { xStreamBufferIsFull(handle) != pdFALSE }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::boxed::Box;
    use std::vec;

    fn static_stream(
        context: &TaskContext,
        storage_len: usize,
        trigger: usize,
    ) -> Option<StreamBuffer> {
        let storage = Box::leak(vec![0_u8; storage_len].into_boxed_slice());
        let control = Box::leak(Box::new(StaticStreamBuffer_t::new()));
        StreamBuffer::new_static(context, storage, trigger, control)
    }

    #[test]
    fn rejects_unusable_static_sizes_and_trigger_levels() {
        crate::port::test_port_reset();
        // SAFETY: this single-threaded test models pre-scheduler setup.
        let context = unsafe { TaskContext::assume() };
        assert!(static_stream(&context, 0, 1).is_none());
        assert!(static_stream(&context, 1, 1).is_none());
        assert!(static_stream(&context, 8, 0).is_none());
        assert!(static_stream(&context, 8, 8).is_none());
    }

    #[test]
    fn static_capacity_accounts_for_freertos_sentinel() {
        crate::port::test_port_reset();
        // SAFETY: this single-threaded test models pre-scheduler setup.
        let context = unsafe { TaskContext::assume() };
        let mut stream = static_stream(&context, 8, 7).expect("valid stream");
        assert_eq!(stream.capacity(), 7);
        assert_eq!(stream.try_send(&context, &[1, 2, 3, 4, 5, 6, 7, 8]), 7);
        assert!(stream.is_full(&context));

        let mut output = [0_u8; 8];
        assert_eq!(stream.try_receive(&context, &mut output), 7);
        assert_eq!(&output[..7], &[1, 2, 3, 4, 5, 6, 7]);
    }

    #[test]
    fn split_endpoints_transfer_bytes() {
        crate::port::test_port_reset();
        // SAFETY: this single-threaded test models two scheduler-serialized
        // FreeRTOS tasks. The port-test scheduler globals deliberately model a
        // single core and therefore must not be driven by concurrent host OS
        // threads.
        let context = unsafe { TaskContext::assume() };
        let mut stream = static_stream(&context, 32, 1).expect("valid stream");
        let (mut sender, mut receiver) = stream.split();
        assert_eq!(sender.try_send(&context, b"byte stream"), 11);
        let mut output = [0_u8; 11];
        assert_eq!(receiver.try_receive(&context, &mut output), 11);
        assert_eq!(output, *b"byte stream");
    }
}
