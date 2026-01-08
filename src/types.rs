/*
 * FreeRTOS Kernel <DEVELOPMENT BRANCH>
 * Copyright (C) 2021 Amazon.com, Inc. or its affiliates. All Rights Reserved.
 *
 * SPDX-License-Identifier: MIT
 *
 * [AMENDMENT] This module provides the Rust equivalents of FreeRTOS base types.
 * These are typically defined in portmacro.h for each port. Here we provide
 * generic definitions controlled by Cargo features.
 */

//! FreeRTOS Base Types
//!
//! This module defines the fundamental types used throughout FreeRTOS:
//! - `BaseType_t` - Signed type, architecture word size
//! - `UBaseType_t` - Unsigned type, architecture word size
//! - `StackType_t` - Stack element type
//! - `TickType_t` - Tick counter type
//!
//! ## Architecture Width
//! - `arch-32bit` feature: 32-bit types (default)
//! - `arch-64bit` feature: 64-bit types
//!
//! ## Tick Width
//! - `tick-16bit` feature: 16-bit tick counter
//! - `tick-32bit` feature: 32-bit tick counter (default)
//! - `tick-64bit` feature: 64-bit tick counter

// =============================================================================
// Architecture-dependent types (BaseType_t, UBaseType_t, StackType_t)
// =============================================================================

/// Signed base type - architecture word size
/// Used for boolean returns, error codes, and small signed values.
#[cfg(feature = "arch-32bit")]
pub type BaseType_t = i32;

#[cfg(feature = "arch-64bit")]
pub type BaseType_t = i64;

/// Unsigned base type - architecture word size
/// Used for counts, indices, and priority values.
#[cfg(feature = "arch-32bit")]
pub type UBaseType_t = u32;

#[cfg(feature = "arch-64bit")]
pub type UBaseType_t = u64;

/// Stack element type
/// Each stack "slot" is this size.
#[cfg(feature = "arch-32bit")]
pub type StackType_t = u32;

#[cfg(feature = "arch-64bit")]
pub type StackType_t = u64;

// =============================================================================
// Tick type (configurable width independent of architecture)
// =============================================================================

/// Tick counter type - 16-bit variant
/// Suitable for very resource-constrained systems.
#[cfg(feature = "tick-16bit")]
pub type TickType_t = u16;

/// Maximum tick value for 16-bit ticks
#[cfg(feature = "tick-16bit")]
pub const portMAX_DELAY: TickType_t = 0xFFFF;

/// Tick counter type - 32-bit variant (most common)
#[cfg(feature = "tick-32bit")]
pub type TickType_t = u32;

/// Maximum tick value for 32-bit ticks
#[cfg(feature = "tick-32bit")]
pub const portMAX_DELAY: TickType_t = 0xFFFF_FFFF;

/// Tick counter type - 64-bit variant
/// For systems needing very long delays without overflow.
#[cfg(feature = "tick-64bit")]
pub type TickType_t = u64;

/// Maximum tick value for 64-bit ticks
#[cfg(feature = "tick-64bit")]
pub const portMAX_DELAY: TickType_t = 0xFFFF_FFFF_FFFF_FFFF;

// =============================================================================
// Tick type width constants (for conditional compilation in kernel code)
// =============================================================================

/// Tick type width indicator - 16-bit
pub const TICK_TYPE_WIDTH_16_BITS: u8 = 0;
/// Tick type width indicator - 32-bit
pub const TICK_TYPE_WIDTH_32_BITS: u8 = 1;
/// Tick type width indicator - 64-bit
pub const TICK_TYPE_WIDTH_64_BITS: u8 = 2;

/// Current tick type width (for runtime checks if needed)
#[cfg(feature = "tick-16bit")]
pub const configTICK_TYPE_WIDTH_IN_BITS: u8 = TICK_TYPE_WIDTH_16_BITS;

#[cfg(feature = "tick-32bit")]
pub const configTICK_TYPE_WIDTH_IN_BITS: u8 = TICK_TYPE_WIDTH_32_BITS;

#[cfg(feature = "tick-64bit")]
pub const configTICK_TYPE_WIDTH_IN_BITS: u8 = TICK_TYPE_WIDTH_64_BITS;

// =============================================================================
// Boolean-like constants (from projdefs.h)
// =============================================================================

/// Boolean false as BaseType_t
pub const pdFALSE: BaseType_t = 0;

/// Boolean true as BaseType_t
pub const pdTRUE: BaseType_t = 1;

/// Pass/success return value
pub const pdPASS: BaseType_t = pdTRUE;

/// Fail return value
pub const pdFAIL: BaseType_t = pdFALSE;

/// Queue empty indicator
pub const errQUEUE_EMPTY: BaseType_t = 0;

/// Queue full indicator
pub const errQUEUE_FULL: BaseType_t = 0;

// =============================================================================
// Error codes (from projdefs.h)
// =============================================================================

/// Could not allocate required memory
pub const errCOULD_NOT_ALLOCATE_REQUIRED_MEMORY: BaseType_t = -1;

/// Queue blocked
pub const errQUEUE_BLOCKED: BaseType_t = -4;

/// Queue yield
pub const errQUEUE_YIELD: BaseType_t = -5;

// =============================================================================
// Integrity check value (from projdefs.h)
// =============================================================================

/// Integrity check magic value - 16-bit ticks
#[cfg(feature = "tick-16bit")]
pub const pdINTEGRITY_CHECK_VALUE: TickType_t = 0x5A5A;

/// Integrity check magic value - 32-bit ticks
#[cfg(feature = "tick-32bit")]
pub const pdINTEGRITY_CHECK_VALUE: TickType_t = 0x5A5A_5A5A;

