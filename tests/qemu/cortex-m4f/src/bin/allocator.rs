//! Bounded heap alignment, realloc, and overflow regressions.

#![no_std]
#![no_main]

extern crate panic_semihosting;

use core::alloc::{GlobalAlloc, Layout};

use cortex_m_rt::entry;
use freertos_in_rust::memory::{pvPortMalloc, vPortFree, FreeRtosAllocator};
use freertos_qemu_cortex_m4f::{finish, tap};

#[entry]
fn main() -> ! {
    let allocator = FreeRtosAllocator;
    let mut output = tap();

    unsafe {
        let raw = pvPortMalloc(37) as *mut u8;
        let raw_aligned = !raw.is_null()
            && (raw as usize % freertos_in_rust::port::portBYTE_ALIGNMENT) == 0;
        if !raw.is_null() {
            core::ptr::write_bytes(raw, 0xa5, 37);
            vPortFree(raw.cast());
        }
        output.check(raw_aligned, "raw_allocator_preserves_port_alignment");

        let layout = Layout::from_size_align(31, 64).unwrap();
        let allocation = allocator.alloc(layout);
        let over_aligned = !allocation.is_null() && (allocation as usize % 64) == 0;
        if !allocation.is_null() {
            let mut index = 0usize;
            while index < layout.size() {
                *allocation.add(index) = index as u8;
                index += 1;
            }
        }
        output.check(over_aligned, "global_allocator_honors_alignment_64");

        let grown = if allocation.is_null() {
            core::ptr::null_mut()
        } else {
            allocator.realloc(allocation, layout, 97)
        };
        let mut realloc_valid = !grown.is_null() && (grown as usize % 64) == 0;
        if !grown.is_null() {
            let mut index = 0usize;
            while index < layout.size() {
                if *grown.add(index) != index as u8 {
                    realloc_valid = false;
                    break;
                }
                index += 1;
            }
            allocator.dealloc(grown, Layout::from_size_align(97, 64).unwrap());
        }
        output.check(realloc_valid, "realloc_preserves_alignment_and_contents");

        output.check(
            pvPortMalloc(usize::MAX).is_null(),
            "allocation_overflow_returns_null",
        );
    }

    finish(output, 4)
}
