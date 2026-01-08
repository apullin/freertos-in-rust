/*
 * FreeRTOS Kernel <DEVELOPMENT BRANCH>
 * Copyright (C) 2021 Amazon.com, Inc. or its affiliates. All Rights Reserved.
 *
 * SPDX-License-Identifier: MIT
 *
 * [AMENDMENT] This is the Cortex-A9 port for FreeRusTOS.
 * Ported from portable/GCC/ARM_CA9/port.c and portASM.S
 *
 * This port is for ARM Cortex-A9 (ARMv7-A) application processors.
 * It uses:
 * - GIC (Generic Interrupt Controller) for interrupt priority masking
 * - SWI (Software Interrupt) for context switching
 * - Platform-specific timer for tick interrupts (user provides configSETUP_TICK_INTERRUPT)
 *
 * Key differences from Cortex-M:
 * - Runs in System mode (privileged, uses user-mode registers)
 * - Uses GIC priority masking instead of NVIC BASEPRI
 * - Context switch via SWI instead of PendSV
 * - Multiple ARM processor modes (SYS, SVC, IRQ, etc.)
 */

//! ARM Cortex-A9 Port Implementation
//!
//! This port provides hardware-specific implementations for ARM Cortex-A9:
//! - Critical sections using GIC priority masking
//! - Context switching via SWI exception
//! - First task start via vPortRestoreTaskContext
//! - Tick timer is platform-specific (user-provided)
//!
//! ## Usage
//!
//! Enable with `--features port-cortex-a9` and compile for `armv7a-none-eabi`.
//!
//! ## Configuration Required
//!
//! Users must define the following in their configuration:
//! - `configINTERRUPT_CONTROLLER_BASE_ADDRESS` - GIC base address
//! - `configINTERRUPT_CONTROLLER_CPU_INTERFACE_OFFSET` - Offset to CPU interface
//! - `configUNIQUE_INTERRUPT_PRIORITIES` - Number of priority levels (16, 32, 64, etc.)
//! - `configMAX_API_CALL_INTERRUPT_PRIORITY` - Max priority for FreeRTOS API calls
//!
//! ## Vector Table
//!
//! The application must install FreeRTOS_SWI_Handler and FreeRTOS_IRQ_Handler
//! in the ARM vector table for SWI and IRQ exceptions.

use core::arch::global_asm;
use core::ffi::c_void;

use crate::config::*;
use crate::types::*;

// =============================================================================
// Port Constants
// =============================================================================

/// Stack growth direction: -1 for descending (ARM uses descending stack)
pub const portSTACK_GROWTH: BaseType_t = -1;

/// Byte alignment requirement for stack
pub const portBYTE_ALIGNMENT: usize = 8;

/// Architecture name string for this port
pub const portARCH_NAME: &str = "ARM Cortex-A9";

// =============================================================================
// ARM Processor Mode Constants
// =============================================================================

/// System mode - privileged, uses user-mode registers
const SYS_MODE: u32 = 0x1F;

/// Supervisor mode
const SVC_MODE: u32 = 0x13;

/// IRQ mode
const IRQ_MODE: u32 = 0x12;

/// Initial SPSR value: System mode, ARM state, IRQ+FIQ enabled
const portINITIAL_SPSR: StackType_t = SYS_MODE as StackType_t;

/// Thumb mode bit in SPSR
const portTHUMB_MODE_BIT: StackType_t = 0x20;

/// Thumb mode address indicator (LSB set)
const portTHUMB_MODE_ADDRESS: u32 = 0x01;

/// No critical nesting value
const portNO_CRITICAL_NESTING: u32 = 0;

/// No floating point context flag
const portNO_FLOATING_POINT_CONTEXT: StackType_t = 0;

/// APSR mode bits mask
const portAPSR_MODE_BITS_MASK: u32 = 0x1F;

/// User mode APSR value
const portAPSR_USER_MODE: u32 = 0x10;

/// Unmask all interrupt priorities
const portUNMASK_VALUE: u32 = 0xFF;

// =============================================================================
// GIC Configuration
// =============================================================================

/// GIC CPU interface register offsets
const portICCPMR_PRIORITY_MASK_OFFSET: usize = 0x04;
const portICCIAR_INTERRUPT_ACKNOWLEDGE_OFFSET: usize = 0x0C;
const portICCEOIR_END_OF_INTERRUPT_OFFSET: usize = 0x10;
const portICCBPR_BINARY_POINT_OFFSET: usize = 0x08;
const portICCRPR_RUNNING_PRIORITY_OFFSET: usize = 0x14;