/// Integrity check magic value - 64-bit ticks
#[cfg(feature = "tick-64bit")]
pub const pdINTEGRITY_CHECK_VALUE: TickType_t = 0x5A5A_5A5A_5A5A_5A5A;

// =============================================================================
// Task function type (from projdefs.h)
// =============================================================================

/// Task function prototype
/// `void vTaskFunction(void *pvParameters)`
///
/// [AMENDMENT] In Rust, we use a function pointer type. The `extern "C"` ensures
/// C-compatible calling convention for potential interop.
pub type TaskFunction_t = extern "C" fn(*mut core::ffi::c_void);

// =============================================================================
// Utility macros as functions (from projdefs.h)
// =============================================================================

/// Convert milliseconds to ticks
/// Equivalent to pdMS_TO_TICKS macro
#[inline(always)]
pub const fn pdMS_TO_TICKS(xTimeInMs: TickType_t) -> TickType_t {
    // [AMENDMENT] Using const fn for compile-time evaluation where possible
    // Original: ( ( TickType_t ) ( ( ( uint64_t ) ( xTimeInMs ) * ( uint64_t ) configTICK_RATE_HZ ) / ( uint64_t ) 1000U ) )
    ((xTimeInMs as u64 * crate::config::configTICK_RATE_HZ as u64) / 1000u64) as TickType_t
}

/// Convert ticks to milliseconds
/// Equivalent to pdTICKS_TO_MS macro
#[inline(always)]
pub const fn pdTICKS_TO_MS(xTimeInTicks: TickType_t) -> TickType_t {
    ((xTimeInTicks as u64 * 1000u64) / crate::config::configTICK_RATE_HZ as u64) as TickType_t
}

// =============================================================================
// Handle types (opaque pointers to kernel objects)
// =============================================================================

/// Task handle - opaque pointer to a task control block
pub type TaskHandle_t = *mut core::ffi::c_void;

// NOTE: QueueHandle_t, SemaphoreHandle_t, MutexHandle_t are defined in queue.rs
// to avoid circular dependencies

/// Timer handle - opaque pointer to a timer
pub type TimerHandle_t = *mut core::ffi::c_void;

/// Event group handle
pub type EventGroupHandle_t = *mut core::ffi::c_void;

/// Stream buffer handle
pub type StreamBufferHandle_t = *mut core::ffi::c_void;

/// Message buffer handle (same as stream buffer)
pub type MessageBufferHandle_t = StreamBufferHandle_t;

// =============================================================================
// Sleep Mode Status (for tickless idle)
// =============================================================================

/// Return type for eTaskConfirmSleepModeStatus
///
/// Used by the port layer to determine whether to enter a sleep mode.
#[cfg(feature = "tickless-idle")]
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum eSleepModeStatus {
    /// A task has been made ready or a context switch pended since
    /// portSUPPRESS_TICKS_AND_SLEEP() was called - abort entering a sleep mode.
    eAbortSleep = 0,

    /// Enter a sleep mode that will not last any longer than the expected idle time.
    eStandardSleep = 1,

    /// No tasks are waiting for a timeout so it is safe to enter a sleep mode
    /// that can only be exited by an external interrupt.
    #[cfg(feature = "task-suspend")]
    eNoTasksWaitingTimeout = 2,
}

// =============================================================================
// Null handle constant
// =============================================================================

/// Null handle value
pub const NULL_HANDLE: *mut core::ffi::c_void = core::ptr::null_mut();

// =============================================================================
// Task Status Structure (for vTaskGetInfo / uxTaskGetSystemState)
// =============================================================================

// Forward declare eTaskState - the actual enum is in tasks.rs
// We re-export it here for the TaskStatus_t struct
// [AMENDMENT] In C, eTaskState is defined in task.h. In Rust, the enum
// is in tasks.rs, so TaskStatus_t references it via the kernel module.

/// Task status information structure
///
/// Used by `vTaskGetInfo` and `uxTaskGetSystemState` to return detailed
/// information about a task.
#[repr(C)]
#[derive(Clone)]
pub struct TaskStatus_t {
    /// Handle of the task to which the rest of the information relates
    pub xHandle: TaskHandle_t,
    /// Pointer to the task's name (null-terminated)
    pub pcTaskName: *const u8,
    /// Task number assigned to the task (for trace tools)
    pub xTaskNumber: UBaseType_t,
    /// Current state of the task
    pub eCurrentState: u8, // Actually eTaskState, but using u8 for C compatibility
    /// Priority at which the task was running when the structure was populated
    pub uxCurrentPriority: UBaseType_t,
    /// Base priority of the task (priority before any inheritance)
    pub uxBasePriority: UBaseType_t,
    /// Total run time allocated to the task so far (if configured)
    pub ulRunTimeCounter: u32,
    /// Pointer to the start of the stack (lowest address)
    pub pxStackBase: *mut StackType_t,
    /// Minimum free stack space since the task was created (high water mark)
    /// Closer to zero = closer to overflow
    pub usStackHighWaterMark: u16,
}

impl TaskStatus_t {
    /// Create a zeroed TaskStatus_t
    pub const fn new() -> Self {
        TaskStatus_t {
            xHandle: core::ptr::null_mut(),
            pcTaskName: core::ptr::null(),
            xTaskNumber: 0,
            eCurrentState: 0,
            uxCurrentPriority: 0,
            uxBasePriority: 0,
            ulRunTimeCounter: 0,
            pxStackBase: core::ptr::null_mut(),
            usStackHighWaterMark: 0,
        }
    }
}
