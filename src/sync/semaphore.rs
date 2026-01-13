//! Safe Semaphore wrappers
//!
//! Provides binary and counting semaphores for task synchronization.
//! Unlike mutexes, semaphores don't have priority inheritance and
//! can be given from ISR context.

use crate::kernel::queue::{
    queueQUEUE_TYPE_BINARY_SEMAPHORE, queueSEND_TO_BACK, xQueueGenericCreate, xQueueGenericSend,
    xQueueSemaphoreTake, QueueHandle_t,
};
use crate::types::*;

/// A binary semaphore for task synchronization.
///
/// Binary semaphores are useful for signaling between tasks or from
/// an ISR to a task. Unlike mutexes, they don't have priority inheritance
/// and can be "given" from interrupt context.
///
/// # Example
///
/// ```ignore
/// use freertos_in_rust::sync::BinarySemaphore;
///
/// let sem = BinarySemaphore::new().expect("Failed to create semaphore");
///
/// // In one task: wait for signal
/// sem.take();  // Blocks until signaled
///
/// // In another task or ISR: signal
/// sem.give();
/// ```
pub struct BinarySemaphore {
    handle: QueueHandle_t,
}

// Safety: BinarySemaphore can be shared between tasks
unsafe impl Sync for BinarySemaphore {}
unsafe impl Send for BinarySemaphore {}

impl BinarySemaphore {
    /// Creates a new binary semaphore in the "not given" state.
    ///
    /// Returns `None` if creation failed (e.g., out of memory).
    #[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
    pub fn new() -> Option<Self> {
        let handle =
            unsafe { xQueueGenericCreate(1, 0, queueQUEUE_TYPE_BINARY_SEMAPHORE) };
        if handle.is_null() {
            None
        } else {
            Some(Self { handle })
        }
    }

    /// Takes (acquires) the semaphore, blocking indefinitely.
    ///
    /// Returns `true` if taken, though with infinite timeout this
    /// always returns `true` under normal operation.
    pub fn take(&self) -> bool {
        unsafe { xQueueSemaphoreTake(self.handle, portMAX_DELAY) == pdTRUE }
    }

    /// Attempts to take the semaphore without blocking.
    ///
    /// Returns `true` if the semaphore was taken, `false` if it
    /// wasn't available.
    pub fn try_take(&self) -> bool {
        unsafe { xQueueSemaphoreTake(self.handle, 0) == pdTRUE }
    }

    /// Attempts to take the semaphore with a timeout.
    ///
    /// Returns `true` if taken within the timeout, `false` otherwise.
    pub fn take_timeout(&self, ticks: TickType_t) -> bool {
        unsafe { xQueueSemaphoreTake(self.handle, ticks) == pdTRUE }
    }

    /// Gives (releases) the semaphore.
    ///
    /// This can be called even if the semaphore hasn't been taken -
    /// it simply ensures the semaphore is in the "given" state.
    pub fn give(&self) -> bool {
        unsafe {
            xQueueGenericSend(self.handle, core::ptr::null(), 0, queueSEND_TO_BACK) == pdTRUE
        }
    }

    /// Returns the raw FreeRTOS handle for interop with raw APIs.
    pub unsafe fn raw_handle(&self) -> QueueHandle_t {
        self.handle
    }
}

/// A counting semaphore for resource management.
///
/// Counting semaphores track a count of available resources. Each `take`
/// decrements the count (blocking if zero), and each `give` increments it.
///
/// # Example
///
/// ```ignore
/// use freertos_in_rust::sync::CountingSemaphore;
///
/// // Create semaphore with max 5, initial 3 available
/// let pool = CountingSemaphore::new(5, 3).expect("Failed to create");
///
/// // Acquire a resource (decrements count)
/// pool.take();
///
/// // Release a resource (increments count)
/// pool.give();
/// ```
pub struct CountingSemaphore {
    handle: QueueHandle_t,
}

unsafe impl Sync for CountingSemaphore {}
unsafe impl Send for CountingSemaphore {}

impl CountingSemaphore {
    /// Creates a counting semaphore.
    ///
    /// # Arguments
    ///
    /// * `max_count` - Maximum count value
    /// * `initial_count` - Initial count (must be <= max_count)
    #[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
    pub fn new(max_count: UBaseType_t, initial_count: UBaseType_t) -> Option<Self> {
        let handle = unsafe {
            crate::kernel::queue::xQueueCreateCountingSemaphore(max_count, initial_count)
        };
        if handle.is_null() {
            None
        } else {
            Some(Self { handle })
        }
    }

    /// Takes (decrements) the semaphore, blocking indefinitely.
    pub fn take(&self) -> bool {
        unsafe { xQueueSemaphoreTake(self.handle, portMAX_DELAY) == pdTRUE }
    }

    /// Attempts to take without blocking.
    pub fn try_take(&self) -> bool {
        unsafe { xQueueSemaphoreTake(self.handle, 0) == pdTRUE }
    }

    /// Attempts to take with timeout.
    pub fn take_timeout(&self, ticks: TickType_t) -> bool {
        unsafe { xQueueSemaphoreTake(self.handle, ticks) == pdTRUE }
    }

    /// Gives (increments) the semaphore.
    pub fn give(&self) -> bool {
        unsafe {
            xQueueGenericSend(self.handle, core::ptr::null(), 0, queueSEND_TO_BACK) == pdTRUE
        }
    }

    /// Returns current count (if trace facility enabled).
    #[cfg(feature = "trace-facility")]
    pub fn count(&self) -> UBaseType_t {
        unsafe { crate::kernel::queue::uxQueueMessagesWaiting(self.handle) }
    }

    /// Returns the raw handle.
    pub unsafe fn raw_handle(&self) -> QueueHandle_t {
        self.handle
    }
}
