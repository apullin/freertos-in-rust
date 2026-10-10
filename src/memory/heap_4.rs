/*
 * FreeRTOS Kernel <DEVELOPMENT BRANCH>
 * Copyright (C) 2021 Amazon.com, Inc. or its affiliates. All Rights Reserved.
 *
 * SPDX-License-Identifier: MIT
 *
 * [AMENDMENT] This is a line-by-line port of heap_4.c to Rust.
 * heap_4 provides a first-fit allocator with coalescing of adjacent
 * free blocks on free(), which reduces fragmentation.
 */

//! FreeRTOS heap_4 Allocator
//!
//! A first-fit allocator that coalesces adjacent free blocks.
//! This is the most commonly used FreeRTOS heap implementation.
//!
//! ## Features
//! - First-fit allocation strategy
//! - Coalesces adjacent free blocks on free() to reduce fragmentation
//! - O(n) allocation where n = number of free blocks
//! - O(n) free (must find insertion point in sorted list)
//! - Fixed heap size defined at compile time
//!
//! ## Usage
//! Enable with the `heap-4` Cargo feature. Mutually exclusive with
//! using Rust's `#[global_allocator]` for FreeRTOS allocations.

use core::ffi::c_void;
use core::ptr;

use crate::config::configTOTAL_HEAP_SIZE;
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
// Heap Storage
// =============================================================================

/// The heap memory - a static byte array
///
/// \[AMENDMENT\] In C, this can be placed in a specific section.
/// In Rust, you'd use #[link_section] if needed.
static mut UC_HEAP: [u8; configTOTAL_HEAP_SIZE] = [0u8; configTOTAL_HEAP_SIZE];

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
// Helper Macros as Functions
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
// Heap Initialization
// =============================================================================

/// Initialize the heap on first use
///
/// Sets up the free list with a single block spanning the entire heap.
unsafe fn prv_heap_init() {
    // [AMENDMENT] Keep writable provenance; as_ptr() would borrow the heap shared.
    let mut ux_start_address = ptr::addr_of_mut!(UC_HEAP).cast::<u8>() as usize;
    let mut x_total_heap_size = configTOTAL_HEAP_SIZE;

    // Ensure the heap starts on a correctly aligned boundary
    if (ux_start_address & PORT_BYTE_ALIGNMENT_MASK) != 0 {
        ux_start_address += PORT_BYTE_ALIGNMENT_MASK;
        ux_start_address &= !PORT_BYTE_ALIGNMENT_MASK;
        x_total_heap_size -= ux_start_address - (ptr::addr_of_mut!(UC_HEAP).cast::<u8>() as usize);
    }

    // Calculate end address (leaving room for end marker)
    let mut ux_end_address = ux_start_address + x_total_heap_size;
    ux_end_address -= HEAP_STRUCT_SIZE;
    ux_end_address &= !PORT_BYTE_ALIGNMENT_MASK;

    // Set up the end marker
    PX_END = ux_end_address as *mut BlockLink;
    (*PX_END).xBlockSize = 0;
    (*PX_END).pxNextFreeBlock = ptr::null_mut();

    // Set up the start sentinel to point to the first free block
    X_START.pxNextFreeBlock = ux_start_address as *mut BlockLink;
    X_START.xBlockSize = 0;

    // Create the first (and only) free block spanning the usable heap
    let px_first_free_block = ux_start_address as *mut BlockLink;
    (*px_first_free_block).xBlockSize = ux_end_address - ux_start_address;
    (*px_first_free_block).pxNextFreeBlock = PX_END;

    // Initialize tracking variables
    X_MINIMUM_EVER_FREE_BYTES_REMAINING = (*px_first_free_block).xBlockSize;
    X_FREE_BYTES_REMAINING = (*px_first_free_block).xBlockSize;
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
/// The port's scheduler-suspension primitives must be initialized and usable
/// from the current task context. A non-null result designates exactly
/// `x_wanted_size` writable, uninitialized bytes and must be passed exactly
/// once to [`vPortFree`] when no access can remain.
pub unsafe fn pvPortMalloc(x_wanted_size: usize) -> *mut c_void {
    let mut pv_return: *mut c_void = ptr::null_mut();
    let mut x_wanted_size = x_wanted_size;

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
        // Initialize heap on first call
        if PX_END.is_null() {
            prv_heap_init();
        }

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

                    // Insert remainder back into free list
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
/// The same scheduler/port requirements as [`pvPortMalloc`] apply. A non-null
/// result owns exactly `x_num * x_size` writable zeroed bytes and must be
/// passed exactly once to [`vPortFree`].
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
                    if block_size < x_min_size {
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
         * matching the critical section in upstream heap_4.c. */
        taskENTER_CRITICAL();
        (*px_heap_stats).xAvailableHeapSpaceInBytes = X_FREE_BYTES_REMAINING;
        (*px_heap_stats).xNumberOfSuccessfulAllocations = X_NUMBER_OF_SUCCESSFUL_ALLOCATIONS;
        (*px_heap_stats).xNumberOfSuccessfulFrees = X_NUMBER_OF_SUCCESSFUL_FREES;
        (*px_heap_stats).xMinimumEverFreeBytesRemaining = X_MINIMUM_EVER_FREE_BYTES_REMAINING;
        taskEXIT_CRITICAL();
    }
}

/// Initialize heap (no-op for heap_4 - auto-initializes on first malloc)
pub fn vPortInitialiseBlocks() {
    // No-op - heap initializes on first pvPortMalloc call
}

/// Reset heap state for a scheduler restart.
///
/// # Safety
///
/// There must be no live allocations, no allocator operation may be in
/// progress, and the scheduler must be stopped. Every pointer previously
/// returned by this heap becomes invalid for allocator operations.
pub unsafe fn vPortHeapResetState() {
    unsafe {
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

/// Global allocator that wraps FreeRTOS heap_4
///
/// \[AMENDMENT\] This allows Rust's `alloc` crate (Vec, Box, etc.) to use
/// the FreeRTOS heap_4 allocator. Applications should add:
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

/// Allocate a Rust `Layout` from heap_4 while retaining the original pointer
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
        // heap_4 doesn't have realloc, so allocate new, copy, free old
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
