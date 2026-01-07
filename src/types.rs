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
// Null handle constant
// =============================================================================

/// Null handle value
pub const NULL_HANDLE: *mut core::ffi::c_void = core::ptr::null_mut();
