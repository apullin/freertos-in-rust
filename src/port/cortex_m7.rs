/*
 * FreeRTOS Kernel <DEVELOPMENT BRANCH>
 * Copyright (C) 2021 Amazon.com, Inc. or its affiliates. All Rights Reserved.
 *
 * SPDX-License-Identifier: MIT
 *
 * [AMENDMENT] This is the Cortex-M7 port for FreeRusTOS.
 *
 * Per the FreeRTOS documentation:
 * "It is recommended to use the FreeRTOS ARM Cortex-M4F port located in
 * /FreeRTOS/Source/portable/GCC/ARM_CM4F directory."
 *
 * The Cortex-M7 is architecturally very similar to the Cortex-M4F.
 * Both are ARMv7E-M architecture with optional FPU.
 * This port reexports the CM4F implementation.
 */

//! ARM Cortex-M7 Port Implementation
//!
//! This port reexports the Cortex-M4F implementation, which is compatible
//! with Cortex-M7 per FreeRTOS documentation.
//!
//! ## Usage
//!
//! Enable with `--features port-cortex-m7` and compile for `thumbv7em-none-eabihf`.

// Re-export everything from the CM4F port
pub use super::cortex_m4f::*;
