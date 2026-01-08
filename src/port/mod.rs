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
//! - `port-cortex-m0` - ARM Cortex-M0/M0+ (ARMv6-M, no FPU, PRIMASK)
//! - `port-cortex-m3` - ARM Cortex-M3 (ARMv7-M, no FPU, BASEPRI)
//! - `port-cortex-m4f` - ARM Cortex-M4F with FPU (ARMv7E-M, BASEPRI)
//! - `port-cortex-m7` - ARM Cortex-M7 (same as CM4F per FreeRTOS docs)
//! - `port-riscv32` - RISC-V RV32 (standard CLINT timer)
//! - `port-cortex-a9` - ARM Cortex-A9 (ARMv7-A, GIC, SWI for context switch)
//! - `port-cortex-a53` - ARM Cortex-A53 (ARMv8-A/AArch64, GIC, SMC for context switch)

// Port selection - only one can be active at a time
// Priority: CM0 > CM3 > CM4F > CM7 > RISCV32 > dummy

#[cfg(feature = "port-cortex-m0")]
mod cortex_m0;
#[cfg(feature = "port-cortex-m0")]
pub use cortex_m0::*;

#[cfg(all(feature = "port-cortex-m3", not(feature = "port-cortex-m0")))]
mod cortex_m3;
#[cfg(all(feature = "port-cortex-m3", not(feature = "port-cortex-m0")))]
pub use cortex_m3::*;

#[cfg(all(
    feature = "port-cortex-m4f",
    not(feature = "port-cortex-m0"),
    not(feature = "port-cortex-m3")
))]
mod cortex_m4f;
#[cfg(all(
    feature = "port-cortex-m4f",
    not(feature = "port-cortex-m0"),
    not(feature = "port-cortex-m3")
))]
pub use cortex_m4f::*;

#[cfg(all(
    feature = "port-cortex-m7",
    not(feature = "port-cortex-m0"),
    not(feature = "port-cortex-m3"),
    not(feature = "port-cortex-m4f")
))]
mod cortex_m7;
#[cfg(all(
    feature = "port-cortex-m7",
    not(feature = "port-cortex-m0"),
    not(feature = "port-cortex-m3"),
    not(feature = "port-cortex-m4f")
))]
pub use cortex_m7::*;

#[cfg(all(
    feature = "port-riscv32",
    not(feature = "port-cortex-m0"),
    not(feature = "port-cortex-m3"),
    not(feature = "port-cortex-m4f"),
    not(feature = "port-cortex-m7")
))]
mod riscv32;
#[cfg(all(
    feature = "port-riscv32",
    not(feature = "port-cortex-m0"),
    not(feature = "port-cortex-m3"),
    not(feature = "port-cortex-m4f"),
    not(feature = "port-cortex-m7")
))]
pub use riscv32::*;

#[cfg(all(
    feature = "port-cortex-a9",
    not(feature = "port-cortex-m0"),
    not(feature = "port-cortex-m3"),
    not(feature = "port-cortex-m4f"),
    not(feature = "port-cortex-m7"),
    not(feature = "port-riscv32")
))]
mod cortex_a9;
#[cfg(all(
    feature = "port-cortex-a9",
    not(feature = "port-cortex-m0"),
    not(feature = "port-cortex-m3"),
    not(feature = "port-cortex-m4f"),
    not(feature = "port-cortex-m7"),
    not(feature = "port-riscv32")
))]
pub use cortex_a9::*;

#[cfg(all(
    feature = "port-cortex-a53",
    not(feature = "port-cortex-m0"),
    not(feature = "port-cortex-m3"),
    not(feature = "port-cortex-m4f"),
    not(feature = "port-cortex-m7"),
    not(feature = "port-riscv32"),
    not(feature = "port-cortex-a9")
))]
mod cortex_a53;
#[cfg(all(
    feature = "port-cortex-a53",
    not(feature = "port-cortex-m0"),
    not(feature = "port-cortex-m3"),
    not(feature = "port-cortex-m4f"),
    not(feature = "port-cortex-m7"),
    not(feature = "port-riscv32"),
    not(feature = "port-cortex-a9")
))]
pub use cortex_a53::*;

#[cfg(all(
    feature = "port-dummy",
    not(feature = "port-cortex-m0"),
    not(feature = "port-cortex-m3"),
    not(feature = "port-cortex-m4f"),
    not(feature = "port-cortex-m7"),
    not(feature = "port-riscv32"),
    not(feature = "port-cortex-a9"),
    not(feature = "port-cortex-a53")
))]
mod dummy;
#[cfg(all(
    feature = "port-dummy",
    not(feature = "port-cortex-m0"),
    not(feature = "port-cortex-m3"),
    not(feature = "port-cortex-m4f"),
    not(feature = "port-cortex-m7"),
    not(feature = "port-riscv32"),
    not(feature = "port-cortex-a9"),
    not(feature = "port-cortex-a53")
))]
pub use dummy::*;

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
