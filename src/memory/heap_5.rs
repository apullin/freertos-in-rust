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

use crate::kernel::tasks::{
    taskENTER_CRITICAL, taskEXIT_CRITICAL, vTaskSuspendAll, xTaskResumeAll,
};
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
    /// Create a new heap region descriptor.
    ///
    /// Constructing a descriptor does not inspect or dereference `start`.
    /// Region validity and exclusivity become safety requirements only when
    /// the descriptor is passed to [`vPortDefineHeapRegions`].
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

/// Return the aligned start, end-marker address, and usable block size for a
/// region. Empty, undersized, or overflowing regions are rejected without
/// touching their storage.
fn prv_region_bounds(region: &HeapRegion) -> Option<(usize, usize, usize)> {
    if region.x_size_in_bytes == 0 || region.puc_start_address.is_null() {
        return None;
    }

    let original_address = region.puc_start_address as usize;
    let adjustment = if (original_address & PORT_BYTE_ALIGNMENT_MASK) != 0 {
        portBYTE_ALIGNMENT - (original_address & PORT_BYTE_ALIGNMENT_MASK)
    } else {
        0
    };
    let ux_address = original_address.checked_add(adjustment)?;
    let x_total_region_size = region.x_size_in_bytes.checked_sub(adjustment)?;

    let ux_region_end = ux_address.checked_add(x_total_region_size)?;
    let ux_end_address = ux_region_end.checked_sub(HEAP_STRUCT_SIZE)? & !PORT_BYTE_ALIGNMENT_MASK;
    let block_size = ux_end_address.checked_sub(ux_address)?;

    if block_size <= HEAP_MINIMUM_BLOCK_SIZE {
        return None;
    }

    Some((ux_address, ux_end_address, block_size))
}

