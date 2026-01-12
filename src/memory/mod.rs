/*
 * FreeRTOS Kernel <DEVELOPMENT BRANCH>
 * Copyright (C) 2021 Amazon.com, Inc. or its affiliates. All Rights Reserved.
 *
 * SPDX-License-Identifier: MIT
 *
 * [AMENDMENT] This module provides memory allocation for FreeRTOS.
 * The allocator implementation is selected via Cargo features:
 *
 * - `heap-4`: Use FreeRTOS heap_4 (first-fit with coalescing, single region)
 * - `heap-5`: Use FreeRTOS heap_5 (like heap_4 but supports multiple regions)
 * - `alloc` (without heap-4/5): Wrap Rust's #[global_allocator]
 * - Neither: Stub implementations that panic at runtime
 *
 * Only ONE of heap-4, heap-5, or alloc should be enabled.
 * Priority: heap-4 > heap-5 > alloc > stubs
 */

//! Memory Allocation
//!
//! This module provides the FreeRTOS memory allocation interface.
//!
//! ## Allocator Selection
//!
//! Choose your allocator via Cargo features:
//!
//! | Feature | Allocator | Use Case |
//! |---------|-----------|----------|
//! | `heap-4` | FreeRTOS heap_4 | Single contiguous heap region |
//! | `heap-5` | FreeRTOS heap_5 | Multiple non-contiguous regions (PSRAM, TCM, etc.) |
//! | `alloc` | Rust global allocator | When you have an existing allocator |
//! | (none) | Stubs (panic) | Static-only allocation |
//!
//! ### heap-4 (Single region)
//!
//! The authentic FreeRTOS heap_4 implementation:
//! - First-fit allocation
//! - Coalesces adjacent free blocks to reduce fragmentation
//! - Fixed heap size (`configTOTAL_HEAP_SIZE` in config.rs)
//! - Deterministic timing (important for real-time systems)
//!
//! ```toml
//! [dependencies]
//! freertos-in-rust = { version = "0.1", features = ["heap-4"] }
//! ```
//!
//! ### heap-5 (Multiple regions)
//!
//! Like heap_4, but supports multiple non-contiguous memory regions:
//! - Ideal for systems with internal SRAM + external PSRAM
//! - Or Cortex-M7 with TCM + main RAM
//! - Must call `vPortDefineHeapRegions` before any allocation
//!
//! ```toml
//! [dependencies]
//! freertos-in-rust = { version = "0.1", features = ["heap-5"] }
//! ```
//!
//! ```ignore
//! use freertos_in_rust::memory::{vPortDefineHeapRegions, HeapRegion};
//!
//! static mut SRAM: [u8; 32768] = [0; 32768];
//! static mut PSRAM: [u8; 65536] = [0; 65536];
//!
//! unsafe {
//!     vPortDefineHeapRegions(&[
//!         HeapRegion::new(SRAM.as_mut_ptr(), SRAM.len()),
//!         HeapRegion::new(PSRAM.as_mut_ptr(), PSRAM.len()),
//!     ]);
//! }
//! ```
//!
//! ### alloc (Wrap existing allocator)
//!
//! If your application already has a `#[global_allocator]`, you can use it:
//!
//! ```toml
//! [dependencies]
//! freertos-in-rust = { version = "0.1", features = ["alloc"] }
//! ```
//!
//! Note: This wraps whatever allocator you provide. The kernel doesn't
//! know about heap size or fragmentation.
//!
//! ### Static-only (no heap)
//!
//! For fully static allocation, don't enable any heap feature:
//! - Use only `*Static` API variants (xTaskCreateStatic, etc.)
//! - pvPortMalloc will panic if called

// =============================================================================
// Heap Statistics (common to all implementations)
// =============================================================================

/// Heap statistics
#[repr(C)]
pub struct HeapStats_t {
    /// Total available heap space (sum of all free blocks)
    pub xAvailableHeapSpaceInBytes: usize,
    /// Size of largest free block
    pub xSizeOfLargestFreeBlockInBytes: usize,
    /// Size of smallest free block
    pub xSizeOfSmallestFreeBlockInBytes: usize,
    /// Number of free blocks
    pub xNumberOfFreeBlocks: usize,
    /// Minimum ever free bytes
    pub xMinimumEverFreeBytesRemaining: usize,
    /// Number of successful allocations
    pub xNumberOfSuccessfulAllocations: usize,
    /// Number of successful frees
    pub xNumberOfSuccessfulFrees: usize,
}

