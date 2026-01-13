//! Safe Mutex wrapper with RAII guard pattern
//!
//! This provides a Rust-idiomatic mutex that wraps FreeRTOS's mutex
//! with priority inheritance. The mutex protects data of type `T`
//! and ensures exclusive access through the guard pattern.

use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};

use crate::kernel::queue::{
    queueQUEUE_TYPE_MUTEX, xQueueCreateMutex, xQueueGenericSend, xQueueSemaphoreTake,
    QueueHandle_t, queueSEND_TO_BACK,
};
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
/// ```ignore
/// use freertos_in_rust::sync::Mutex;
///
/// let data: Mutex<u32> = Mutex::new(42).expect("Failed to create mutex");
///
/// // Access the protected data
/// {
///     let mut guard = data.lock();
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
pub struct Mutex<T> {
    handle: QueueHandle_t,
    data: UnsafeCell<T>,
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
    /// ```ignore
    /// let mutex = Mutex::new(vec![1, 2, 3]).expect("Failed to create mutex");
    /// ```
    #[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
    pub fn new(value: T) -> Option<Self> {
        let handle = unsafe { xQueueCreateMutex(queueQUEUE_TYPE_MUTEX) };
        if handle.is_null() {
            None
        } else {
            Some(Self {
                handle,
                data: UnsafeCell::new(value),
            })
        }
    }

    /// Acquires the mutex, blocking until it becomes available.
    ///
    /// Returns a guard that provides access to the protected data.
    /// The mutex is automatically released when the guard is dropped.
    ///
    /// # Panics
    ///
    /// This function will never panic under normal operation. The underlying
    /// FreeRTOS call blocks indefinitely until the mutex is available.
    pub fn lock(&self) -> MutexGuard<'_, T> {
        unsafe {
            // Block forever waiting for mutex
            let result = xQueueSemaphoreTake(self.handle, portMAX_DELAY);
            debug_assert_eq!(result, pdTRUE, "Mutex take with infinite timeout failed");
        }
        MutexGuard { mutex: self, _not_send: core::marker::PhantomData }
    }

    /// Attempts to acquire the mutex without blocking.
    ///
    /// Returns `Some(guard)` if the mutex was acquired, `None` if it's
    /// currently held by another task.
    ///
    /// # Example
    ///
    /// ```ignore
    /// if let Some(guard) = mutex.try_lock() {
    ///     // Got the mutex
    ///     println!("Value: {}", *guard);
    /// } else {
    ///     // Mutex is held by another task
    /// }
    /// ```
    pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
        let result = unsafe { xQueueSemaphoreTake(self.handle, 0) };
        if result == pdTRUE {
            Some(MutexGuard { mutex: self, _not_send: core::marker::PhantomData })
        } else {
            None
        }
    }

    /// Attempts to acquire the mutex, blocking for up to `ticks` tick periods.
    ///
    /// Returns `Some(guard)` if the mutex was acquired within the timeout,
    /// `None` if the timeout expired.
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Wait up to 100 ticks for the mutex
    /// if let Some(guard) = mutex.lock_timeout(100) {
    ///     *guard += 1;
    /// } else {
    ///     // Timeout expired
    /// }
    /// ```
    pub fn lock_timeout(&self, ticks: TickType_t) -> Option<MutexGuard<'_, T>> {
        let result = unsafe { xQueueSemaphoreTake(self.handle, ticks) };
        if result == pdTRUE {
            Some(MutexGuard { mutex: self, _not_send: core::marker::PhantomData })
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

    /// Releases the mutex (internal use by MutexGuard).
    fn release(&self) {
        unsafe {
            xQueueGenericSend(self.handle, core::ptr::null(), 0, queueSEND_TO_BACK);
        }
    }
}

// Note: We don't implement Drop for Mutex because:
// 1. FreeRTOS static mutexes shouldn't be deleted
// 2. Dynamic deletion (vQueueDelete) requires knowing if tasks are waiting
// 3. The mutex handle typically lives for the program's lifetime
//
// If deletion is needed, users can call the raw vQueueDelete API.

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
