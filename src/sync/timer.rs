//! Safe Timer wrapper
//!
//! Provides a safe wrapper around FreeRTOS software timers.
//! Timers run callbacks in the timer daemon task context.

use core::ffi::{c_void, CStr};

use crate::kernel::tasks::{
    taskENTER_CRITICAL, taskEXIT_CRITICAL, taskSCHEDULER_RUNNING, xTaskGetSchedulerState,
};
#[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
use crate::kernel::timers::xTimerCreate;
use crate::kernel::timers::{
    pvTimerGetTimerID, vTimerSetTimerID, xTimerChangePeriod, xTimerChangePeriodFromISR,
    xTimerCreateStatic, xTimerDelete, xTimerGetExpiryTime, xTimerGetPeriod,
    xTimerGetTimerDaemonTaskHandle, xTimerIsTimerActive, xTimerReset, xTimerResetFromISR,
    xTimerStart, xTimerStartFromISR, xTimerStop, xTimerStopFromISR, StaticTimer_t,
    TimerCallbackFunction_t,
};
use crate::sync::task::TaskContext;
use crate::sync::{assert_can_block, assert_task_context, is_in_isr};
use crate::types::*;

/// Callback accepted by the safe timer constructors.
///
/// Unlike the raw kernel callback type, this is a safe function pointer: a
/// callback installed through the safe veneer cannot impose hidden caller
/// preconditions on the timer daemon.
pub type TimerCallback = extern "C" fn(TimerHandle_t);

