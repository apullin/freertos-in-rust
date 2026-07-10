//! Cortex-M context, masking, stack, ISR, and floating-point port stress test.

#![no_std]
#![no_main]
#![allow(static_mut_refs)]

extern crate panic_semihosting;

use core::arch::{asm, global_asm};
use core::ffi::c_void;
use core::ptr;
use core::sync::atomic::{AtomicPtr, AtomicU32, AtomicUsize, Ordering};

use cortex_m_rt::{entry, exception};
use freertos_in_rust::kernel::queue::{
    xQueueCreateStatic, xQueueReceive, xQueueSendFromISR, Queue_t, StaticQueue_t,
};
use freertos_in_rust::port::{
    portENTER_CRITICAL, portEXIT_CRITICAL, portYIELD, portYIELD_FROM_ISR,
};
use freertos_in_rust::sync::TaskContext;
use freertos_in_rust::types::{pdFALSE, pdPASS, BaseType_t, StackType_t, UBaseType_t};
use freertos_qemu_cortex_m4f::{finish, spawn_static, start_scheduler, tap, AlignedTaskBuffer};

#[cfg(any(
    all(feature = "integer-context", feature = "fpu-context"),
    not(any(feature = "integer-context", feature = "fpu-context"))
))]
compile_error!("select exactly one of integer-context or fpu-context");

#[cfg(any(
    all(feature = "primask-port", feature = "basepri-port"),
    not(any(feature = "primask-port", feature = "basepri-port"))
))]
compile_error!("select exactly one of primask-port or basepri-port");

const STACK_WORDS: usize = 320;
const GUARD_WORDS: usize = 8;
const LOCAL_WORDS: usize = 64;
const SWITCH_ITERATIONS: u32 = 2048;
const MAXIMUM_WAIT_TICKS: u32 = 2000;
const WORKER_PRIORITY: UBaseType_t = 1;
const CONTROLLER_PRIORITY: UBaseType_t = 2;
const WAITER_PRIORITY: UBaseType_t = 3;
const REPORTER_PRIORITY: UBaseType_t = 4;
const GUARD_VALUE: u32 = 0xd15e_a5e5;
const IRQ_VALUE: u32 = 0x1a5f_00d5;

const ERROR_CORE_REGISTERS: u32 = 1 << 0;
const ERROR_LOCAL_STACK: u32 = 1 << 1;
#[cfg(feature = "fpu-context")]
const ERROR_FPU_REGISTERS: u32 = 1 << 2;

const NVIC_ISER0: *mut u32 = 0xE000_E100 as *mut u32;
const NVIC_ISPR0: *mut u32 = 0xE000_E200 as *mut u32;
const NVIC_IPR0: *mut u8 = 0xE000_E400 as *mut u8;

#[repr(C, align(8))]
struct GuardedStack<const WORDS: usize> {
    low_guard: [u32; GUARD_WORDS],
    words: [StackType_t; WORDS],
    high_guard: [u32; GUARD_WORDS],
}

impl<const WORDS: usize> GuardedStack<WORDS> {
    const fn new() -> Self {
        Self {
            low_guard: [GUARD_VALUE; GUARD_WORDS],
            words: [0; WORDS],
            high_guard: [GUARD_VALUE; GUARD_WORDS],
        }
    }
}

#[repr(align(8))]
struct DeepLocal([u32; LOCAL_WORDS]);

#[repr(C, align(8))]
struct RegisterPattern {
    core: [u32; 8],
    fpu: [u32; 16],
}

impl RegisterPattern {
    const fn zeroed() -> Self {
        Self {
            core: [0; 8],
            fpu: [0; 16],
        }
    }
}

static WORKER_A_CORE_EPOCH: AtomicU32 = AtomicU32::new(0);
static WORKER_B_CORE_EPOCH: AtomicU32 = AtomicU32::new(0);
static WORKER_A_DONE: AtomicU32 = AtomicU32::new(0);
static WORKER_B_DONE: AtomicU32 = AtomicU32::new(0);
static WORKER_A_ERRORS: AtomicU32 = AtomicU32::new(0);
static WORKER_B_ERRORS: AtomicU32 = AtomicU32::new(0);
static WORKER_A_LOCAL: AtomicUsize = AtomicUsize::new(0);
static WORKER_B_LOCAL: AtomicUsize = AtomicUsize::new(0);

