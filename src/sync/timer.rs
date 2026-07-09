//! Safe Timer wrapper
//!
//! Provides a safe wrapper around FreeRTOS software timers.
//! Timers run callbacks in the timer daemon task context.

use core::ffi::c_void;

use crate::kernel::timers::{
    pvTimerGetTimerID, vTimerSetTimerID, xTimerChangePeriod, xTimerCreateStatic,
    xTimerGetExpiryTime, xTimerGetPeriod, xTimerGetTimerDaemonTaskHandle, xTimerIsTimerActive,
    xTimerReset, xTimerStart, xTimerStop, StaticTimer_t, TimerCallbackFunction_t,
};
#[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
use crate::kernel::timers::xTimerCreate;
use crate::types::*;

/// A software timer that executes a callback after a specified period.
///
/// Timers can be one-shot (fire once) or auto-reload (periodic).
/// Callbacks run in the timer daemon task context, not in interrupt context,
/// so they can use blocking FreeRTOS APIs.
///
/// # Example
///
/// ```ignore
/// use freertos_in_rust::sync::Timer;
///
/// extern "C" fn my_callback(_timer: TimerHandle_t) {
///     // Called every 1000 ticks
///     println!("Timer fired!");
/// }
///
/// let timer = Timer::new_periodic(
///     b"MyTimer\0",
///     1000,  // period in ticks
///     my_callback,
/// ).expect("Failed to create timer");
///
/// timer.start();
/// ```
///
/// # Callback Context
///
/// Timer callbacks run in the timer daemon task, not ISR context.
/// This means:
/// - You CAN use blocking APIs (vTaskDelay, queue send with timeout, etc.)
/// - You SHOULD keep callbacks short to avoid blocking other timers
/// - You SHOULD NOT perform heavy computation in callbacks
pub struct Timer {
    handle: TimerHandle_t,
}

// Safety: Timer handles can be used from any task
unsafe impl Sync for Timer {}
unsafe impl Send for Timer {}

impl Timer {
    /// Creates a new periodic (auto-reload) timer.
    ///
    /// The callback will be called every `period_ticks` ticks until stopped.
    ///
    /// # Arguments
    ///
    /// * `name` - Null-terminated name for debugging (e.g., `b"MyTimer\0"`)
    /// * `period_ticks` - Time between callbacks in ticks
    /// * `callback` - Function called when timer expires
    ///
    /// # Returns
    ///
    /// `Some(Timer)` on success, `None` if creation failed.
    #[cfg(all(feature = "timers", any(feature = "alloc", feature = "heap-4", feature = "heap-5")))]
    pub fn new_periodic(
        name: &[u8],
        period_ticks: TickType_t,
        callback: TimerCallbackFunction_t,
    ) -> Option<Self> {
        Self::new_internal(name, period_ticks, true, callback)
    }

    /// Creates a new one-shot timer.
    ///
    /// The callback will be called once after `period_ticks`, then the timer
    /// stops automatically. Call `start()` or `reset()` to fire again.
    ///
    /// # Arguments
    ///
    /// * `name` - Null-terminated name for debugging
    /// * `period_ticks` - Delay before callback in ticks
    /// * `callback` - Function called when timer expires
    #[cfg(all(feature = "timers", any(feature = "alloc", feature = "heap-4", feature = "heap-5")))]
    pub fn new_oneshot(
        name: &[u8],
        period_ticks: TickType_t,
        callback: TimerCallbackFunction_t,
    ) -> Option<Self> {
        Self::new_internal(name, period_ticks, false, callback)
    }

    #[cfg(all(feature = "timers", any(feature = "alloc", feature = "heap-4", feature = "heap-5")))]
    fn new_internal(
        name: &[u8],
        period_ticks: TickType_t,
        auto_reload: bool,
        callback: TimerCallbackFunction_t,
    ) -> Option<Self> {
        let handle = xTimerCreate(
            name.as_ptr(),
            period_ticks,
            if auto_reload { pdTRUE } else { pdFALSE },
            core::ptr::null_mut(), // pvTimerID - user can set later if needed
            callback,
        );

        if handle.is_null() {
            None
        } else {
            Some(Self { handle })
        }
    }

