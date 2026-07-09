#![cfg(feature = "port-test")]

use core::ptr;

use freertos_in_rust::memory::{pvPortCalloc, pvPortMalloc, vPortFree, xPortGetFreeHeapSize};

#[cfg(any(feature = "heap-4", feature = "heap-5"))]
use core::alloc::{GlobalAlloc, Layout};
#[cfg(any(feature = "heap-4", feature = "heap-5"))]
use freertos_in_rust::memory::FreeRtosAllocator;

unsafe fn check_raw_allocator_contract() {
    let before = xPortGetFreeHeapSize();
    let allocation = pvPortMalloc(37) as *mut u8;
    assert!(!allocation.is_null());
    assert_eq!(
        allocation as usize % freertos_in_rust::port::portBYTE_ALIGNMENT,
        0
    );

    ptr::write_bytes(allocation, 0xa5, 37);
    for offset in 0..37 {
        assert_eq!(*allocation.add(offset), 0xa5);
    }

    vPortFree(allocation.cast());
    assert_eq!(xPortGetFreeHeapSize(), before);
    assert!(pvPortMalloc(usize::MAX).is_null());
    assert!(pvPortCalloc(usize::MAX, 2).is_null());
}

#[cfg(any(feature = "heap-4", feature = "heap-5"))]
unsafe fn check_global_allocator_contract<A: GlobalAlloc>(allocator: &A) {
    for alignment in [1, 8, 16, 32, 64] {
        let layout = Layout::from_size_align(37, alignment).unwrap();
        let allocation = allocator.alloc(layout);
        assert!(!allocation.is_null(), "alignment {alignment}");
        assert_eq!(allocation as usize % alignment, 0, "alignment {alignment}");
        ptr::write_bytes(allocation, alignment as u8, layout.size());
        allocator.dealloc(allocation, layout);
    }

    let original_layout = Layout::from_size_align(31, 64).unwrap();
    let original = allocator.alloc(original_layout);
    assert!(!original.is_null());
    for offset in 0..original_layout.size() {
        *original.add(offset) = offset as u8;
    }

    let grown = allocator.realloc(original, original_layout, 97);
    assert!(!grown.is_null());
    assert_eq!(grown as usize % original_layout.align(), 0);
    for offset in 0..original_layout.size() {
        assert_eq!(*grown.add(offset), offset as u8);
    }

    let grown_layout = Layout::from_size_align(97, original_layout.align()).unwrap();
    let shrunk = allocator.realloc(grown, grown_layout, 11);
    assert!(!shrunk.is_null());
    assert_eq!(shrunk as usize % grown_layout.align(), 0);
    for offset in 0..11 {
        assert_eq!(*shrunk.add(offset), offset as u8);
    }
    allocator.dealloc(
        shrunk,
        Layout::from_size_align(11, original_layout.align()).unwrap(),
    );

    // A failed realloc must leave the original allocation valid and owned by
    // the caller.
    let failure_layout = Layout::from_size_align(16, 8).unwrap();
    let original = allocator.alloc(failure_layout);
    assert!(!original.is_null());
    ptr::write_bytes(original, 0x5a, failure_layout.size());
    let failed = allocator.realloc(original, failure_layout, usize::MAX);
    assert!(failed.is_null());
    for offset in 0..failure_layout.size() {
        assert_eq!(*original.add(offset), 0x5a);
    }
    allocator.dealloc(original, failure_layout);
}

#[cfg(any(feature = "heap-4", feature = "heap-5"))]
unsafe fn check_exhaustion_and_reuse(initial_free: usize) {
    let mut allocations = [ptr::null_mut(); 512];
    let mut allocation_count = 0;

    while allocation_count < allocations.len() {
        let allocation = pvPortMalloc(64);
        if allocation.is_null() {
            break;
        }
        allocations[allocation_count] = allocation;
        allocation_count += 1;
    }

    assert!(allocation_count > 0);
    assert!(allocation_count < allocations.len());
    assert!(pvPortMalloc(64).is_null());

    let mut index = 0;
    while index < allocation_count {
        vPortFree(allocations[index]);
        allocations[index] = ptr::null_mut();
        index += 2;
    }

    index = 0;
    while index < allocation_count {
        let replacement = pvPortMalloc(64);
        assert!(!replacement.is_null());
        allocations[index] = replacement;
        index += 2;
    }

    for allocation in allocations.into_iter().take(allocation_count) {
        vPortFree(allocation);
    }
    assert_eq!(xPortGetFreeHeapSize(), initial_free);
}

#[cfg(all(feature = "alloc", not(any(feature = "heap-4", feature = "heap-5"))))]
#[test]
fn global_allocator_wrapper_preserves_port_alignment_and_rejects_overflow() {
    freertos_in_rust::port::test_port_reset();

    unsafe {
        check_raw_allocator_contract();
    }
}

