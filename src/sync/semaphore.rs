//! Safe Semaphore wrappers
//!
//! Provides binary and counting semaphores for task synchronization.
//! Unlike mutexes, semaphores don't have priority inheritance and
//! can be given from ISR context.

use core::ptr;

use crate::kernel::queue::{
    queueQUEUE_TYPE_BINARY_SEMAPHORE, queueSEND_TO_BACK, uxQueueMessagesWaiting,
    uxQueueMessagesWaitingFromISR, xQueueCreateCountingSemaphoreStatic, xQueueGenericCreateStatic,
    xQueueGenericSend, xQueueGiveFromISR, xQueueReceiveFromISR, xQueueSemaphoreTake, QueueHandle_t,
    StaticQueue_t,
};
#[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
use crate::kernel::queue::{vQueueDelete, xQueueCreateCountingSemaphore, xQueueGenericCreate};
#[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
use crate::sync::is_in_isr;
use crate::sync::task::TaskContext;
use crate::sync::{assert_can_block, assert_task_context};
use crate::types::*;

/// A binary semaphore for task synchronization.
///
/// Binary semaphores are useful for signaling between tasks or from
/// an ISR to a task. Unlike mutexes, they don't have priority inheritance
/// and can be "given" from interrupt context.
///
/// # Example
///
/// ```no_run
/// use freertos_in_rust::sync::{BinarySemaphore, TaskContext};
/// use freertos_in_rust::kernel::queue::StaticQueue_t;
///
/// let context = unsafe { TaskContext::assume() };
/// let control = Box::leak(Box::new(StaticQueue_t::new()));
/// let sem = BinarySemaphore::new_static(&context, control).expect("semaphore creation failed");
///
/// // In one task: wait for signal
/// sem.take(&context);  // Blocks until signaled
///
/// // In another task: signal
/// sem.give(&context);
///
/// // An ISR uses unsafe `give_from_isr` instead.
/// ```
///
/// ```compile_fail
/// use freertos_in_rust::sync::BinarySemaphore;
/// fn no_context(semaphore: &BinarySemaphore) {
///     let _ = semaphore.try_take(); // a `&TaskContext` capability is required
/// }
/// ```
pub struct BinarySemaphore {
    handle: QueueHandle_t,
    owns_handle: bool,
}

// Safety: BinarySemaphore can be shared between tasks
unsafe impl Sync for BinarySemaphore {}
unsafe impl Send for BinarySemaphore {}

impl BinarySemaphore {
    /// Creates a new binary semaphore in the "not given" state.
    ///
    /// Returns `None` if creation failed (e.g., out of memory).
    #[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
    pub fn new(_context: &TaskContext) -> Option<Self> {
        assert_task_context("BinarySemaphore::new");
        let handle = unsafe { xQueueGenericCreate(1, 0, queueQUEUE_TYPE_BINARY_SEMAPHORE) };
        if handle.is_null() {
            None
        } else {
            Some(Self {
                handle,
                owns_handle: true,
            })
        }
    }

    /// Creates a binary semaphore using static storage.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use freertos_in_rust::sync::{BinarySemaphore, TaskContext};
    /// use freertos_in_rust::kernel::queue::StaticQueue_t;
    ///
    /// let context = unsafe { TaskContext::assume() };
    /// let control = Box::leak(Box::new(StaticQueue_t::new()));
    /// let sem = BinarySemaphore::new_static(&context, control).expect("creation failed");
    /// ```
    pub fn new_static(
        _context: &TaskContext,
        sem_buffer: &'static mut StaticQueue_t,
    ) -> Option<Self> {
        assert_task_context("BinarySemaphore::new_static");
        let handle = unsafe {
            xQueueGenericCreateStatic(
                1,
                0,
                ptr::null_mut(),
                sem_buffer as *mut StaticQueue_t,
                queueQUEUE_TYPE_BINARY_SEMAPHORE,
            )
        };
        if handle.is_null() {
            None
        } else {
            Some(Self {
                handle,
                owns_handle: true,
            })
        }
    }

    /// Takes (acquires) the semaphore, blocking indefinitely.
    ///
    /// Returns `true` if taken, though with infinite timeout this
    /// always returns `true` under normal operation.
    pub fn take(&self, context: &TaskContext) -> bool {
        self.take_timeout(context, portMAX_DELAY)
    }

    /// Attempts to take the semaphore without blocking.
    ///
    /// Returns `true` if the semaphore was taken, `false` if it
    /// wasn't available.
    pub fn try_take(&self, context: &TaskContext) -> bool {
        self.take_timeout(context, 0)
    }

    /// Attempts to take the semaphore with a timeout.
    ///
    /// Returns `true` if taken within the timeout, `false` otherwise.
    pub fn take_timeout(&self, _context: &TaskContext, ticks: TickType_t) -> bool {
        assert_can_block("BinarySemaphore::take_timeout", ticks);
        unsafe { xQueueSemaphoreTake(self.handle, ticks) == pdTRUE }
    }