    // =========================================================================
    // Static Allocation
    // =========================================================================

    /// Creates a periodic timer using static storage.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use freertos_in_rust::sync::Timer;
    /// use freertos_in_rust::kernel::timers::StaticTimer_t;
    ///
    /// static mut TIMER_BUF: StaticTimer_t = StaticTimer_t::new();
    ///
    /// extern "C" fn callback(_timer: TimerHandle_t) { /* ... */ }
    ///
    /// let timer = Timer::new_periodic_static(
    ///     b"MyTimer\0",
    ///     1000,
    ///     callback,
    ///     unsafe { &mut TIMER_BUF },
    /// ).expect("Failed to create timer");
    /// ```
    #[cfg(feature = "timers")]
    pub fn new_periodic_static(
        name: &[u8],
        period_ticks: TickType_t,
        callback: TimerCallbackFunction_t,
        timer_buffer: &'static mut StaticTimer_t,
    ) -> Option<Self> {
        Self::new_static_internal(name, period_ticks, true, callback, timer_buffer)
    }

    /// Creates a one-shot timer using static storage.
    #[cfg(feature = "timers")]
    pub fn new_oneshot_static(
        name: &[u8],
        period_ticks: TickType_t,
        callback: TimerCallbackFunction_t,
        timer_buffer: &'static mut StaticTimer_t,
    ) -> Option<Self> {
        Self::new_static_internal(name, period_ticks, false, callback, timer_buffer)
    }

    #[cfg(feature = "timers")]
    fn new_static_internal(
        name: &[u8],
        period_ticks: TickType_t,
        auto_reload: bool,
        callback: TimerCallbackFunction_t,
        timer_buffer: &'static mut StaticTimer_t,
    ) -> Option<Self> {
        let handle = xTimerCreateStatic(
            name.as_ptr(),
            period_ticks,
            if auto_reload { pdTRUE } else { pdFALSE },
            core::ptr::null_mut(),
            callback,
            timer_buffer as *mut StaticTimer_t,
        );

        if handle.is_null() {
            None
        } else {
            Some(Self { handle })
        }
    }

    /// Starts the timer.
    ///
    /// For periodic timers, the callback will fire every period.
    /// For one-shot timers, the callback will fire once after the period.
    ///
    /// Returns `true` if the start command was successfully sent to the
    /// timer daemon task.
    #[cfg(feature = "timers")]
    pub fn start(&self) -> bool {
        xTimerStart(self.handle, 0) == pdPASS
    }

    /// Starts the timer with a timeout for sending to timer command queue.
    #[cfg(feature = "timers")]
    pub fn start_timeout(&self, ticks: TickType_t) -> bool {
        xTimerStart(self.handle, ticks) == pdPASS
    }

    /// Stops the timer.
    ///
    /// The callback will not fire until the timer is started again.
    #[cfg(feature = "timers")]
    pub fn stop(&self) -> bool {
        xTimerStop(self.handle, 0) == pdPASS
    }

    /// Stops the timer with a timeout.
    #[cfg(feature = "timers")]
    pub fn stop_timeout(&self, ticks: TickType_t) -> bool {
        xTimerStop(self.handle, ticks) == pdPASS
    }

    /// Resets the timer, restarting its period from now.
    ///
    /// If the timer was stopped, this also starts it.
    /// If the timer was running, this restarts the period countdown.
    #[cfg(feature = "timers")]
    pub fn reset(&self) -> bool {
        xTimerReset(self.handle, 0) == pdPASS
    }

    /// Resets with a timeout.
    #[cfg(feature = "timers")]
    pub fn reset_timeout(&self, ticks: TickType_t) -> bool {
        xTimerReset(self.handle, ticks) == pdPASS
    }