/// Define the memory regions that make up the heap
///
/// This function must be called before any calls to `pvPortMalloc`.
/// Regions must be provided in ascending address order.
///
/// # Safety
/// - Every non-empty region must be a valid, writable, non-wrapping byte range
///   reserved exclusively for this allocator until the heap is reset.
/// - Regions must not overlap and must be in strictly ascending address order.
/// - No allocator operation may run concurrently with initialization.
/// - This function must only be called once between heap resets.
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

    configASSERT!(PX_END.is_null(), "Heap regions may only be defined once");

    /* [AMENDMENT] Validate the complete region set before writing any block
     * headers. The C implementation relies on valid region sizes; the Rust
     * translation must additionally prevent integer underflow/overflow from
     * creating a wild end marker for a tiny or wrapping region. */
    let mut x_total_heap_size: usize = 0;
    let mut ux_previous_end_address: Option<usize> = None;

    for region in regions.iter() {
        if region.x_size_in_bytes != 0 {
            configASSERT!(
                !region.puc_start_address.is_null(),
                "Non-empty heap regions must have a non-null start address"
            );
        }

        if let Some((ux_address, ux_end_address, block_size)) = prv_region_bounds(region) {
            if let Some(previous_end) = ux_previous_end_address {
                configASSERT!(
                    ux_address > previous_end,
                    "Heap regions must be non-overlapping and in ascending address order"
                );
            }

            x_total_heap_size = match x_total_heap_size.checked_add(block_size) {
                Some(size) => size,
                None => panic!("Total heap region size overflow"),
            };
            ux_previous_end_address = Some(ux_end_address);
        }
    }

    configASSERT!(
        x_total_heap_size > 0,
        "At least one usable heap region must be provided"
    );

    X_START.pxNextFreeBlock = ptr::null_mut();
    X_START.xBlockSize = 0;

    /* Process the already-validated regions. As in upstream heap_5, each prior
     * region's zero-sized end marker links to the first free block in the next
     * region. */
    for region in regions.iter() {
        let Some((ux_address, ux_end_address, block_size)) = prv_region_bounds(region) else {
            continue;
        };

        let px_previous_end = PX_END;
        PX_END = ux_end_address as *mut BlockLink;
        (*PX_END).xBlockSize = 0;
        (*PX_END).pxNextFreeBlock = ptr::null_mut();

        let px_first_free_block_in_region = ux_address as *mut BlockLink;
        (*px_first_free_block_in_region).xBlockSize = block_size;
        (*px_first_free_block_in_region).pxNextFreeBlock = PX_END;

        if px_previous_end.is_null() {
            X_START.pxNextFreeBlock = px_first_free_block_in_region;
        } else {
            (*px_previous_end).pxNextFreeBlock = px_first_free_block_in_region;
        }
    }

    X_MINIMUM_EVER_FREE_BYTES_REMAINING = x_total_heap_size;
    X_FREE_BYTES_REMAINING = x_total_heap_size;
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
///
/// [`vPortDefineHeapRegions`] must have initialized valid regions and the
/// port's scheduler-suspension primitives must be usable in the current task
/// context. A non-null result designates exactly `x_wanted_size` writable,
/// uninitialized bytes and must be passed exactly once to [`vPortFree`].
pub unsafe fn pvPortMalloc(x_wanted_size: usize) -> *mut c_void {
    let mut pv_return: *mut c_void = ptr::null_mut();
    let mut x_wanted_size = x_wanted_size;

    // Check that heap has been initialized
    configASSERT!(
        !PX_END.is_null(),
        "Heap not initialized - call vPortDefineHeapRegions first"
    );

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
                pv_return = ((*px_previous_block).pxNextFreeBlock as *mut u8).add(HEAP_STRUCT_SIZE)
                    as *mut c_void;

                // Remove this block from the free list
                (*px_previous_block).pxNextFreeBlock = (*px_block).pxNextFreeBlock;

                // Can we split this block?
                let block_size = heap_get_block_size(&*px_block);
                if block_size - x_wanted_size > HEAP_MINIMUM_BLOCK_SIZE {
                    // Split: create new block after our allocation
                    let px_new_block = (px_block as *mut u8).add(x_wanted_size) as *mut BlockLink;
                    (*px_new_block).xBlockSize = block_size - x_wanted_size;

                    // Update our block size
                    (*px_block).xBlockSize = x_wanted_size;

                    /* Insert the remainder directly at the removed block's
                     * former position.  Do not run coalescing here: upstream
                     * deliberately preserves the zero-sized link marker at an
                     * intermediate heap_5 region boundary. */
                    (*px_new_block).pxNextFreeBlock = (*px_previous_block).pxNextFreeBlock;
                    (*px_previous_block).pxNextFreeBlock = px_new_block;
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
///
/// Apart from null, `pv` must be the live pointer returned by this heap's
/// [`pvPortMalloc`] or [`pvPortCalloc`], must not have been freed already, and
/// no pointer derived from the allocation may be accessed after this call.
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
    #[cfg(feature = "smp")]
    let saved = crate::port::portSET_INTERRUPT_MASK_FROM_ISR();
    let bytes = unsafe { X_FREE_BYTES_REMAINING };
    #[cfg(feature = "smp")]
    crate::port::portCLEAR_INTERRUPT_MASK_FROM_ISR(saved);
    bytes
}

/// Get minimum ever free heap size (high water mark)
pub fn xPortGetMinimumEverFreeHeapSize() -> usize {
    #[cfg(feature = "smp")]
    let saved = crate::port::portSET_INTERRUPT_MASK_FROM_ISR();
    let bytes = unsafe { X_MINIMUM_EVER_FREE_BYTES_REMAINING };
    #[cfg(feature = "smp")]
    crate::port::portCLEAR_INTERRUPT_MASK_FROM_ISR(saved);
    bytes
}

/// Reset the minimum ever free heap tracking
pub fn xPortResetHeapMinimumEverFreeHeapSize() {
    #[cfg(feature = "smp")]
    let saved = crate::port::portSET_INTERRUPT_MASK_FROM_ISR();
    unsafe {
        X_MINIMUM_EVER_FREE_BYTES_REMAINING = X_FREE_BYTES_REMAINING;
    }
    #[cfg(feature = "smp")]
    crate::port::portCLEAR_INTERRUPT_MASK_FROM_ISR(saved);
}

/// Allocate and zero memory.
///
/// # Safety
///
/// The same initialization and port requirements as [`pvPortMalloc`] apply. A
/// non-null result owns exactly `x_num * x_size` writable zeroed bytes and must
/// be passed exactly once to [`vPortFree`].
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

/// Get detailed heap statistics.
///
/// # Safety
///
/// `px_heap_stats` must be non-null, aligned, and valid for one complete
/// `HeapStats_t` write, with no conflicting access for the duration.
pub unsafe fn vPortGetHeapStats(px_heap_stats: *mut super::HeapStats_t) {
    if px_heap_stats.is_null() {
        return;
    }

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

                    // Intermediate heap_5 region end markers have size zero and
                    // link to the next region; they are not usable free blocks.
                    if block_size != 0 && block_size < x_min_size {
                        x_min_size = block_size;
                    }

                    px_block = (*px_block).pxNextFreeBlock;
                }
            }
        }
        xTaskResumeAll();

        (*px_heap_stats).xSizeOfLargestFreeBlockInBytes = x_max_size;
        (*px_heap_stats).xSizeOfSmallestFreeBlockInBytes = x_min_size;
        (*px_heap_stats).xNumberOfFreeBlocks = x_blocks;

        /* Snapshot counters atomically with respect to allocator operations,
         * matching the critical section in upstream heap_5.c. */
        taskENTER_CRITICAL();
        (*px_heap_stats).xAvailableHeapSpaceInBytes = X_FREE_BYTES_REMAINING;
        (*px_heap_stats).xNumberOfSuccessfulAllocations = X_NUMBER_OF_SUCCESSFUL_ALLOCATIONS;
        (*px_heap_stats).xNumberOfSuccessfulFrees = X_NUMBER_OF_SUCCESSFUL_FREES;
        (*px_heap_stats).xMinimumEverFreeBytesRemaining = X_MINIMUM_EVER_FREE_BYTES_REMAINING;
        taskEXIT_CRITICAL();
    }
}

