//! Safe EventGroup wrapper
//!
//! Provides a safe wrapper around FreeRTOS event groups.
//! Event groups allow tasks to wait on combinations of event bits.

#[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
use crate::kernel::event_groups::xEventGroupCreate;
use crate::kernel::event_groups::{
    eventEVENT_BITS_CONTROL_BYTES, vEventGroupDelete, xEventGroupClearBits,
    xEventGroupCreateStatic, xEventGroupGetBits, xEventGroupGetBitsFromISR, xEventGroupSetBits,
    xEventGroupSync, xEventGroupWaitBits, EventBits_t, StaticEventGroup_t,
};
#[cfg(all(feature = "pend-function-call", feature = "timers"))]
use crate::kernel::event_groups::{xEventGroupClearBitsFromISR, xEventGroupSetBitsFromISR};
use crate::sync::task::TaskContext;
use crate::sync::{assert_can_block, assert_task_context, is_in_isr};
use crate::types::*;

/// An event group for synchronizing tasks using bit flags.
///
/// Event groups contain a set of bits that tasks can wait on. Tasks can:
/// - Set bits to signal events
/// - Clear bits to consume events
/// - Wait for any or all of a set of bits to be set
/// - Synchronize (rendezvous) with other tasks
///
/// # Bit Availability
///
/// With 32-bit ticks: 24 usable bits (bits 0-23)
/// With 64-bit ticks: 56 usable bits (bits 0-55)
/// The upper bits are reserved for internal control flags.
///
/// ```compile_fail
/// use freertos_in_rust::sync::EventGroup;
/// fn no_context(events: &EventGroup) {
///     let _ = events.get(); // a `&TaskContext` capability is required
/// }
/// ```
pub struct EventGroup {
    handle: EventGroupHandle_t,
    owns_handle: bool,
}

// Safety: EventGroup can be shared between tasks - that's its purpose
unsafe impl Sync for EventGroup {}
unsafe impl Send for EventGroup {}

impl EventGroup {
    /// Creates a new event group with all bits cleared.
    ///
    /// Returns `None` if creation failed (e.g., out of memory).
    #[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
    pub fn new(_context: &TaskContext) -> Option<Self> {
        assert_task_context("EventGroup::new");
        // Safety: TaskContext and the check above establish task context; the
        // owning wrapper serializes deletion and does not reset the allocator.
        let handle = unsafe { xEventGroupCreate() };
        if handle.is_null() {
            None
        } else {
            Some(Self {
                handle,
                owns_handle: true,
            })
        }
    }

    /// Creates an event group using static storage.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use freertos_in_rust::sync::{EventGroup, TaskContext};
    /// use freertos_in_rust::kernel::event_groups::StaticEventGroup_t;
    ///
    /// let context = unsafe { TaskContext::assume() };
    /// let storage = Box::leak(Box::new(StaticEventGroup_t::new()));
    /// let events = EventGroup::new_static(&context, storage).expect("creation failed");
    /// ```
    pub fn new_static(
        _context: &TaskContext,
        event_group_buffer: &'static mut StaticEventGroup_t,
    ) -> Option<Self> {
        assert_task_context("EventGroup::new_static");
        // Safety: the exclusive static borrow supplies valid, suitably aligned
        // storage that cannot be reused while the wrapper is alive.
        let handle =
            unsafe { xEventGroupCreateStatic(event_group_buffer as *mut StaticEventGroup_t) };
        if handle.is_null() {
            None
        } else {
            Some(Self {
                handle,
                owns_handle: true,
            })
        }
    }

    fn valid_bits(bits: EventBits_t) -> bool {
        bits != 0 && (bits & eventEVENT_BITS_CONTROL_BYTES) == 0
    }

    fn assert_valid_bits(operation: &str, bits: EventBits_t) {
        assert!(
            Self::valid_bits(bits),
            "{operation} requires at least one user event bit and forbids FreeRTOS control bits"
        );
    }

    // =========================================================================
    // Set/Clear/Get
    // =========================================================================

    /// Sets bits in the event group.
    ///
    /// Any tasks waiting for these bits may be unblocked.
    ///
    /// Returns the bits value after setting (other bits may have been
    /// cleared by unblocked tasks).
    pub fn set(&self, _context: &TaskContext, bits: EventBits_t) -> EventBits_t {
        assert_task_context("EventGroup::set");
        Self::assert_valid_bits("EventGroup::set", bits);
        // Safety: constructors establish a live non-null handle and `&self`
        // keeps the owning wrapper alive for the duration of this call.
        unsafe { xEventGroupSetBits(self.handle, bits) }
    }

