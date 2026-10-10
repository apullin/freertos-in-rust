//! Register/FPSCR preservation with migration, preemption, and non-FP tasks.
#![no_std]
#![no_main]

use core::arch::global_asm;
use core::ffi::c_void;
use core::ptr;
use core::sync::atomic::{AtomicU32, Ordering};
use freertos_in_rust::kernel::tasks::*;
use freertos_in_rust::port::*;
use freertos_in_rust::types::*;
use freertos_qemu_cortex_a9::{finish, init, start_scheduler, start_secondary, tap};

const WORDS: usize = 1024;
static mut STACKS: [[StackType_t; WORDS]; 4] = [[0; WORDS]; 4];
static mut TCBS: [StaticTask_t; 4] = [const { StaticTask_t::new() }; 4];
static mut HANDLES: [TaskHandle_t; 4] = [ptr::null_mut(); 4];
static RESULTS: [AtomicU32; 2] = [const { AtomicU32::new(u32::MAX) }; 2];
static NON_FP_RUNS: AtomicU32 = AtomicU32::new(0);

// The SVC is inside the assembly probe: an ordinary Rust function call would
// be allowed to clobber caller-saved registers and would not test the port.
global_asm!(
    r#"
    .arm
    .fpu neon
    .global smp_fp_probe
    .type smp_fp_probe, %function
    smp_fp_probe:
        push {{r4-r11, lr}}
        sub sp, sp, #4
        vmrs r12, fpscr
        str r12, [sp]
        vpush {{d8-d15}}
        mov r4, r0
        mov r5, r1
        mov r6, r2
        mov r9, r3
        mov r7, #0
        mov r8, #0
        vmsr fpscr, r9
        vldmia r4, {{d0-d15}}
        add r0, r4, #128
        vldmia r0, {{d16-d31}}
    1:
        mov r0, #0x10
        mov r1, #0x11
        mov r2, #0x12
        mov r3, #0x13
        mov r10, #0x1a
        mov r11, #0x1b
        mov r12, #0x1c
        svc 0
        cmp r0, #0x10
        orrne r7, r7, #1
        cmp r1, #0x11
        orrne r7, r7, #1
        cmp r2, #0x12
        orrne r7, r7, #1
        cmp r3, #0x13
        orrne r7, r7, #1
        cmp r10, #0x1a
        orrne r7, r7, #1
        cmp r11, #0x1b
        orrne r7, r7, #1
        cmp r12, #0x1c
        orrne r7, r7, #1
        vmrs r0, fpscr
        cmp r0, r9
        orrne r7, r7, #2
        mrc p15, 0, r0, c0, c0, 5
        and r0, r0, #0xff
        mov r1, #1
        orr r8, r8, r1, lsl r0
        vstmia r5, {{d0-d15}}
        add r0, r5, #128
        vstmia r0, {{d16-d31}}
        mov r0, #0
    2:
        ldr r1, [r4, r0]
        ldr r2, [r5, r0]
        cmp r1, r2
        orrne r7, r7, #4
        add r0, r0, #4
        cmp r0, #256
        bne 2b
        subs r6, r6, #1
        bne 1b
        orr r0, r7, r8, lsl #8
        vpop {{d8-d15}}
        ldr r12, [sp]
        vmsr fpscr, r12
        add sp, sp, #4
        pop {{r4-r11, pc}}
"#
);

extern "C" {
    fn smp_fp_probe(expected: *const u64, observed: *mut u64, loops: u32, fpscr: u32) -> u32;
}

extern "C" fn fp_task(parameter: *mut c_void) {
    let identity = parameter as usize;
    vPortTaskUsesFPU();
    let mut expected = [0u64; 32];
    let mut observed = [0u64; 32];
    let mut i = 0;
    while i < expected.len() {
        expected[i] = 0x1020_3040_5060_7080u64 ^ ((identity as u64 + 1) << 56) ^ i as u64;
        i += 1;
    }
    let result = unsafe {
        smp_fp_probe(
            expected.as_ptr(),
            observed.as_mut_ptr(),
            12000,
            (identity as u32 + 1) << 22,
        )
    };
    RESULTS[identity].store(result, Ordering::Release);
    loop {
        unsafe {
            vTaskDelay(1000);
        }
    }
}

extern "C" fn non_fp_task(_: *mut c_void) {
    loop {
        NON_FP_RUNS.fetch_add(1, Ordering::Relaxed);
        portYIELD();
    }
}

extern "C" fn reporter(_: *mut c_void) {
    unsafe {
        let mut i = 0;
        while i < 100 {
            vTaskCoreAffinitySet(HANDLES[0], 1 << (i % 2));
            vTaskCoreAffinitySet(HANDLES[1], 1 << (1 - i % 2));
            vTaskDelay(1);
            i += 1;
        }
        let start = xTaskGetTickCount();
        while (RESULTS[0].load(Ordering::Acquire) == u32::MAX
            || RESULTS[1].load(Ordering::Acquire) == u32::MAX)
            && xTaskGetTickCount().wrapping_sub(start) < 3000
        {
            vTaskDelay(5);
        }
        let a = RESULTS[0].load(Ordering::Acquire);
        let b = RESULTS[1].load(Ordering::Acquire);
        let mut out = tap();
        out.check(
            a != u32::MAX && b != u32::MAX,
            "fp_tasks_complete_under_two_core_contention",
        );
        out.check(
            (a | b) & 1 == 0,
            "general_registers_survive_svc_and_interrupt_preemption",
        );
        out.check(
            (a | b) & 2 == 0,
            "fpscr_is_private_to_each_task_across_migration",
        );
        out.check(
            (a | b) & 4 == 0,
            "all_32_fp_registers_survive_context_switches",
        );
        out.check(
            a >> 8 == 3 && b >> 8 == 3,
            "fp_contexts_execute_on_both_cores",
        );
        out.check(
            NON_FP_RUNS.load(Ordering::Relaxed) > 100,
            "fp_and_non_fp_tasks_interleave",
        );
        out.diagnostic(format_args!(
            "results={a:x}/{b:x} non_fp_runs={}",
            NON_FP_RUNS.load(Ordering::Relaxed)
        ));
        finish(out, 6)
    }
}

#[no_mangle]
pub extern "C" fn main() -> ! {
    init();
    unsafe {
        let entries: [TaskFunction_t; 4] = [fp_task, fp_task, non_fp_task, reporter];
        let priorities = [2, 2, 2, 4];
        let mut i = 0;
        while i < 4 {
            HANDLES[i] = xTaskCreateStatic(
                entries[i],
                b"smp-port\0".as_ptr(),
                WORDS,
                i as *mut c_void,
                priorities[i],
                ptr::addr_of_mut!(STACKS[i]).cast(),
                ptr::addr_of_mut!(TCBS[i]),
            );
            assert!(!HANDLES[i].is_null());
            i += 1;
        }
        vTaskCoreAffinitySet(HANDLES[3], 1);
    }
    start_secondary();
    start_scheduler()
}
