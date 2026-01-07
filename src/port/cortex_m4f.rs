/*
 * FreeRTOS Kernel <DEVELOPMENT BRANCH>
 * Copyright (C) 2021 Amazon.com, Inc. or its affiliates. All Rights Reserved.
 *
 * SPDX-License-Identifier: MIT
 *
 * [AMENDMENT] This is the Cortex-M4F port for FreeRusTOS.
 * Ported from portable/GCC/ARM_CM4F/port.c
 *
 * This port is for ARM Cortex-M4 with FPU (Floating Point Unit).
 * It uses:
 * - SysTick for tick interrupts
 * - PendSV for context switching
 * - SVC for starting the first task
 * - BASEPRI for interrupt masking (allows configMAX_SYSCALL_INTERRUPT_PRIORITY)
 */

//! ARM Cortex-M4F Port Implementation
//!
//! This port provides hardware-specific implementations for ARM Cortex-M4F:
//! - Critical sections using BASEPRI register
//! - Context switching via PendSV exception
//! - First task start via SVC exception
//! - Tick timer using SysTick
//!
//! ## Usage
//!
//! Enable with `--features port-cortex-m4f` and compile for `thumbv7em-none-eabihf`.
//!
//! ## Vector Table
//!
//! The application must ensure that `PendSV`, `SVCall`, and `SysTick` handlers
//! in the vector table point to the handlers exported by this module.

use core::arch::naked_asm;
use core::ffi::c_void;
use core::sync::atomic::{AtomicUsize, Ordering};

use cortex_m::register::basepri;

use crate::config::*;
use crate::types::*;

// =============================================================================
// Port Constants
// =============================================================================

/// Stack growth direction: -1 for descending (Cortex-M uses descending stack)
pub const portSTACK_GROWTH: BaseType_t = -1;

/// Byte alignment requirement for stack
pub const portBYTE_ALIGNMENT: usize = 8;

/// Initial xPSR value - thumb bit set
const portINITIAL_XPSR: StackType_t = 0x0100_0000;

/// Initial EXC_RETURN value for tasks using PSP and no FPU
const portINITIAL_EXC_RETURN: StackType_t = 0xFFFF_FFFD;

/// Mask to ensure PC has bit 0 clear (required for exception return)
const portSTART_ADDRESS_MASK: StackType_t = 0xFFFF_FFFE;

/// Priority for PendSV and SysTick (lowest priority = 255)
const portMIN_INTERRUPT_PRIORITY: u8 = 255;

/// BASEPRI value for masking interrupts during critical sections
const portMAX_SYSCALL_INTERRUPT_PRIORITY: u8 = configMAX_SYSCALL_INTERRUPT_PRIORITY as u8;

/// NVIC ICSR register address (for pending PendSV)
const NVIC_ICSR: *mut u32 = 0xE000_ED04 as *mut u32;

/// Bit to pend PendSV
const NVIC_PENDSVSET_BIT: u32 = 1 << 28;

/// SysTick control register
const SYST_CSR: *mut u32 = 0xE000_E010 as *mut u32;
/// SysTick reload value register
const SYST_RVR: *mut u32 = 0xE000_E014 as *mut u32;
/// SysTick current value register
const SYST_CVR: *mut u32 = 0xE000_E018 as *mut u32;

/// SysTick enable bit
const SYST_CSR_ENABLE: u32 = 1 << 0;
/// SysTick interrupt enable bit
const SYST_CSR_TICKINT: u32 = 1 << 1;
/// SysTick clock source (processor clock)
const SYST_CSR_CLKSOURCE: u32 = 1 << 2;

// =============================================================================
// Critical Section Management
// =============================================================================

/// Critical section nesting counter
static CRITICAL_NESTING: AtomicUsize = AtomicUsize::new(0);

/// Enter a critical section (disable interrupts via BASEPRI)
#[inline(always)]
pub fn portENTER_CRITICAL() {
    portDISABLE_INTERRUPTS();
    CRITICAL_NESTING.fetch_add(1, Ordering::SeqCst);
    cortex_m::asm::dsb();
    cortex_m::asm::isb();
}

/// Exit a critical section (potentially re-enable interrupts)
#[inline(always)]
pub fn portEXIT_CRITICAL() {
    let prev = CRITICAL_NESTING.fetch_sub(1, Ordering::SeqCst);
    if prev == 1 {
        portENABLE_INTERRUPTS();
    }
}