    /// Clears bits in the event group.
    ///
    /// Returns the bits value *before* clearing.
    pub fn clear(&self, _context: &TaskContext, bits: EventBits_t) -> EventBits_t {
        assert_task_context("EventGroup::clear");
        Self::assert_valid_bits("EventGroup::clear", bits);
        // Safety: the borrowed wrapper owns a live event-group handle.
        unsafe { xEventGroupClearBits(self.handle, bits) }
    }

    /// Gets the current bits value.
    pub fn get(&self, _context: &TaskContext) -> EventBits_t {
        assert_task_context("EventGroup::get");
        // Safety: the borrowed wrapper owns a live event-group handle.
        unsafe { xEventGroupGetBits(self.handle) }
    }

    // =========================================================================
    // Wait for ANY bit (OR condition)
    // =========================================================================

    /// Waits for ANY of the specified bits to be set, blocking indefinitely.
    ///
    /// Does not clear bits on exit. Use `wait_any_clear` to auto-clear.
    ///
    /// Returns the bits value when unblocked.
    pub fn wait_any(&self, _context: &TaskContext, bits: EventBits_t) -> EventBits_t {
        assert_can_block("EventGroup::wait_any", portMAX_DELAY);
        Self::assert_valid_bits("EventGroup::wait_any", bits);
        // Safety: the borrowed wrapper owns a live event-group handle.
        unsafe { xEventGroupWaitBits(self.handle, bits, pdFALSE, pdFALSE, portMAX_DELAY) }
    }

    /// Waits for ANY of the specified bits with a timeout.
    ///
    /// Returns `Some(bits)` if any bit was set, `None` on timeout.
    pub fn wait_any_timeout(
        &self,
        _context: &TaskContext,
        bits: EventBits_t,
        ticks: TickType_t,
    ) -> Option<EventBits_t> {
        assert_can_block("EventGroup::wait_any_timeout", ticks);
        Self::assert_valid_bits("EventGroup::wait_any_timeout", bits);
        // Safety: the borrowed wrapper owns a live event-group handle.
        let result = unsafe { xEventGroupWaitBits(self.handle, bits, pdFALSE, pdFALSE, ticks) };
        if (result & bits) != 0 {
            Some(result)
        } else {
            None
        }
    }

    /// Attempts to check if ANY of the specified bits are set without blocking.
    ///
    /// Returns `Some(bits)` if any bit is set, `None` otherwise.
    pub fn try_wait_any(&self, context: &TaskContext, bits: EventBits_t) -> Option<EventBits_t> {
        self.wait_any_timeout(context, bits, 0)
    }

    /// Waits for ANY bit and clears the matched bits on exit.
    ///
    /// This is the common "consume event" pattern.
    pub fn wait_any_clear(&self, _context: &TaskContext, bits: EventBits_t) -> EventBits_t {
        assert_can_block("EventGroup::wait_any_clear", portMAX_DELAY);
        Self::assert_valid_bits("EventGroup::wait_any_clear", bits);
        // Safety: the borrowed wrapper owns a live event-group handle.
        unsafe { xEventGroupWaitBits(self.handle, bits, pdTRUE, pdFALSE, portMAX_DELAY) }
    }

    /// Waits for ANY bit with timeout, clearing matched bits on success.
    pub fn wait_any_clear_timeout(
        &self,
        _context: &TaskContext,
        bits: EventBits_t,
        ticks: TickType_t,
    ) -> Option<EventBits_t> {
        assert_can_block("EventGroup::wait_any_clear_timeout", ticks);
        Self::assert_valid_bits("EventGroup::wait_any_clear_timeout", bits);
        // Safety: the borrowed wrapper owns a live event-group handle.
        let result = unsafe { xEventGroupWaitBits(self.handle, bits, pdTRUE, pdFALSE, ticks) };
        if (result & bits) != 0 {
            Some(result)
        } else {
            None
        }
    }

    // =========================================================================
    // Wait for ALL bits (AND condition)
    // =========================================================================

