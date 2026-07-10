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
//! freertos-in-rust = { version = "0.3", default-features = false, features = ["port-cortex-m4f", "heap-4"] }
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
//! freertos-in-rust = { version = "0.3", default-features = false, features = ["port-cortex-m4f", "heap-5"] }
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
//! freertos-in-rust = { version = "0.3", default-features = false, features = ["port-cortex-m4f", "alloc"] }
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

    /// Track total allocated bytes (approximate, for xPortGetFreeHeapSize)
    ///
    /// \[AMENDMENT\] This is protected by the port critical-section primitives
    /// rather than atomic read-modify-write operations. ARMv6-M exposes atomic
    /// loads and stores but not the `AtomicUsize::fetch_add/fetch_sub` operations
    /// used by the original Rust wrapper.
    static mut ALLOCATED_BYTES: usize = 0;

    const ALLOCATION_ALIGNMENT: usize = {
        let port_alignment = crate::port::portBYTE_ALIGNMENT;
        let metadata_alignment = core::mem::align_of::<usize>();
        if port_alignment > metadata_alignment {
            port_alignment
        } else {
            metadata_alignment
        }
    };

    const ALLOCATION_HEADER_SIZE: usize = {
        let mask = ALLOCATION_ALIGNMENT - 1;
        (core::mem::size_of::<usize>() + mask) & !mask
    };

    #[inline(always)]
    unsafe fn add_allocated_bytes(size: usize) {
        crate::port::portENTER_CRITICAL();
        ALLOCATED_BYTES += size;
        crate::port::portEXIT_CRITICAL();
    }

    #[inline(always)]
    unsafe fn subtract_allocated_bytes(size: usize) {
        crate::port::portENTER_CRITICAL();
        ALLOCATED_BYTES -= size;
        crate::port::portEXIT_CRITICAL();
    }

    #[inline(always)]
    fn allocated_bytes() -> usize {
        crate::port::portENTER_CRITICAL();
        let allocated = unsafe { ALLOCATED_BYTES };
        crate::port::portEXIT_CRITICAL();
        allocated
    }

    /// Allocate memory through Rust's global allocator.
    ///
    /// \[AMENDMENT\] This wraps Rust's global allocator. The allocation metadata
    /// occupies a complete alignment unit, so the returned pointer retains
    /// `portBYTE_ALIGNMENT` alignment on both 32-bit and 64-bit targets.
    ///
    /// # Safety
    ///
    /// The selected global allocator and port critical-section implementation
    /// must be initialized for the current execution context. A non-null result
    /// designates exactly `xWantedSize` writable, uninitialized bytes and must
    /// be passed exactly once to [`vPortFree`] when no access can remain.
    pub unsafe fn pvPortMalloc(xWantedSize: usize) -> *mut c_void {
        if xWantedSize == 0 {
            return ptr::null_mut();
        }

        // Create layout with proper alignment
        // We store the size at the beginning for deallocation
        let total_size = match xWantedSize.checked_add(ALLOCATION_HEADER_SIZE) {
            Some(size) => size,
            None => return ptr::null_mut(),
        };
        let layout = match Layout::from_size_align(total_size, ALLOCATION_ALIGNMENT) {
            Ok(l) => l,
            Err(_) => return ptr::null_mut(),
        };

        let ptr = alloc(layout);
        if ptr.is_null() {
            return ptr::null_mut();
        }

        // Store the size at the beginning
        *(ptr as *mut usize) = total_size;
        add_allocated_bytes(total_size);

        // Return pointer after the padded metadata header.
        ptr.add(ALLOCATION_HEADER_SIZE) as *mut c_void
    }

    /// Free previously allocated memory.
    ///
    /// # Safety
    ///
    /// Apart from null, `pv` must be the live pointer returned by this module's
    /// `pvPortMalloc`/`pvPortCalloc`, must not have been freed already, and no
    /// pointer derived from the allocation may be accessed after this call.
    pub unsafe fn vPortFree(pv: *mut c_void) {
        if pv.is_null() {
            return;
        }

        // Get the original pointer (before size field)
        let size_ptr = (pv as *mut u8).sub(ALLOCATION_HEADER_SIZE) as *mut usize;
        let total_size = *size_ptr;

        let layout = Layout::from_size_align_unchecked(total_size, ALLOCATION_ALIGNMENT);

        subtract_allocated_bytes(total_size);
        dealloc(size_ptr as *mut u8, layout);
    }

    /// Get approximate free heap size
    ///
    /// \[AMENDMENT\] With Rust's global allocator, we can't accurately report
    /// free heap. Returns a large placeholder value.
    pub fn xPortGetFreeHeapSize() -> usize {
        usize::MAX - allocated_bytes()
    }

    /// Get minimum ever free heap size
    pub fn xPortGetMinimumEverFreeHeapSize() -> usize {
        xPortGetFreeHeapSize()
    }

    /// Allocate and zero memory.
    ///
    /// # Safety
    ///
    /// The allocator and port must be initialized as required by
    /// [`pvPortMalloc`]. A non-null result owns exactly `xNum * xSize` writable
    /// zeroed bytes and must eventually be passed exactly once to
    /// [`vPortFree`].
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

    /// Reset heap state (no-op with global allocator).
    ///
    /// # Safety
    ///
    /// Kept unsafe for API consistency with the FreeRTOS heap backends, where
    /// reset requires exclusive allocator access and no live allocations.
    pub unsafe fn vPortHeapResetState() {}

    /// Reset minimum ever free heap tracking
    pub fn xPortResetHeapMinimumEverFreeHeapSize() {}

    /// Get heap statistics (placeholder values).
    ///
    /// # Safety
    ///
    /// `pxHeapStats` must be non-null, aligned, and valid for one complete
    /// `HeapStats_t` write, with no conflicting access for the duration.
    pub unsafe fn vPortGetHeapStats(pxHeapStats: *mut super::HeapStats_t) {
        if pxHeapStats.is_null() {
            return;
        }

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

    /// Allocate memory (stub - panics).
    ///
    /// # Safety
    ///
    /// This stub never returns. The signature matches allocator-enabled
    /// configurations so feature changes cannot silently weaken call sites.
    pub unsafe fn pvPortMalloc(_xWantedSize: usize) -> *mut c_void {
        panic!("pvPortMalloc called but no heap feature is enabled (use `heap-4` or `alloc`)");
    }

    /// Free memory (stub - panics).
    ///
    /// # Safety
    ///
    /// This stub never returns. The signature matches allocator-enabled
    /// configurations.
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

    /// Allocate and zero memory (stub - panics).
    ///
    /// # Safety
    ///
    /// This stub never returns. The signature matches allocator-enabled
    /// configurations.
    pub unsafe fn pvPortCalloc(_xNum: usize, _xSize: usize) -> *mut c_void {
        panic!("pvPortCalloc called but no heap feature is enabled");
    }

    /// Initialize heap blocks (stub)
    pub fn vPortInitialiseBlocks() {}

    /// Reset heap state (stub).
    ///
    /// # Safety
    ///
    /// Kept unsafe for API consistency with allocator-enabled configurations.
    pub unsafe fn vPortHeapResetState() {}

    /// Reset minimum ever free heap tracking (stub)
    pub fn xPortResetHeapMinimumEverFreeHeapSize() {}

    /// Get heap statistics (stub).
    ///
    /// # Safety
    ///
    /// `pxHeapStats` must be non-null, aligned, and valid for one complete
    /// `HeapStats_t` write, with no conflicting access for the duration.
    pub unsafe fn vPortGetHeapStats(pxHeapStats: *mut super::HeapStats_t) {
        if pxHeapStats.is_null() {
            return;
        }

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
