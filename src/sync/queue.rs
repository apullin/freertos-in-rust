//! Safe Queue wrapper
//!
//! Provides a type-safe queue for inter-task communication.
//! Unlike the raw FreeRTOS API which uses void pointers,
//! this wrapper ensures type safety at compile time.

use core::ffi::c_void;
use core::marker::PhantomData;
use core::mem::{size_of, MaybeUninit};

use crate::kernel::queue::{
    queueQUEUE_TYPE_BASE, queueSEND_TO_BACK, queueSEND_TO_FRONT, uxQueueMessagesWaitingFromISR,
    xQueueGenericCreateStatic, xQueueGenericSend, xQueueIsQueueEmptyFromISR,
    xQueueIsQueueFullFromISR, xQueuePeek, xQueuePeekFromISR, xQueueReceive, xQueueReceiveFromISR,
    xQueueSendToBackFromISR, xQueueSendToFrontFromISR, QueueHandle_t, StaticQueue_t,
};
#[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
use crate::kernel::queue::{vQueueDelete, xQueueGenericCreate};
#[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
use crate::sync::is_in_isr;
use crate::sync::task::TaskContext;
use crate::sync::{assert_can_block, assert_task_context};
use crate::types::*;

/// A type-safe FIFO queue for inter-task communication.
///
/// Queues allow tasks to send and receive messages of type `T`.
/// The queue stores copies of items, so `T` must implement `Copy`.
///
/// # Example
///
/// ```no_run
/// use freertos_in_rust::sync::{Queue, TaskContext};
/// use freertos_in_rust::kernel::queue::StaticQueue_t;
///
/// let context = unsafe { TaskContext::assume() };
/// let storage = Box::leak(Box::new([0_u32; 10]));
/// let control = Box::leak(Box::new(StaticQueue_t::new()));
/// let queue = Queue::new_static(&context, storage, control).expect("queue creation failed");
///
/// // Send a value (from one task)
/// queue.try_send(&context, &42);
///
/// // Receive a value (from another task)
/// if let Some(value) = queue.try_receive(&context) {
///     println!("Got: {}", value);
/// }
/// ```
///
/// # Memory Layout
///
/// The queue stores items by value (copying them). For large types,
/// consider sending pointers or indices instead.
///
/// ```compile_fail
/// use freertos_in_rust::sync::Queue;
/// fn no_context(queue: &Queue<u32>) {
///     let _ = queue.try_send(&42); // a `&TaskContext` capability is required
/// }
/// ```
///
/// The queue is invariant in `T`, so a reference cannot be shortened while
/// the queue can still return the originally enqueued value:
///
/// ```compile_fail
/// use freertos_in_rust::sync::{Queue, TaskContext};
///
/// fn cannot_outlive_referent(queue: &Queue<&u32>, context: &TaskContext) {
///     {
///         let temporary = 42_u32;
///         assert!(queue.try_send(context, &&temporary));
///     }
///
///     // This could otherwise reconstruct a dangling `&u32` from the raw
///     // bytes retained by FreeRTOS.
///     let _dangling = queue.try_receive(context);
/// }
/// ```
pub struct Queue<T: Copy> {
    handle: QueueHandle_t,
    capacity: UBaseType_t,
    owns_handle: bool,
    // FreeRTOS owns copies of `T` in storage that Rust cannot see.  Invariance
    // is required so the compiler cannot shorten a reference lifetime inside
    // `T` between a send and a later receive.
    _marker: PhantomData<fn(T) -> T>,
}

// Safety: Queue can be shared between tasks - that's its purpose
unsafe impl<T: Copy + Send> Sync for Queue<T> {}
unsafe impl<T: Copy + Send> Send for Queue<T> {}

impl<T: Copy> Queue<T> {
    /// Creates a new queue that can hold `length` items of type `T`.
    ///
    /// Returns `None` if the queue could not be created (e.g., out of memory).
    ///
    /// # Example
    ///
    /// ```no_run
    /// use freertos_in_rust::sync::{Queue, TaskContext};
    ///
    /// let context = unsafe { TaskContext::assume() };
    /// // Queue of 5 sensor readings
    /// let readings: Queue<i16> = Queue::new(&context, 5).expect("Queue creation failed");
    /// ```
    #[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
    pub fn new(_context: &TaskContext, length: usize) -> Option<Self> {
        assert_task_context("Queue::new");
        let (length, item_size) = Self::checked_dimensions(length)?;
        let handle = unsafe { xQueueGenericCreate(length, item_size, queueQUEUE_TYPE_BASE) };
        if handle.is_null() {
            None
        } else {
            Some(Self {
                handle,
                capacity: length,
                owns_handle: true,
                _marker: PhantomData,
            })
        }
    }