#[cfg(feature = "heap-4")]
#[test]
fn heap_4_alignment_realloc_overflow_and_coalescing() {
    use freertos_in_rust::memory::{vPortGetHeapStats, vPortHeapResetState, HeapStats_t};

    freertos_in_rust::port::test_port_reset();
    vPortHeapResetState();

    unsafe {
        // Force initialization, then establish the completely free baseline.
        let probe = pvPortMalloc(1);
        assert!(!probe.is_null());
        vPortFree(probe);
        let initial_free = xPortGetFreeHeapSize();

        check_raw_allocator_contract();
        check_global_allocator_contract(&FreeRtosAllocator);
        check_exhaustion_and_reuse(initial_free);

        let first = pvPortMalloc(128);
        let middle = pvPortMalloc(256);
        let last = pvPortMalloc(128);
        assert!(!first.is_null() && !middle.is_null() && !last.is_null());

        vPortFree(middle);
        vPortFree(first);
        vPortFree(last);

        assert_eq!(xPortGetFreeHeapSize(), initial_free);

        let mut stats: HeapStats_t = core::mem::zeroed();
        vPortGetHeapStats(&mut stats);
        assert_eq!(stats.xNumberOfFreeBlocks, 1);
        assert_eq!(stats.xAvailableHeapSpaceInBytes, initial_free);
        assert_eq!(stats.xSizeOfLargestFreeBlockInBytes, initial_free);
    }

    vPortHeapResetState();
}

#[cfg(all(feature = "heap-5", not(feature = "heap-4")))]
#[test]
fn heap_5_alignment_realloc_overflow_tiny_regions_and_coalescing() {
    use freertos_in_rust::memory::{
        vPortDefineHeapRegions, vPortGetHeapStats, vPortHeapResetState, HeapRegion, HeapStats_t,
    };

    #[repr(C, align(64))]
    struct AlignedHeap([u8; 8192]);

    freertos_in_rust::port::test_port_reset();
    vPortHeapResetState();

    let mut storage = AlignedHeap([0; 8192]);
    let base = storage.0.as_mut_ptr();

    // An all-tiny definition is rejected before any region memory is touched.
    let tiny = [HeapRegion::new(base, 8)];
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        vPortDefineHeapRegions(&tiny);
    }));
    assert!(result.is_err());
    assert_eq!(xPortGetFreeHeapSize(), 0);
    vPortHeapResetState();

    // Ordering is validated before writing either region's block headers.
    let descending = unsafe {
        [
            HeapRegion::new(base.add(4096), 3000),
            HeapRegion::new(base.add(64), 3000),
        ]
    };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        vPortDefineHeapRegions(&descending);
    }));
    assert!(result.is_err());
    assert_eq!(xPortGetFreeHeapSize(), 0);
    vPortHeapResetState();

    unsafe {
        // The tiny leading region is ignored. The two usable regions exercise
        // the intermediate zero-sized end-marker link used by upstream heap_5.
        let regions = [
            HeapRegion::new(base, 8),
            HeapRegion::new(base.add(64), 3000),
            HeapRegion::new(base.add(4096), 3000),
        ];
        vPortDefineHeapRegions(&regions);
        let initial_free = xPortGetFreeHeapSize();
        assert!(initial_free > 0);

        check_raw_allocator_contract();
        check_global_allocator_contract(&FreeRtosAllocator);
        check_exhaustion_and_reuse(initial_free);

        // Exhaust most of the first region so first-fit traversal must cross
        // its intermediate end marker and allocate from the second region.
        let first_region = pvPortMalloc(2700);
        let second_region = pvPortMalloc(512);
        assert!(!first_region.is_null() && !second_region.is_null());
        assert!((first_region as usize) < base.add(4096) as usize);
        assert!((second_region as usize) >= base.add(4096) as usize);
        vPortFree(second_region);
        vPortFree(first_region);
        assert_eq!(xPortGetFreeHeapSize(), initial_free);

        let first = pvPortMalloc(128);
        let middle = pvPortMalloc(256);
        let last = pvPortMalloc(128);
        assert!(!first.is_null() && !middle.is_null() && !last.is_null());

        vPortFree(middle);
        vPortFree(first);
        vPortFree(last);
        assert_eq!(xPortGetFreeHeapSize(), initial_free);

        let mut stats: HeapStats_t = core::mem::zeroed();
        vPortGetHeapStats(&mut stats);
        assert_eq!(stats.xAvailableHeapSpaceInBytes, initial_free);
        assert_eq!(stats.xNumberOfFreeBlocks, 2);
        assert!(stats.xSizeOfSmallestFreeBlockInBytes > 0);
    }

    vPortHeapResetState();
}
