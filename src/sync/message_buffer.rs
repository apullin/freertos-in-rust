//! Safe byte-oriented message-buffer wrapper.
//!
//! FreeRTOS message buffers transfer complete, variable-length byte messages
//! and support exactly one producer and one consumer.  The
//! [`MessageBuffer::split`] API represents those roles as distinct, non-clone
//! capabilities.  Neither the owner nor its endpoints is `Sync`.
//!
//! This wrapper deliberately does not reinterpret arbitrary Rust values as
//! bytes.  `T: Copy` does not guarantee that every bit pattern is valid or that
//! a type contains no references or padding.  Serialize typed values into a
//! defined byte representation before sending them.

use core::cell::Cell;
use core::ffi::c_void;
use core::marker::PhantomData;
use core::mem::size_of;

#[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
use crate::kernel::stream_buffer::xStreamBufferGenericCreate;
use crate::kernel::stream_buffer::{
    configMESSAGE_BUFFER_LENGTH_TYPE, sbTYPE_MESSAGE_BUFFER, vStreamBufferDelete,
    xStreamBufferBytesAvailable, xStreamBufferGenericCreateStatic, xStreamBufferIsEmpty,
    xStreamBufferIsFull, xStreamBufferNextMessageLengthBytes, xStreamBufferReceive,
    xStreamBufferReceiveFromISR, xStreamBufferReset, xStreamBufferResetFromISR, xStreamBufferSend,
    xStreamBufferSendFromISR, xStreamBufferSpacesAvailable, StaticStreamBuffer_t,
    StreamBufferHandle_t,
};
use crate::sync::task::TaskContext;
use crate::sync::{assert_can_block, assert_task_context};
use crate::types::*;

const MESSAGE_LENGTH_BYTES: usize = size_of::<configMESSAGE_BUFFER_LENGTH_TYPE>();

/// An owned, single-producer/single-consumer byte-message channel.
///
/// Use the methods on this type when one task owns both sides. To transfer
/// messages between two execution contexts, call [`Self::split`] and move the
/// endpoints to the producer and consumer respectively.
///
/// The wrapper is `Send` but intentionally not `Sync`.
///
/// ```compile_fail
/// use freertos_in_rust::sync::MessageBuffer;
///
/// fn assert_sync<T: Sync>() {}
/// assert_sync::<MessageBuffer>();
/// ```
///
/// ```compile_fail
/// use freertos_in_rust::sync::MessageBuffer;
///
/// fn arbitrary_values_are_not_messages(buffer: &mut MessageBuffer) {
///     buffer.send_val(&true);
/// }
/// ```
pub struct MessageBuffer {
    handle: StreamBufferHandle_t,
    capacity: usize,
    _not_sync: PhantomData<Cell<()>>,
}

// SAFETY: Moving the unique owner to another task does not access the buffer.
// Concurrent access is available only through the partitioned endpoints below.
unsafe impl Send for MessageBuffer {}

/// The sole producer endpoint of a [`MessageBuffer`].
///
/// This endpoint is `Send` but intentionally neither `Clone` nor `Sync`.
pub struct MessageBufferSender<'a> {
    handle: StreamBufferHandle_t,
    capacity: usize,
    _owner: PhantomData<&'a mut MessageBuffer>,
    _not_sync: PhantomData<Cell<()>>,
}

// SAFETY: A sender is a unique capability, and FreeRTOS permits its single
// producer to move between tasks. It cannot be shared by safe Rust code.
unsafe impl Send for MessageBufferSender<'_> {}

/// The sole consumer endpoint of a [`MessageBuffer`].
///
/// This endpoint is `Send` but intentionally neither `Clone` nor `Sync`.
pub struct MessageBufferReceiver<'a> {
    handle: StreamBufferHandle_t,
    capacity: usize,
    _owner: PhantomData<&'a mut MessageBuffer>,
    _not_sync: PhantomData<Cell<()>>,
}

// SAFETY: A receiver is a unique capability, and FreeRTOS permits its single
// consumer to move between tasks. It cannot be shared by safe Rust code.
unsafe impl Send for MessageBufferReceiver<'_> {}

