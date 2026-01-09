/*
 * FreeRTOS Kernel <DEVELOPMENT BRANCH>
 * Copyright (C) 2021 Amazon.com, Inc. or its affiliates. All Rights Reserved.
 *
 * SPDX-License-Identifier: MIT
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy of
 * this software and associated documentation files (the "Software"), to deal in
 * the Software without restriction, including without limitation the rights to
 * use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of
 * the Software, and to permit persons to whom the Software is furnished to do so,
 * subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in all
 * copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS
 * FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR
 * COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER
 * IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN
 * CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
 *
 * https://www.FreeRTOS.org
 * https://github.com/FreeRTOS
 *
 * [AMENDMENT] This file is part of FreeRusTOS, a Rust port of the FreeRTOS kernel.
 * The original C code has been translated to Rust while preserving the original
 * structure, control flow, and comments as much as possible.
 */

//! # FreeRusTOS - FreeRTOS Kernel in Rust
//!
//! This crate provides a pure-Rust implementation of the FreeRTOS kernel,
//! translated line-by-line from the original C source code.
//!
//! ## Features
//!
//! - `port-dummy` - Dummy port for compilation (default, panics at runtime)
//! - `arch-32bit` - 32-bit architecture types (default)
//! - `arch-64bit` - 64-bit architecture types
//! - `tick-16bit` - 16-bit tick counter
//! - `tick-32bit` - 32-bit tick counter (default)
//! - `tick-64bit` - 64-bit tick counter
//! - `alloc` - Enable memory allocation via Rust's global allocator
//! - `std` - Enable std for testing (implies `alloc`)
//! - `list-data-integrity-check` - Enable list data integrity checks

#![no_std]
#![allow(non_snake_case)]
#![allow(non_camel_case_types)]
#![allow(non_upper_case_globals)]
#![allow(dead_code)] // During development
#![allow(static_mut_refs)] // TODO: Migrate to raw pointers for Rust 2024

#[cfg(feature = "std")]
extern crate std;

#[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
extern crate alloc;

// Core modules
pub mod config;
pub mod trace;
pub mod types;

// Port layer
pub mod port;

// Memory management
pub mod memory;

// Kernel modules
pub mod kernel;

// Re-export commonly used items at crate root (like FreeRTOS.h does)
pub use config::*;
pub use types::*;