static IRQ_QUEUE: AtomicPtr<Queue_t> = AtomicPtr::new(ptr::null_mut());
static WAITER_STARTED: AtomicU32 = AtomicU32::new(0);
static WAITER_RESULT: AtomicU32 = AtomicU32::new(0);
static WAITER_VALUE: AtomicU32 = AtomicU32::new(0);
static WAITER_STAGE: AtomicU32 = AtomicU32::new(u32::MAX);
static ISR_RAN: AtomicU32 = AtomicU32::new(0);
static ISR_SEND_RESULT: AtomicU32 = AtomicU32::new(0);
static ISR_WOKEN: AtomicU32 = AtomicU32::new(0);
static CONTROLLER_STAGE: AtomicU32 = AtomicU32::new(0);
static CRITICAL_RESULT: AtomicU32 = AtomicU32::new(0);

static mut IRQ_QUEUE_STORAGE: [u32; 1] = [0; 1];
static mut IRQ_QUEUE_CONTROL: StaticQueue_t = StaticQueue_t::new();

static mut WORKER_A_STACK: GuardedStack<STACK_WORDS> = GuardedStack::new();
static mut WORKER_A_TCB: AlignedTaskBuffer = AlignedTaskBuffer::new();
static mut WORKER_B_STACK: GuardedStack<STACK_WORDS> = GuardedStack::new();
static mut WORKER_B_TCB: AlignedTaskBuffer = AlignedTaskBuffer::new();
static mut CONTROLLER_STACK: GuardedStack<STACK_WORDS> = GuardedStack::new();
static mut CONTROLLER_TCB: AlignedTaskBuffer = AlignedTaskBuffer::new();
static mut WAITER_STACK: GuardedStack<STACK_WORDS> = GuardedStack::new();
static mut WAITER_TCB: AlignedTaskBuffer = AlignedTaskBuffer::new();
static mut REPORTER_STACK: GuardedStack<STACK_WORDS> = GuardedStack::new();
static mut REPORTER_TCB: AlignedTaskBuffer = AlignedTaskBuffer::new();

/* This helper deliberately uses only ARMv6-M instructions so the exact same
 * register probe qualifies M0, M3, M4F, and M7. It preserves the caller's
 * callee-saved registers, installs distinct values in r4-r11, requests
 * PendSV, and refuses to continue until the peer task has run. */
#[cfg(feature = "integer-context")]
global_asm!(
    ".syntax unified",
    ".thumb",
    ".section .text.freertos_qemu_core_register_probe,\"ax\",%progbits",
    ".global freertos_qemu_core_register_probe",
    ".type freertos_qemu_core_register_probe, %function",
    ".thumb_func",
    "freertos_qemu_core_register_probe:",
    "push {{r3-r7, lr}}",
    "mov r4, r8",
    "mov r5, r9",
    "mov r6, r10",
    "mov r7, r11",
    "push {{r4-r7}}",
    "mov r12, r1",
    "mov r1, r0",
    "adds r1, #16",
    "ldmia r1!, {{r4-r7}}",
    "mov r8, r4",
    "mov r9, r5",
    "mov r10, r6",
    "mov r11, r7",
    "ldmia r0!, {{r4-r7}}",
    "ldr r1, =0xE000ED04",
    "movs r0, #1",
    "lsls r0, r0, #28",
    "str r0, [r1]",
    "dsb",
    "isb",
    "1:",
    "ldr r0, [r2]",
    "cmp r0, r3",
    "beq 1b",
    "mov r1, r12",
    "stmia r1!, {{r4-r7}}",
    "mov r4, r8",
    "mov r5, r9",
    "mov r6, r10",
    "mov r7, r11",
    "stmia r1!, {{r4-r7}}",
    "pop {{r4-r7}}",
    "mov r8, r4",
    "mov r9, r5",
    "mov r10, r6",
    "mov r11, r7",
    "pop {{r3-r7, pc}}",
    ".size freertos_qemu_core_register_probe, .-freertos_qemu_core_register_probe",
);

