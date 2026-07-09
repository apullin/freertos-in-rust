//! Cortex-M4F QEMU test BSP and common kernel setup.

#![no_std]
#![allow(non_snake_case)]

use core::ffi::c_void;
use core::fmt;

use cortex_m_semihosting::{debug, hprint};
use freertos_in_rust::kernel::tasks::{xTaskCreateStatic, StaticTask_t};
use freertos_in_rust::memory::FreeRtosAllocator;
use freertos_in_rust::port::{vPortSVCHandler, xPortPendSVHandler, xPortSysTickHandler};
use freertos_in_rust::types::{pdPASS, StackType_t, TaskFunction_t, UBaseType_t};
use freertos_qemu_test_common::Tap;

/// Keep the port exception handlers and their assembly aliases in the image.
#[used]
static HANDLERS: [unsafe extern "C" fn(); 3] =
    [vPortSVCHandler, xPortPendSVHandler, xPortSysTickHandler];

#[global_allocator]
static ALLOCATOR: FreeRtosAllocator = FreeRtosAllocator;

/// `StaticTask_t` deliberately hides the TCB representation. This wrapper
/// supplies the port's required alignment without exposing that representation
/// in test code.
#[repr(C, align(8))]
pub struct AlignedTaskBuffer {
    tcb: StaticTask_t,
}

impl AlignedTaskBuffer {
    pub const fn new() -> Self {
        Self {
            tcb: StaticTask_t::new(),
        }
    }
}

impl Default for AlignedTaskBuffer {
    fn default() -> Self {
        Self::new()
    }
}

/// Create one statically allocated test task.
///
/// # Safety
///
/// The stack and TCB must each be uniquely owned for the lifetime of the
/// scheduler. `name` must be a static, NUL-terminated byte string.
pub unsafe fn spawn_static(
    name: &'static [u8],
    stack: *mut StackType_t,
    stack_depth: usize,
    task_buffer: *mut AlignedTaskBuffer,
    priority: UBaseType_t,
    task: TaskFunction_t,
) {
    assert!(name.last() == Some(&0));
    assert!(stack_depth > 0);

    let handle = xTaskCreateStatic(
        task,
        name.as_ptr(),
        stack_depth,
        core::ptr::null_mut::<c_void>(),
        priority,
        stack,
        core::ptr::addr_of_mut!((*task_buffer).tcb),
    );

    assert!(!handle.is_null());
}

/// Semihosting-backed formatter used by the target-independent TAP helper.
pub struct SemihostingOutput;

impl fmt::Write for SemihostingOutput {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        hprint!("{}", value);
        Ok(())
    }
}

pub fn tap() -> Tap<SemihostingOutput> {
    Tap::new(SemihostingOutput)
}

/// Finish a bounded test and terminate QEMU through semihosting.
pub fn finish(mut tap: Tap<SemihostingOutput>, planned_tests: usize) -> ! {
    let passed = tap.finish(planned_tests);
    let exit_status = if passed {
        debug::EXIT_SUCCESS
    } else {
        debug::EXIT_FAILURE
    };

    debug::exit(exit_status);

    loop {
        cortex_m::asm::wfi();
    }
}

/// Start the kernel after all test tasks have been created.
pub fn start_scheduler() -> ! {
    freertos_in_rust::start_scheduler()
}

/// Assert that the kernel constants used by test setup retain their expected
/// boolean convention.
pub const fn kernel_pass_value() -> i32 {
    pdPASS
}
