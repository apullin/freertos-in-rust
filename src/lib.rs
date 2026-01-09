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
 * [AMENDMENT] This file is part of FreeRTOS-in-Rust, a Rust port of the FreeRTOS kernel.
 * The original C code has been translated to Rust while preserving the original
 * structure, control flow, and comments as much as possible.
 */

//! # FreeRTOS-in-Rust - FreeRTOS Kernel in Rust
//!
//! This crate provides a pure-Rust implementation of the FreeRTOS kernel,
//! translated line-by-line from the original C source code.
//!
//! ## Features
//!
//! - `port-dummy` - Dummy port for compilation (default, panics at runtime)
//! - `arch-32bit` - 32-bit architecture types (default)
//! - `arch-64bit` - 64-bit architecture types
//! - `tick-32bit` - 32-bit tick counter (default)
//! - `tick-64bit` - 64-bit tick counter
//! - `alloc` - Enable memory allocation via Rust's global allocator
//! - `std` - Enable std for host testing (allocation is selected independently)
//! - `port-test` - Deterministic host test port
//! - `all-kernel-features` - Enable kernel capabilities without selecting a port,
//!   integer width, allocator, or SMP mode
//! - `list-data-integrity-check` - Enable list data integrity checks
//!
//! Production targets must set `default-features = false` and enable exactly
//! one hardware port. The default `port-dummy` selection is compile-only and
//! cannot start a scheduler.

#![no_std]
#![allow(non_snake_case)]
#![allow(non_camel_case_types)]
#![allow(non_upper_case_globals)]
#![allow(dead_code)] // During development
#![allow(static_mut_refs)] // TODO: Migrate to raw pointers for Rust 2024

#[cfg(all(feature = "smp", not(feature = "port-cortex-a9")))]
compile_error!("the experimental SMP scaffolding currently supports only port-cortex-a9");

#[cfg(not(any(
    feature = "port-cortex-m0",
    feature = "port-cortex-m3",
    feature = "port-cortex-m4f",
    feature = "port-cortex-m7",
    feature = "port-riscv32",
    feature = "port-cortex-a9",
    feature = "port-cortex-a53",
    feature = "port-test",
    feature = "port-dummy"
)))]
compile_error!("select exactly one FreeRTOS-in-Rust port feature");

#[cfg(any(
    all(
        feature = "port-cortex-m0",
        any(
            feature = "port-cortex-m3",
            feature = "port-cortex-m4f",
            feature = "port-cortex-m7",
            feature = "port-riscv32",
            feature = "port-cortex-a9",
            feature = "port-cortex-a53",
            feature = "port-test",
            feature = "port-dummy"
        )
    ),
    all(
        feature = "port-cortex-m3",
        any(
            feature = "port-cortex-m4f",
            feature = "port-cortex-m7",
            feature = "port-riscv32",
            feature = "port-cortex-a9",
            feature = "port-cortex-a53",
            feature = "port-test",
            feature = "port-dummy"
        )
    ),
    all(
        feature = "port-cortex-m4f",
        any(
            feature = "port-cortex-m7",
            feature = "port-riscv32",
            feature = "port-cortex-a9",
            feature = "port-cortex-a53",
            feature = "port-test",
            feature = "port-dummy"
        )
    ),
    all(
        feature = "port-cortex-m7",
        any(
            feature = "port-riscv32",
            feature = "port-cortex-a9",
            feature = "port-cortex-a53",
            feature = "port-test",
            feature = "port-dummy"
        )
    ),
    all(
        feature = "port-riscv32",
        any(
            feature = "port-cortex-a9",
            feature = "port-cortex-a53",
            feature = "port-test",
            feature = "port-dummy"
        )
    ),
    all(
        feature = "port-cortex-a9",
        any(
            feature = "port-cortex-a53",
            feature = "port-test",
            feature = "port-dummy"
        )
    ),
    all(
        feature = "port-cortex-a53",
        any(feature = "port-test", feature = "port-dummy")
    ),
    all(feature = "port-test", feature = "port-dummy")
))]
compile_error!("FreeRTOS-in-Rust port features are mutually exclusive");

#[cfg(any(
    all(feature = "heap-4", feature = "heap-5"),
    all(feature = "heap-4", feature = "alloc"),
    all(feature = "heap-5", feature = "alloc")
))]
compile_error!("allocator features are mutually exclusive");

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

// Safe synchronization wrappers. Individual capabilities such as Mutex,
// streams, and timers retain their own feature gates.
pub mod sync;

// Re-export commonly used items at crate root (like FreeRTOS.h does)
pub use config::*;
pub use types::*;

// =============================================================================
// Convenience Functions
// =============================================================================

/// Delays the current task for the specified number of ticks.
///
/// This is a safe wrapper around `vTaskDelay`. The context capability prevents
/// task-only delay operations from being called through an ISR or timer
/// callback API.
#[inline]
pub fn delay(context: &sync::TaskContext, ticks: TickType_t) {
    assert!(context.current().is_some(), "delay requires a running task");
    sync::assert_can_block("delay", ticks);
    // Safety: TaskContext and the checks above establish a live current task,
    // running scheduler, task context, and an unsuspended blocking path.
    unsafe { kernel::tasks::vTaskDelay(ticks) };
}

/// Delays the current task for the specified number of milliseconds.
///
/// Converts milliseconds to ticks using the configured tick rate without
/// narrowing the millisecond value first.
///
/// # Panics
///
/// Panics when the requested duration cannot be represented by `TickType_t`.
/// Use [`milliseconds_to_ticks_checked`] when the duration is not known to fit.
#[inline]
pub fn delay_ms(context: &sync::TaskContext, ms: u32) {
    let ticks = milliseconds_to_ticks_checked(u64::from(ms))
        .expect("millisecond delay does not fit in TickType_t");
    delay(context, ticks);
}

/// Starts the FreeRTOS scheduler.
///
/// This function does not return under normal operation. Tasks must be
/// created before calling this function.
#[inline]
pub fn start_scheduler(_context: sync::TaskContext) -> ! {
    sync::assert_task_context("start_scheduler");
    assert_eq!(
        kernel::tasks::xTaskGetSchedulerState(),
        kernel::tasks::taskSCHEDULER_NOT_STARTED,
        "start_scheduler requires exclusive pre-scheduler initialization context"
    );
    // Safety: consuming the non-sendable startup capability establishes the
    // crate's exclusive pre-scheduler entry boundary, and the unconditional
    // state check rejects a TaskContext obtained from a running task. Internal
    // task creation failures are handled by the raw scheduler path before it
    // returns.
    unsafe { kernel::tasks::vTaskStartScheduler() };
    // vTaskStartScheduler only returns if creating an internal task failed.
    panic!("FreeRTOS scheduler failed to start")
}