#[cfg(feature = "fpu-context")]
global_asm!(
    ".syntax unified",
    ".thumb",
    ".fpu fpv4-sp-d16",
    ".section .text.freertos_qemu_core_register_probe,\"ax\",%progbits",
    ".global freertos_qemu_core_register_probe",
    ".type freertos_qemu_core_register_probe, %function",
    ".thumb_func",
    "freertos_qemu_core_register_probe:",
    "push {{r3-r7, lr}}",
    "mov r4, r8",
    "mov r5, r9",
    "mov r6, r10",
    "mov r7, r11",
    "push {{r4-r7}}",
    "vpush {{s16-s31}}",
    "mov r12, r1",
    "add r1, r0, #32",
    "vldmia r1!, {{s16-s31}}",
    "mov r1, r0",
    "adds r1, #16",
    "ldmia r1!, {{r4-r7}}",
    "mov r8, r4",
    "mov r9, r5",
    "mov r10, r6",
    "mov r11, r7",
    "ldmia r0!, {{r4-r7}}",
    "ldr r1, =0xE000ED04",
    "movs r0, #1",
    "lsls r0, r0, #28",
    "str r0, [r1]",
    "dsb",
    "isb",
    "1:",
    "ldr r0, [r2]",
    "cmp r0, r3",
    "beq 1b",
    "mov r1, r12",
    "stmia r1!, {{r4-r7}}",
    "mov r4, r8",
    "mov r5, r9",
    "mov r6, r10",
    "mov r7, r11",
    "stmia r1!, {{r4-r7}}",
    "vstmia r1!, {{s16-s31}}",
    "vpop {{s16-s31}}",
    "pop {{r4-r7}}",
    "mov r8, r4",
    "mov r9, r5",
    "mov r10, r6",
    "mov r11, r7",
    "pop {{r3-r7, pc}}",
    ".size freertos_qemu_core_register_probe, .-freertos_qemu_core_register_probe",
);

extern "C" {
    fn freertos_qemu_core_register_probe(
        input: *const RegisterPattern,
        output: *mut RegisterPattern,
        peer_epoch: *const u32,
        previous_peer_epoch: u32,
    );
}

unsafe fn stack_bounds<const WORDS: usize>(stack: *const GuardedStack<WORDS>) -> (usize, usize) {
    let start = unsafe { ptr::addr_of!((*stack).words).cast::<StackType_t>() as usize };
    let end = start + WORDS * core::mem::size_of::<StackType_t>();
    (start, end)
}

unsafe fn guards_intact<const WORDS: usize>(stack: *const GuardedStack<WORDS>) -> bool {
    let low = unsafe { ptr::addr_of!((*stack).low_guard).cast::<u32>() };
    let high = unsafe { ptr::addr_of!((*stack).high_guard).cast::<u32>() };
    let mut index = 0;
    while index < GUARD_WORDS {
        if unsafe { ptr::read_volatile(low.add(index)) != GUARD_VALUE }
            || unsafe { ptr::read_volatile(high.add(index)) != GUARD_VALUE }
        {
            return false;
        }
        index += 1;
    }
    true
}

fn arrays_match(left: &[u32], right: &[u32]) -> bool {
    let mut index = 0;
    while index < left.len() {
        if left[index] != right[index] {
            return false;
        }
        index += 1;
    }
    true
}

fn worker(
    identity: u32,
    own_core_epoch: &AtomicU32,
    peer_core_epoch: &AtomicU32,
    done: &AtomicU32,
    errors: &AtomicU32,
    local_address: &AtomicUsize,
) -> ! {
    // Safety: a FreeRTOS task entry runs in task context for its entire call.
    let context = unsafe { TaskContext::assume() };
    let local_pattern = 0x600d_0000 ^ identity.rotate_left(9);
    let mut local = DeepLocal([0; LOCAL_WORDS]);
    let local_pointer = local.0.as_mut_ptr();
    let mut index = 0;
    while index < LOCAL_WORDS {
        unsafe { ptr::write_volatile(local_pointer.add(index), local_pattern ^ index as u32) };
        index += 1;
    }
    local_address.store(local_pointer as usize, Ordering::Release);

    let mut worker_errors = 0;
    let mut iteration = 0;
    while iteration < SWITCH_ITERATIONS {
        let mut input = RegisterPattern::zeroed();
        let mut output = RegisterPattern::zeroed();
        index = 0;
        while index < input.core.len() {
            input.core[index] = (0x4100_0000 ^ identity ^ iteration)
                .rotate_left((index as u32 * 3) & 31)
                ^ (index as u32 * 0x0101_0101);
            index += 1;
        }

        #[cfg(feature = "fpu-context")]
        {
            index = 0;
            while index < input.fpu.len() {
                input.fpu[index] = 0x3f00_0000
                    ^ identity.rotate_left((index as u32) & 15)
                    ^ iteration.rotate_left((index as u32 * 5) & 31)
                    ^ index as u32;
                index += 1;
            }
        }

        let previous_peer = peer_core_epoch.load(Ordering::Acquire);
        own_core_epoch.store(iteration + 1, Ordering::Release);
        unsafe {
            freertos_qemu_core_register_probe(
                &input,
                &mut output,
                peer_core_epoch.as_ptr(),
                previous_peer,
            );
        }
        if !arrays_match(&input.core, &output.core) {
            worker_errors |= ERROR_CORE_REGISTERS;
        }

        #[cfg(feature = "fpu-context")]
        if !arrays_match(&input.fpu, &output.fpu) {
            worker_errors |= ERROR_FPU_REGISTERS;
        }

        let probes = [0, 1, 17, LOCAL_WORDS / 2, LOCAL_WORDS - 2, LOCAL_WORDS - 1];
        index = 0;
        while index < probes.len() {
            let item = probes[index];
            if unsafe { ptr::read_volatile(local_pointer.add(item)) } != local_pattern ^ item as u32
            {
                worker_errors |= ERROR_LOCAL_STACK;
            }
            index += 1;
        }

        iteration += 1;
    }

    // Release a peer that may be waiting on this task's final epoch before
    // retiring into the long delay loop.
    own_core_epoch.store(SWITCH_ITERATIONS + 1, Ordering::Release);
    errors.store(worker_errors, Ordering::Release);
    done.store(1, Ordering::Release);
    portYIELD();
    loop {
        freertos_in_rust::delay(&context, 1000);
    }
}