/// Number of unique interrupt priorities (must be defined by user config)
/// Default to 32 if not specified (common for GICv1/v2)
#[cfg(not(any(
    feature = "gic-priorities-16",
    feature = "gic-priorities-64",
    feature = "gic-priorities-128",
    feature = "gic-priorities-256"
)))]
pub const configUNIQUE_INTERRUPT_PRIORITIES: u32 = 32;

/// Priority shift based on number of implemented priority bits
#[cfg(not(any(
    feature = "gic-priorities-16",
    feature = "gic-priorities-64",
    feature = "gic-priorities-128",
    feature = "gic-priorities-256"
)))]
const portPRIORITY_SHIFT: u32 = 3; // 32 priorities = 5 bits, shift by 3

/// Binary point maximum value
#[cfg(not(any(
    feature = "gic-priorities-16",
    feature = "gic-priorities-64",
    feature = "gic-priorities-128",
    feature = "gic-priorities-256"
)))]
const portMAX_BINARY_POINT_VALUE: u32 = 2;

/// Lowest interrupt priority
pub const portLOWEST_INTERRUPT_PRIORITY: u32 = configUNIQUE_INTERRUPT_PRIORITIES - 1;

/// Lowest usable interrupt priority (one above lowest)
pub const portLOWEST_USABLE_INTERRUPT_PRIORITY: u32 = portLOWEST_INTERRUPT_PRIORITY - 1;

// =============================================================================
// GIC Address Calculation
// =============================================================================

/// GIC base address - must be provided by user configuration
/// For QEMU vexpress-a9: 0x1E000000
/// Users should define this in their config or pass via build script
#[cfg(not(feature = "gic-base-custom"))]
pub const configINTERRUPT_CONTROLLER_BASE_ADDRESS: usize = 0x1E00_0000;

/// GIC CPU interface offset from base
/// For most GICv1/v2: 0x100
#[cfg(not(feature = "gic-base-custom"))]
pub const configINTERRUPT_CONTROLLER_CPU_INTERFACE_OFFSET: usize = 0x100;

/// Maximum API call interrupt priority (must be > UNIQUE_PRIORITIES / 2)
/// Interrupts at this priority or lower can call FreeRTOS FromISR APIs
#[cfg(not(feature = "gic-base-custom"))]
pub const configMAX_API_CALL_INTERRUPT_PRIORITY: u32 = 18;

/// Computed GIC CPU interface address
const GIC_CPU_INTERFACE_ADDRESS: usize =
    configINTERRUPT_CONTROLLER_BASE_ADDRESS + configINTERRUPT_CONTROLLER_CPU_INTERFACE_OFFSET;

/// GIC Priority Mask Register address
const ICCPMR_ADDRESS: usize = GIC_CPU_INTERFACE_ADDRESS + portICCPMR_PRIORITY_MASK_OFFSET;

/// GIC Interrupt Acknowledge Register address
const ICCIAR_ADDRESS: usize = GIC_CPU_INTERFACE_ADDRESS + portICCIAR_INTERRUPT_ACKNOWLEDGE_OFFSET;

/// GIC End of Interrupt Register address
const ICCEOIR_ADDRESS: usize = GIC_CPU_INTERFACE_ADDRESS + portICCEOIR_END_OF_INTERRUPT_OFFSET;

/// GIC Binary Point Register address
const ICCBPR_ADDRESS: usize = GIC_CPU_INTERFACE_ADDRESS + portICCBPR_BINARY_POINT_OFFSET;

/// GIC Running Priority Register address
const ICCRPR_ADDRESS: usize = GIC_CPU_INTERFACE_ADDRESS + portICCRPR_RUNNING_PRIORITY_OFFSET;

/// Maximum API priority mask value (shifted)
const ulMaxAPIPriorityMask: u32 = configMAX_API_CALL_INTERRUPT_PRIORITY << portPRIORITY_SHIFT;

// =============================================================================
// Port State Variables
// =============================================================================

/// Critical section nesting counter.
/// Initialized to a non-zero value to ensure interrupts don't get unmasked
/// before the scheduler starts. Set to 0 when first task starts.
#[no_mangle]
pub static mut ulCriticalNesting: u32 = 9999;

