//! Safe Mutex wrapper with RAII guard pattern
//!
//! This provides a Rust-idiomatic mutex that wraps FreeRTOS's mutex
//! with priority inheritance. The mutex protects data of type `T`
//! and ensures exclusive access through the guard pattern.

use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};

use crate::kernel::queue::{
    queueQUEUE_TYPE_MUTEX, queueSEND_TO_BACK, xQueueCreateMutexStatic, xQueueGenericSend,
    xQueueSemaphoreTake, QueueHandle_t, StaticQueue_t,
};
#[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
use crate::kernel::queue::{vQueueDelete, xQueueCreateMutex};
#[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
use crate::sync::is_in_isr;
use crate::sync::task::TaskContext;
use crate::sync::{assert_can_block, assert_task_context};
use crate::types::*;

/// A mutual exclusion primitive with priority inheritance.
///
/// This mutex wraps FreeRTOS's mutex implementation, which includes
/// priority inheritance to prevent priority inversion. When a high-priority
/// task blocks on a mutex held by a lower-priority task, the lower-priority
/// task temporarily inherits the higher priority.
///
/// # Example
///
/// ```no_run
/// use freertos_in_rust::sync::{Mutex, TaskContext};
/// use freertos_in_rust::kernel::queue::StaticQueue_t;
///
/// let context = unsafe { TaskContext::assume() };
/// let control = Box::leak(Box::new(StaticQueue_t::new()));
/// let data = Mutex::new_static(&context, 42_u32, control).expect("mutex creation failed");
///
/// // Access the protected data
/// {
///     let mut guard = data.lock(&context).expect("mutex acquisition failed");
///     *guard += 1;
///     // mutex is held while guard is in scope
/// }
/// // mutex automatically released when guard is dropped
/// ```
///
/// # Priority Inheritance
///
/// Unlike binary semaphores, mutexes in FreeRTOS implement priority
/// inheritance. If you don't need the protected data pattern or priority
/// inheritance, consider using a `Semaphore` instead.
///
/// ```compile_fail
/// use freertos_in_rust::sync::Mutex;
/// fn no_context(mutex: &Mutex<u32>) {
///     let _ = mutex.try_lock(); // a `&TaskContext` capability is required
/// }
/// ```
pub struct Mutex<T> {
    handle: QueueHandle_t,
    data: UnsafeCell<T>,
    owns_handle: bool,
}

// Safety: Mutex<T> can be shared between tasks if T can be sent between tasks.
// The mutex ensures only one task accesses T at a time.
unsafe impl<T: Send> Sync for Mutex<T> {}
unsafe impl<T: Send> Send for Mutex<T> {}

impl<T> Mutex<T> {
    /// Creates a new mutex protecting the given value.
    ///
    /// Returns `None` if the mutex could not be created (e.g., out of memory).
    ///
    /// # Example
    ///
    /// ```no_run
    /// use freertos_in_rust::sync::{Mutex, TaskContext};
    ///
    /// let context = unsafe { TaskContext::assume() };
    /// let mutex = Mutex::new(&context, vec![1, 2, 3]).expect("Failed to create mutex");
    /// ```
    #[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
    pub fn new(_context: &TaskContext, value: T) -> Option<Self> {
        assert_task_context("Mutex::new");
        let handle = unsafe { xQueueCreateMutex(queueQUEUE_TYPE_MUTEX) };
        if handle.is_null() {
            None
        } else {
            Some(Self {
                handle,
                data: UnsafeCell::new(value),
                owns_handle: true,
            })
        }
    }

    /// Creates a mutex using static storage.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use freertos_in_rust::sync::{Mutex, TaskContext};
    /// use freertos_in_rust::kernel::queue::StaticQueue_t;
    ///
    /// let context = unsafe { TaskContext::assume() };
    /// let control = Box::leak(Box::new(StaticQueue_t::new()));
    /// let mutex = Mutex::new_static(&context, 42_u32, control).expect("mutex creation failed");
    /// ```
    pub fn new_static(
        _context: &TaskContext,
        value: T,
        mutex_buffer: &'static mut StaticQueue_t,
    ) -> Option<Self> {
        assert_task_context("Mutex::new_static");
        let handle = unsafe {
            xQueueCreateMutexStatic(queueQUEUE_TYPE_MUTEX, mutex_buffer as *mut StaticQueue_t)
        };
        if handle.is_null() {
            None
        } else {
            Some(Self {
                handle,
                data: UnsafeCell::new(value),
                owns_handle: true,
            })
        }
    }

