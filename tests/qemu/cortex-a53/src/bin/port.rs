//! Cortex-A53 task-stack, exception-entry, and context preservation stress test.

#![no_std]
#![no_main]

use core::arch::asm;
use core::ffi::c_void;
use core::ptr;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use freertos_in_rust::kernel::tasks::{vTaskDelay, xTaskGetTickCount};
use freertos_in_rust::port::{
    portDISABLE_INTERRUPTS, portENABLE_INTERRUPTS, portYIELD, vPortTaskUsesFPU,
};
use freertos_in_rust::types::{StackType_t, TickType_t};
use freertos_qemu_cortex_a53::{
    finish, init, set_tick_isr_hook, spawn_static, start_scheduler, tap, timer_interrupts,
    AlignedTaskBuffer,
};

const STACK_WORDS: usize = 2048;
const REPORTER_STACK_WORDS: usize = 1024;
const GUARD_WORDS: usize = 8;
const LOCAL_WORDS: usize = 384;
const WORKER_PRIORITY: u64 = 2;
const REPORTER_PRIORITY: u64 = 3;
const OBSERVATION_TICKS: TickType_t = 150;
const MINIMUM_SWITCHES: u64 = 500;
const GUARD_VALUE: u64 = 0xd15e_a5e5_cafe_f00d;

const ERROR_LOCAL_PAYLOAD: u64 = 1 << 0;
const ERROR_STACK_RANGE: u64 = 1 << 1;
const ERROR_STACK_SELECTOR: u64 = 1 << 2;
const ERROR_GP_REGISTERS: u64 = 1 << 3;
const ERROR_FP_REGISTERS: u64 = 1 << 4;
const ERROR_IRQ_MASK: u64 = 1 << 5;
const ERROR_FP_CONTROL: u64 = 1 << 6;
const ERROR_FP_STATUS: u64 = 1 << 7;

