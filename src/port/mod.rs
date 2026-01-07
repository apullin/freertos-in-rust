/*
 * FreeRTOS Kernel <DEVELOPMENT BRANCH>
 * Copyright (C) 2021 Amazon.com, Inc. or its affiliates. All Rights Reserved.
 *
 * SPDX-License-Identifier: MIT
 *
 * [AMENDMENT] This module provides the port layer abstraction for FreeRusTOS.
 * The port layer contains hardware-specific implementations of:
 * - Critical sections (interrupt enable/disable)
 * - Context switching
 * - Stack initialization
 * - Scheduler start/stop
 *
 * Select a port via Cargo features (e.g., `port-dummy`, `port-cortex-m4f`).
 */

//! Port Layer
//!
//! This module provides hardware abstraction for the FreeRTOS kernel.
//! Each port implements the same interface but with hardware-specific code.
//!
//! ## Available Ports
//!
//! - `port-dummy` - Dummy port for compilation testing (panics at runtime)
//!
//! ## Future Ports (TODO)
//!
//! - `port-cortex-m4f` - ARM Cortex-M4F with FPU

// Only use dummy if port-dummy is enabled AND port-cortex-m4f is NOT enabled
// This makes the ports mutually exclusive
#[cfg(all(feature = "port-dummy", not(feature = "port-cortex-m4f")))]
mod dummy;

#[cfg(all(feature = "port-dummy", not(feature = "port-cortex-m4f")))]
pub use dummy::*;

#[cfg(feature = "port-cortex-m4f")]
mod cortex_m4f;

#[cfg(feature = "port-cortex-m4f")]
pub use cortex_m4f::*;

// =============================================================================
// Port-independent constants (from portable.h)
// =============================================================================

use crate::types::*;

// NOTE: portBYTE_ALIGNMENT is defined in each port module.

/// Byte alignment mask (computed from portBYTE_ALIGNMENT)
/// [AMENDMENT] This is computed at the call site since portBYTE_ALIGNMENT
/// comes from the port module.
#[inline(always)]
pub const fn portBYTE_ALIGNMENT_MASK() -> usize {
    portBYTE_ALIGNMENT - 1
}

/// Tick period in milliseconds
/// [AMENDMENT] Computed from configTICK_RATE_HZ
#[inline(always)]
pub const fn portTICK_PERIOD_MS() -> TickType_t {
    1000 / crate::config::configTICK_RATE_HZ
}

// =============================================================================
// Privileged function attribute
// =============================================================================

/// Marker for privileged functions (no-op in non-MPU ports)
/// [AMENDMENT] In Rust, this is a no-op. MPU support would need different handling.
#[macro_export]
macro_rules! PRIVILEGED_FUNCTION {
    () => {};
}
