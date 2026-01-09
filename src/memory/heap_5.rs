/*
 * FreeRTOS Kernel <DEVELOPMENT BRANCH>
 * Copyright (C) 2021 Amazon.com, Inc. or its affiliates. All Rights Reserved.
 *
 * SPDX-License-Identifier: MIT
 *
 * [AMENDMENT] This is a line-by-line port of heap_5.c to Rust.
 * heap_5 provides a first-fit allocator with coalescing, supporting
 * multiple non-contiguous memory regions (e.g., internal SRAM + external PSRAM).
 */

//! FreeRTOS heap_5 Allocator
//!
//! A first-fit allocator that supports multiple non-contiguous memory regions.
//! This is ideal for systems with multiple RAM banks (internal SRAM, external
//! PSRAM, TCM, etc.).
//!
//! ## Features
//! - First-fit allocation strategy
//! - Coalesces adjacent free blocks to reduce fragmentation
//! - Supports multiple non-contiguous memory regions
//! - O(n) allocation where n = number of free blocks
//! - O(n) free (must find insertion point in sorted list)
//!
//! ## Usage
//!
//! Enable with the `heap-5` Cargo feature. You must call `vPortDefineHeapRegions`
//! before any allocation:
//!
//! ```ignore
//! use freertos_in_rust::memory::{vPortDefineHeapRegions, HeapRegion};
//!
//! // Define your memory regions (must be in ascending address order)
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

use core::ffi::c_void;
use core::ptr;

use crate::kernel::tasks::{vTaskSuspendAll, xTaskResumeAll};
use crate::port::portBYTE_ALIGNMENT;

// =============================================================================
// Constants
// =============================================================================

/// Alignment mask for checking alignment
const PORT_BYTE_ALIGNMENT_MASK: usize = portBYTE_ALIGNMENT - 1;

/// Minimum block size - must be at least twice the header size
/// to allow splitting blocks
const HEAP_MINIMUM_BLOCK_SIZE: usize = HEAP_STRUCT_SIZE << 1;

/// MSB used to mark a block as allocated
const HEAP_BLOCK_ALLOCATED_BITMASK: usize = 1 << (usize::BITS - 1);

/// Size of the block header, properly aligned
const HEAP_STRUCT_SIZE: usize = {
    let base = core::mem::size_of::<BlockLink>();
    (base + PORT_BYTE_ALIGNMENT_MASK) & !PORT_BYTE_ALIGNMENT_MASK
};

// =============================================================================
// Block Link Structure
// =============================================================================

/// Free block header - stored at the start of each free block
///
/// The free list is sorted by memory address to enable coalescing.
#[repr(C)]
struct BlockLink {
    /// Pointer to next free block (or null if end)
    pxNextFreeBlock: *mut BlockLink,
    /// Size of this block (including header). MSB = allocated flag.
    xBlockSize: usize,
}

impl BlockLink {
    const fn new() -> Self {
        BlockLink {
            pxNextFreeBlock: ptr::null_mut(),
            xBlockSize: 0,
        }
    }
}

// =============================================================================
// Heap Region Definition
// =============================================================================

/// Memory region descriptor for heap_5
///
/// Used to define multiple non-contiguous memory regions that will be
/// combined into a single heap.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct HeapRegion {
    /// Start address of the region
    pub puc_start_address: *mut u8,
    /// Size of the region in bytes
    pub x_size_in_bytes: usize,
}

impl HeapRegion {
    /// Create a new heap region descriptor
    ///
    /// # Safety
    /// The memory region must be valid and not used for any other purpose.
    pub const fn new(start: *mut u8, size: usize) -> Self {
        HeapRegion {
            puc_start_address: start,
            x_size_in_bytes: size,
        }
    }
}

// =============================================================================
// Heap State (no static storage - regions provided by user)
// =============================================================================

/// Start of the free list (sentinel node)
static mut X_START: BlockLink = BlockLink::new();

/// End marker of the free list
static mut PX_END: *mut BlockLink = ptr::null_mut();

/// Bytes remaining in the heap
static mut X_FREE_BYTES_REMAINING: usize = 0;

/// Minimum free bytes ever seen (high water mark)
static mut X_MINIMUM_EVER_FREE_BYTES_REMAINING: usize = 0;

/// Number of successful allocations
static mut X_NUMBER_OF_SUCCESSFUL_ALLOCATIONS: usize = 0;

/// Number of successful frees
static mut X_NUMBER_OF_SUCCESSFUL_FREES: usize = 0;