    /// Gives (releases) the semaphore.
    ///
    /// This can be called even if the semaphore hasn't been taken -
    /// it simply ensures the semaphore is in the "given" state.
    pub fn give(&self, _context: &TaskContext) -> bool {
        assert_task_context("BinarySemaphore::give");
        unsafe { xQueueGenericSend(self.handle, core::ptr::null(), 0, queueSEND_TO_BACK) == pdTRUE }
    }

    /// Gives the semaphore from interrupt context.
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
    pub unsafe fn give_from_isr(&self, higher_priority_task_woken: &mut BaseType_t) -> bool {
        // Safety: `self` owns a live semaphore, the wake flag is valid, and
        // the caller upholds the ISR-priority contract.
        unsafe { xQueueGiveFromISR(self.handle, higher_priority_task_woken) == pdPASS }
    }

    /// Takes the semaphore from interrupt context without blocking.
    ///
    /// # Safety
    ///
    /// The caller must obey the interrupt-priority and wake-flag requirements
    /// documented on [`BinarySemaphore::give_from_isr`].
    pub unsafe fn take_from_isr(&self, higher_priority_task_woken: &mut BaseType_t) -> bool {
        // Safety: item size is zero for a semaphore, so a null receive buffer
        // is valid; the caller upholds the ISR-priority contract.
        unsafe {
            xQueueReceiveFromISR(self.handle, ptr::null_mut(), higher_priority_task_woken) == pdPASS
        }
    }

    /// Returns `true` if the semaphore is currently available.
    pub fn is_available(&self, _context: &TaskContext) -> bool {
        assert_task_context("BinarySemaphore::is_available");
        unsafe { uxQueueMessagesWaiting(self.handle) != 0 }
    }

    /// Returns `true` if the semaphore is available from interrupt context.
    ///
    /// # Safety
    ///
    /// The caller must be executing at an interrupt priority from which
    /// FreeRTOS `FromISR` APIs are permitted.
    pub unsafe fn is_available_from_isr(&self) -> bool {
        // Safety: `self` owns a live semaphore and the caller upholds the ISR
        // context contract.
        unsafe { uxQueueMessagesWaitingFromISR(self.handle) != 0 }
    }

    /// Returns the raw FreeRTOS handle for interop with raw APIs.
    ///
    /// # Safety
    ///
    /// The caller must not let the handle outlive this owner, delete or reset
    /// the semaphore behind the wrapper, or violate the raw operation's
    /// task/ISR-context requirements.
    pub unsafe fn raw_handle(&self) -> QueueHandle_t {
        self.handle
    }

    /// Consumes the wrapper and returns the raw handle without deleting it.
    pub fn into_raw(mut self) -> QueueHandle_t {
        self.owns_handle = false;
        self.handle
    }

    /// Explicitly deletes the underlying FreeRTOS semaphore.
    #[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
    pub fn delete(mut self, _context: &TaskContext) {
        assert_task_context("BinarySemaphore::delete");
        unsafe { vQueueDelete(self.handle) };
        self.owns_handle = false;
    }
}

/// A counting semaphore for resource management.
///
/// Counting semaphores track a count of available resources. Each `take`
/// decrements the count (blocking if zero), and each `give` increments it.
///
/// # Example
///
/// ```no_run
/// use freertos_in_rust::sync::{CountingSemaphore, TaskContext};
/// use freertos_in_rust::kernel::queue::StaticQueue_t;
///
/// let context = unsafe { TaskContext::assume() };
/// let control = Box::leak(Box::new(StaticQueue_t::new()));
/// let pool = CountingSemaphore::new_static(&context, 5, 3, control).expect("creation failed");
///
/// // Acquire a resource (decrements count)
/// pool.take(&context);
///
/// // Release a resource (increments count)
/// pool.give(&context);
/// ```
pub struct CountingSemaphore {
    handle: QueueHandle_t,
    owns_handle: bool,
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
    pub fn new(
        _context: &TaskContext,
        max_count: UBaseType_t,
        initial_count: UBaseType_t,
    ) -> Option<Self> {
        assert_task_context("CountingSemaphore::new");
        Self::validate_counts(max_count, initial_count)?;
        let handle = unsafe { xQueueCreateCountingSemaphore(max_count, initial_count) };
        if handle.is_null() {
            None
        } else {
            Some(Self {
                handle,
                owns_handle: true,
            })
        }
    }

    /// Creates a counting semaphore using static storage.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use freertos_in_rust::sync::{CountingSemaphore, TaskContext};
    /// use freertos_in_rust::kernel::queue::StaticQueue_t;
    ///
    /// let context = unsafe { TaskContext::assume() };
    /// let control = Box::leak(Box::new(StaticQueue_t::new()));
    /// let pool = CountingSemaphore::new_static(&context, 5, 3, control)
    ///     .expect("semaphore creation failed");
    /// ```
    pub fn new_static(
        _context: &TaskContext,
        max_count: UBaseType_t,
        initial_count: UBaseType_t,
        sem_buffer: &'static mut StaticQueue_t,
    ) -> Option<Self> {
        assert_task_context("CountingSemaphore::new_static");
        Self::validate_counts(max_count, initial_count)?;
        let handle = unsafe {
            xQueueCreateCountingSemaphoreStatic(
                max_count,
                initial_count,
                sem_buffer as *mut StaticQueue_t,
            )
        };
        if handle.is_null() {
            None
        } else {
            Some(Self {
                handle,
                owns_handle: true,
            })
        }
    }