    /// Creates a queue using statically allocated storage.
    ///
    /// This is the preferred method for embedded systems where dynamic
    /// allocation should be avoided.
    ///
    /// # Arguments
    ///
    /// * `storage` - Static buffer for queue items (must hold at least `length` items)
    /// * `queue_buffer` - Static queue control structure
    ///
    /// # Example
    ///
    /// ```no_run
    /// use freertos_in_rust::sync::{Queue, TaskContext};
    /// use freertos_in_rust::kernel::queue::StaticQueue_t;
    ///
    /// let context = unsafe { TaskContext::assume() };
    /// let storage = Box::leak(Box::new([0_u32; 10]));
    /// let control = Box::leak(Box::new(StaticQueue_t::new()));
    /// let queue = Queue::new_static(&context, storage, control).expect("queue creation failed");
    /// ```
    pub fn new_static(
        _context: &TaskContext,
        storage: &'static mut [T],
        queue_buffer: &'static mut StaticQueue_t,
    ) -> Option<Self> {
        assert_task_context("Queue::new_static");
        let (length, item_size) = Self::checked_dimensions(storage.len())?;
        let handle = unsafe {
            xQueueGenericCreateStatic(
                length,
                item_size,
                storage.as_mut_ptr() as *mut u8,
                queue_buffer as *mut StaticQueue_t,
                queueQUEUE_TYPE_BASE,
            )
        };
        if handle.is_null() {
            None
        } else {
            Some(Self {
                handle,
                capacity: length,
                owns_handle: true,
                _marker: PhantomData,
            })
        }
    }

    fn checked_dimensions(length: usize) -> Option<(UBaseType_t, UBaseType_t)> {
        if length == 0 || size_of::<T>() == 0 {
            return None;
        }

        let length = UBaseType_t::try_from(length).ok()?;
        let item_size = UBaseType_t::try_from(size_of::<T>()).ok()?;
        Some((length, item_size))
    }

    /// Sends an item to the back of the queue, blocking indefinitely.
    ///
    /// Returns `true` if the item was sent. With infinite timeout,
    /// this always returns `true` under normal operation.
    pub fn send(&self, context: &TaskContext, item: &T) -> bool {
        self.send_timeout(context, item, portMAX_DELAY)
    }

    /// Attempts to send an item without blocking.
    ///
    /// Returns `true` if sent, `false` if queue is full.
    pub fn try_send(&self, context: &TaskContext, item: &T) -> bool {
        self.send_timeout(context, item, 0)
    }

    /// Sends an item with a timeout.
    ///
    /// Returns `true` if sent within the timeout, `false` otherwise.
    pub fn send_timeout(&self, _context: &TaskContext, item: &T, ticks: TickType_t) -> bool {
        assert_can_block("Queue::send_timeout", ticks);
        unsafe {
            xQueueGenericSend(
                self.handle,
                item as *const T as *const c_void,
                ticks,
                queueSEND_TO_BACK,
            ) == pdTRUE
        }
    }

    /// Sends an item to the front of the queue, blocking indefinitely.
    ///
    /// Items sent to front are received before items sent to back.
    pub fn send_to_front(&self, context: &TaskContext, item: &T) -> bool {
        self.send_to_front_timeout(context, item, portMAX_DELAY)
    }

    /// Sends an item to the front with a timeout.
    pub fn send_to_front_timeout(
        &self,
        _context: &TaskContext,
        item: &T,
        ticks: TickType_t,
    ) -> bool {
        assert_can_block("Queue::send_to_front_timeout", ticks);
        unsafe {
            xQueueGenericSend(
                self.handle,
                item as *const T as *const c_void,
                ticks,
                queueSEND_TO_FRONT,
            ) == pdTRUE
        }
    }

    /// Receives an item from the queue, blocking indefinitely.
    ///
    /// Returns `Some(item)` when an item is received. With infinite
    /// timeout, this always returns `Some` under normal operation.
    pub fn receive(&self, context: &TaskContext) -> Option<T> {
        self.receive_timeout(context, portMAX_DELAY)
    }

    /// Attempts to receive an item without blocking.
    ///
    /// Returns `Some(item)` if available, `None` if queue is empty.
    pub fn try_receive(&self, context: &TaskContext) -> Option<T> {
        self.receive_timeout(context, 0)
    }