/// Disable interrupts by setting BASEPRI
#[inline(always)]
pub fn portDISABLE_INTERRUPTS() {
    unsafe {
        basepri::write(portMAX_SYSCALL_INTERRUPT_PRIORITY);
    }
    cortex_m::asm::dsb();
    cortex_m::asm::isb();
}

/// Enable interrupts by clearing BASEPRI
#[inline(always)]
pub fn portENABLE_INTERRUPTS() {
    unsafe {
        basepri::write(0);
    }
}

/// Set interrupt mask from ISR (save and disable)
#[inline(always)]
pub fn portSET_INTERRUPT_MASK_FROM_ISR() -> UBaseType_t {
    let saved = basepri::read() as UBaseType_t;
    unsafe {
        basepri::write(portMAX_SYSCALL_INTERRUPT_PRIORITY);
    }
    cortex_m::asm::dsb();
    cortex_m::asm::isb();
    saved
}

/// Clear interrupt mask from ISR (restore previous state)
#[inline(always)]
pub fn portCLEAR_INTERRUPT_MASK_FROM_ISR(uxSavedInterruptStatus: UBaseType_t) {
    unsafe {
        basepri::write(uxSavedInterruptStatus as u8);
    }
}

// =============================================================================
// Context Switching / Yield
// =============================================================================

/// Trigger a context switch by pending PendSV
#[inline(always)]
pub fn portYIELD() {
    unsafe {
        core::ptr::write_volatile(NVIC_ICSR, NVIC_PENDSVSET_BIT);
    }
    cortex_m::asm::dsb();
    cortex_m::asm::isb();
}

/// Yield from ISR if needed
#[inline(always)]
pub fn portYIELD_FROM_ISR(xSwitchRequired: BaseType_t) {
    if xSwitchRequired != pdFALSE {
        portYIELD();
    }
}

/// End switching ISR (same as yield from ISR)
#[inline(always)]
pub fn portEND_SWITCHING_ISR(xSwitchRequired: BaseType_t) {
    portYIELD_FROM_ISR(xSwitchRequired);
}

// =============================================================================
// Stack Initialization
// =============================================================================

/// Initialize a task's stack
///
/// Sets up the initial stack frame so the task can be started by the scheduler.
/// The stack frame mimics what the hardware saves on exception entry, plus
/// the registers we save manually in PendSV.
///
/// Stack layout (high to low address):
/// - xPSR (thumb bit set)
/// - PC (task entry point)
/// - LR (task exit error handler)
/// - R12, R3, R2, R1, R0 (R0 = pvParameters)
/// - EXC_RETURN
/// - R11, R10, R9, R8, R7, R6, R5, R4
pub fn pxPortInitialiseStack(
    pxTopOfStack: *mut StackType_t,
    pxCode: TaskFunction_t,
    pvParameters: *mut c_void,
) -> *mut StackType_t {
    unsafe {
        let mut pxStack = pxTopOfStack;

        // Offset for 8-byte alignment
        pxStack = pxStack.sub(1);

        // xPSR - thumb bit must be set
        *pxStack = portINITIAL_XPSR;
        pxStack = pxStack.sub(1);

        // PC - task entry point (clear bit 0 for exception return)
        *pxStack = (pxCode as StackType_t) & portSTART_ADDRESS_MASK;
        pxStack = pxStack.sub(1);

        // LR - return address (prvTaskExitError would go here)
        *pxStack = prvTaskExitError as StackType_t;
        pxStack = pxStack.sub(1);

        // R12 - skip initialization
        pxStack = pxStack.sub(1);
        // R3
        pxStack = pxStack.sub(1);
        // R2
        pxStack = pxStack.sub(1);
        // R1
        pxStack = pxStack.sub(1);

        // R0 - first argument = pvParameters
        *pxStack = pvParameters as StackType_t;
        pxStack = pxStack.sub(1);

        // EXC_RETURN - indicates return to thread mode using PSP, no FPU
        *pxStack = portINITIAL_EXC_RETURN;

        // R11, R10, R9, R8, R7, R6, R5, R4 - skip initialization
        pxStack = pxStack.sub(8);

        pxStack
    }
}

/// Called if a task returns from its implementing function
fn prvTaskExitError() -> ! {
    portDISABLE_INTERRUPTS();
    loop {
        cortex_m::asm::wfi();
    }
}

// =============================================================================
// Exception Handlers
// =============================================================================