/// Heap region definition for heap_5 style allocation
#[repr(C)]
pub struct HeapRegion_t {
    /// Start address of the region
    pub pucStartAddress: *mut u8,
    /// Size of the region in bytes
    pub xSizeInBytes: usize,
}

// =============================================================================
// Allocator Selection via Features
// =============================================================================

// Priority: heap-4 > heap-5 > alloc > stubs
// This ensures deterministic behavior when a specific heap is explicitly requested

#[cfg(feature = "heap-4")]
mod heap_4;

#[cfg(feature = "heap-4")]
pub use heap_4::*;

// Re-export FreeRtosAllocator for users to set as #[global_allocator]
#[cfg(feature = "heap-4")]
pub use heap_4::FreeRtosAllocator;

// =============================================================================
// heap-5: Multi-region allocator (when heap-4 is not enabled)
// =============================================================================

#[cfg(all(feature = "heap-5", not(feature = "heap-4")))]
mod heap_5;

#[cfg(all(feature = "heap-5", not(feature = "heap-4")))]
pub use heap_5::*;

#[cfg(all(feature = "heap-5", not(feature = "heap-4")))]
pub use heap_5::FreeRtosAllocator;

// Re-export HeapRegion type for heap-5 users
#[cfg(all(feature = "heap-5", not(feature = "heap-4")))]
pub use heap_5::HeapRegion;

// =============================================================================
// With alloc feature (but NOT heap-4 or heap-5): Use Rust's global allocator
// =============================================================================

#[cfg(all(feature = "alloc", not(any(feature = "heap-4", feature = "heap-5"))))]
mod alloc_impl {
    use alloc::alloc::{alloc, dealloc, Layout};
    use core::ffi::c_void;
    use core::ptr;
    use core::sync::atomic::{AtomicUsize, Ordering};

    /// Track total allocated bytes (approximate, for xPortGetFreeHeapSize)
    static ALLOCATED_BYTES: AtomicUsize = AtomicUsize::new(0);

    /// Allocate memory
    ///
    /// [AMENDMENT] This wraps Rust's global allocator. The returned pointer
    /// has 8-byte alignment (matching portBYTE_ALIGNMENT).
    pub unsafe fn pvPortMalloc(xWantedSize: usize) -> *mut c_void {
        if xWantedSize == 0 {
            return ptr::null_mut();
        }

        // Create layout with proper alignment
        // We store the size at the beginning for deallocation
        let total_size = xWantedSize + core::mem::size_of::<usize>();
        let layout = match Layout::from_size_align(total_size, crate::port::portBYTE_ALIGNMENT) {
            Ok(l) => l,
            Err(_) => return ptr::null_mut(),
        };

        let ptr = alloc(layout);
        if ptr.is_null() {
            return ptr::null_mut();
        }

        // Store the size at the beginning
        *(ptr as *mut usize) = total_size;
        ALLOCATED_BYTES.fetch_add(total_size, Ordering::Relaxed);

        // Return pointer after the size field
        ptr.add(core::mem::size_of::<usize>()) as *mut c_void
    }

    /// Free previously allocated memory
    pub unsafe fn vPortFree(pv: *mut c_void) {
        if pv.is_null() {
            return;
        }

        // Get the original pointer (before size field)
        let size_ptr = (pv as *mut u8).sub(core::mem::size_of::<usize>()) as *mut usize;
        let total_size = *size_ptr;

        let layout = Layout::from_size_align_unchecked(total_size, crate::port::portBYTE_ALIGNMENT);

        ALLOCATED_BYTES.fetch_sub(total_size, Ordering::Relaxed);
        dealloc(size_ptr as *mut u8, layout);
    }

    /// Get approximate free heap size
    ///
    /// [AMENDMENT] With Rust's global allocator, we can't accurately report
    /// free heap. Returns a large placeholder value.
    pub fn xPortGetFreeHeapSize() -> usize {
        usize::MAX - ALLOCATED_BYTES.load(Ordering::Relaxed)
    }

    /// Get minimum ever free heap size
    pub fn xPortGetMinimumEverFreeHeapSize() -> usize {
        xPortGetFreeHeapSize()
    }

