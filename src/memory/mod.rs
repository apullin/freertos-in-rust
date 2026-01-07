/*
 * FreeRTOS Kernel <DEVELOPMENT BRANCH>
 * Copyright (C) 2021 Amazon.com, Inc. or its affiliates. All Rights Reserved.
 *
 * SPDX-License-Identifier: MIT
 *
 * [AMENDMENT] This module provides memory allocation for FreeRTOS.
 * The primary interface is pvPortMalloc/vPortFree.
 *
 * When the `alloc` feature is enabled, these wrap Rust's global allocator.
 * Without `alloc`, they are stubs that panic.
 */

//! Memory Allocation
//!
//! This module provides the FreeRTOS memory allocation interface:
//! - [`pvPortMalloc`] - Allocate memory
//! - [`vPortFree`] - Free memory
//! - [`xPortGetFreeHeapSize`] - Get remaining heap (stub)
//!
//! ## With `alloc` feature
//!
//! Uses Rust's global allocator via the `alloc` crate.
//!
//! ## Without `alloc` feature
//!
//! Stub implementations that panic at runtime.

use core::ffi::c_void;

// =============================================================================
// With alloc feature: Use Rust's global allocator
// =============================================================================

#[cfg(feature = "alloc")]
mod alloc_impl {
    use super::*;
    use alloc::alloc::{alloc, dealloc, Layout};
    use core::sync::atomic::{AtomicUsize, Ordering};

    /// Track total allocated bytes (approximate, for xPortGetFreeHeapSize)
    static ALLOCATED_BYTES: AtomicUsize = AtomicUsize::new(0);

    /// Allocate memory
    ///
    /// Equivalent to pvPortMalloc in FreeRTOS.
    ///
    /// # Safety
    ///
    /// Returns a raw pointer that must be freed with [`vPortFree`].
    /// The pointer is valid for `xWantedSize` bytes.
    ///
    /// [AMENDMENT] This wraps Rust's global allocator. The returned pointer
    /// has 8-byte alignment (matching portBYTE_ALIGNMENT).
    pub unsafe fn pvPortMalloc(xWantedSize: usize) -> *mut c_void {
        if xWantedSize == 0 {
            return core::ptr::null_mut();
        }

        // Create layout with proper alignment
        // We store the size at the beginning for deallocation
        let total_size = xWantedSize + core::mem::size_of::<usize>();
        let layout = match Layout::from_size_align(total_size, crate::port::portBYTE_ALIGNMENT) {
            Ok(l) => l,
            Err(_) => return core::ptr::null_mut(),
        };

        let ptr = alloc(layout);
        if ptr.is_null() {
            // Allocation failed
            #[cfg(feature = "alloc")]
            {
                if crate::config::configUSE_MALLOC_FAILED_HOOK != 0 {
                    // TODO: Call vApplicationMallocFailedHook
                }
            }
            return core::ptr::null_mut();
        }

        // Store the size at the beginning
        *(ptr as *mut usize) = total_size;
        ALLOCATED_BYTES.fetch_add(total_size, Ordering::Relaxed);

        // Return pointer after the size field
        ptr.add(core::mem::size_of::<usize>()) as *mut c_void
    }

    /// Free previously allocated memory
    ///
    /// Equivalent to vPortFree in FreeRTOS.
    ///
    /// # Safety
    ///
    /// `pv` must have been returned by [`pvPortMalloc`] and not yet freed.
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
    /// free heap. This returns a placeholder value.
    pub fn xPortGetFreeHeapSize() -> usize {
        // We don't know the actual heap size with global allocator
        // Return a large value to indicate "plenty available"
        usize::MAX - ALLOCATED_BYTES.load(Ordering::Relaxed)
    }

    /// Get minimum ever free heap size
    ///
    /// [AMENDMENT] Not accurately trackable with global allocator.
    pub fn xPortGetMinimumEverFreeHeapSize() -> usize {
        // Not tracked - return same as current
        xPortGetFreeHeapSize()
    }
}