/// FPU context flag for current task.
/// 0 = no FPU context, non-zero = task has FPU context
#[no_mangle]
pub static mut ulPortTaskHasFPUContext: u32 = pdFALSE as u32;

/// Yield required flag. Set to pdTRUE in ISR to request context switch.
#[no_mangle]
pub static mut ulPortYieldRequired: u32 = pdFALSE as u32;

/// Interrupt nesting depth. Context switch only allowed when this is 0.
#[no_mangle]
pub static mut ulPortInterruptNesting: u32 = 0;

// GIC register addresses for assembly code
#[no_mangle]
pub static ulICCIARAddress: u32 = ICCIAR_ADDRESS as u32;
#[no_mangle]
pub static ulICCEOIRAddress: u32 = ICCEOIR_ADDRESS as u32;
#[no_mangle]
pub static ulICCPMRAddress: u32 = ICCPMR_ADDRESS as u32;
#[no_mangle]
pub static ulMaxAPIPriorityMaskConst: u32 = ulMaxAPIPriorityMask;

// =============================================================================
// Critical Section Management
// =============================================================================

/// Enter a critical section by masking interrupts via GIC priority register
#[inline(always)]
pub fn portENTER_CRITICAL() {
    vPortEnterCritical();
}

/// Exit a critical section, potentially unmasking interrupts
#[inline(always)]
pub fn portEXIT_CRITICAL() {
    vPortExitCritical();
}

/// Enter critical section implementation
#[no_mangle]
pub extern "C" fn vPortEnterCritical() {
    // Mask interrupts up to max syscall priority
    ulPortSetInterruptMask();

    // Increment nesting count
    unsafe {
        ulCriticalNesting += 1;

        // Assert we're not in interrupt context (only on first entry)
        if ulCriticalNesting == 1 {
            configASSERT(ulPortInterruptNesting == 0);
        }
    }
}

/// Exit critical section implementation
#[no_mangle]
pub extern "C" fn vPortExitCritical() {
    unsafe {
        if ulCriticalNesting > portNO_CRITICAL_NESTING {
            ulCriticalNesting -= 1;

            if ulCriticalNesting == portNO_CRITICAL_NESTING {
                // Unmask all interrupt priorities
                portCLEAR_INTERRUPT_MASK();
            }
        }
    }
}

/// Disable interrupts by setting GIC priority mask
#[inline(always)]
pub fn portDISABLE_INTERRUPTS() -> UBaseType_t {
    ulPortSetInterruptMask()
}

/// Enable interrupts by clearing GIC priority mask
#[inline(always)]
pub fn portENABLE_INTERRUPTS() {
    vPortClearInterruptMask(0);
}

/// Set interrupt mask and return previous state
#[no_mangle]
pub extern "C" fn ulPortSetInterruptMask() -> u32 {
    let ulReturn: u32;

    unsafe {
        // Disable CPU IRQ while modifying ICCPMR
        core::arch::asm!("cpsid i", options(nomem, nostack));

        let iccpmr = ICCPMR_ADDRESS as *mut u32;
        let current = core::ptr::read_volatile(iccpmr);

        if current == ulMaxAPIPriorityMask {
            // Already masked
            ulReturn = pdTRUE as u32;
        } else {
            ulReturn = pdFALSE as u32;
            core::ptr::write_volatile(iccpmr, ulMaxAPIPriorityMask);
            core::arch::asm!("dsb", "isb", options(nomem, nostack));
        }

        // Re-enable CPU IRQ
        core::arch::asm!("cpsie i", options(nomem, nostack));
    }

    ulReturn
}

/// Clear interrupt mask (restore previous state)
#[no_mangle]
pub extern "C" fn vPortClearInterruptMask(ulNewMaskValue: u32) {
    if ulNewMaskValue == pdFALSE as u32 {
        portCLEAR_INTERRUPT_MASK();
    }
}

/// Clear interrupt mask macro implementation
#[inline(always)]
fn portCLEAR_INTERRUPT_MASK() {
    unsafe {
        core::arch::asm!("cpsid i", options(nomem, nostack));

        let iccpmr = ICCPMR_ADDRESS as *mut u32;
        core::ptr::write_volatile(iccpmr, portUNMASK_VALUE);

        core::arch::asm!("dsb", "isb", options(nomem, nostack));
        core::arch::asm!("cpsie i", options(nomem, nostack));
    }
}