// =============================================================================
// Helper Functions
// =============================================================================

#[inline(always)]
fn heap_block_size_is_valid(size: usize) -> bool {
    (size & HEAP_BLOCK_ALLOCATED_BITMASK) == 0
}

#[inline(always)]
fn heap_block_is_allocated(block: &BlockLink) -> bool {
    (block.xBlockSize & HEAP_BLOCK_ALLOCATED_BITMASK) != 0
}

#[inline(always)]
unsafe fn heap_allocate_block(block: *mut BlockLink) {
    (*block).xBlockSize |= HEAP_BLOCK_ALLOCATED_BITMASK;
}

#[inline(always)]
unsafe fn heap_free_block(block: *mut BlockLink) {
    (*block).xBlockSize &= !HEAP_BLOCK_ALLOCATED_BITMASK;
}

#[inline(always)]
fn heap_get_block_size(block: &BlockLink) -> usize {
    block.xBlockSize & !HEAP_BLOCK_ALLOCATED_BITMASK
}

// =============================================================================
// Heap Initialization from Multiple Regions
// =============================================================================

/// Define the memory regions that make up the heap
///
/// This function must be called before any calls to `pvPortMalloc`.
/// Regions must be provided in ascending address order.
///
/// # Safety
/// - Each region must point to valid, unused memory
/// - Regions must not overlap
/// - Regions must be in ascending address order
/// - This function must only be called once
///
/// # Example
/// ```ignore
/// static mut SRAM: [u8; 32768] = [0; 32768];
/// static mut PSRAM: [u8; 65536] = [0; 65536];
///
/// unsafe {
///     vPortDefineHeapRegions(&[
///         HeapRegion::new(SRAM.as_mut_ptr(), SRAM.len()),
///         HeapRegion::new(PSRAM.as_mut_ptr(), PSRAM.len()),
///     ]);
/// }
/// ```
pub unsafe fn vPortDefineHeapRegions<const N: usize>(regions: &[HeapRegion; N]) {
    // Compile-time check that we have at least one region
    const { assert!(N > 0, "At least one heap region must be provided") };

    let mut px_first_free_block_in_region: *mut BlockLink;
    let mut px_previous_free_block: *mut BlockLink;
    let mut ux_address: usize;
    let mut x_total_region_size: usize;
    let mut x_total_heap_size: usize = 0;

    // Start with the sentinel pointing to nothing
    px_previous_free_block = &mut X_START as *mut BlockLink;

    // Process each region
    for region in regions.iter() {
        x_total_region_size = region.x_size_in_bytes;

        // Skip empty regions
        if x_total_region_size == 0 {
            continue;
        }

        ux_address = region.puc_start_address as usize;

        // Ensure the region starts on a correctly aligned boundary
        if (ux_address & PORT_BYTE_ALIGNMENT_MASK) != 0 {
            let adjustment = portBYTE_ALIGNMENT - (ux_address & PORT_BYTE_ALIGNMENT_MASK);
            ux_address += adjustment;

            // Reduce region size by alignment adjustment
            if x_total_region_size > adjustment {
                x_total_region_size -= adjustment;
            } else {
                continue; // Region too small after alignment
            }
        }

        // Calculate the end address, leaving room for the end marker
        let mut ux_end_address = ux_address + x_total_region_size;
        ux_end_address -= HEAP_STRUCT_SIZE;
        ux_end_address &= !PORT_BYTE_ALIGNMENT_MASK;

        // Ensure we have enough space for at least one block
        if ux_end_address <= ux_address + HEAP_MINIMUM_BLOCK_SIZE {
            continue; // Region too small
        }

        // Verify regions are in ascending address order
        #[cfg(debug_assertions)]
        {
            if !px_previous_free_block.is_null() && px_previous_free_block != &mut X_START as *mut BlockLink {
                debug_assert!(
                    ux_address > px_previous_free_block as usize,
                    "Heap regions must be in ascending address order"
                );
            }
        }

        // Set up the end marker for this region
        // Note: In heap_5, we have a single end marker at the highest region
        // For now, track the end marker location (will be set after all regions)
        PX_END = ux_end_address as *mut BlockLink;
        (*PX_END).xBlockSize = 0;
        (*PX_END).pxNextFreeBlock = ptr::null_mut();

        // Create the first free block in this region
        px_first_free_block_in_region = ux_address as *mut BlockLink;
        (*px_first_free_block_in_region).xBlockSize = ux_end_address - ux_address;
        (*px_first_free_block_in_region).pxNextFreeBlock = PX_END;

        // Link this region's free block to the previous region's last block
        (*px_previous_free_block).pxNextFreeBlock = px_first_free_block_in_region;

        // Update for next iteration
        px_previous_free_block = px_first_free_block_in_region;

        // Track total heap size
        x_total_heap_size += (*px_first_free_block_in_region).xBlockSize;
    }

    // Initialize tracking variables
    X_MINIMUM_EVER_FREE_BYTES_REMAINING = x_total_heap_size;
    X_FREE_BYTES_REMAINING = x_total_heap_size;

    // Initialize the start sentinel
    X_START.xBlockSize = 0;
}

