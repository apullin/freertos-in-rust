//! Safe Timer wrapper
//!
//! Provides a safe wrapper around FreeRTOS software timers.
//! Timers run callbacks in the timer daemon task context.

use crate::kernel::timers::{
    xTimerChangePeriod, xTimerCreate, xTimerGetExpiryTime, xTimerGetPeriod,
    xTimerGetTimerDaemonTaskHandle, xTimerIsTimerActive, xTimerReset, xTimerStart, xTimerStop,
    TimerCallbackFunction_t,
};
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
    #[cfg(feature = "timers")]
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
    #[cfg(feature = "timers")]
    pub fn new_oneshot(
        name: &[u8],
        period_ticks: TickType_t,
        callback: TimerCallbackFunction_t,
    ) -> Option<Self> {
        Self::new_internal(name, period_ticks, false, callback)
    }

    #[cfg(feature = "timers")]
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

    /// Returns the raw FreeRTOS handle for interop.
    pub unsafe fn raw_handle(&self) -> TimerHandle_t {
        self.handle
    }

    /// Returns a handle to the timer daemon task.
    ///
    /// Useful for checking if the current context is the timer daemon.
    #[cfg(feature = "timers")]
    pub fn daemon_task_handle() -> TaskHandle_t {
        xTimerGetTimerDaemonTaskHandle()
    }
}