    /// Receives an item with a timeout.
    ///
    /// Returns `Some(item)` if received within timeout, `None` otherwise.
    pub fn receive_timeout(&self, _context: &TaskContext, ticks: TickType_t) -> Option<T> {
        assert_can_block("Queue::receive_timeout", ticks);
        let mut item = MaybeUninit::<T>::uninit();
        let result = unsafe { xQueueReceive(self.handle, item.as_mut_ptr() as *mut c_void, ticks) };
        if result == pdTRUE {
            Some(unsafe { item.assume_init() })
        } else {
            None
        }
    }

    /// Peeks at the front item without removing it, blocking indefinitely.
    ///
    /// Returns a copy of the front item. The item remains in the queue.
    pub fn peek(&self, context: &TaskContext) -> Option<T> {
        self.peek_timeout(context, portMAX_DELAY)
    }

    /// Attempts to peek without blocking.
    pub fn try_peek(&self, context: &TaskContext) -> Option<T> {
        self.peek_timeout(context, 0)
    }

    /// Peeks with a timeout.
    pub fn peek_timeout(&self, _context: &TaskContext, ticks: TickType_t) -> Option<T> {
        assert_can_block("Queue::peek_timeout", ticks);
        let mut item = MaybeUninit::<T>::uninit();
        let result = unsafe { xQueuePeek(self.handle, item.as_mut_ptr() as *mut c_void, ticks) };
        if result == pdTRUE {
            Some(unsafe { item.assume_init() })
        } else {
            None
        }
    }

    /// Returns the number of items currently in the queue.
    pub fn len(&self, _context: &TaskContext) -> usize {
        assert_task_context("Queue::len");
        unsafe { crate::kernel::queue::uxQueueMessagesWaiting(self.handle) as usize }
    }

    /// Returns `true` if the queue is empty.
    pub fn is_empty(&self, context: &TaskContext) -> bool {
        self.len(context) == 0
    }

    /// Returns the number of free spaces in the queue.
    pub fn available(&self, _context: &TaskContext) -> usize {
        assert_task_context("Queue::available");
        unsafe { crate::kernel::queue::uxQueueSpacesAvailable(self.handle) as usize }
    }

    /// Returns `true` if the queue is full.
    pub fn is_full(&self, context: &TaskContext) -> bool {
        self.available(context) == 0
    }

    /// Removes all items from the queue.
    pub fn reset(&self, _context: &TaskContext) -> bool {
        assert_task_context("Queue::reset");
        unsafe { crate::kernel::queue::xQueueGenericReset(self.handle, pdFALSE) == pdPASS }
    }

    /// Sends an item to the back of the queue from interrupt context.
    ///
    /// This method does not request a context switch. If it sets
    /// `higher_priority_task_woken` to `pdTRUE`, the ISR must yield using the
    /// active port's ISR-yield primitive before returning.
    ///
    /// # Safety
    ///
    /// The caller must be executing at an interrupt priority from which
    /// FreeRTOS `FromISR` APIs are permitted. The wake flag must be initialized
    /// to `pdFALSE` before the first operation in an ISR.
    pub unsafe fn send_from_isr(
        &self,
        item: &T,
        higher_priority_task_woken: &mut BaseType_t,
    ) -> bool {
        // Safety: `self` owns a live handle, the item and wake flag are valid
        // for the call, and the caller upholds the ISR-priority contract.
        unsafe {
            xQueueSendToBackFromISR(
                self.handle,
                item as *const T as *const c_void,
                higher_priority_task_woken,
            ) == pdPASS
        }
    }

    /// Sends an item to the front of the queue from interrupt context.
    ///
    /// # Safety
    ///
    /// The caller must obey the interrupt-priority and wake-flag requirements
    /// documented on [`Queue::send_from_isr`].
    pub unsafe fn send_to_front_from_isr(
        &self,
        item: &T,
        higher_priority_task_woken: &mut BaseType_t,
    ) -> bool {
        // Safety: the same handle, buffer, and ISR invariants documented on
        // `send_from_isr` are upheld by this method's caller.
        unsafe {
            xQueueSendToFrontFromISR(
                self.handle,
                item as *const T as *const c_void,
                higher_priority_task_woken,
            ) == pdPASS
        }
    }