// =============================================================================
// Insert Block Into Free List (with Coalescing)
// =============================================================================

/// Insert a freed block into the free list, coalescing with adjacent blocks
///
/// The free list is sorted by address, so we find the right position and
/// merge with neighbors if they're contiguous in memory.
unsafe fn prv_insert_block_into_free_list(px_block_to_insert: *mut BlockLink) {
    // Find the block that should come before this one (by address)
    let mut px_iterator = &mut X_START as *mut BlockLink;

    while (*px_iterator).pxNextFreeBlock < px_block_to_insert {
        px_iterator = (*px_iterator).pxNextFreeBlock;
    }

    // Check if we can merge with the block before us
    let puc_iterator = px_iterator as *mut u8;
    let iterator_block_size = heap_get_block_size(&*px_iterator);

    if puc_iterator.add(iterator_block_size) == px_block_to_insert as *mut u8 {
        // Merge with previous block
        (*px_iterator).xBlockSize += heap_get_block_size(&*px_block_to_insert);
        // Now px_block_to_insert points to the merged block
        let px_block_to_insert = px_iterator;

        // Check if we can also merge with the block after us
        let puc_block = px_block_to_insert as *mut u8;
        let block_size = heap_get_block_size(&*px_block_to_insert);
        let px_next = (*px_iterator).pxNextFreeBlock;

        if puc_block.add(block_size) == px_next as *mut u8 && px_next != PX_END {
            // Merge with next block too
            (*px_block_to_insert).xBlockSize += heap_get_block_size(&*px_next);
            (*px_block_to_insert).pxNextFreeBlock = (*px_next).pxNextFreeBlock;
        }
    } else {
        // Can't merge with previous - check if we can merge with next
        let puc_block = px_block_to_insert as *mut u8;
        let block_size = heap_get_block_size(&*px_block_to_insert);
        let px_next = (*px_iterator).pxNextFreeBlock;

        if puc_block.add(block_size) == px_next as *mut u8 && px_next != PX_END {
            // Merge with next block
            (*px_block_to_insert).xBlockSize += heap_get_block_size(&*px_next);
            (*px_block_to_insert).pxNextFreeBlock = (*px_next).pxNextFreeBlock;
        } else {
            // No merge possible - just link into the list
            (*px_block_to_insert).pxNextFreeBlock = px_next;
        }

        // Link the previous block to this one
        (*px_iterator).pxNextFreeBlock = px_block_to_insert;
    }
}

// =============================================================================
// Public API
// =============================================================================