extern "C" fn worker_a(_parameter: *mut c_void) {
    worker(
        0xaaaa_0001,
        &WORKER_A_CORE_EPOCH,
        &WORKER_B_CORE_EPOCH,
        &WORKER_A_DONE,
        &WORKER_A_ERRORS,
        &WORKER_A_LOCAL,
    )
}

extern "C" fn worker_b(_parameter: *mut c_void) {
    worker(
        0xbbbb_0002,
        &WORKER_B_CORE_EPOCH,
        &WORKER_A_CORE_EPOCH,
        &WORKER_B_DONE,
        &WORKER_B_ERRORS,
        &WORKER_B_LOCAL,
    )
}

#[cfg(feature = "primask-port")]
fn interrupts_masked() -> bool {
    let value: u32;
    unsafe { asm!("mrs {}, PRIMASK", out(reg) value, options(nomem, nostack)) };
    value & 1 != 0
}

#[cfg(feature = "basepri-port")]
fn interrupts_masked() -> bool {
    let value: u32;
    unsafe { asm!("mrs {}, BASEPRI", out(reg) value, options(nomem, nostack)) };
    value != 0
}

fn pend_application_interrupt() {
    unsafe {
        // IRQ0 is below the maximum syscall priority on BASEPRI ports and is
        // globally masked by PRIMASK on ARMv6-M.
        ptr::write_volatile(NVIC_IPR0, 0xc0);
        ptr::write_volatile(NVIC_ISER0, 1);
        ptr::write_volatile(NVIC_ISPR0, 1);
    }
    cortex_m::asm::dsb();
    cortex_m::asm::isb();
}

#[exception]
unsafe fn DefaultHandler(interrupt_number: i16) {
    if interrupt_number == 0 {
        let queue = IRQ_QUEUE.load(Ordering::Acquire);
        let value = IRQ_VALUE;
        let mut higher_priority_task_woken: BaseType_t = pdFALSE;
        let result = unsafe {
            xQueueSendFromISR(
                queue,
                ptr::addr_of!(value).cast::<c_void>(),
                &mut higher_priority_task_woken,
            )
        };

        ISR_SEND_RESULT.store(result as u32, Ordering::Relaxed);
        ISR_WOKEN.store(higher_priority_task_woken as u32, Ordering::Relaxed);
        ISR_RAN.store(1, Ordering::Release);
        portYIELD_FROM_ISR(higher_priority_task_woken);
    }
}

extern "C" fn waiter(_parameter: *mut c_void) {
    // Safety: a FreeRTOS task entry runs in task context for its entire call.
    let context = unsafe { TaskContext::assume() };
    let queue = IRQ_QUEUE.load(Ordering::Acquire);
    let mut value = 0_u32;
    WAITER_STARTED.store(1, Ordering::Release);
    let result = unsafe { xQueueReceive(queue, ptr::addr_of_mut!(value).cast::<c_void>(), 1000) };
    WAITER_STAGE.store(CONTROLLER_STAGE.load(Ordering::Acquire), Ordering::Relaxed);
    WAITER_VALUE.store(value, Ordering::Relaxed);
    WAITER_RESULT.store(result as u32, Ordering::Release);

    loop {
        freertos_in_rust::delay(&context, 1000);
    }
}