/// Set interrupt mask from ISR
#[inline(always)]
pub fn portSET_INTERRUPT_MASK_FROM_ISR() -> UBaseType_t {
    ulPortSetInterruptMask()
}

/// Clear interrupt mask from ISR
#[inline(always)]
pub fn portCLEAR_INTERRUPT_MASK_FROM_ISR(uxSavedInterruptStatus: UBaseType_t) {
    vPortClearInterruptMask(uxSavedInterruptStatus as u32);
}

// =============================================================================
// Context Switching / Yield
// =============================================================================

/// Trigger a context switch via SWI
#[inline(always)]
pub fn portYIELD() {
    unsafe {
        core::arch::asm!("svc 0", options(nomem, nostack));
    }
}

/// Yield from ISR by setting yield required flag
#[inline(always)]
pub fn portYIELD_FROM_ISR(xSwitchRequired: BaseType_t) {
    if xSwitchRequired != pdFALSE {
        unsafe {
            ulPortYieldRequired = pdTRUE as u32;
        }
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

/// Initialize a task's stack for Cortex-A9
///
/// Stack layout (high to low address):
/// - 3 NULL values (for GDB stack unwinding)
/// - SPSR (System mode, optionally Thumb bit)
/// - PC (task entry point)
/// - LR (R14) - prvTaskExitError
/// - R12, R11, R10, R9, R8, R7, R6, R5, R4, R3, R2, R1
/// - R0 (pvParameters)
/// - ulCriticalNesting (0)
/// - ulPortTaskHasFPUContext (0 or pdTRUE with FPU registers)
pub fn pxPortInitialiseStack(
    pxTopOfStack: *mut StackType_t,
    pxCode: TaskFunction_t,
    pvParameters: *mut c_void,
) -> *mut StackType_t {
    unsafe {
        let mut pxStack = pxTopOfStack;

        // Add NULL values for GDB stack unwinding
        *pxStack = 0;
        pxStack = pxStack.sub(1);
        *pxStack = 0;
        pxStack = pxStack.sub(1);
        *pxStack = 0;
        pxStack = pxStack.sub(1);

        // SPSR - System mode, ARM state (check for Thumb entry)
        let mut spsr = portINITIAL_SPSR;
        if (pxCode as u32 & portTHUMB_MODE_ADDRESS) != 0 {
            spsr |= portTHUMB_MODE_BIT;
        }
        *pxStack = spsr;
        pxStack = pxStack.sub(1);

        // PC - task entry point
        *pxStack = pxCode as StackType_t;
        pxStack = pxStack.sub(1);

        // LR (R14) - task exit error handler
        *pxStack = prvTaskExitError as StackType_t;
        pxStack = pxStack.sub(1);

        // R12
        *pxStack = 0x12121212;
        pxStack = pxStack.sub(1);

        // R11
        *pxStack = 0x11111111;
        pxStack = pxStack.sub(1);

        // R10
        *pxStack = 0x10101010;
        pxStack = pxStack.sub(1);

        // R9
        *pxStack = 0x09090909;
        pxStack = pxStack.sub(1);

        // R8
        *pxStack = 0x08080808;
        pxStack = pxStack.sub(1);

        // R7
        *pxStack = 0x07070707;
        pxStack = pxStack.sub(1);

        // R6
        *pxStack = 0x06060606;
        pxStack = pxStack.sub(1);

        // R5
        *pxStack = 0x05050505;
        pxStack = pxStack.sub(1);

        // R4
        *pxStack = 0x04040404;
        pxStack = pxStack.sub(1);

        // R3
        *pxStack = 0x03030303;
        pxStack = pxStack.sub(1);

        // R2
        *pxStack = 0x02020202;
        pxStack = pxStack.sub(1);

        // R1
        *pxStack = 0x01010101;
        pxStack = pxStack.sub(1);

        // R0 - first argument (pvParameters)
        *pxStack = pvParameters as StackType_t;
        pxStack = pxStack.sub(1);

        // Critical nesting count (0 = interrupts enabled)
        *pxStack = portNO_CRITICAL_NESTING as StackType_t;
        pxStack = pxStack.sub(1);

        // FPU context flag (no FPU context initially)
        *pxStack = portNO_FLOATING_POINT_CONTEXT;

        pxStack
    }
}

/// Called if a task returns from its implementing function
fn prvTaskExitError() -> ! {
    // Tasks should never return. If they do, halt.
    configASSERT(false);
    portDISABLE_INTERRUPTS();
    loop {
        unsafe {
            core::arch::asm!("wfi", options(nomem, nostack));
        }
    }
}

// =============================================================================
// Scheduler Start
// =============================================================================

/// Start the scheduler
///
/// Verifies we're not in user mode, sets up tick interrupt, and
/// starts the first task via vPortRestoreTaskContext.
#[no_mangle]
pub extern "C" fn xPortStartScheduler() -> BaseType_t {
    let ulAPSR: u32;

    // Check processor mode
    unsafe {
        core::arch::asm!("mrs {}, cpsr", out(reg) ulAPSR);
    }
    let mode = ulAPSR & portAPSR_MODE_BITS_MASK;

    configASSERT(mode != portAPSR_USER_MODE);

    if mode != portAPSR_USER_MODE {
        // Check binary point register is set correctly
        let bpr = unsafe { core::ptr::read_volatile(ICCBPR_ADDRESS as *const u32) };
        configASSERT((bpr & 0x03) <= portMAX_BINARY_POINT_VALUE);

        if (bpr & 0x03) <= portMAX_BINARY_POINT_VALUE {
            // Disable CPU IRQ before starting
            unsafe {
                core::arch::asm!("cpsid i", options(nomem, nostack));
            }

            // User must call vPortSetupTimerInterrupt or equivalent
            // before calling vTaskStartScheduler

            // Start first task
            unsafe {
                vPortRestoreTaskContext();
            }
        }
    }

    // Should never get here
    0
}

/// End the scheduler (not really supported on Cortex-A)
pub fn vPortEndScheduler() {
    configASSERT(false);
    loop {
        unsafe {
            core::arch::asm!("wfi", options(nomem, nostack));
        }
    }
}

// =============================================================================
// Tick Handler
// =============================================================================

/// FreeRTOS tick handler - called from platform-specific timer ISR
///
/// The user must:
/// 1. Set up a timer to call this function at configTICK_RATE_HZ
/// 2. Clear the timer interrupt after calling this
#[no_mangle]
pub extern "C" fn FreeRTOS_Tick_Handler() {
    // Mask interrupts up to max syscall priority
    unsafe {
        core::arch::asm!("cpsid i", options(nomem, nostack));

        let iccpmr = ICCPMR_ADDRESS as *mut u32;
        core::ptr::write_volatile(iccpmr, ulMaxAPIPriorityMask);

        core::arch::asm!("dsb", "isb", options(nomem, nostack));
        core::arch::asm!("cpsie i", options(nomem, nostack));
    }

    // Increment tick count
    if crate::kernel::tasks::xTaskIncrementTick() != pdFALSE {
        unsafe {
            ulPortYieldRequired = pdTRUE as u32;
        }
    }

    // Unmask all priorities
    portCLEAR_INTERRUPT_MASK();

    // User must clear tick interrupt in their timer handler
}

// =============================================================================
// FPU Support
// =============================================================================

/// Mark current task as using FPU
///
/// Call this from a task before using any FPU instructions.
/// This ensures the FPU context will be saved/restored on context switch.
#[no_mangle]
pub extern "C" fn vPortTaskUsesFPU() {
    unsafe {
        ulPortTaskHasFPUContext = pdTRUE as u32;

        // Initialize FPSCR
        let fpscr: u32 = 0;
        core::arch::asm!("vmsr fpscr, {}", in(reg) fpscr, options(nomem, nostack));
    }
}

// =============================================================================
// Utility Functions
// =============================================================================

/// Check if currently executing in an interrupt context
#[inline(always)]
pub fn xPortIsInsideInterrupt() -> BaseType_t {
    unsafe {
        if ulPortInterruptNesting > 0 {
            pdTRUE
        } else {
            pdFALSE
        }
    }
}

/// No-operation
#[inline(always)]
pub fn portNOP() {
    unsafe {
        core::arch::asm!("nop", options(nomem, nostack));
    }
}

/// Memory barrier
#[inline(always)]
pub fn portMEMORY_BARRIER() {
    unsafe {
        core::arch::asm!("dmb", options(nomem, nostack));
    }
}

// =============================================================================
// Run-time Stats Timer Support
// =============================================================================

#[cfg(feature = "generate-run-time-stats")]
static mut ulRunTimeCounterValue: crate::config::configRUN_TIME_COUNTER_TYPE = 0;

#[cfg(feature = "generate-run-time-stats")]
#[inline(always)]
pub fn portCONFIGURE_TIMER_FOR_RUN_TIME_STATS() {
    unsafe {
        ulRunTimeCounterValue = 0;
    }
}

#[cfg(feature = "generate-run-time-stats")]
#[inline(always)]
pub fn portGET_RUN_TIME_COUNTER_VALUE() -> crate::config::configRUN_TIME_COUNTER_TYPE {
    unsafe { ulRunTimeCounterValue }
}

#[cfg(feature = "generate-run-time-stats")]
#[inline(always)]
pub fn portINCREMENT_RUN_TIME_COUNTER() {
    unsafe {
        ulRunTimeCounterValue = ulRunTimeCounterValue.wrapping_add(1);
    }
}

// =============================================================================
// Tickless Idle (Stub)
// =============================================================================

/// Tickless idle is platform-specific for Cortex-A
pub fn vPortSuppressTicksAndSleep(_xExpectedIdleTime: TickType_t) {
    // Platform-specific implementation required
}

// =============================================================================
// Assembly Handlers
// =============================================================================

// External references for assembly code
extern "C" {
    fn vPortRestoreTaskContext();
}

// Assembly code for context save/restore and exception handlers
global_asm!(
    // ARM mode
    ".arm",
    ".align 4",

    // Processor mode constants
    ".set SYS_MODE, 0x1f",
    ".set SVC_MODE, 0x13",
    ".set IRQ_MODE, 0x12",

    // ==========================================================================
    // portSAVE_CONTEXT macro
    // ==========================================================================
    ".macro portSAVE_CONTEXT",
    // Save LR and SPSR to system mode stack, then switch to system mode
    "srsdb sp!, #SYS_MODE",
    "cps #SYS_MODE",
    // Save R0-R12 and R14
    "push {{r0-r12, r14}}",

    // Push critical nesting count
    "ldr r2, =ulCriticalNesting",
    "ldr r1, [r2]",
    "push {{r1}}",

    // Check if FPU context needs saving
    "ldr r2, =ulPortTaskHasFPUContext",
    "ldr r3, [r2]",
    "cmp r3, #0",

    // Save FPU context if needed (conditional)
    "fmrxne r1, fpscr",
    "pushne {{r1}}",
    "vpushne {{d0-d15}}",
    "vpushne {{d16-d31}}",

    // Push FPU context flag
    "push {{r3}}",

    // Save stack pointer to TCB
    "ldr r0, =pxCurrentTCB",
    "ldr r1, [r0]",
    "str sp, [r1]",
    ".endm",

    // ==========================================================================
    // portRESTORE_CONTEXT macro
    // ==========================================================================
    ".macro portRESTORE_CONTEXT",
    // Get stack pointer from TCB
    "ldr r0, =pxCurrentTCB",
    "ldr r1, [r0]",
    "ldr sp, [r1]",

    // Pop FPU context flag
    "ldr r0, =ulPortTaskHasFPUContext",
    "pop {{r1}}",
    "str r1, [r0]",
    "cmp r1, #0",

    // Restore FPU context if needed (conditional)
    "vpopne {{d16-d31}}",
    "vpopne {{d0-d15}}",
    "popne {{r0}}",
    "vmsrne fpscr, r0",

    // Pop critical nesting count
    "ldr r0, =ulCriticalNesting",
    "pop {{r1}}",
    "str r1, [r0]",

    // Set GIC priority mask based on critical nesting
    "ldr r2, =ulICCPMRAddress",
    "ldr r2, [r2]",
    "cmp r1, #0",
    "moveq r4, #255",
    "ldrne r4, =ulMaxAPIPriorityMaskConst",
    "ldrne r4, [r4]",
    "str r4, [r2]",

    // Restore R0-R12 and R14
    "pop {{r0-r12, r14}}",

    // Return to task, loading CPSR
    "rfeia sp!",
    ".endm",

    // ==========================================================================
    // FreeRTOS_SWI_Handler - Software Interrupt Handler (Context Switch)
    // ==========================================================================
    ".global FreeRTOS_SWI_Handler",
    ".type FreeRTOS_SWI_Handler, %function",
    "FreeRTOS_SWI_Handler:",
    // Save context of current task
    "portSAVE_CONTEXT",

    // Ensure 8-byte stack alignment
    "mov r2, sp",
    "and r2, r2, #4",
    "sub sp, sp, r2",

    // Call vTaskSwitchContext
    "bl vTaskSwitchContext",

    // Restore context of next task
    "portRESTORE_CONTEXT",

    // ==========================================================================
    // vPortRestoreTaskContext - Start First Task
    // ==========================================================================
    ".global vPortRestoreTaskContext",
    ".type vPortRestoreTaskContext, %function",
    "vPortRestoreTaskContext:",
    // Switch to system mode and restore first task
    "cps #SYS_MODE",
    "portRESTORE_CONTEXT",

    // ==========================================================================
    // FreeRTOS_IRQ_Handler - IRQ Handler
    // ==========================================================================
    ".global FreeRTOS_IRQ_Handler",
    ".type FreeRTOS_IRQ_Handler, %function",
    "FreeRTOS_IRQ_Handler:",
    // Adjust LR for return (IRQ returns to next instruction)
    "sub lr, lr, #4",

    // Push return address and SPSR
    "push {{lr}}",
    "mrs lr, spsr",
    "push {{lr}}",

    // Switch to supervisor mode for reentry
    "cps #SVC_MODE",

    // Save used registers
    "push {{r0-r4, r12}}",

    // Increment interrupt nesting count
    "ldr r3, =ulPortInterruptNesting",
    "ldr r1, [r3]",
    "add r4, r1, #1",
    "str r4, [r3]",

    // Read interrupt acknowledge register
    "ldr r2, =ulICCIARAddress",
    "ldr r2, [r2]",
    "ldr r0, [r2]",

    // Ensure 8-byte stack alignment
    "mov r2, sp",
    "and r2, r2, #4",
    "sub sp, sp, r2",

    // Call application IRQ handler (r0 = ICCIAR value)
    "push {{r0-r4, lr}}",
    "bl vApplicationIRQHandler",
    "pop {{r0-r4, lr}}",
    "add sp, sp, r2",

    // Disable interrupts for EOI
    "cpsid i",
    "dsb",
    "isb",

    // Write to End of Interrupt register
    "ldr r4, =ulICCEOIRAddress",
    "ldr r4, [r4]",
    "str r0, [r4]",

    // Restore interrupt nesting count
    "str r1, [r3]",

    // Only switch context if nesting is 0
    "cmp r1, #0",
    "bne exit_without_switch",

    // Check if context switch requested
    "ldr r1, =ulPortYieldRequired",
    "ldr r0, [r1]",
    "cmp r0, #0",
    "bne switch_before_exit",

    "exit_without_switch:",
    // Restore used registers and return
    "pop {{r0-r4, r12}}",
    "cps #IRQ_MODE",
    "pop {{lr}}",
    "msr spsr_cxsf, lr",
    "pop {{lr}}",
    "movs pc, lr",

    "switch_before_exit:",
    // Clear yield flag
    "mov r0, #0",
    "str r0, [r1]",

    // Restore registers and prepare for context switch
    "pop {{r0-r4, r12}}",
    "cps #IRQ_MODE",
    "pop {{lr}}",
    "msr spsr_cxsf, lr",
    "pop {{lr}}",

    // Save context and switch
    "portSAVE_CONTEXT",

    // Ensure 8-byte alignment
    "mov r2, sp",
    "and r2, r2, #4",
    "sub sp, sp, r2",

    // Call vTaskSwitchContext
    "bl vTaskSwitchContext",

    // Restore new task context
    "portRESTORE_CONTEXT",

    // ==========================================================================
    // Weak default IRQ handler (calls vApplicationFPUSafeIRQHandler)
    // ==========================================================================
    ".weak vApplicationIRQHandler",
    ".type vApplicationIRQHandler, %function",
    "vApplicationIRQHandler:",
    // Save FPU registers before calling safe handler
    "push {{lr}}",
    "fmrx r1, fpscr",
    "vpush {{d0-d7}}",
    "vpush {{d16-d31}}",
    "push {{r1}}",

    // Call FPU-safe handler
    "bl vApplicationFPUSafeIRQHandler",

    // Restore FPU registers
    "pop {{r0}}",
    "vpop {{d16-d31}}",
    "vpop {{d0-d7}}",
    "vmsr fpscr, r0",

    "pop {{pc}}",
);

// vApplicationFPUSafeIRQHandler is declared in assembly as .weak and expects
// users to provide a strong implementation. If not provided, the assembly
// default will call this (which will assert), but typically users override it.