    /// Waits for ALL of the specified bits to be set, blocking indefinitely.
    ///
    /// Does not clear bits on exit.
    pub fn wait_all(&self, _context: &TaskContext, bits: EventBits_t) -> EventBits_t {
        assert_can_block("EventGroup::wait_all", portMAX_DELAY);
        Self::assert_valid_bits("EventGroup::wait_all", bits);
        // Safety: the borrowed wrapper owns a live event-group handle.
        unsafe { xEventGroupWaitBits(self.handle, bits, pdFALSE, pdTRUE, portMAX_DELAY) }
    }

    /// Waits for ALL of the specified bits with a timeout.
    ///
    /// Returns `Some(bits)` if all bits were set, `None` on timeout.
    pub fn wait_all_timeout(
        &self,
        _context: &TaskContext,
        bits: EventBits_t,
        ticks: TickType_t,
    ) -> Option<EventBits_t> {
        assert_can_block("EventGroup::wait_all_timeout", ticks);
        Self::assert_valid_bits("EventGroup::wait_all_timeout", bits);
        // Safety: the borrowed wrapper owns a live event-group handle.
        let result = unsafe { xEventGroupWaitBits(self.handle, bits, pdFALSE, pdTRUE, ticks) };
        if (result & bits) == bits {
            Some(result)
        } else {
            None
        }
    }

    /// Attempts to check if ALL of the specified bits are set without blocking.
    pub fn try_wait_all(&self, context: &TaskContext, bits: EventBits_t) -> Option<EventBits_t> {
        self.wait_all_timeout(context, bits, 0)
    }

    /// Waits for ALL bits and clears them on exit.
    pub fn wait_all_clear(&self, _context: &TaskContext, bits: EventBits_t) -> EventBits_t {
        assert_can_block("EventGroup::wait_all_clear", portMAX_DELAY);
        Self::assert_valid_bits("EventGroup::wait_all_clear", bits);
        // Safety: the borrowed wrapper owns a live event-group handle.
        unsafe { xEventGroupWaitBits(self.handle, bits, pdTRUE, pdTRUE, portMAX_DELAY) }
    }

    /// Waits for ALL bits with timeout, clearing them on success.
    pub fn wait_all_clear_timeout(
        &self,
        _context: &TaskContext,
        bits: EventBits_t,
        ticks: TickType_t,
    ) -> Option<EventBits_t> {
        assert_can_block("EventGroup::wait_all_clear_timeout", ticks);
        Self::assert_valid_bits("EventGroup::wait_all_clear_timeout", bits);
        // Safety: the borrowed wrapper owns a live event-group handle.
        let result = unsafe { xEventGroupWaitBits(self.handle, bits, pdTRUE, pdTRUE, ticks) };
        if (result & bits) == bits {
            Some(result)
        } else {
            None
        }
    }

    // =========================================================================
    // Sync (Rendezvous)
    // =========================================================================

    /// Synchronization point (rendezvous) for multiple tasks.
    ///
    /// Sets `bits_to_set`, then waits for ALL `bits_to_wait` to be set.
    /// Clears `bits_to_wait` when all participating tasks have arrived.
    ///
    /// This enables barrier-style synchronization where N tasks each set
    /// their own bit and wait for all N bits.
    pub fn sync(
        &self,
        _context: &TaskContext,
        bits_to_set: EventBits_t,
        bits_to_wait: EventBits_t,
    ) -> EventBits_t {
        assert_can_block("EventGroup::sync", portMAX_DELAY);
        Self::assert_valid_bits("EventGroup::sync bits_to_set", bits_to_set);
        Self::assert_valid_bits("EventGroup::sync bits_to_wait", bits_to_wait);
        // Safety: the borrowed wrapper owns a live event-group handle.
        unsafe { xEventGroupSync(self.handle, bits_to_set, bits_to_wait, portMAX_DELAY) }
    }

    /// Sync with timeout.
    ///
    /// Returns `Some(bits)` if sync completed, `None` on timeout.
    pub fn sync_timeout(
        &self,
        _context: &TaskContext,
        bits_to_set: EventBits_t,
        bits_to_wait: EventBits_t,
        ticks: TickType_t,
    ) -> Option<EventBits_t> {
        assert_can_block("EventGroup::sync_timeout", ticks);
        Self::assert_valid_bits("EventGroup::sync_timeout bits_to_set", bits_to_set);
        Self::assert_valid_bits("EventGroup::sync_timeout bits_to_wait", bits_to_wait);
        // Safety: the borrowed wrapper owns a live event-group handle.
        let result = unsafe { xEventGroupSync(self.handle, bits_to_set, bits_to_wait, ticks) };
        if (result & bits_to_wait) == bits_to_wait {
            Some(result)
        } else {
            None
        }
    }