#[repr(C, align(16))]
struct GuardedStack<const WORDS: usize> {
    low_guard: [u64; GUARD_WORDS],
    words: [StackType_t; WORDS],
    high_guard: [u64; GUARD_WORDS],
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

#[repr(align(16))]
struct DeepLocal([u64; LOCAL_WORDS]);

static WORKER_A_RUNS: AtomicU64 = AtomicU64::new(0);
static WORKER_B_RUNS: AtomicU64 = AtomicU64::new(0);
static WORKER_A_ERRORS: AtomicU64 = AtomicU64::new(0);
static WORKER_B_ERRORS: AtomicU64 = AtomicU64::new(0);
static WORKER_A_LOCAL: AtomicUsize = AtomicUsize::new(0);
static WORKER_B_LOCAL: AtomicUsize = AtomicUsize::new(0);

static mut WORKER_A_STACK: GuardedStack<STACK_WORDS> = GuardedStack::new();
static mut WORKER_A_TCB: AlignedTaskBuffer = AlignedTaskBuffer::new();
static mut WORKER_B_STACK: GuardedStack<STACK_WORDS> = GuardedStack::new();
static mut WORKER_B_TCB: AlignedTaskBuffer = AlignedTaskBuffer::new();
static mut REPORTER_STACK: GuardedStack<REPORTER_STACK_WORDS> = GuardedStack::new();
static mut REPORTER_TCB: AlignedTaskBuffer = AlignedTaskBuffer::new();

unsafe fn stack_bounds<const WORDS: usize>(stack: *const GuardedStack<WORDS>) -> (usize, usize) {
    let start = ptr::addr_of!((*stack).words).cast::<StackType_t>() as usize;
    let end = start + WORDS * core::mem::size_of::<StackType_t>();
    (start, end)
}

unsafe fn guards_intact<const WORDS: usize>(stack: *const GuardedStack<WORDS>) -> bool {
    let low = ptr::addr_of!((*stack).low_guard).cast::<u64>();
    let high = ptr::addr_of!((*stack).high_guard).cast::<u64>();
    let mut index = 0;
    while index < GUARD_WORDS {
        if ptr::read_volatile(low.add(index)) != GUARD_VALUE
            || ptr::read_volatile(high.add(index)) != GUARD_VALUE
        {
            return false;
        }
        index += 1;
    }
    true
}

#[inline(always)]
fn yield_with_gp_registers(seed: u64) -> bool {
    let expected_x9 = seed ^ 0x0909_0909_0909_0909;
    let expected_x10 = seed ^ 0x1010_1010_1010_1010;
    let expected_x11 = seed ^ 0x1111_1111_1111_1111;
    let expected_x12 = seed ^ 0x1212_1212_1212_1212;
    let mut x9 = expected_x9;
    let mut x10 = expected_x10;
    let mut x11 = expected_x11;
    let mut x12 = expected_x12;

    unsafe {
        asm!(
            "svc 0",
            inout("x9") x9,
            inout("x10") x10,
            inout("x11") x11,
            inout("x12") x12,
            options(nostack)
        );
    }

    x9 == expected_x9 && x10 == expected_x10 && x11 == expected_x11 && x12 == expected_x12
}

#[inline(always)]
fn yield_with_fp_register(seed: u64) -> bool {
    let expected = f64::from_bits(seed ^ 0x5a5a_a5a5_9696_6969);
    let mut value = expected;
    unsafe {
        asm!("svc 0", inout("v8") value, options(nostack));
    }
    value.to_bits() == expected.to_bits()
}

fn yield_with_fp_state(identity: u64) -> u64 {
    let expected_control = if identity == 0xaaaa { 0 } else { 1 << 22 };
    let expected_status = if identity == 0xaaaa { 0 } else { 1 << 27 };
    let control: u64;
    let status: u64;
    unsafe {
        asm!(
            "msr fpcr, {expected_control}",
            "msr fpsr, {expected_status}",
            "svc 0",
            "mrs {control}, fpcr",
            "mrs {status}, fpsr",
            expected_control = in(reg) expected_control,
            expected_status = in(reg) expected_status,
            control = out(reg) control,
            status = out(reg) status,
            options(nostack)
        );
    }
    let mut errors = 0;
    if control != expected_control {
        errors |= ERROR_FP_CONTROL;
    }
    if status != expected_status {
        errors |= ERROR_FP_STATUS;
    }
    errors
}

// The C ABI permits this IRQ callback to clobber v0 and the upper half of v8.
extern "C" fn clobber_irq_fp_state() {
    unsafe {
        asm!(
            "movi v0.16b, #0", "movi v8.16b, #0",
            "msr fpcr, xzr", "msr fpsr, xzr",
            out("v0") _, out("v8") _, options(nostack)
        );
    }
}

fn irq_runs_without_fp_access() -> bool {
    // Reporter has not registered FP context yet and remains the highest
    // priority task, so this IRQ need not restore any other task's FP state.
    let before = timer_interrupts();
    unsafe {
        asm!(
            "mrs x9, cpacr_el1",
            "bic x10, x9, #(3 << 20)",
            "msr cpacr_el1, x10", "isb",
            "wfi",
            "msr cpacr_el1, x9", "isb",
            out("x9") _, out("x10") _, options(nostack)
        );
    }
    timer_interrupts() > before
}

fn irq_preserves_fp_state() -> bool {
    vPortTaskUsesFPU();
    set_tick_isr_hook(clobber_irq_fp_state);
    let before = timer_interrupts();
    let value = 0x1234_5678_9abc_def0_u64;
    let control = 1_u64 << 22;
    let status = 1_u64 << 27;
    let v0_low: u64;
    let v0_high: u64;
    let v8_high: u64;
    let actual_control: u64;
    let actual_status: u64;
    unsafe {
        asm!(
            "fmov d0, {value}", "ins v0.d[1], {value}",
            "fmov d8, {value}", "ins v8.d[1], {value}",
            "msr fpcr, {control}", "msr fpsr, {status}",
            "wfi",
            "fmov {v0_low}, d0", "umov {v0_high}, v0.d[1]",
            "umov {v8_high}, v8.d[1]",
            "mrs {actual_control}, fpcr", "mrs {actual_status}, fpsr",
            value = in(reg) value, control = in(reg) control, status = in(reg) status,
            v0_low = out(reg) v0_low, v0_high = out(reg) v0_high,
            v8_high = out(reg) v8_high, actual_control = out(reg) actual_control,
            actual_status = out(reg) actual_status,
            out("v0") _, out("v8") _, options(nostack)
        );
    }
    timer_interrupts() > before
        && v0_low == value
        && v0_high == value
        && v8_high == value
        && actual_control == control
        && actual_status == status
}

#[inline(always)]
fn interrupt_disable_sets_daif_i() -> bool {
    portDISABLE_INTERRUPTS();
    let daif: u64;
    unsafe { asm!("mrs {}, daif", out(reg) daif, options(nomem, nostack)) };
    portENABLE_INTERRUPTS();
    daif & (1 << 7) != 0
}

fn worker(
    identity: u64,
    stack: *const GuardedStack<STACK_WORDS>,
    runs: &AtomicU64,
    errors: &AtomicU64,
    local_address: &AtomicUsize,
) -> ! {
    let pattern = 0xa53a_0000_0000_0000_u64 ^ identity.rotate_left(17);
    let mut local = DeepLocal([0; LOCAL_WORDS]);
    let local_pointer = local.0.as_mut_ptr();

    let mut index = 0;
    while index < LOCAL_WORDS {
        unsafe { ptr::write_volatile(local_pointer.add(index), pattern ^ index as u64) };
        index += 1;
    }
    local_address.store(local_pointer as usize, Ordering::Release);

    vPortTaskUsesFPU();

    let mut iteration = 0_u64;
    loop {
        let mut worker_errors = 0;
        if iteration == 0 && !interrupt_disable_sets_daif_i() {
            worker_errors |= ERROR_IRQ_MASK;
        }
        let probes = [0, 1, 17, LOCAL_WORDS / 2, LOCAL_WORDS - 2, LOCAL_WORDS - 1];
        let mut probe = 0;
        while probe < probes.len() {
            let item = probes[probe];
            if unsafe { ptr::read_volatile(local_pointer.add(item)) } != pattern ^ item as u64 {
                worker_errors |= ERROR_LOCAL_PAYLOAD;
            }
            probe += 1;
        }

        let (stack_start, stack_end) = unsafe { stack_bounds(stack) };
        let local_start = local_pointer as usize;
        let local_end = local_start + core::mem::size_of::<DeepLocal>();
        let selected_sp: u64;
        let stack_selector: u64;
        unsafe {
            asm!("mov {}, sp", out(reg) selected_sp, options(nomem, nostack));
            asm!("mrs {}, spsel", out(reg) stack_selector, options(nomem, nostack));
        }
        if local_start < stack_start
            || local_end > stack_end
            || (selected_sp as usize) < stack_start
            || (selected_sp as usize) >= stack_end
        {
            worker_errors |= ERROR_STACK_RANGE;
        }
        if stack_selector != 0 {
            worker_errors |= ERROR_STACK_SELECTOR;
        }

        if !yield_with_gp_registers(pattern ^ iteration) {
            worker_errors |= ERROR_GP_REGISTERS;
        }
        if !yield_with_fp_register(pattern ^ iteration) {
            worker_errors |= ERROR_FP_REGISTERS;
        }
        worker_errors |= yield_with_fp_state(identity);

        errors.fetch_or(worker_errors, Ordering::Relaxed);
        runs.fetch_add(1, Ordering::Release);
        iteration = iteration.wrapping_add(1);

        // Exercise the ordinary portYIELD wrapper as well as inline register
        // preservation yields, and let the timer preempt both busy workers.
        portYIELD();
    }
}

extern "C" fn worker_a(_parameter: *mut c_void) {
    worker(
        0xaaaa,
        ptr::addr_of!(WORKER_A_STACK),
        &WORKER_A_RUNS,
        &WORKER_A_ERRORS,
        &WORKER_A_LOCAL,
    )
}

extern "C" fn worker_b(_parameter: *mut c_void) {
    worker(
        0xbbbb,
        ptr::addr_of!(WORKER_B_STACK),
        &WORKER_B_RUNS,
        &WORKER_B_ERRORS,
        &WORKER_B_LOCAL,
    )
}

extern "C" fn reporter(_parameter: *mut c_void) {
    let start = xTaskGetTickCount();
    // Safety: reporter is a live task and the scheduler is running.
    unsafe { vTaskDelay(OBSERVATION_TICKS) };
    let elapsed = xTaskGetTickCount().wrapping_sub(start);

    let a_runs = WORKER_A_RUNS.load(Ordering::Acquire);
    let b_runs = WORKER_B_RUNS.load(Ordering::Acquire);
    let a_errors = WORKER_A_ERRORS.load(Ordering::Relaxed);
    let b_errors = WORKER_B_ERRORS.load(Ordering::Relaxed);
    let a_local = WORKER_A_LOCAL.load(Ordering::Acquire);
    let b_local = WORKER_B_LOCAL.load(Ordering::Acquire);
    let interrupts = timer_interrupts();
    let difference = a_runs.abs_diff(b_runs);

    let (a_start, a_end) = unsafe { stack_bounds(ptr::addr_of!(WORKER_A_STACK)) };
    let (b_start, b_end) = unsafe { stack_bounds(ptr::addr_of!(WORKER_B_STACK)) };
    let local_bytes = core::mem::size_of::<DeepLocal>();
    let locals_in_distinct_stacks = a_local >= a_start
        && a_local + local_bytes <= a_end
        && b_local >= b_start
        && b_local + local_bytes <= b_end
        && (a_local + local_bytes <= b_local || b_local + local_bytes <= a_local);
    let guards_ok = unsafe {
        guards_intact(ptr::addr_of!(WORKER_A_STACK))
            && guards_intact(ptr::addr_of!(WORKER_B_STACK))
            && guards_intact(ptr::addr_of!(REPORTER_STACK))
    };

    let mut output = tap();
    output.check(
        a_runs >= MINIMUM_SWITCHES && b_runs >= MINIMUM_SWITCHES,
        "many_context_switches_complete",
    );
    output.check(difference <= 32, "equal_priority_tasks_remain_balanced");
    output.check(
        elapsed >= OBSERVATION_TICKS && interrupts >= OBSERVATION_TICKS as u64,
        "timer_irqs_preempt_busy_tasks",
    );
    output.check(
        (a_errors | b_errors) & ERROR_STACK_SELECTOR == 0,
        "tasks_execute_in_el1t_with_sp_el0",
    );
    output.check(
        locals_in_distinct_stacks && (a_errors | b_errors) & ERROR_STACK_RANGE == 0,
        "deep_locals_use_distinct_task_stacks",
    );
    output.check(guards_ok, "task_stack_canaries_remain_intact");
    output.check(
        (a_errors | b_errors) & ERROR_LOCAL_PAYLOAD == 0,
        "deep_local_payload_survives_switches",
    );
    output.check(
        (a_errors | b_errors) & ERROR_GP_REGISTERS == 0,
        "general_registers_survive_svc_switches",
    );
    output.check(
        (a_errors | b_errors) & ERROR_FP_REGISTERS == 0,
        "simd_registers_survive_svc_switches",
    );
    output.check(
        (a_errors | b_errors) & ERROR_IRQ_MASK == 0,
        "global_interrupt_disable_leaves_daif_masked",
    );
    output.check(
        (a_errors | b_errors) & ERROR_FP_CONTROL == 0,
        "fpcr_is_task_local",
    );
    output.check(
        (a_errors | b_errors) & ERROR_FP_STATUS == 0,
        "fpsr_is_task_local",
    );
    output.check(
        irq_runs_without_fp_access(),
        "irq_runs_with_fp_access_disabled",
    );
    output.check(
        irq_preserves_fp_state(),
        "irq_preserves_full_simd_and_fp_state",
    );
    output.diagnostic(format_args!(
        "elapsed={elapsed} irq={interrupts} a={a_runs}/0x{a_errors:x}/0x{a_local:x} b={b_runs}/0x{b_errors:x}/0x{b_local:x}"
    ));
    finish(output, 14)
}

#[no_mangle]
pub extern "C" fn main() -> ! {
    init();

    unsafe {
        spawn_static(
            b"port-a\0",
            ptr::addr_of_mut!(WORKER_A_STACK.words).cast::<StackType_t>(),
            STACK_WORDS,
            ptr::addr_of_mut!(WORKER_A_TCB),
            WORKER_PRIORITY,
            worker_a,
        );
        spawn_static(
            b"port-b\0",
            ptr::addr_of_mut!(WORKER_B_STACK.words).cast::<StackType_t>(),
            STACK_WORDS,
            ptr::addr_of_mut!(WORKER_B_TCB),
            WORKER_PRIORITY,
            worker_b,
        );
        spawn_static(
            b"report\0",
            ptr::addr_of_mut!(REPORTER_STACK.words).cast::<StackType_t>(),
            REPORTER_STACK_WORDS,
            ptr::addr_of_mut!(REPORTER_TCB),
            REPORTER_PRIORITY,
            reporter,
        );
    }

    start_scheduler()
}