/// A software timer that executes a callback after a specified period.
///
/// Timers can be one-shot (fire once) or auto-reload (periodic).
/// Callbacks run in the timer daemon task context, not in interrupt context.
/// They must not call APIs that can block: doing so stalls every software
/// timer and can deadlock the timer command queue.
///
/// # Example
///
/// ```no_run
/// use freertos_in_rust::sync::{TaskContext, Timer};
/// use freertos_in_rust::kernel::timers::StaticTimer_t;
/// use freertos_in_rust::types::TimerHandle_t;
///
/// extern "C" fn my_callback(_timer: TimerHandle_t) {
///     // Called every 1000 ticks; do not block here.
/// }
///
/// let context = unsafe { TaskContext::assume() };
/// let control = Box::leak(Box::new(StaticTimer_t::new()));
/// let timer = Timer::new_periodic_static(
///     &context,
///     c"MyTimer",
///     1000,  // period in ticks
///     my_callback,
///     control,
/// ).expect("Failed to create timer");
///
/// timer.start(&context);
/// ```
///
/// # Callback Context
///
/// Timer callbacks run in the timer daemon task, not ISR context.
/// This means:
/// - Never delay or wait for a queue, semaphore, mutex, event, or notification.
/// - Timer commands issued by a callback must use a zero command-queue timeout.
/// - Keep callbacks short because all software timers share this one task.
///
/// ```compile_fail
/// use freertos_in_rust::sync::Timer;
/// fn no_context(timer: &Timer) {
///     let _ = timer.start(); // a `&TaskContext` capability is required
/// }
/// ```
pub struct Timer {
    handle: TimerHandle_t,
    owns_handle: bool,
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
    /// * `name` - A static C string retained by FreeRTOS for the timer lifetime
    /// * `period_ticks` - Nonzero time between callbacks in ticks
    /// * `callback` - Function called when timer expires
    ///
    /// # Returns
    ///
    /// `Some(Timer)` on success, `None` if creation failed.
    #[cfg(all(
        feature = "timers",
        any(feature = "alloc", feature = "heap-4", feature = "heap-5")
    ))]
    pub fn new_periodic(
        context: &TaskContext,
        name: &'static CStr,
        period_ticks: TickType_t,
        callback: TimerCallback,
    ) -> Option<Self> {
        Self::new_internal(context, name, period_ticks, true, callback)
    }

    /// Creates a new one-shot timer.
    ///
    /// The callback will be called once after `period_ticks`, then the timer
    /// stops automatically. Call `start()` or `reset()` to fire again.
    ///
    /// # Arguments
    ///
    /// * `name` - A static C string retained by FreeRTOS for the timer lifetime
    /// * `period_ticks` - Nonzero delay before callback in ticks
    /// * `callback` - Function called when timer expires
    #[cfg(all(
        feature = "timers",
        any(feature = "alloc", feature = "heap-4", feature = "heap-5")
    ))]
    pub fn new_oneshot(
        context: &TaskContext,
        name: &'static CStr,
        period_ticks: TickType_t,
        callback: TimerCallback,
    ) -> Option<Self> {
        Self::new_internal(context, name, period_ticks, false, callback)
    }

    #[cfg(all(
        feature = "timers",
        any(feature = "alloc", feature = "heap-4", feature = "heap-5")
    ))]
    fn new_internal(
        _context: &TaskContext,
        name: &'static CStr,
        period_ticks: TickType_t,
        auto_reload: bool,
        callback: TimerCallback,
    ) -> Option<Self> {
        assert_task_context("Timer::new");
        Self::assert_nonzero_period("Timer::new", period_ticks);
        // Safety: `name` is a NUL-terminated static C string, the callback is
        // a stable function pointer, and the validated period is nonzero.
        let handle = unsafe {
            xTimerCreate(
                name.as_ptr().cast::<u8>(),
                period_ticks,
                if auto_reload { pdTRUE } else { pdFALSE },
                core::ptr::null_mut(), // pvTimerID - user can set later if needed
                callback as TimerCallbackFunction_t,
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

    // =========================================================================
    // Static Allocation
    // =========================================================================

    /// Creates a periodic timer using static storage.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use freertos_in_rust::kernel::timers::StaticTimer_t;
    /// use freertos_in_rust::sync::{TaskContext, Timer};
    /// use freertos_in_rust::types::TimerHandle_t;
    ///
    /// extern "C" fn callback(_timer: TimerHandle_t) { /* ... */ }
    ///
    /// let context = unsafe { TaskContext::assume() };
    /// let control = Box::leak(Box::new(StaticTimer_t::new()));
    /// let timer = Timer::new_periodic_static(
    ///     &context,
    ///     c"MyTimer",
    ///     1000,
    ///     callback,
    ///     control,
    /// ).expect("Failed to create timer");
    /// ```
    #[cfg(feature = "timers")]
    pub fn new_periodic_static(
        context: &TaskContext,
        name: &'static CStr,
        period_ticks: TickType_t,
        callback: TimerCallback,
        timer_buffer: &'static mut StaticTimer_t,
    ) -> Option<Self> {
        Self::new_static_internal(context, name, period_ticks, true, callback, timer_buffer)
    }

    /// Creates a one-shot timer using static storage.
    #[cfg(feature = "timers")]
    pub fn new_oneshot_static(
        context: &TaskContext,
        name: &'static CStr,
        period_ticks: TickType_t,
        callback: TimerCallback,
        timer_buffer: &'static mut StaticTimer_t,
    ) -> Option<Self> {
        Self::new_static_internal(context, name, period_ticks, false, callback, timer_buffer)
    }

    #[cfg(feature = "timers")]
    fn new_static_internal(
        _context: &TaskContext,
        name: &'static CStr,
        period_ticks: TickType_t,
        auto_reload: bool,
        callback: TimerCallback,
        timer_buffer: &'static mut StaticTimer_t,
    ) -> Option<Self> {
        assert_task_context("Timer::new_static");
        Self::assert_nonzero_period("Timer::new_static", period_ticks);
        // Safety: the name and buffer have static stable storage, the buffer
        // is exclusively borrowed, and the validated period is nonzero.
        let handle = unsafe {
            xTimerCreateStatic(
                name.as_ptr().cast::<u8>(),
                period_ticks,
                if auto_reload { pdTRUE } else { pdFALSE },
                core::ptr::null_mut(),
                callback as TimerCallbackFunction_t,
                timer_buffer as *mut StaticTimer_t,
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

    fn assert_nonzero_period(operation: &str, period_ticks: TickType_t) {
        assert!(
            period_ticks != 0,
            "{operation} requires a nonzero timer period"
        );
    }

    fn assert_command_context(_context: &TaskContext, operation: &str, ticks_to_wait: TickType_t) {
        assert_can_block(operation, ticks_to_wait);
    }

    /// Starts the timer.
    ///
    /// For periodic timers, the callback will fire every period.
    /// For one-shot timers, the callback will fire once after the period.
    ///
    /// Returns `true` if the start command was successfully sent to the
    /// timer daemon task.
    #[cfg(feature = "timers")]
    pub fn start(&self, context: &TaskContext) -> bool {
        self.start_timeout(context, 0)
    }

    /// Starts the timer with a timeout for sending to timer command queue.
    #[cfg(feature = "timers")]
    pub fn start_timeout(&self, context: &TaskContext, ticks: TickType_t) -> bool {
        Self::assert_command_context(context, "Timer::start_timeout", ticks);
        // Safety: `self` keeps its live handle valid until the daemon accepts
        // and later processes this command.
        unsafe { xTimerStart(self.handle, ticks) == pdPASS }
    }

    /// Stops the timer.
    ///
    /// The callback will not fire until the timer is started again.
    #[cfg(feature = "timers")]
    pub fn stop(&self, context: &TaskContext) -> bool {
        self.stop_timeout(context, 0)
    }

    /// Stops the timer with a timeout.
    #[cfg(feature = "timers")]
    pub fn stop_timeout(&self, context: &TaskContext, ticks: TickType_t) -> bool {
        Self::assert_command_context(context, "Timer::stop_timeout", ticks);
        // Safety: the borrowed wrapper owns a live timer handle.
        unsafe { xTimerStop(self.handle, ticks) == pdPASS }
    }

    /// Resets the timer, restarting its period from now.
    ///
    /// If the timer was stopped, this also starts it.
    /// If the timer was running, this restarts the period countdown.
    #[cfg(feature = "timers")]
    pub fn reset(&self, context: &TaskContext) -> bool {
        self.reset_timeout(context, 0)
    }

    /// Resets with a timeout.
    #[cfg(feature = "timers")]
    pub fn reset_timeout(&self, context: &TaskContext, ticks: TickType_t) -> bool {
        Self::assert_command_context(context, "Timer::reset_timeout", ticks);
        // Safety: the borrowed wrapper owns a live timer handle.
        unsafe { xTimerReset(self.handle, ticks) == pdPASS }
    }

    /// Changes the timer's period.
    ///
    /// Takes effect on the next timer expiry or reset.
    #[cfg(feature = "timers")]
    pub fn set_period(&self, context: &TaskContext, new_period_ticks: TickType_t) -> bool {
        self.set_period_timeout(context, new_period_ticks, 0)
    }

    /// Changes period with a timeout.
    #[cfg(feature = "timers")]
    pub fn set_period_timeout(
        &self,
        context: &TaskContext,
        new_period_ticks: TickType_t,
        ticks: TickType_t,
    ) -> bool {
        Self::assert_nonzero_period("Timer::set_period_timeout", new_period_ticks);
        Self::assert_command_context(context, "Timer::set_period_timeout", ticks);
        // Safety: the borrowed wrapper owns a live timer handle and the period
        // has been checked before reaching the raw command API.
        unsafe { xTimerChangePeriod(self.handle, new_period_ticks, ticks) == pdPASS }
    }

    /// Starts the timer from interrupt context.
    ///
    /// This method does not request a context switch. If it sets
    /// `higher_priority_task_woken` to `pdTRUE`, the ISR must yield using the
    /// active port's ISR-yield primitive before returning.
    ///
    /// # Safety
    ///
    /// The caller must be executing at an interrupt priority from which
    /// FreeRTOS `FromISR` APIs are permitted. The wake flag must be initialized
    /// to `pdFALSE` before the first operation in an ISR. `self` must remain
    /// live until the timer daemon consumes the queued command; do not delete
    /// or drop the final owner immediately after this call.
    pub unsafe fn start_from_isr(&self, higher_priority_task_woken: &mut BaseType_t) -> bool {
        // Safety: `self` owns a live handle, the mutable wake flag is valid,
        // and the caller upholds the ISR-priority contract.
        unsafe { xTimerStartFromISR(self.handle, higher_priority_task_woken) == pdPASS }
    }

    /// Stops the timer from interrupt context.
    ///
    /// # Safety
    ///
    /// The caller must obey the interrupt-priority and wake-flag requirements
    /// documented on [`Timer::start_from_isr`].
    pub unsafe fn stop_from_isr(&self, higher_priority_task_woken: &mut BaseType_t) -> bool {
        // Safety: `self` owns a live handle, the mutable wake flag is valid,
        // and the caller upholds the ISR-priority contract.
        unsafe { xTimerStopFromISR(self.handle, higher_priority_task_woken) == pdPASS }
    }

    /// Resets the timer from interrupt context.
    ///
    /// # Safety
    ///
    /// The caller must obey the interrupt-priority and wake-flag requirements
    /// documented on [`Timer::start_from_isr`].
    pub unsafe fn reset_from_isr(&self, higher_priority_task_woken: &mut BaseType_t) -> bool {
        // Safety: `self` owns a live handle, the mutable wake flag is valid,
        // and the caller upholds the ISR-priority contract.
        unsafe { xTimerResetFromISR(self.handle, higher_priority_task_woken) == pdPASS }
    }

    /// Changes the timer period from interrupt context.
    ///
    /// # Panics
    ///
    /// Panics if `new_period_ticks` is zero.
    ///
    /// # Safety
    ///
    /// The caller must obey the interrupt-priority and wake-flag requirements
    /// documented on [`Timer::start_from_isr`].
    pub unsafe fn set_period_from_isr(
        &self,
        new_period_ticks: TickType_t,
        higher_priority_task_woken: &mut BaseType_t,
    ) -> bool {
        Self::assert_nonzero_period("Timer::set_period_from_isr", new_period_ticks);
        // Safety: `self` owns a live handle, the mutable wake flag is valid,
        // the period is nonzero, and the caller upholds the ISR contract.
        unsafe {
            xTimerChangePeriodFromISR(self.handle, new_period_ticks, higher_priority_task_woken)
                == pdPASS
        }
    }

    /// Returns `true` if the timer is currently running.
    #[cfg(feature = "timers")]
    pub fn is_active(&self, _context: &TaskContext) -> bool {
        assert_task_context("Timer::is_active");
        // Safety: the borrowed wrapper owns a live timer handle.
        unsafe { xTimerIsTimerActive(self.handle) != pdFALSE }
    }

    /// Returns the timer's current period in ticks.
    #[cfg(feature = "timers")]
    pub fn period(&self, _context: &TaskContext) -> TickType_t {
        assert_task_context("Timer::period");
        // The daemon updates the period directly when it processes a change
        // command. Exclude preemption so this safe Sync wrapper never races
        // that non-atomic field access.
        taskENTER_CRITICAL();
        // Safety: the borrowed wrapper owns a live timer handle, and the
        // critical section prevents the daemon from mutating it concurrently.
        let period = unsafe { xTimerGetPeriod(self.handle) };
        taskEXIT_CRITICAL();
        period
    }

    /// Returns the tick count at which the timer will next expire.
    ///
    /// Only meaningful if the timer is active.
    #[cfg(feature = "timers")]
    pub fn expiry_time(&self, _context: &TaskContext) -> TickType_t {
        assert_task_context("Timer::expiry_time");
        // The daemon mutates the timer list item directly. Exclude preemption
        // around the raw getter for the same reason as `period` above.
        taskENTER_CRITICAL();
        // Safety: the borrowed wrapper owns a live timer handle, and the
        // critical section prevents the daemon from mutating it concurrently.
        let expiry_time = unsafe { xTimerGetExpiryTime(self.handle) };
        taskEXIT_CRITICAL();
        expiry_time
    }

    // =========================================================================
    // Timer ID (Context)
    // =========================================================================

    /// Sets the timer's ID to a shared static context value.
    ///
    /// The timer ID is typically used to pass context to the timer callback.
    /// This is useful when multiple timers share the same callback function.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use freertos_in_rust::sync::{TaskContext, Timer};
    /// use core::sync::atomic::AtomicU32;
    /// static COUNTER: AtomicU32 = AtomicU32::new(0);
    ///
    /// fn install_context(context: &TaskContext, timer: &Timer) {
    ///     timer.set_id(context, &COUNTER);
    /// }
    ///
    /// // A callback may recover the erased type with unsafe `get_id`.
    /// ```
    #[cfg(feature = "timers")]
    pub fn set_id<T: Sync + 'static>(&self, _context: &TaskContext, id: &'static T) {
        assert_task_context("Timer::set_id");
        // Safety: `self` owns a live timer and the stored pointer refers to a
        // synchronized value with static lifetime.
        unsafe { vTimerSetTimerID(self.handle, (id as *const T).cast_mut().cast::<c_void>()) };
    }

    /// Sets an arbitrary raw timer ID.
    ///
    /// # Safety
    ///
    /// Any non-null pointer must remain valid for every possible use by the
    /// timer callback. The caller must synchronize access to its pointee.
    #[cfg(feature = "timers")]
    pub unsafe fn set_id_raw(&self, _context: &TaskContext, id: *mut c_void) {
        assert_task_context("Timer::set_id_raw");
        // Safety: the wrapper provides the live handle; the caller guarantees
        // the raw ID's validity as documented above.
        unsafe { vTimerSetTimerID(self.handle, id) };
    }

    /// Gets the timer's ID as a typed pointer.
    ///
    /// # Safety
    ///
    /// `T` must be the exact pointee type stored in the erased timer ID. The
    /// caller must also uphold the pointee's lifetime and synchronization
    /// requirements before dereferencing the returned pointer. An ID installed
    /// with [`Timer::set_id`] came from a shared reference: it may only be read
    /// through a shared reference and must never be written through this raw
    /// pointer or used to construct `&mut T`.
    #[cfg(feature = "timers")]
    pub unsafe fn get_id<T>(&self, context: &TaskContext) -> Option<*mut T> {
        let id = self.get_id_raw(context);
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
    pub fn get_id_raw(&self, _context: &TaskContext) -> *mut c_void {
        assert_task_context("Timer::get_id_raw");
        // Safety: the borrowed wrapper owns a live timer handle. Returning the
        // opaque pointer does not dereference its pointee.
        unsafe { pvTimerGetTimerID(self.handle) }
    }

    // =========================================================================
    // Interop
    // =========================================================================

    /// Returns the raw FreeRTOS handle for interop.
    ///
    /// # Safety
    ///
    /// The caller must not let the handle outlive this owner, independently
    /// delete the timer, or use raw commands in a way that violates the
    /// wrapper's timer-ID, daemon-context, and deferred-command contracts.
    pub unsafe fn raw_handle(&self) -> TimerHandle_t {
        self.handle
    }

    /// Consumes the wrapper and returns the raw handle without deleting it.
    pub fn into_raw(mut self) -> TimerHandle_t {
        self.owns_handle = false;
        self.handle
    }

    /// Creates a Timer from a raw FreeRTOS handle.
    ///
    /// # Safety
    ///
    /// The handle must be a valid live FreeRTOS timer handle for the entire
    /// lifetime of the returned wrapper. The wrapper borrows the raw timer: it
    /// does not enqueue a delete command when dropped. This is useful in timer
    /// callbacks for temporary access to the timer ID.
    pub unsafe fn from_raw(handle: TimerHandle_t) -> Self {
        assert!(
            !handle.is_null(),
            "Timer::from_raw requires a non-null handle"
        );
        Self {
            handle,
            owns_handle: false,
        }
    }

    /// Enqueues deletion of this timer.
    ///
    /// A successful return means the daemon accepted the delete command; the
    /// timer storage is actually retired later when that command is processed.
    /// On queue failure, ownership is returned so the caller can retry.
    pub fn delete(mut self, context: &TaskContext, ticks: TickType_t) -> Result<(), Self> {
        Self::assert_command_context(context, "Timer::delete", ticks);
        // Safety: consuming `self` keeps the live handle valid until the
        // daemon accepts and processes the delete command.
        if unsafe { xTimerDelete(self.handle, ticks) } == pdPASS {
            self.owns_handle = false;
            Ok(())
        } else {
            Err(self)
        }
    }

    /// Returns a handle to the running timer daemon task.
    ///
    /// Returns `None` before the scheduler and timer daemon are running.
    #[cfg(feature = "timers")]
    pub fn daemon_task_handle(_context: &TaskContext) -> Option<TaskHandle_t> {
        assert_task_context("Timer::daemon_task_handle");
        if xTaskGetSchedulerState() == taskSCHEDULER_RUNNING {
            Some(xTimerGetTimerDaemonTaskHandle())
        } else {
            None
        }
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        // FreeRTOS has no delete-from-ISR API. A zero-timeout command is safe
        // in normal task and timer-callback contexts; if it cannot be queued,
        // leaking is the only safe fallback during destruction.
        if self.owns_handle && !is_in_isr() {
            // Safety: Drop has exclusive ownership of the live handle and
            // uses the only nonblocking task-context deletion path.
            let _ = unsafe { xTimerDelete(self.handle, 0) };
            self.owns_handle = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic(expected = "requires a nonzero timer period")]
    fn zero_timer_period_is_rejected_before_kernel_call() {
        Timer::assert_nonzero_period("test", 0);
    }

    #[test]
    fn timer_handle_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Timer>();
    }

    #[cfg(feature = "port-test")]
    #[test]
    fn direct_timer_queries_are_preemption_safe() {
        extern "C" fn callback(_timer: TimerHandle_t) {}

        crate::port::test_port_reset();
        // Safety: this single-threaded test models pre-scheduler setup.
        let context = unsafe { TaskContext::assume() };
        let control = std::boxed::Box::leak(std::boxed::Box::new(StaticTimer_t::new()));
        let timer = Timer::new_periodic_static(&context, c"query", 7, callback, control)
            .expect("static timer creation");
        let before = crate::port::test_port_snapshot();

        assert_eq!(timer.period(&context), 7);
        assert_eq!(timer.expiry_time(&context), 0);

        let after = crate::port::test_port_snapshot();
        assert_eq!(after.critical_nesting, 0);
        assert!(!after.interrupts_masked);
        assert_eq!(
            after.interrupt_disable_count,
            before.interrupt_disable_count + 2
        );
        assert_eq!(
            after.interrupt_enable_count,
            before.interrupt_enable_count + 2
        );
    }
}
