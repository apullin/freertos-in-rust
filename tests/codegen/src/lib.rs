//! Optimized compiler-barrier regressions, checked by `xtask check-features`.
#![no_std]

use freertos_in_rust::port::*;

// These raw pointers model ordinary RAM shared with a task/ISR, synchronized
// by the port primitives. Volatile/atomic loads would hide a missing barrier.
#[no_mangle]
pub unsafe extern "C" fn read_across_yield(value: *const u32) -> u32 {
    let before = *value;
    portYIELD();
    (*value).wrapping_sub(before)
}

#[inline(always)]
unsafe fn masked_read(value: *const u32) -> u32 {
    let saved = portSET_INTERRUPT_MASK_FROM_ISR();
    let result = *value;
    portCLEAR_INTERRUPT_MASK_FROM_ISR(saved);
    result
}

#[no_mangle]
pub unsafe extern "C" fn read_in_separate_masked_regions(value: *const u32) -> u32 {
    let before = masked_read(value);
    masked_read(value).wrapping_sub(before)
}

#[no_mangle]
pub unsafe extern "C" fn read_across_memory_barrier(value: *const u32) -> u32 {
    let before = *value;
    portMEMORY_BARRIER();
    (*value).wrapping_sub(before)
}