    /// Changes the timer's period.
    ///
    /// Takes effect on the next timer expiry or reset.
    #[cfg(feature = "timers")]
    pub fn set_period(&self, new_period_ticks: TickType_t) -> bool {
        xTimerChangePeriod(self.handle, new_period_ticks, 0) == pdPASS
    }

    /// Changes period with a timeout.
    #[cfg(feature = "timers")]
    pub fn set_period_timeout(&self, new_period_ticks: TickType_t, ticks: TickType_t) -> bool {
        xTimerChangePeriod(self.handle, new_period_ticks, ticks) == pdPASS
    }

    /// Returns `true` if the timer is currently running.
    #[cfg(feature = "timers")]
    pub fn is_active(&self) -> bool {
        xTimerIsTimerActive(self.handle) != pdFALSE
    }

    /// Returns the timer's current period in ticks.
    #[cfg(feature = "timers")]
    pub fn period(&self) -> TickType_t {
        xTimerGetPeriod(self.handle)
    }

    /// Returns the tick count at which the timer will next expire.
    ///
    /// Only meaningful if the timer is active.
    #[cfg(feature = "timers")]
    pub fn expiry_time(&self) -> TickType_t {
        xTimerGetExpiryTime(self.handle)
    }

    // =========================================================================
    // Timer ID (Context)
    // =========================================================================

    /// Sets the timer's ID to a typed pointer.
    ///
    /// The timer ID is typically used to pass context to the timer callback.
    /// This is useful when multiple timers share the same callback function.
    ///
    /// # Safety Note
    ///
    /// The pointed-to data must outlive the timer. The timer stores a raw
    /// pointer, so Rust's lifetime system cannot enforce this.
    ///
    /// # Example
    ///
    /// ```ignore
    /// struct TimerContext { counter: u32 }
    /// static mut CTX: TimerContext = TimerContext { counter: 0 };
    ///
    /// // In setup:
    /// timer.set_id(unsafe { &mut CTX });
    ///
    /// // In callback:
    /// extern "C" fn callback(timer_handle: TimerHandle_t) {
    ///     let timer = unsafe { Timer::from_raw(timer_handle) };
    ///     if let Some(ctx) = timer.get_id::<TimerContext>() {
    ///         unsafe { (*ctx).counter += 1; }
    ///     }
    /// }
    /// ```
    #[cfg(feature = "timers")]
    pub fn set_id<T>(&self, id: *mut T) {
        vTimerSetTimerID(self.handle, id as *mut c_void);
    }

    /// Gets the timer's ID as a typed pointer.
    ///
    /// Returns `None` if the ID was never set (is null).
    /// Returns `Some(ptr)` otherwise - caller must ensure the type matches
    /// what was set.
    #[cfg(feature = "timers")]
    pub fn get_id<T>(&self) -> Option<*mut T> {
        let id = pvTimerGetTimerID(self.handle);
        if id.is_null() {
            None
        } else {
            Some(id as *mut T)
        }
    }

    /// Gets the timer's raw ID pointer.
    ///
    /// This is the low-level version that returns the raw `*mut c_void`.
    #[cfg(feature = "timers")]
    pub fn get_id_raw(&self) -> *mut c_void {
        pvTimerGetTimerID(self.handle)
    }

    // =========================================================================
    // Interop
    // =========================================================================

    /// Returns the raw FreeRTOS handle for interop.
    pub unsafe fn raw_handle(&self) -> TimerHandle_t {
        self.handle
    }

    /// Creates a Timer from a raw FreeRTOS handle.
    ///
    /// # Safety
    ///
    /// The handle must be a valid FreeRTOS timer handle.
    /// This is useful in timer callbacks to access the Timer wrapper.
    pub unsafe fn from_raw(handle: TimerHandle_t) -> Self {
        Self { handle }
    }

    /// Returns a handle to the timer daemon task.
    ///
    /// Useful for checking if the current context is the timer daemon.
    #[cfg(feature = "timers")]
    pub fn daemon_task_handle() -> TaskHandle_t {
        xTimerGetTimerDaemonTaskHandle()
    }
}