    /// Acquires the mutex, blocking until it becomes available.
    ///
    /// Returns a guard that provides access to the protected data, or `None`
    /// if the kernel rejects the acquisition.
    /// The mutex is automatically released when the guard is dropped.
    ///
    /// # Panics
    ///
    /// Panics if called from interrupt context. FreeRTOS mutexes are strictly
    /// task-context primitives and must never be taken from an ISR.
    pub fn lock(&self, context: &TaskContext) -> Option<MutexGuard<'_, T>> {
        self.lock_timeout(context, portMAX_DELAY)
    }

    /// Attempts to acquire the mutex without blocking.
    ///
    /// Returns `Some(guard)` if the mutex was acquired, `None` if it's
    /// currently held by another task.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use freertos_in_rust::kernel::queue::StaticQueue_t;
    /// use freertos_in_rust::sync::{Mutex, TaskContext};
    /// # let control = Box::leak(Box::new(StaticQueue_t::new()));
    /// # let context = unsafe { TaskContext::assume() };
    /// # let mutex = Mutex::new_static(&context, 42_u32, control).unwrap();
    ///
    /// if let Some(guard) = mutex.try_lock(&context) {
    ///     // Got the mutex
    ///     println!("Value: {}", *guard);
    /// } else {
    ///     // Mutex is held by another task
    /// };
    /// ```
    pub fn try_lock(&self, context: &TaskContext) -> Option<MutexGuard<'_, T>> {
        self.lock_timeout(context, 0)
    }

    /// Attempts to acquire the mutex, blocking for up to `ticks` tick periods.
    ///
    /// Returns `Some(guard)` if the mutex was acquired within the timeout,
    /// `None` if the timeout expired.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use freertos_in_rust::kernel::queue::StaticQueue_t;
    /// use freertos_in_rust::sync::{Mutex, TaskContext};
    /// # let control = Box::leak(Box::new(StaticQueue_t::new()));
    /// # let context = unsafe { TaskContext::assume() };
    /// # let mutex = Mutex::new_static(&context, 42_u32, control).unwrap();
    ///
    /// // Wait up to 100 ticks for the mutex
    /// if let Some(mut guard) = mutex.lock_timeout(&context, 100) {
    ///     *guard += 1;
    /// } else {
    ///     // Timeout expired
    /// };
    /// ```
    pub fn lock_timeout(
        &self,
        _context: &TaskContext,
        ticks: TickType_t,
    ) -> Option<MutexGuard<'_, T>> {
        assert_can_block("Mutex::lock_timeout", ticks);
        let result = unsafe { xQueueSemaphoreTake(self.handle, ticks) };
        if result == pdTRUE {
            Some(MutexGuard {
                mutex: self,
                _not_send: core::marker::PhantomData,
            })
        } else {
            None
        }
    }

    /// Returns the raw FreeRTOS handle.
    ///
    /// # Safety
    ///
    /// This is provided for interoperability with raw FreeRTOS APIs.
    /// Using this handle directly can violate the safety guarantees
    /// of the Mutex wrapper.
    pub unsafe fn raw_handle(&self) -> QueueHandle_t {
        self.handle
    }

    /// Consumes the wrapper and returns its raw handle without deleting it.
    ///
    /// The caller becomes responsible for the handle's lifetime. The protected
    /// Rust value is dropped when this method returns; it is not stored in the
    /// FreeRTOS mutex object.
    pub fn into_raw(mut self) -> QueueHandle_t {
        self.owns_handle = false;
        self.handle
    }

    /// Explicitly deletes the underlying FreeRTOS mutex.
    ///
    /// Static mutex storage is not freed; FreeRTOS merely retires the handle.
    #[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
    pub fn delete(mut self, _context: &TaskContext) {
        assert_task_context("Mutex::delete");
        unsafe { vQueueDelete(self.handle) };
        self.owns_handle = false;
    }

    /// Releases the mutex (internal use by MutexGuard).
    fn release(&self) {
        let result =
            unsafe { xQueueGenericSend(self.handle, core::ptr::null(), 0, queueSEND_TO_BACK) };
        assert_eq!(result, pdTRUE, "FreeRTOS mutex release failed");
    }
}

#[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
impl<T> Drop for Mutex<T> {
    fn drop(&mut self) {
        // Queue deletion is not ISR-safe. A value dropped from an ISR is
        // retired without touching the kernel, preferring a leak to UB.
        if self.owns_handle && !is_in_isr() {
            unsafe { vQueueDelete(self.handle) };
            self.owns_handle = false;
        }
    }
}

/// RAII guard for a locked mutex.
///
/// When this guard is dropped, the mutex is automatically released.
/// The guard provides exclusive access to the data protected by the mutex.
///
/// This guard is intentionally `!Send` - it must be released on the same
/// task that acquired it for proper priority inheritance unwinding.
pub struct MutexGuard<'a, T> {
    mutex: &'a Mutex<T>,
    // Marker to make this type !Send (raw pointers aren't Send)
    _not_send: core::marker::PhantomData<*const ()>,
}

impl<T> Deref for MutexGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        // Safety: We hold the mutex, so we have exclusive access
        unsafe { &*self.mutex.data.get() }
    }
}

impl<T> DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // Safety: We hold the mutex, so we have exclusive access
        unsafe { &mut *self.mutex.data.get() }
    }
}

impl<T> Drop for MutexGuard<'_, T> {
    fn drop(&mut self) {
        self.mutex.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_acquisition_never_constructs_a_guard() {
        let storage = std::boxed::Box::leak(std::boxed::Box::new(StaticQueue_t::new()));
        let context = unsafe { TaskContext::assume() };
        let mutex =
            Mutex::new_static(&context, 7_u32, storage).expect("static mutex creation failed");

        let first = mutex
            .try_lock(&context)
            .expect("initial mutex acquisition failed");
        assert!(mutex.try_lock(&context).is_none());
        drop(first);

        assert!(mutex.try_lock(&context).is_some());
    }

    #[test]
    fn mutex_send_sync_tracks_protected_value_send() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Mutex<u32>>();
    }
}