/// Initialize heap (no-op for heap_5 - use vPortDefineHeapRegions instead)
pub fn vPortInitialiseBlocks() {
    // No-op - use vPortDefineHeapRegions to initialize
}

/// Reset heap state for a scheduler restart.
///
/// # Safety
///
/// There must be no live allocations, no allocator operation may be in
/// progress, and the scheduler must be stopped. Every pointer previously
/// returned by this heap becomes invalid for allocator operations, and the
/// backing regions may only be reused after this call.
pub unsafe fn vPortHeapResetState() {
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
/// \[AMENDMENT\] This allows Rust's `alloc` crate (Vec, Box, etc.) to use
/// the FreeRTOS heap_5 allocator. Applications must call `vPortDefineHeapRegions`
/// before any allocations, then add:
///
/// ```ignore
/// #[global_allocator]
/// static ALLOCATOR: freertos_in_rust::memory::FreeRtosAllocator =
///     freertos_in_rust::memory::FreeRtosAllocator;
/// ```
///
/// This is a task-context allocator, not an ISR allocator. ISR allocation or
/// reallocation returns null; ISR deallocation deliberately leaks the block
/// without accessing the heap. Move owned allocations back to a task to free
/// them. These checks must not panic: GlobalAlloc may not unwind.
pub struct FreeRtosAllocator;

const GLOBAL_ALLOC_HEADER_SIZE: usize = core::mem::size_of::<*mut u8>();

/// Allocate a Rust `Layout` from heap_5 while retaining the original pointer
/// immediately before the aligned result for `dealloc`.
///
/// \[AMENDMENT\] `pvPortMalloc` only promises `portBYTE_ALIGNMENT`. Rust's
/// `GlobalAlloc` contract permits callers to request a larger alignment, so an
/// over-allocation and an unaligned metadata write are required here.
unsafe fn prv_alloc_layout(layout: Layout) -> *mut u8 {
    // [AMENDMENT] Scheduler suspension does not protect the heap from an ISR.
    if crate::port::xPortIsInsideInterrupt() != crate::types::pdFALSE {
        return ptr::null_mut();
    }
    let alignment = layout.align().max(portBYTE_ALIGNMENT);
    let allocation_size = match layout
        .size()
        .checked_add(GLOBAL_ALLOC_HEADER_SIZE)
        .and_then(|size| size.checked_add(alignment - 1))
    {
        Some(size) => size,
        None => return ptr::null_mut(),
    };

    let raw = pvPortMalloc(allocation_size) as *mut u8;
    if raw.is_null() {
        return ptr::null_mut();
    }

    let unaligned_address = raw.add(GLOBAL_ALLOC_HEADER_SIZE) as usize;
    let aligned_address = match unaligned_address.checked_add(alignment - 1) {
        Some(address) => address & !(alignment - 1),
        None => {
            vPortFree(raw as *mut c_void);
            return ptr::null_mut();
        }
    };
    let aligned = aligned_address as *mut u8;

    ptr::write_unaligned(aligned.sub(GLOBAL_ALLOC_HEADER_SIZE) as *mut *mut u8, raw);
    aligned
}

unsafe fn prv_free_layout(ptr: *mut u8) {
    if crate::port::xPortIsInsideInterrupt() != crate::types::pdFALSE {
        return;
    }
    if !ptr.is_null() {
        let raw = ptr::read_unaligned(ptr.sub(GLOBAL_ALLOC_HEADER_SIZE) as *const *mut u8);
        vPortFree(raw as *mut c_void);
    }
}

unsafe impl GlobalAlloc for FreeRtosAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        prv_alloc_layout(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
        prv_free_layout(ptr);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // heap_5 doesn't have realloc, so allocate new, copy, free old
        let new_layout = match Layout::from_size_align(new_size, layout.align()) {
            Ok(layout) => layout,
            Err(_) => return ptr::null_mut(),
        };
        let new_ptr = prv_alloc_layout(new_layout);
        if !new_ptr.is_null() && !ptr.is_null() {
            let copy_size = layout.size().min(new_size);
            ptr::copy_nonoverlapping(ptr, new_ptr, copy_size);
            prv_free_layout(ptr);
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