#[cfg(feature = "alloc")]
pub use alloc_impl::*;

// =============================================================================
// Without alloc feature: Stub implementations
// =============================================================================

#[cfg(not(feature = "alloc"))]
mod stub_impl {
    use super::*;

    /// Allocate memory (stub - panics)
    ///
    /// # Panics
    ///
    /// Always panics because `alloc` feature is not enabled.
    pub unsafe fn pvPortMalloc(_xWantedSize: usize) -> *mut c_void {
        panic!("pvPortMalloc called but `alloc` feature is not enabled");
    }

    /// Free memory (stub - panics)
    ///
    /// # Panics
    ///
    /// Always panics because `alloc` feature is not enabled.
    pub unsafe fn vPortFree(_pv: *mut c_void) {
        panic!("vPortFree called but `alloc` feature is not enabled");
    }

    /// Get free heap size (stub)
    pub fn xPortGetFreeHeapSize() -> usize {
        0
    }

    /// Get minimum ever free heap size (stub)
    pub fn xPortGetMinimumEverFreeHeapSize() -> usize {
        0
    }
}

#[cfg(not(feature = "alloc"))]
pub use stub_impl::*;

// =============================================================================
// Common functions
// =============================================================================

/// Allocate and zero memory
///
/// Equivalent to pvPortCalloc in FreeRTOS.
///
/// # Safety
///
/// Same as [`pvPortMalloc`].
pub unsafe fn pvPortCalloc(xNum: usize, xSize: usize) -> *mut c_void {
    let total = match xNum.checked_mul(xSize) {
        Some(t) => t,
        None => return core::ptr::null_mut(),
    };

    let ptr = pvPortMalloc(total);
    if !ptr.is_null() {
        core::ptr::write_bytes(ptr as *mut u8, 0, total);
    }
    ptr
}

/// Initialize heap blocks (no-op with global allocator)
///
/// [AMENDMENT] This is a no-op when using Rust's global allocator.
/// It would be needed for FreeRTOS heap_1 through heap_5 implementations.
pub fn vPortInitialiseBlocks() {
    // No-op with global allocator
}

/// Reset heap state (no-op with global allocator)
///
/// [AMENDMENT] This is a no-op when using Rust's global allocator.
pub fn vPortHeapResetState() {
    // No-op with global allocator
}

/// Reset minimum ever free heap tracking
pub fn xPortResetHeapMinimumEverFreeHeapSize() {
    // No-op with global allocator
}

// =============================================================================
// Heap region support (for heap_5 style)
// =============================================================================

/// Heap region definition for heap_5 style allocation
#[repr(C)]
pub struct HeapRegion_t {
    /// Start address of the region
    pub pucStartAddress: *mut u8,
    /// Size of the region in bytes
    pub xSizeInBytes: usize,
}

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

/// Define heap regions (for heap_5)
///
/// [AMENDMENT] No-op with global allocator. Would be used with heap_5.
pub fn vPortDefineHeapRegions(_pxHeapRegions: *const HeapRegion_t) {
    // No-op with global allocator
}

/// Get heap statistics
///
/// [AMENDMENT] Returns placeholder values with global allocator.
pub fn vPortGetHeapStats(pxHeapStats: *mut HeapStats_t) {
    unsafe {
        (*pxHeapStats).xAvailableHeapSpaceInBytes = xPortGetFreeHeapSize();
        (*pxHeapStats).xSizeOfLargestFreeBlockInBytes = xPortGetFreeHeapSize();
        (*pxHeapStats).xSizeOfSmallestFreeBlockInBytes = 0;
        (*pxHeapStats).xNumberOfFreeBlocks = 1;
        (*pxHeapStats).xMinimumEverFreeBytesRemaining = xPortGetMinimumEverFreeHeapSize();
        (*pxHeapStats).xNumberOfSuccessfulAllocations = 0; // Not tracked
        (*pxHeapStats).xNumberOfSuccessfulFrees = 0; // Not tracked
    }
}