impl MessageBuffer {
    /// Creates a dynamically allocated message buffer.
    ///
    /// `size` is the usable ring capacity, including the length prefix stored
    /// for each message. A message can contain at most
    /// `size - size_of::<configMESSAGE_BUFFER_LENGTH_TYPE>()` bytes. Returns
    /// `None` when the size cannot hold a nonempty message, arithmetic
    /// overflows, or allocation fails.
    ///
    /// This is a task-context operation; do not allocate a buffer in an ISR.
    #[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
    pub fn new(_context: &TaskContext, size: usize) -> Option<Self> {
        assert_task_context("MessageBuffer::new");
        if size <= MESSAGE_LENGTH_BYTES || size.checked_add(1).is_none() {
            return None;
        }

        let handle =
            unsafe { xStreamBufferGenericCreate(size, 1, sbTYPE_MESSAGE_BUFFER, None, None) };
        Self::from_created_handle(handle, size)
    }

    /// Creates a message buffer in caller-provided static storage.
    ///
    /// FreeRTOS reserves one byte of static storage to distinguish full from
    /// empty. The usable capacity is therefore `storage.len() - 1`, and that
    /// capacity must exceed the message-length prefix size so at least one data
    /// byte can be stored.
    pub fn new_static(
        _context: &TaskContext,
        storage: &'static mut [u8],
        message_buffer: &'static mut StaticStreamBuffer_t,
    ) -> Option<Self> {
        assert_task_context("MessageBuffer::new_static");
        let capacity = storage.len().checked_sub(1)?;
        if capacity <= MESSAGE_LENGTH_BYTES {
            return None;
        }

        let handle = unsafe {
            xStreamBufferGenericCreateStatic(
                storage.len(),
                1,
                sbTYPE_MESSAGE_BUFFER,
                storage.as_mut_ptr(),
                message_buffer as *mut StaticStreamBuffer_t,
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

    /// Splits the buffer into its unique producer and consumer capabilities.
    ///
    /// The owner remains borrowed until both endpoints are dropped. This also
    /// prevents resetting or deleting the buffer while a safe endpoint exists.
    pub fn split(&mut self) -> (MessageBufferSender<'_>, MessageBufferReceiver<'_>) {
        (
            MessageBufferSender {
                handle: self.handle,
                capacity: self.capacity,
                _owner: PhantomData,
                _not_sync: PhantomData,
            },
            MessageBufferReceiver {
                handle: self.handle,
                capacity: self.capacity,
                _owner: PhantomData,
                _not_sync: PhantomData,
            },
        )
    }

    /// Returns the usable ring capacity, including message prefixes.
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// Returns the largest message payload that can fit in an empty buffer.
    pub const fn max_message_size(&self) -> usize {
        self.capacity - MESSAGE_LENGTH_BYTES
    }

    /// Sends one complete message from task context, waiting indefinitely.
    ///
    /// Empty messages are rejected. This method must not be called from an ISR.
    pub fn send(&mut self, _context: &TaskContext, data: &[u8]) -> bool {
        send_task(self.handle, self.capacity, data, portMAX_DELAY)
    }

    /// Attempts a nonblocking send from task context.
    ///
    /// This still uses the task-context API. Use [`Self::send_from_isr`] from an
    /// interrupt handler.
    pub fn try_send(&mut self, _context: &TaskContext, data: &[u8]) -> bool {
        send_task(self.handle, self.capacity, data, 0)
    }

    /// Sends one complete message from task context, waiting at most `ticks`.
    ///
    /// Returns `false` for an empty/oversized message or if the timeout elapses.
    pub fn send_timeout(&mut self, _context: &TaskContext, data: &[u8], ticks: TickType_t) -> bool {
        send_task(self.handle, self.capacity, data, ticks)
    }

    /// Performs a nonblocking send from interrupt context.
    ///
    /// Returns `(message_sent, higher_priority_task_woken)`. If the second
    /// value is true, request a context switch using the active port's ISR
    /// yield mechanism before returning from the interrupt.
    ///
    /// # Safety
    ///
    /// The caller must be executing in an ISR at a priority from which
    /// FreeRTOS system calls are permitted. This value must be the interrupt's
    /// sole producer; it must not race a task-context producer.
    pub unsafe fn send_from_isr(&mut self, data: &[u8]) -> (bool, bool) {
        unsafe { send_isr(self.handle, self.capacity, data) }
    }

    /// Receives one complete message in task context, waiting indefinitely.
    ///
    /// Returns zero and leaves the message queued when `output` is too small.
    /// This method must not be called from an ISR.
    pub fn receive(&mut self, _context: &TaskContext, output: &mut [u8]) -> usize {
        receive_task(self.handle, output, portMAX_DELAY)
    }

    /// Attempts a nonblocking receive from task context.
    ///
    /// This still uses the task-context API. Use [`Self::receive_from_isr`]
    /// from an interrupt handler.
    pub fn try_receive(&mut self, _context: &TaskContext, output: &mut [u8]) -> usize {
        receive_task(self.handle, output, 0)
    }

    /// Receives one complete message in task context, waiting at most `ticks`.
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

    /// Returns the next message's payload size, or zero when empty.
    pub fn next_message_size(&self, _context: &TaskContext) -> usize {
        assert_task_context("MessageBuffer::next_message_size");
        next_message_size(self.handle)
    }

    /// Returns bytes currently used, including message-length prefixes.
    pub fn bytes_used(&self, _context: &TaskContext) -> usize {
        assert_task_context("MessageBuffer::bytes_used");
        bytes_used(self.handle)
    }

    /// Returns the number of free bytes in the ring.
    pub fn spaces(&self, _context: &TaskContext) -> usize {
        assert_task_context("MessageBuffer::spaces");
        spaces(self.handle)
    }

    /// Returns `true` if no message is queued.
    pub fn is_empty(&self, _context: &TaskContext) -> bool {
        assert_task_context("MessageBuffer::is_empty");
        is_empty(self.handle)
    }

    /// Returns `true` when no nonempty message can be added.
    pub fn is_full(&self, _context: &TaskContext) -> bool {
        assert_task_context("MessageBuffer::is_full");
        is_full(self.handle)
    }

    /// Resets the buffer from task context.
    ///
    /// Returns `false` if a task is blocked on the buffer.
    pub fn reset(&mut self, _context: &TaskContext) -> bool {
        assert_task_context("MessageBuffer::reset");
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

impl MessageBufferSender<'_> {
    /// Returns the usable ring capacity, including message prefixes.
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// Returns the largest message payload that can fit in an empty buffer.
    pub const fn max_message_size(&self) -> usize {
        self.capacity - MESSAGE_LENGTH_BYTES
    }

    /// Sends one complete message from task context, waiting indefinitely.
    pub fn send(&mut self, _context: &TaskContext, data: &[u8]) -> bool {
        send_task(self.handle, self.capacity, data, portMAX_DELAY)
    }

    /// Attempts a nonblocking send from task context.
    pub fn try_send(&mut self, _context: &TaskContext, data: &[u8]) -> bool {
        send_task(self.handle, self.capacity, data, 0)
    }

    /// Sends one complete message from task context, waiting at most `ticks`.
    pub fn send_timeout(&mut self, _context: &TaskContext, data: &[u8], ticks: TickType_t) -> bool {
        send_task(self.handle, self.capacity, data, ticks)
    }

    /// Performs a nonblocking send from interrupt context.
    ///
    /// Returns `(message_sent, higher_priority_task_woken)`.
    ///
    /// # Safety
    ///
    /// The caller must be in an ISR at a FreeRTOS-compatible priority. This
    /// endpoint must be the ISR's sole producer and must not also be used by a
    /// task-context producer.
    pub unsafe fn send_from_isr(&mut self, data: &[u8]) -> (bool, bool) {
        unsafe { send_isr(self.handle, self.capacity, data) }
    }

    /// Returns the number of free bytes in the ring.
    pub fn spaces(&self, _context: &TaskContext) -> usize {
        assert_task_context("MessageBufferSender::spaces");
        spaces(self.handle)
    }

    /// Returns `true` when no nonempty message can be added.
    pub fn is_full(&self, _context: &TaskContext) -> bool {
        assert_task_context("MessageBufferSender::is_full");
        is_full(self.handle)
    }
}

impl MessageBufferReceiver<'_> {
    /// Returns the usable ring capacity, including message prefixes.
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// Returns the largest message payload that can fit in an empty buffer.
    pub const fn max_message_size(&self) -> usize {
        self.capacity - MESSAGE_LENGTH_BYTES
    }

    /// Receives one complete message in task context, waiting indefinitely.
    pub fn receive(&mut self, _context: &TaskContext, output: &mut [u8]) -> usize {
        receive_task(self.handle, output, portMAX_DELAY)
    }

    /// Attempts a nonblocking receive from task context.
    pub fn try_receive(&mut self, _context: &TaskContext, output: &mut [u8]) -> usize {
        receive_task(self.handle, output, 0)
    }

    /// Receives one complete message in task context, waiting at most `ticks`.
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

    /// Returns the next message's payload size, or zero when empty.
    pub fn next_message_size(&self, _context: &TaskContext) -> usize {
        assert_task_context("MessageBufferReceiver::next_message_size");
        next_message_size(self.handle)
    }

    /// Returns bytes currently used, including message-length prefixes.
    pub fn bytes_used(&self, _context: &TaskContext) -> usize {
        assert_task_context("MessageBufferReceiver::bytes_used");
        bytes_used(self.handle)
    }

    /// Returns `true` if no message is queued.
    pub fn is_empty(&self, _context: &TaskContext) -> bool {
        assert_task_context("MessageBufferReceiver::is_empty");
        is_empty(self.handle)
    }
}

fn message_fits(capacity: usize, data_len: usize) -> bool {
    data_len != 0
        && data_len
            .checked_add(MESSAGE_LENGTH_BYTES)
            .is_some_and(|required| required <= capacity)
}

fn send_task(
    handle: StreamBufferHandle_t,
    capacity: usize,
    data: &[u8],
    ticks: TickType_t,
) -> bool {
    assert_can_block("MessageBuffer::send", ticks);
    if !message_fits(capacity, data.len()) {
        return false;
    }
    unsafe {
        xStreamBufferSend(handle, data.as_ptr().cast::<c_void>(), data.len(), ticks) == data.len()
    }
}

fn receive_task(handle: StreamBufferHandle_t, output: &mut [u8], ticks: TickType_t) -> usize {
    assert_can_block("MessageBuffer::receive", ticks);
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

unsafe fn send_isr(handle: StreamBufferHandle_t, capacity: usize, data: &[u8]) -> (bool, bool) {
    if !message_fits(capacity, data.len()) {
        return (false, false);
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
    (sent == data.len(), higher_priority_task_woken != pdFALSE)
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

fn next_message_size(handle: StreamBufferHandle_t) -> usize {
    unsafe { xStreamBufferNextMessageLengthBytes(handle) }
}

fn bytes_used(handle: StreamBufferHandle_t) -> usize {
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

    fn static_messages(context: &TaskContext, storage_len: usize) -> Option<MessageBuffer> {
        let storage = Box::leak(vec![0_u8; storage_len].into_boxed_slice());
        let control = Box::leak(Box::new(StaticStreamBuffer_t::new()));
        MessageBuffer::new_static(context, storage, control)
    }

    #[test]
    fn rejects_storage_that_cannot_hold_a_nonempty_message() {
        crate::port::test_port_reset();
        // SAFETY: this single-threaded test models pre-scheduler setup.
        let context = unsafe { TaskContext::assume() };
        assert!(static_messages(&context, 0).is_none());
        assert!(static_messages(&context, MESSAGE_LENGTH_BYTES + 1).is_none());
        assert!(static_messages(&context, MESSAGE_LENGTH_BYTES + 2).is_some());
    }

    #[test]
    fn validates_message_size_before_calling_the_kernel() {
        crate::port::test_port_reset();
        // SAFETY: this single-threaded test models pre-scheduler setup.
        let context = unsafe { TaskContext::assume() };
        let mut messages = static_messages(&context, 16).expect("valid message buffer");
        assert_eq!(messages.capacity(), 15);
        assert_eq!(messages.max_message_size(), 15 - MESSAGE_LENGTH_BYTES);
        assert!(!messages.try_send(&context, &[]));
        assert!(!messages.try_send(&context, &[0_u8; 16]));

        assert!(messages.try_send(&context, b"hello"));
        assert_eq!(messages.next_message_size(&context), 5);
        let mut too_small = [0_u8; 4];
        assert_eq!(messages.try_receive(&context, &mut too_small), 0);
        assert_eq!(messages.next_message_size(&context), 5);

        let mut output = [0_u8; 5];
        assert_eq!(messages.try_receive(&context, &mut output), 5);
        assert_eq!(&output, b"hello");
    }

    #[test]
    fn split_endpoints_preserve_message_boundaries() {
        crate::port::test_port_reset();
        // SAFETY: this single-threaded test models pre-scheduler task context.
        let context = unsafe { TaskContext::assume() };
        let mut messages = static_messages(&context, 32).expect("valid message buffer");
        let (mut sender, mut receiver) = messages.split();
        assert!(sender.try_send(&context, b"first"));
        assert!(sender.try_send(&context, b"second"));

        let mut output = [0_u8; 8];
        assert_eq!(receiver.try_receive(&context, &mut output), 5);
        assert_eq!(&output[..5], b"first");
        assert_eq!(receiver.try_receive(&context, &mut output), 6);
        assert_eq!(&output[..6], b"second");
    }

    #[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
    #[test]
    fn rejects_dynamic_size_overflow() {
        crate::port::test_port_reset();
        // SAFETY: this single-threaded test models pre-scheduler setup.
        let context = unsafe { TaskContext::assume() };
        assert!(MessageBuffer::new(&context, usize::MAX).is_none());
    }
}
