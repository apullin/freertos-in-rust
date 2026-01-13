//! Safe Queue wrapper
//!
//! Provides a type-safe queue for inter-task communication.
//! Unlike the raw FreeRTOS API which uses void pointers,
//! this wrapper ensures type safety at compile time.

use core::ffi::c_void;
use core::marker::PhantomData;
use core::mem::{size_of, MaybeUninit};

use crate::kernel::queue::{
    queueQUEUE_TYPE_BASE, queueSEND_TO_BACK, queueSEND_TO_FRONT, xQueueGenericCreateStatic,
    xQueueGenericSend, xQueuePeek, xQueueReceive, QueueHandle_t, StaticQueue_t,
};
#[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
use crate::kernel::queue::xQueueGenericCreate;
use crate::types::*;

/// A type-safe FIFO queue for inter-task communication.
///
/// Queues allow tasks to send and receive messages of type `T`.
/// The queue stores copies of items, so `T` must implement `Copy`.
///
/// # Example
///
/// ```ignore
/// use freertos_in_rust::sync::Queue;
///
/// // Create a queue that can hold 10 u32 values
/// let queue: Queue<u32> = Queue::new(10).expect("Failed to create queue");
///
/// // Send a value (from one task)
/// queue.send(&42);
///
/// // Receive a value (from another task)
/// if let Some(value) = queue.receive() {
///     println!("Got: {}", value);
/// }
/// ```
///
/// # Memory Layout
///
/// The queue stores items by value (copying them). For large types,
/// consider sending pointers or indices instead.
pub struct Queue<T: Copy> {
    handle: QueueHandle_t,
    _marker: PhantomData<T>,
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
    /// ```ignore
    /// // Queue of 5 sensor readings
    /// let readings: Queue<i16> = Queue::new(5).expect("Queue creation failed");
    /// ```
    #[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
    pub fn new(length: usize) -> Option<Self> {
        let item_size = size_of::<T>() as UBaseType_t;
        let handle =
            unsafe { xQueueGenericCreate(length as UBaseType_t, item_size, queueQUEUE_TYPE_BASE) };
        if handle.is_null() {
            None
        } else {
            Some(Self {
                handle,
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
    /// ```ignore
    /// use freertos_in_rust::sync::Queue;
    /// use freertos_in_rust::kernel::queue::StaticQueue_t;
    ///
    /// static mut STORAGE: [u32; 10] = [0; 10];
    /// static mut QUEUE_BUF: StaticQueue_t = StaticQueue_t::new();
    ///
    /// let queue: Queue<u32> = Queue::new_static(
    ///     unsafe { &mut STORAGE },
    ///     unsafe { &mut QUEUE_BUF },
    /// ).expect("Failed to create queue");
    /// ```
    pub fn new_static(
        storage: &'static mut [T],
        queue_buffer: &'static mut StaticQueue_t,
    ) -> Option<Self> {
        let length = storage.len();
        let item_size = size_of::<T>();
        let handle = unsafe {
            xQueueGenericCreateStatic(
                length as UBaseType_t,
                item_size as UBaseType_t,
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
                _marker: PhantomData,
            })
        }
    }

    /// Sends an item to the back of the queue, blocking indefinitely.
    ///
    /// Returns `true` if the item was sent. With infinite timeout,
    /// this always returns `true` under normal operation.
    pub fn send(&self, item: &T) -> bool {
        self.send_timeout(item, portMAX_DELAY)
    }

    /// Attempts to send an item without blocking.
    ///
    /// Returns `true` if sent, `false` if queue is full.
    pub fn try_send(&self, item: &T) -> bool {
        self.send_timeout(item, 0)
    }

    /// Sends an item with a timeout.
    ///
    /// Returns `true` if sent within the timeout, `false` otherwise.
    pub fn send_timeout(&self, item: &T, ticks: TickType_t) -> bool {
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
    pub fn send_to_front(&self, item: &T) -> bool {
        self.send_to_front_timeout(item, portMAX_DELAY)
    }

    /// Sends an item to the front with a timeout.
    pub fn send_to_front_timeout(&self, item: &T, ticks: TickType_t) -> bool {
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
    pub fn receive(&self) -> Option<T> {
        self.receive_timeout(portMAX_DELAY)
    }

    /// Attempts to receive an item without blocking.
    ///
    /// Returns `Some(item)` if available, `None` if queue is empty.
    pub fn try_receive(&self) -> Option<T> {
        self.receive_timeout(0)
    }

    /// Receives an item with a timeout.
    ///
    /// Returns `Some(item)` if received within timeout, `None` otherwise.
    pub fn receive_timeout(&self, ticks: TickType_t) -> Option<T> {
        let mut item = MaybeUninit::<T>::uninit();
        let result =
            unsafe { xQueueReceive(self.handle, item.as_mut_ptr() as *mut c_void, ticks) };
        if result == pdTRUE {
            Some(unsafe { item.assume_init() })
        } else {
            None
        }
    }

    /// Peeks at the front item without removing it, blocking indefinitely.
    ///
    /// Returns a copy of the front item. The item remains in the queue.
    pub fn peek(&self) -> Option<T> {
        self.peek_timeout(portMAX_DELAY)
    }

    /// Attempts to peek without blocking.
    pub fn try_peek(&self) -> Option<T> {
        self.peek_timeout(0)
    }

    /// Peeks with a timeout.
    pub fn peek_timeout(&self, ticks: TickType_t) -> Option<T> {
        let mut item = MaybeUninit::<T>::uninit();
        let result = unsafe { xQueuePeek(self.handle, item.as_mut_ptr() as *mut c_void, ticks) };
        if result == pdTRUE {
            Some(unsafe { item.assume_init() })
        } else {
            None
        }
    }

    /// Returns the number of items currently in the queue.
    pub fn len(&self) -> usize {
        unsafe { crate::kernel::queue::uxQueueMessagesWaiting(self.handle) as usize }
    }

    /// Returns `true` if the queue is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns the number of free spaces in the queue.
    pub fn available(&self) -> usize {
        unsafe { crate::kernel::queue::uxQueueSpacesAvailable(self.handle) as usize }
    }

    /// Returns `true` if the queue is full.
    pub fn is_full(&self) -> bool {
        self.available() == 0
    }

    /// Removes all items from the queue.
    pub fn reset(&self) {
        unsafe {
            crate::kernel::queue::xQueueGenericReset(self.handle, pdFALSE);
        }
    }

    /// Returns the raw FreeRTOS handle for interop.
    pub unsafe fn raw_handle(&self) -> QueueHandle_t {
        self.handle
    }
}