/// Allocate memory from the heap
///
/// Uses first-fit algorithm: walks the free list and returns the first
/// block that's large enough. Splits the block if it's significantly larger
/// than needed.
///
/// # Safety
/// - `vPortDefineHeapRegions` must have been called first
/// - The returned pointer must be freed with [`vPortFree`]
pub unsafe fn pvPortMalloc(x_wanted_size: usize) -> *mut c_void {
    let mut pv_return: *mut c_void = ptr::null_mut();
    let mut x_wanted_size = x_wanted_size;

    // Check that heap has been initialized
    configASSERT!(!PX_END.is_null(), "Heap not initialized - call vPortDefineHeapRegions first");

    if x_wanted_size > 0 {
        // Add space for the block header
        if let Some(size) = x_wanted_size.checked_add(HEAP_STRUCT_SIZE) {
            x_wanted_size = size;

            // Align up to required alignment
            if (x_wanted_size & PORT_BYTE_ALIGNMENT_MASK) != 0 {
                let additional = portBYTE_ALIGNMENT - (x_wanted_size & PORT_BYTE_ALIGNMENT_MASK);
                if let Some(size) = x_wanted_size.checked_add(additional) {
                    x_wanted_size = size;
                } else {
                    x_wanted_size = 0; // Overflow
                }
            }
        } else {
            x_wanted_size = 0; // Overflow
        }
    }

    vTaskSuspendAll();
    {
        // Check the size is valid (MSB not set) and we have enough space
        if heap_block_size_is_valid(x_wanted_size)
            && x_wanted_size > 0
            && x_wanted_size <= X_FREE_BYTES_REMAINING
        {
            // Walk the free list to find a suitable block (first-fit)
            let mut px_previous_block = &mut X_START as *mut BlockLink;
            let mut px_block = X_START.pxNextFreeBlock;

            while heap_get_block_size(&*px_block) < x_wanted_size
                && !(*px_block).pxNextFreeBlock.is_null()
            {
                px_previous_block = px_block;
                px_block = (*px_block).pxNextFreeBlock;
            }

            // Did we find a block?
            if px_block != PX_END {
                // Return the memory after the header
                pv_return = ((*px_previous_block).pxNextFreeBlock as *mut u8)
                    .add(HEAP_STRUCT_SIZE) as *mut c_void;

                // Remove this block from the free list
                (*px_previous_block).pxNextFreeBlock = (*px_block).pxNextFreeBlock;

                // Can we split this block?
                let block_size = heap_get_block_size(&*px_block);
                if block_size - x_wanted_size > HEAP_MINIMUM_BLOCK_SIZE {
                    // Split: create new block after our allocation
                    let px_new_block =
                        (px_block as *mut u8).add(x_wanted_size) as *mut BlockLink;
                    (*px_new_block).xBlockSize = block_size - x_wanted_size;

                    // Update our block size
                    (*px_block).xBlockSize = x_wanted_size;

                    // Insert remainder back into free list
                    prv_insert_block_into_free_list(px_new_block);
                }

                // Update stats
                X_FREE_BYTES_REMAINING -= heap_get_block_size(&*px_block);
                if X_FREE_BYTES_REMAINING < X_MINIMUM_EVER_FREE_BYTES_REMAINING {
                    X_MINIMUM_EVER_FREE_BYTES_REMAINING = X_FREE_BYTES_REMAINING;
                }

                // Mark block as allocated
                heap_allocate_block(px_block);
                (*px_block).pxNextFreeBlock = ptr::null_mut();
                X_NUMBER_OF_SUCCESSFUL_ALLOCATIONS += 1;
            }
        }
    }
    xTaskResumeAll();

    pv_return
}

/// Free previously allocated memory
///
/// Returns the block to the free list and coalesces with adjacent free blocks.
///
/// # Safety
/// `pv` must have been returned by [`pvPortMalloc`] and not yet freed.
pub unsafe fn vPortFree(pv: *mut c_void) {
    if pv.is_null() {
        return;
    }

    // The block header is just before the returned pointer
    let puc = pv as *mut u8;
    let px_link = puc.sub(HEAP_STRUCT_SIZE) as *mut BlockLink;

    // Validate the block
    debug_assert!(
        heap_block_is_allocated(&*px_link),
        "vPortFree: block not allocated"
    );
    debug_assert!(
        (*px_link).pxNextFreeBlock.is_null(),
        "vPortFree: block still in free list"
    );

    if heap_block_is_allocated(&*px_link) && (*px_link).pxNextFreeBlock.is_null() {
        // Clear the allocated flag
        heap_free_block(px_link);

        vTaskSuspendAll();
        {
            // Add back to free bytes
            X_FREE_BYTES_REMAINING += heap_get_block_size(&*px_link);

            // Insert into free list (with coalescing)
            prv_insert_block_into_free_list(px_link);

            X_NUMBER_OF_SUCCESSFUL_FREES += 1;
        }
        xTaskResumeAll();
    }
}

/// Get current free heap size
pub fn xPortGetFreeHeapSize() -> usize {
    unsafe { X_FREE_BYTES_REMAINING }
}

/// Get minimum ever free heap size (high water mark)
pub fn xPortGetMinimumEverFreeHeapSize() -> usize {
    unsafe { X_MINIMUM_EVER_FREE_BYTES_REMAINING }
}

/// Reset the minimum ever free heap tracking
pub fn xPortResetHeapMinimumEverFreeHeapSize() {
    unsafe {
        X_MINIMUM_EVER_FREE_BYTES_REMAINING = X_FREE_BYTES_REMAINING;
    }
}

/// Allocate and zero memory
pub unsafe fn pvPortCalloc(x_num: usize, x_size: usize) -> *mut c_void {
    let total = match x_num.checked_mul(x_size) {
        Some(t) => t,
        None => return ptr::null_mut(),
    };

    let pv = pvPortMalloc(total);
    if !pv.is_null() {
        ptr::write_bytes(pv as *mut u8, 0, total);
    }
    pv
}