/// SVC Handler - starts the first task
///
/// This is called via `svc 0` from prvPortStartFirstTask.
/// It restores the context of the first task from pxCurrentTCB.
#[unsafe(naked)]
#[no_mangle]
pub unsafe extern "C" fn vPortSVCHandler() {
    naked_asm!(
        // Get pxCurrentTCB address
        "ldr r3, =pxCurrentTCB",
        "ldr r1, [r3]",           // r1 = pxCurrentTCB
        "ldr r0, [r1]",           // r0 = pxCurrentTCB->pxTopOfStack

        // Pop the core registers (R4-R11, R14/EXC_RETURN)
        "ldmia r0!, {{r4-r11, r14}}",

        // Set the process stack pointer
        "msr psp, r0",
        "isb",

        // Enable interrupts
        "mov r0, #0",
        "msr basepri, r0",

        // Return to the first task
        "bx r14",
    );
}

/// PendSV Handler - performs context switch
///
/// This is triggered by setting the PENDSVSET bit in ICSR.
/// It saves the current task's context and restores the next task's context.
#[unsafe(naked)]
#[no_mangle]
pub unsafe extern "C" fn xPortPendSVHandler() {
    naked_asm!(
        // Specify FPU for Cortex-M4F
        ".fpu fpv4-sp-d16",

        // Get the process stack pointer (current task's stack)
        "mrs r0, psp",
        "isb",

        // Get pxCurrentTCB address
        "ldr r3, =pxCurrentTCB",
        "ldr r2, [r3]",           // r2 = pxCurrentTCB

        // Check if FPU context needs saving (bit 4 of EXC_RETURN)
        "tst r14, #0x10",
        "it eq",
        "vstmdbeq r0!, {{s16-s31}}",  // Save FPU high registers if used

        // Save core registers R4-R11 and EXC_RETURN (R14)
        "stmdb r0!, {{r4-r11, r14}}",

        // Save the new top of stack into the TCB
        "str r0, [r2]",           // pxCurrentTCB->pxTopOfStack = r0

        // Save r0 and r3 on main stack before calling C function
        "stmdb sp!, {{r0, r3}}",

        // Disable interrupts during context switch
        "mov r0, #{max_syscall_pri}",
        "msr basepri, r0",
        "dsb",
        "isb",

        // Call vTaskSwitchContext to select the next task
        "bl vTaskSwitchContext",

        // Re-enable interrupts
        "mov r0, #0",
        "msr basepri, r0",

        // Restore r0 and r3 from main stack
        "ldmia sp!, {{r0, r3}}",

        // Get the new task's TCB
        "ldr r1, [r3]",           // r1 = new pxCurrentTCB
        "ldr r0, [r1]",           // r0 = new pxCurrentTCB->pxTopOfStack

        // Pop the core registers (R4-R11, R14/EXC_RETURN)
        "ldmia r0!, {{r4-r11, r14}}",

        // Check if FPU context needs restoring
        "tst r14, #0x10",
        "it eq",
        "vldmiaeq r0!, {{s16-s31}}",  // Restore FPU high registers if needed

        // Set the process stack pointer for the new task
        "msr psp, r0",
        "isb",

        // Return to the new task
        "bx r14",

        max_syscall_pri = const portMAX_SYSCALL_INTERRUPT_PRIORITY,
    );
}

/// SysTick Handler - tick interrupt
///
/// Called on every tick. Increments the tick count and checks if
/// a context switch is needed.
#[no_mangle]
pub extern "C" fn xPortSysTickHandler() {
    let saved = portSET_INTERRUPT_MASK_FROM_ISR();

    crate::trace::traceISR_ENTER();

    if crate::kernel::tasks::xTaskIncrementTick() != pdFALSE {
        crate::trace::traceISR_EXIT_TO_SCHEDULER();
        unsafe {
            core::ptr::write_volatile(NVIC_ICSR, NVIC_PENDSVSET_BIT);
        }
    } else {
        crate::trace::traceISR_EXIT();
    }

    portCLEAR_INTERRUPT_MASK_FROM_ISR(saved);
}

// =============================================================================
// Scheduler Start
// =============================================================================