    /// Gets the current bits from interrupt context.
    ///
    /// # Safety
    ///
    /// The caller must be executing at an interrupt priority from which
    /// FreeRTOS `FromISR` APIs are permitted.
    pub unsafe fn get_from_isr(&self) -> EventBits_t {
        // Safety: `self` holds a live handle; the caller upholds the ISR
        // priority contract documented above.
        unsafe { xEventGroupGetBitsFromISR(self.handle) }
    }

    /// Defers setting bits from interrupt context to the timer daemon.
    ///
    /// This method does not request a context switch. If it sets
    /// `higher_priority_task_woken` to `pdTRUE`, the ISR must yield using the
    /// active port's ISR-yield primitive before returning.
    ///
    /// # Safety
    ///
    /// The caller must be executing at an interrupt priority from which
    /// FreeRTOS `FromISR` APIs are permitted. The wake flag must be initialized
    /// to `pdFALSE` before the first operation in an ISR. Because FreeRTOS
    /// defers this operation to the timer daemon, this event group must not be
    /// deleted until that daemon has executed the command.
    #[cfg(all(feature = "pend-function-call", feature = "timers"))]
    pub unsafe fn set_from_isr(
        &self,
        bits: EventBits_t,
        higher_priority_task_woken: &mut BaseType_t,
    ) -> bool {
        Self::assert_valid_bits("EventGroup::set_from_isr", bits);
        // Safety: `self` holds a live handle, the mutable wake flag is valid,
        // and the caller upholds the ISR-priority contract.
        unsafe {
            xEventGroupSetBitsFromISR(self.handle, bits, higher_priority_task_woken) == pdPASS
        }
    }

    /// Defers clearing bits from interrupt context to the timer daemon.
    ///
    /// FreeRTOS's clear-bits ISR API does not report whether the timer daemon
    /// was woken, so this wrapper cannot return a yield request.
    ///
    /// # Safety
    ///
    /// The caller must be executing at an interrupt priority from which
    /// FreeRTOS `FromISR` APIs are permitted. Because FreeRTOS defers this
    /// operation to the timer daemon, this event group must not be deleted
    /// until that daemon has executed the command.
    #[cfg(all(feature = "pend-function-call", feature = "timers"))]
    pub unsafe fn clear_from_isr(&self, bits: EventBits_t) -> bool {
        Self::assert_valid_bits("EventGroup::clear_from_isr", bits);
        // Safety: `self` holds a live handle and the caller upholds the
        // ISR-priority contract.
        unsafe { xEventGroupClearBitsFromISR(self.handle, bits) == pdPASS }
    }

    /// Returns the raw FreeRTOS handle for interop.
    ///
    /// # Safety
    ///
    /// The caller must not use the returned handle to outlive this owner,
    /// delete the object, or bypass the task/ISR and event-bit contracts of
    /// the corresponding raw operation.
    pub unsafe fn raw_handle(&self) -> EventGroupHandle_t {
        self.handle
    }

    /// Consumes the wrapper and returns the raw handle without deleting it.
    pub fn into_raw(mut self) -> EventGroupHandle_t {
        self.owns_handle = false;
        self.handle
    }

    /// Explicitly deletes the underlying FreeRTOS event group.
    pub fn delete(mut self, _context: &TaskContext) {
        assert_task_context("EventGroup::delete");
        // Safety: consuming `self` provides unique ownership of its live
        // handle, and the context check excludes ISR deletion.
        unsafe { vEventGroupDelete(self.handle) };
        self.owns_handle = false;
    }
}

impl Drop for EventGroup {
    fn drop(&mut self) {
        // Event-group deletion manipulates task lists and is not ISR-safe.
        if self.owns_handle && !is_in_isr() {
            // Safety: Drop has exclusive access to the owned live handle and
            // the branch excludes interrupt context.
            unsafe { vEventGroupDelete(self.handle) };
            self.owns_handle = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_user_event_bits() {
        assert!(EventGroup::valid_bits(1));
        assert!(!EventGroup::valid_bits(0));
        assert!(!EventGroup::valid_bits(eventEVENT_BITS_CONTROL_BYTES));
        assert!(!EventGroup::valid_bits(eventEVENT_BITS_CONTROL_BYTES | 1));
    }

    #[test]
    fn event_group_handle_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<EventGroup>();
    }
}