extern "C" fn controller(_parameter: *mut c_void) {
    // Safety: a FreeRTOS task entry runs in task context for its entire call.
    let context = unsafe { TaskContext::assume() };
    while WAITER_STARTED.load(Ordering::Acquire) == 0 {
        freertos_in_rust::delay(&context, 1);
    }

    CONTROLLER_STAGE.store(1, Ordering::Release);
    portENTER_CRITICAL();
    let outer_masked = interrupts_masked();
    portENTER_CRITICAL();
    let nested_masked = interrupts_masked();

    pend_application_interrupt();
    let mut spins = 0;
    while spins < 64 {
        cortex_m::asm::nop();
        spins += 1;
    }
    let deferred_while_nested = ISR_RAN.load(Ordering::Acquire) == 0;

    portEXIT_CRITICAL();
    let masked_after_inner_exit = interrupts_masked();
    spins = 0;
    while spins < 64 {
        cortex_m::asm::nop();
        spins += 1;
    }
    let deferred_after_inner_exit = ISR_RAN.load(Ordering::Acquire) == 0;

    CONTROLLER_STAGE.store(2, Ordering::Release);
    portEXIT_CRITICAL();
    cortex_m::asm::dsb();
    cortex_m::asm::isb();
    cortex_m::asm::nop();
    let unmasked_after_outer_exit = !interrupts_masked();
    CONTROLLER_STAGE.store(3, Ordering::Release);

    CRITICAL_RESULT.store(
        (outer_masked
            && nested_masked
            && masked_after_inner_exit
            && deferred_while_nested
            && deferred_after_inner_exit
            && unmasked_after_outer_exit) as u32,
        Ordering::Release,
    );

    loop {
        freertos_in_rust::delay(&context, 1000);
    }
}

fn local_is_in_stack<const WORDS: usize>(local: usize, stack: *const GuardedStack<WORDS>) -> bool {
    let (start, end) = unsafe { stack_bounds(stack) };
    local >= start && local + core::mem::size_of::<DeepLocal>() <= end
}