    /// Receives an item from interrupt context.
    ///
    /// # Safety
    ///
    /// The caller must obey the interrupt-priority and wake-flag requirements
    /// documented on [`Queue::send_from_isr`].
    pub unsafe fn receive_from_isr(
        &self,
        higher_priority_task_woken: &mut BaseType_t,
    ) -> Option<T> {
        let mut item = MaybeUninit::<T>::uninit();
        // Safety: MaybeUninit supplies writable storage for one `T`, the wake
        // flag is valid, and the caller upholds the ISR-priority contract.
        let result = unsafe {
            xQueueReceiveFromISR(
                self.handle,
                item.as_mut_ptr() as *mut c_void,
                higher_priority_task_woken,
            )
        };
        if result == pdPASS {
            // Safety: a successful receive initialized the complete item.
            Some(unsafe { item.assume_init() })
        } else {
            None
        }
    }

    /// Peeks at the front item from interrupt context.
    ///
    /// # Safety
    ///
    /// The caller must be executing at an interrupt priority from which
    /// FreeRTOS `FromISR` APIs are permitted.
    pub unsafe fn peek_from_isr(&self) -> Option<T> {
        let mut item = MaybeUninit::<T>::uninit();
        // Safety: the output storage is valid and the caller upholds the
        // ISR-priority contract.
        let result = unsafe { xQueuePeekFromISR(self.handle, item.as_mut_ptr() as *mut c_void) };
        if result == pdPASS {
            // Safety: a successful peek initialized the complete item.
            Some(unsafe { item.assume_init() })
        } else {
            None
        }
    }

    /// Returns the number of queued items from interrupt context.
    ///
    /// # Safety
    ///
    /// The caller must be executing at an interrupt priority from which
    /// FreeRTOS `FromISR` APIs are permitted.
    pub unsafe fn len_from_isr(&self) -> usize {
        // Safety: `self` owns a live queue and the caller upholds the ISR
        // context contract.
        unsafe { uxQueueMessagesWaitingFromISR(self.handle) as usize }
    }

    /// Returns whether the queue is empty from interrupt context.
    ///
    /// # Safety
    ///
    /// See [`Queue::len_from_isr`].
    pub unsafe fn is_empty_from_isr(&self) -> bool {
        // Safety: `self` owns a live queue and the caller upholds the ISR
        // context contract.
        unsafe { xQueueIsQueueEmptyFromISR(self.handle) != pdFALSE }
    }

    /// Returns whether the queue is full from interrupt context.
    ///
    /// # Safety
    ///
    /// See [`Queue::len_from_isr`].
    pub unsafe fn is_full_from_isr(&self) -> bool {
        // Safety: `self` owns a live queue and the caller upholds the ISR
        // context contract.
        unsafe { xQueueIsQueueFullFromISR(self.handle) != pdFALSE }
    }

    /// Returns the free queue capacity from interrupt context.
    ///
    /// # Safety
    ///
    /// See [`Queue::len_from_isr`].
    pub unsafe fn available_from_isr(&self) -> usize {
        // Safety: `self` owns a live queue and the caller upholds the ISR
        // context contract.
        let waiting = unsafe { uxQueueMessagesWaitingFromISR(self.handle) };
        self.capacity.saturating_sub(waiting) as usize
    }

    /// Returns the raw FreeRTOS handle for interop.
    ///
    /// # Safety
    ///
    /// The caller must not let the raw handle outlive this owner, delete or
    /// reconfigure the queue behind the wrapper, or use an item layout other
    /// than the exact `T` configured by this queue.
    pub unsafe fn raw_handle(&self) -> QueueHandle_t {
        self.handle
    }

    /// Consumes the wrapper and returns the raw handle without deleting it.
    pub fn into_raw(mut self) -> QueueHandle_t {
        self.owns_handle = false;
        self.handle
    }

    /// Explicitly deletes the underlying FreeRTOS queue.
    #[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
    pub fn delete(mut self, _context: &TaskContext) {
        assert_task_context("Queue::delete");
        unsafe { vQueueDelete(self.handle) };
        self.owns_handle = false;
    }
}

#[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
impl<T: Copy> Drop for Queue<T> {
    fn drop(&mut self) {
        // vQueueDelete is a task-context API. If destruction happens in an
        // ISR, leak the kernel allocation rather than call the wrong API.
        if self.owns_handle && !is_in_isr() {
            unsafe { vQueueDelete(self.handle) };
            self.owns_handle = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_dimensions_reject_zero_length_and_zero_sized_items() {
        assert!(Queue::<u8>::checked_dimensions(0).is_none());
        assert!(Queue::<()>::checked_dimensions(1).is_none());
        assert_eq!(Queue::<u32>::checked_dimensions(3), Some((3, 4)));
    }

    #[test]
    fn queue_send_sync_tracks_item_send() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Queue<u32>>();
    }
}