/// Get detailed heap statistics
pub fn vPortGetHeapStats(px_heap_stats: *mut super::HeapStats_t) {
    let mut x_blocks: usize = 0;
    let mut x_max_size: usize = 0;
    let mut x_min_size: usize = usize::MAX;

    unsafe {
        vTaskSuspendAll();
        {
            let mut px_block = X_START.pxNextFreeBlock;

            // Walk the free list if heap is initialized
            if !px_block.is_null() {
                while px_block != PX_END {
                    x_blocks += 1;
                    let block_size = heap_get_block_size(&*px_block);

                    if block_size > x_max_size {
                        x_max_size = block_size;
                    }
                    if block_size < x_min_size {
                        x_min_size = block_size;
                    }

                    px_block = (*px_block).pxNextFreeBlock;
                }
            }
        }
        xTaskResumeAll();

        (*px_heap_stats).xSizeOfLargestFreeBlockInBytes = x_max_size;
        (*px_heap_stats).xSizeOfSmallestFreeBlockInBytes = if x_blocks > 0 { x_min_size } else { 0 };
        (*px_heap_stats).xNumberOfFreeBlocks = x_blocks;
        (*px_heap_stats).xAvailableHeapSpaceInBytes = X_FREE_BYTES_REMAINING;
        (*px_heap_stats).xNumberOfSuccessfulAllocations = X_NUMBER_OF_SUCCESSFUL_ALLOCATIONS;
        (*px_heap_stats).xNumberOfSuccessfulFrees = X_NUMBER_OF_SUCCESSFUL_FREES;
        (*px_heap_stats).xMinimumEverFreeBytesRemaining = X_MINIMUM_EVER_FREE_BYTES_REMAINING;
    }
}

/// Initialize heap (no-op for heap_5 - use vPortDefineHeapRegions instead)
pub fn vPortInitialiseBlocks() {
    // No-op - use vPortDefineHeapRegions to initialize
}

/// Reset heap state (for scheduler restart)
pub fn vPortHeapResetState() {
    unsafe {
        X_START.pxNextFreeBlock = ptr::null_mut();
        PX_END = ptr::null_mut();
        X_FREE_BYTES_REMAINING = 0;
        X_MINIMUM_EVER_FREE_BYTES_REMAINING = 0;
        X_NUMBER_OF_SUCCESSFUL_ALLOCATIONS = 0;
        X_NUMBER_OF_SUCCESSFUL_FREES = 0;
    }
}

// =============================================================================
// GlobalAlloc Implementation for Rust's alloc crate
// =============================================================================

use core::alloc::{GlobalAlloc, Layout};

/// Global allocator that wraps FreeRTOS heap_5
///
/// [AMENDMENT] This allows Rust's `alloc` crate (Vec, Box, etc.) to use
/// the FreeRTOS heap_5 allocator. Applications must call `vPortDefineHeapRegions`
/// before any allocations, then add:
///
/// ```ignore
/// #[global_allocator]
/// static ALLOCATOR: freertos_in_rust::memory::FreeRtosAllocator =
///     freertos_in_rust::memory::FreeRtosAllocator;
/// ```
pub struct FreeRtosAllocator;

unsafe impl GlobalAlloc for FreeRtosAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // heap_5 already aligns to portBYTE_ALIGNMENT (8 bytes)
        // If larger alignment is requested, we may waste some space
        pvPortMalloc(layout.size()) as *mut u8
    }

    unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
        vPortFree(ptr as *mut c_void);
    }

    unsafe fn realloc(&self, ptr: *mut u8, _layout: Layout, new_size: usize) -> *mut u8 {
        // heap_5 doesn't have realloc, so allocate new, copy, free old
        let new_ptr = pvPortMalloc(new_size) as *mut u8;
        if !new_ptr.is_null() && !ptr.is_null() {
            // Copy old data (use smaller of old and new size)
            let copy_size = _layout.size().min(new_size);
            ptr::copy_nonoverlapping(ptr, new_ptr, copy_size);
            vPortFree(ptr as *mut c_void);
        }
        new_ptr
    }
}

// =============================================================================
// configASSERT macro
// =============================================================================

macro_rules! configASSERT {
    ($cond:expr, $msg:expr) => {
        if !$cond {
            panic!($msg);
        }
    };
}
use configASSERT;