extern "C" fn reporter(_parameter: *mut c_void) {
    // Safety: a FreeRTOS task entry runs in task context for its entire call.
    let context = unsafe { TaskContext::assume() };
    let mut waited = 0;
    while waited < MAXIMUM_WAIT_TICKS
        && (WORKER_A_DONE.load(Ordering::Acquire) == 0
            || WORKER_B_DONE.load(Ordering::Acquire) == 0
            || WAITER_RESULT.load(Ordering::Acquire) == 0
            || CRITICAL_RESULT.load(Ordering::Acquire) == 0)
    {
        freertos_in_rust::delay(&context, 1);
        waited += 1;
    }

    let a_errors = WORKER_A_ERRORS.load(Ordering::Acquire);
    let b_errors = WORKER_B_ERRORS.load(Ordering::Acquire);
    let errors = a_errors | b_errors;
    let a_local = WORKER_A_LOCAL.load(Ordering::Acquire);
    let b_local = WORKER_B_LOCAL.load(Ordering::Acquire);
    let locals_ok = local_is_in_stack(a_local, ptr::addr_of!(WORKER_A_STACK))
        && local_is_in_stack(b_local, ptr::addr_of!(WORKER_B_STACK));
    let guards_ok = unsafe {
        guards_intact(ptr::addr_of!(WORKER_A_STACK))
            && guards_intact(ptr::addr_of!(WORKER_B_STACK))
            && guards_intact(ptr::addr_of!(CONTROLLER_STACK))
            && guards_intact(ptr::addr_of!(WAITER_STACK))
            && guards_intact(ptr::addr_of!(REPORTER_STACK))
    };

    let mut output = tap();
    output.check(
        WORKER_A_DONE.load(Ordering::Acquire) != 0
            && WORKER_B_DONE.load(Ordering::Acquire) != 0
            && WORKER_A_CORE_EPOCH.load(Ordering::Acquire) >= SWITCH_ITERATIONS
            && WORKER_B_CORE_EPOCH.load(Ordering::Acquire) >= SWITCH_ITERATIONS,
        "thousands_of_forced_context_switches_complete",
    );
    output.check(
        errors & ERROR_CORE_REGISTERS == 0,
        "r4_through_r11_survive_every_forced_switch",
    );
    output.check(
        errors & ERROR_LOCAL_STACK == 0 && locals_ok,
        "task_stack_and_deep_locals_survive_switches",
    );
    output.check(guards_ok, "all_task_stack_guards_remain_intact");
    output.check(
        CRITICAL_RESULT.load(Ordering::Acquire) != 0,
        "nested_critical_sections_retain_interrupt_mask",
    );
    output.check(
        ISR_RAN.load(Ordering::Acquire) != 0
            && ISR_SEND_RESULT.load(Ordering::Relaxed) == pdPASS as u32
            && ISR_WOKEN.load(Ordering::Relaxed) != 0
            && WAITER_RESULT.load(Ordering::Acquire) == pdPASS as u32
            && WAITER_VALUE.load(Ordering::Relaxed) == IRQ_VALUE,
        "real_irq_unblocks_higher_priority_queue_waiter",
    );
    output.check(
        WAITER_STAGE.load(Ordering::Relaxed) == 2 && CONTROLLER_STAGE.load(Ordering::Acquire) == 3,
        "irq_exit_preempts_interrupted_controller",
    );
    #[cfg(feature = "fpu-context")]
    output.check(
        errors & ERROR_FPU_REGISTERS == 0,
        "distinct_s16_through_s31_patterns_survive_preemption",
    );
    output.diagnostic(format_args!(
        "waited={} core={}/{} fpu={} errors=0x{:x}/0x{:x} local=0x{:x}/0x{:x} critical={} isr={}/{}/{} waiter={}/0x{:08x}/stage{}",
        waited,
        WORKER_A_CORE_EPOCH.load(Ordering::Relaxed),
        WORKER_B_CORE_EPOCH.load(Ordering::Relaxed),
        cfg!(feature = "fpu-context"),
        a_errors,
        b_errors,
        a_local,
        b_local,
        CRITICAL_RESULT.load(Ordering::Relaxed),
        ISR_RAN.load(Ordering::Relaxed),
        ISR_SEND_RESULT.load(Ordering::Relaxed),
        ISR_WOKEN.load(Ordering::Relaxed),
        WAITER_RESULT.load(Ordering::Relaxed),
        WAITER_VALUE.load(Ordering::Relaxed),
        WAITER_STAGE.load(Ordering::Relaxed),
    ));

    #[cfg(feature = "integer-context")]
    finish(output, 7);
    #[cfg(feature = "fpu-context")]
    finish(output, 8)
}

#[entry]
fn main() -> ! {
    let queue = unsafe {
        xQueueCreateStatic(
            1,
            core::mem::size_of::<u32>() as UBaseType_t,
            IRQ_QUEUE_STORAGE.as_mut_ptr().cast::<u8>(),
            ptr::addr_of_mut!(IRQ_QUEUE_CONTROL),
        )
    };
    assert!(!queue.is_null());
    IRQ_QUEUE.store(queue, Ordering::Release);

    unsafe {
        spawn_static(
            b"port-a\0",
            WORKER_A_STACK.words.as_mut_ptr(),
            STACK_WORDS,
            ptr::addr_of_mut!(WORKER_A_TCB),
            WORKER_PRIORITY,
            worker_a,
        );
        spawn_static(
            b"port-b\0",
            WORKER_B_STACK.words.as_mut_ptr(),
            STACK_WORDS,
            ptr::addr_of_mut!(WORKER_B_TCB),
            WORKER_PRIORITY,
            worker_b,
        );
        spawn_static(
            b"irq-control\0",
            CONTROLLER_STACK.words.as_mut_ptr(),
            STACK_WORDS,
            ptr::addr_of_mut!(CONTROLLER_TCB),
            CONTROLLER_PRIORITY,
            controller,
        );
        spawn_static(
            b"irq-waiter\0",
            WAITER_STACK.words.as_mut_ptr(),
            STACK_WORDS,
            ptr::addr_of_mut!(WAITER_TCB),
            WAITER_PRIORITY,
            waiter,
        );
        spawn_static(
            b"report\0",
            REPORTER_STACK.words.as_mut_ptr(),
            STACK_WORDS,
            ptr::addr_of_mut!(REPORTER_TCB),
            REPORTER_PRIORITY,
            reporter,
        );
    }

    start_scheduler()
}