/// Start the first task
///
/// Resets MSP, clears CONTROL, enables interrupts, and triggers SVC.
#[unsafe(naked)]
unsafe extern "C" fn prvPortStartFirstTask() {
    naked_asm!(
        // Use the NVIC offset register to locate the stack
        // Load VTOR address using literal pool
        "ldr r0, 1f",             // Load VTOR address from literal pool
        "ldr r0, [r0]",           // Vector table address
        "ldr r0, [r0]",           // Initial MSP value (first entry)
        "msr msp, r0",            // Reset MSP to initial value

        // Clear CONTROL register to use MSP and privileged mode
        "mov r0, #0",
        "msr control, r0",

        // Enable interrupts
        "cpsie i",
        "cpsie f",

        // Synchronization barriers
        "dsb",
        "isb",

        // Trigger SVC to start the first task
        "svc 0",

        // Should never get here
        "nop",
        "b .",

        // Literal pool
        ".align 4",
        "1: .word 0xE000ED08",    // VTOR register address
    );
}

/// Set up the SysTick timer
pub fn vPortSetupTimerInterrupt() {
    unsafe {
        // Disable SysTick
        core::ptr::write_volatile(SYST_CSR, 0);

        // Set reload value = (CPU clock / tick rate) - 1
        let reload = (configCPU_CLOCK_HZ / configTICK_RATE_HZ as u32) - 1;
        core::ptr::write_volatile(SYST_RVR, reload);

        // Clear current value
        core::ptr::write_volatile(SYST_CVR, 0);

        // Enable SysTick with processor clock and interrupt
        core::ptr::write_volatile(
            SYST_CSR,
            SYST_CSR_ENABLE | SYST_CSR_TICKINT | SYST_CSR_CLKSOURCE,
        );
    }
}

/// Start the scheduler
///
/// Sets up interrupt priorities, the tick timer, and starts the first task.
/// This function does not return.
pub fn xPortStartScheduler() -> BaseType_t {
    // Set PendSV and SysTick to lowest priority
    // SHPR3 register: PendSV priority in bits [23:16], SysTick in bits [31:24]
    const SHPR3: *mut u32 = 0xE000_ED20 as *mut u32;
    unsafe {
        let mut shpr3 = core::ptr::read_volatile(SHPR3);
        shpr3 |= (portMIN_INTERRUPT_PRIORITY as u32) << 16; // PendSV
        shpr3 |= (portMIN_INTERRUPT_PRIORITY as u32) << 24; // SysTick
        core::ptr::write_volatile(SHPR3, shpr3);
    }

    // Initialize critical nesting count
    CRITICAL_NESTING.store(0, Ordering::SeqCst);

    // Set up the tick timer
    vPortSetupTimerInterrupt();

    // Start the first task
    unsafe {
        prvPortStartFirstTask();
    }

    // Should never get here
    0
}

/// End the scheduler (not really supported on Cortex-M)
pub fn vPortEndScheduler() {
    loop {
        cortex_m::asm::wfi();
    }
}

// =============================================================================
// Utility Functions
// =============================================================================

/// Check if currently executing in an interrupt context
#[inline(always)]
pub fn xPortIsInsideInterrupt() -> BaseType_t {
    let ipsr: u32;
    unsafe {
        core::arch::asm!("mrs {}, ipsr", out(reg) ipsr);
    }
    if ipsr != 0 {
        pdTRUE
    } else {
        pdFALSE
    }
}

/// No-operation
#[inline(always)]
pub fn portNOP() {
    cortex_m::asm::nop();
}

/// Memory barrier
#[inline(always)]
pub fn portMEMORY_BARRIER() {
    cortex_m::asm::dmb();
}

// =============================================================================
// Architecture Name
// =============================================================================

/// Architecture name string for this port
pub const portARCH_NAME: &str = "ARM Cortex-M4F";

// =============================================================================
// Exception Handler Aliases for cortex-m-rt
// =============================================================================

// cortex-m-rt expects exception handlers named SVCall, PendSV, SysTick.
// FreeRTOS names them vPortSVCHandler, xPortPendSVHandler, xPortSysTickHandler.
// Create aliases via assembly to maintain FreeRTOS naming in the port code.
use core::arch::global_asm;

global_asm!(
    ".thumb_func",
    ".global SVCall",
    ".type SVCall, %function",
    "SVCall:",
    "b vPortSVCHandler",

    ".thumb_func",
    ".global PendSV",
    ".type PendSV, %function",
    "PendSV:",
    "b xPortPendSVHandler",

    ".thumb_func",
    ".global SysTick",
    ".type SysTick, %function",
    "SysTick:",
    "b xPortSysTickHandler",
);