    fn validate_counts(max_count: UBaseType_t, initial_count: UBaseType_t) -> Option<()> {
        if max_count == 0 || initial_count > max_count {
            None
        } else {
            Some(())
        }
    }

    /// Takes (decrements) the semaphore, blocking indefinitely.
    pub fn take(&self, context: &TaskContext) -> bool {
        self.take_timeout(context, portMAX_DELAY)
    }

    /// Attempts to take without blocking.
    pub fn try_take(&self, context: &TaskContext) -> bool {
        self.take_timeout(context, 0)
    }

    /// Attempts to take with timeout.
    pub fn take_timeout(&self, _context: &TaskContext, ticks: TickType_t) -> bool {
        assert_can_block("CountingSemaphore::take_timeout", ticks);
        unsafe { xQueueSemaphoreTake(self.handle, ticks) == pdTRUE }
    }

    /// Gives (increments) the semaphore.
    pub fn give(&self, _context: &TaskContext) -> bool {
        assert_task_context("CountingSemaphore::give");
        unsafe { xQueueGenericSend(self.handle, core::ptr::null(), 0, queueSEND_TO_BACK) == pdTRUE }
    }

    /// Gives the semaphore from interrupt context.
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
    pub unsafe fn give_from_isr(&self, higher_priority_task_woken: &mut BaseType_t) -> bool {
        // Safety: `self` owns a live semaphore, the wake flag is valid, and
        // the caller upholds the ISR-priority contract.
        unsafe { xQueueGiveFromISR(self.handle, higher_priority_task_woken) == pdPASS }
    }

    /// Takes the semaphore from interrupt context without blocking.
    ///
    /// # Safety
    ///
    /// The caller must obey the interrupt-priority and wake-flag requirements
    /// documented on [`CountingSemaphore::give_from_isr`].
    pub unsafe fn take_from_isr(&self, higher_priority_task_woken: &mut BaseType_t) -> bool {
        // Safety: item size is zero for a semaphore, so a null receive buffer
        // is valid; the caller upholds the ISR-priority contract.
        unsafe {
            xQueueReceiveFromISR(self.handle, ptr::null_mut(), higher_priority_task_woken) == pdPASS
        }
    }

    /// Returns current count.
    pub fn count(&self, _context: &TaskContext) -> UBaseType_t {
        assert_task_context("CountingSemaphore::count");
        unsafe { uxQueueMessagesWaiting(self.handle) }
    }

    /// Returns current count from interrupt context.
    ///
    /// # Safety
    ///
    /// The caller must be executing at an interrupt priority from which
    /// FreeRTOS `FromISR` APIs are permitted.
    pub unsafe fn count_from_isr(&self) -> UBaseType_t {
        // Safety: `self` owns a live semaphore and the caller upholds the ISR
        // context contract.
        unsafe { uxQueueMessagesWaitingFromISR(self.handle) }
    }

    /// Returns the raw handle.
    ///
    /// # Safety
    ///
    /// The caller must not let the handle outlive this owner, delete or reset
    /// the semaphore behind the wrapper, or violate the raw operation's
    /// task/ISR-context requirements.
    pub unsafe fn raw_handle(&self) -> QueueHandle_t {
        self.handle
    }

    /// Consumes the wrapper and returns the raw handle without deleting it.
    pub fn into_raw(mut self) -> QueueHandle_t {
        self.owns_handle = false;
        self.handle
    }

    /// Explicitly deletes the underlying FreeRTOS semaphore.
    #[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
    pub fn delete(mut self, _context: &TaskContext) {
        assert_task_context("CountingSemaphore::delete");
        unsafe { vQueueDelete(self.handle) };
        self.owns_handle = false;
    }
}

#[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
impl Drop for BinarySemaphore {
    fn drop(&mut self) {
        if self.owns_handle && !is_in_isr() {
            unsafe { vQueueDelete(self.handle) };
            self.owns_handle = false;
        }
    }
}

#[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
impl Drop for CountingSemaphore {
    fn drop(&mut self) {
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
    fn counting_semaphore_rejects_invalid_counts() {
        assert!(CountingSemaphore::validate_counts(0, 0).is_none());
        assert!(CountingSemaphore::validate_counts(2, 3).is_none());
        assert!(CountingSemaphore::validate_counts(2, 2).is_some());
    }

    #[test]
    fn semaphore_handles_are_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<BinarySemaphore>();
        assert_send_sync::<CountingSemaphore>();
    }
}