    /// Allocate and zero memory
    pub unsafe fn pvPortCalloc(xNum: usize, xSize: usize) -> *mut c_void {
        let total = match xNum.checked_mul(xSize) {
            Some(t) => t,
            None => return ptr::null_mut(),
        };

        let ptr = pvPortMalloc(total);
        if !ptr.is_null() {
            ptr::write_bytes(ptr as *mut u8, 0, total);
        }
        ptr
    }

    /// Initialize heap blocks (no-op with global allocator)
    pub fn vPortInitialiseBlocks() {}

    /// Reset heap state (no-op with global allocator)
    pub fn vPortHeapResetState() {}

    /// Reset minimum ever free heap tracking
    pub fn xPortResetHeapMinimumEverFreeHeapSize() {}

    /// Get heap statistics (placeholder values)
    pub fn vPortGetHeapStats(pxHeapStats: *mut super::HeapStats_t) {
        unsafe {
            (*pxHeapStats).xAvailableHeapSpaceInBytes = xPortGetFreeHeapSize();
            (*pxHeapStats).xSizeOfLargestFreeBlockInBytes = xPortGetFreeHeapSize();
            (*pxHeapStats).xSizeOfSmallestFreeBlockInBytes = 0;
            (*pxHeapStats).xNumberOfFreeBlocks = 1;
            (*pxHeapStats).xMinimumEverFreeBytesRemaining = xPortGetMinimumEverFreeHeapSize();
            (*pxHeapStats).xNumberOfSuccessfulAllocations = 0;
            (*pxHeapStats).xNumberOfSuccessfulFrees = 0;
        }
    }
}

#[cfg(all(feature = "alloc", not(any(feature = "heap-4", feature = "heap-5"))))]
pub use alloc_impl::*;

// =============================================================================
// Without any heap feature: Stub implementations that panic
// =============================================================================

#[cfg(not(any(feature = "heap-4", feature = "heap-5", feature = "alloc")))]
mod stub_impl {
    use core::ffi::c_void;
    use core::ptr;

    /// Allocate memory (stub - panics)
    pub unsafe fn pvPortMalloc(_xWantedSize: usize) -> *mut c_void {
        panic!("pvPortMalloc called but no heap feature is enabled (use `heap-4` or `alloc`)");
    }

    /// Free memory (stub - panics)
    pub unsafe fn vPortFree(_pv: *mut c_void) {
        panic!("vPortFree called but no heap feature is enabled");
    }

    /// Get free heap size (stub)
    pub fn xPortGetFreeHeapSize() -> usize {
        0
    }

    /// Get minimum ever free heap size (stub)
    pub fn xPortGetMinimumEverFreeHeapSize() -> usize {
        0
    }

    /// Allocate and zero memory (stub - panics)
    pub unsafe fn pvPortCalloc(_xNum: usize, _xSize: usize) -> *mut c_void {
        panic!("pvPortCalloc called but no heap feature is enabled");
    }

    /// Initialize heap blocks (stub)
    pub fn vPortInitialiseBlocks() {}

    /// Reset heap state (stub)
    pub fn vPortHeapResetState() {}

    /// Reset minimum ever free heap tracking (stub)
    pub fn xPortResetHeapMinimumEverFreeHeapSize() {}

    /// Get heap statistics (stub)
    pub fn vPortGetHeapStats(pxHeapStats: *mut super::HeapStats_t) {
        unsafe {
            (*pxHeapStats).xAvailableHeapSpaceInBytes = 0;
            (*pxHeapStats).xSizeOfLargestFreeBlockInBytes = 0;
            (*pxHeapStats).xSizeOfSmallestFreeBlockInBytes = 0;
            (*pxHeapStats).xNumberOfFreeBlocks = 0;
            (*pxHeapStats).xMinimumEverFreeBytesRemaining = 0;
            (*pxHeapStats).xNumberOfSuccessfulAllocations = 0;
            (*pxHeapStats).xNumberOfSuccessfulFrees = 0;
        }
    }
}

#[cfg(not(any(feature = "heap-4", feature = "heap-5", feature = "alloc")))]
pub use stub_impl::*;

// =============================================================================
// Common Functions (defined once, use the selected implementation)
// =============================================================================

// Note: vPortDefineHeapRegions is provided by heap_5 module when that feature is enabled
