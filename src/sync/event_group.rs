//! Safe EventGroup wrapper
//!
//! Provides a safe wrapper around FreeRTOS event groups.
//! Event groups allow tasks to wait on combinations of event bits.

use crate::kernel::event_groups::{
    xEventGroupClearBits, xEventGroupCreate, xEventGroupGetBits, xEventGroupSetBits,
    xEventGroupSync, xEventGroupWaitBits, EventBits_t,
};
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
/// With 16-bit ticks: 8 usable bits (bits 0-7)
/// The upper bits are reserved for internal control flags.
pub struct EventGroup {
    handle: EventGroupHandle_t,
}

// Safety: EventGroup can be shared between tasks - that's its purpose
unsafe impl Sync for EventGroup {}
unsafe impl Send for EventGroup {}

impl EventGroup {
    /// Creates a new event group with all bits cleared.
    ///
    /// Returns `None` if creation failed (e.g., out of memory).
    #[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
    pub fn new() -> Option<Self> {
        let handle = xEventGroupCreate();
        if handle.is_null() {
            None
        } else {
            Some(Self { handle })
        }
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
    pub fn set(&self, bits: EventBits_t) -> EventBits_t {
        xEventGroupSetBits(self.handle, bits)
    }

    /// Clears bits in the event group.
    ///
    /// Returns the bits value *before* clearing.
    pub fn clear(&self, bits: EventBits_t) -> EventBits_t {
        xEventGroupClearBits(self.handle, bits)
    }

    /// Gets the current bits value.
    pub fn get(&self) -> EventBits_t {
        xEventGroupGetBits(self.handle)
    }

    // =========================================================================
    // Wait for ANY bit (OR condition)
    // =========================================================================

    /// Waits for ANY of the specified bits to be set, blocking indefinitely.
    ///
    /// Does not clear bits on exit. Use `wait_any_clear` to auto-clear.
    ///
    /// Returns the bits value when unblocked.
    pub fn wait_any(&self, bits: EventBits_t) -> EventBits_t {
        xEventGroupWaitBits(self.handle, bits, pdFALSE, pdFALSE, portMAX_DELAY)
    }

    /// Waits for ANY of the specified bits with a timeout.
    ///
    /// Returns `Some(bits)` if any bit was set, `None` on timeout.
    pub fn wait_any_timeout(&self, bits: EventBits_t, ticks: TickType_t) -> Option<EventBits_t> {
        let result = xEventGroupWaitBits(self.handle, bits, pdFALSE, pdFALSE, ticks);
        if (result & bits) != 0 {
            Some(result)
        } else {
            None
        }
    }

    /// Attempts to check if ANY of the specified bits are set without blocking.
    ///
    /// Returns `Some(bits)` if any bit is set, `None` otherwise.
    pub fn try_wait_any(&self, bits: EventBits_t) -> Option<EventBits_t> {
        self.wait_any_timeout(bits, 0)
    }

    /// Waits for ANY bit and clears the matched bits on exit.
    ///
    /// This is the common "consume event" pattern.
    pub fn wait_any_clear(&self, bits: EventBits_t) -> EventBits_t {
        xEventGroupWaitBits(self.handle, bits, pdTRUE, pdFALSE, portMAX_DELAY)
    }

    /// Waits for ANY bit with timeout, clearing matched bits on success.
    pub fn wait_any_clear_timeout(
        &self,
        bits: EventBits_t,
        ticks: TickType_t,
    ) -> Option<EventBits_t> {
        let result = xEventGroupWaitBits(self.handle, bits, pdTRUE, pdFALSE, ticks);
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
    pub fn wait_all(&self, bits: EventBits_t) -> EventBits_t {
        xEventGroupWaitBits(self.handle, bits, pdFALSE, pdTRUE, portMAX_DELAY)
    }

    /// Waits for ALL of the specified bits with a timeout.
    ///
    /// Returns `Some(bits)` if all bits were set, `None` on timeout.
    pub fn wait_all_timeout(&self, bits: EventBits_t, ticks: TickType_t) -> Option<EventBits_t> {
        let result = xEventGroupWaitBits(self.handle, bits, pdFALSE, pdTRUE, ticks);
        if (result & bits) == bits {
            Some(result)
        } else {
            None
        }
    }

    /// Attempts to check if ALL of the specified bits are set without blocking.
    pub fn try_wait_all(&self, bits: EventBits_t) -> Option<EventBits_t> {
        self.wait_all_timeout(bits, 0)
    }

    /// Waits for ALL bits and clears them on exit.
    pub fn wait_all_clear(&self, bits: EventBits_t) -> EventBits_t {
        xEventGroupWaitBits(self.handle, bits, pdTRUE, pdTRUE, portMAX_DELAY)
    }

    /// Waits for ALL bits with timeout, clearing them on success.
    pub fn wait_all_clear_timeout(
        &self,
        bits: EventBits_t,
        ticks: TickType_t,
    ) -> Option<EventBits_t> {
        let result = xEventGroupWaitBits(self.handle, bits, pdTRUE, pdTRUE, ticks);
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
    pub fn sync(&self, bits_to_set: EventBits_t, bits_to_wait: EventBits_t) -> EventBits_t {
        xEventGroupSync(self.handle, bits_to_set, bits_to_wait, portMAX_DELAY)
    }

    /// Sync with timeout.
    ///
    /// Returns `Some(bits)` if sync completed, `None` on timeout.
    pub fn sync_timeout(
        &self,
        bits_to_set: EventBits_t,
        bits_to_wait: EventBits_t,
        ticks: TickType_t,
    ) -> Option<EventBits_t> {
        let result = xEventGroupSync(self.handle, bits_to_set, bits_to_wait, ticks);
        if (result & bits_to_wait) == bits_to_wait {
            Some(result)
        } else {
            None
        }
    }

    /// Returns the raw FreeRTOS handle for interop.
    pub unsafe fn raw_handle(&self) -> EventGroupHandle_t {
        self.handle
    }
}
