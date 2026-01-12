/*
 * FreeRTOS Kernel <DEVELOPMENT BRANCH>
 * Copyright (C) 2021 Amazon.com, Inc. or its affiliates. All Rights Reserved.
 *
 * SPDX-License-Identifier: MIT
 *
 * [AMENDMENT] This module is the Rust equivalent of FreeRTOSConfig.h.
 * Configuration is done via:
 * - Cargo features for major toggles
 * - Constants in this module for numeric values
 * - Users can override by shadowing these in their own config module
 */

//! FreeRTOS Configuration
//!
//! This module provides the Rust equivalent of `FreeRTOSConfig.h`.
//! Configuration values are exposed as constants and can be overridden
//! by users in their own configuration.

use crate::types::*;

// =============================================================================
// Scheduler Configuration
// =============================================================================

/// Use preemptive scheduling (1) or cooperative scheduling (0)
pub const configUSE_PREEMPTION: BaseType_t = 1;

/// Maximum number of priority levels
pub const configMAX_PRIORITIES: UBaseType_t = 5;

/// Stack size for the idle task (in words, not bytes)
pub const configMINIMAL_STACK_SIZE: usize = 128;

/// Tick rate in Hz
pub const configTICK_RATE_HZ: TickType_t = 1000;

/// CPU clock frequency in Hz
/// [AMENDMENT] This should be set to match your target hardware.
/// Default: 80 MHz (common for Cortex-M4F parts like STM32L4, TI TM4C)
pub const configCPU_CLOCK_HZ: u32 = 80_000_000;

/// RISC-V MTIME timer frequency in Hz
/// [AMENDMENT] For RISC-V ports, the CLINT timer (MTIME) may run at a
/// different frequency than the CPU clock.
/// QEMU sifive_e: Timer is instruction-based (~1 tick per 6500 instructions).
/// Using 32768 Hz as effective rate for QEMU testing.
#[cfg(feature = "port-riscv32")]
pub const configMTIME_HZ: u32 = 32_768; // ~33 ticks per ms in QEMU

/// Maximum syscall interrupt priority
/// Interrupts with priority >= this value can call FreeRTOS "FromISR" APIs.
/// Lower values = higher priority on Cortex-M (0 = highest).
/// This is typically set to leave some high-priority interrupts always enabled.
pub const configMAX_SYSCALL_INTERRUPT_PRIORITY: u32 = 191; // 0xBF = priority 11 (of 0-15)

/// Maximum length of task names
pub const configMAX_TASK_NAME_LEN: usize = 16;

// =============================================================================
// Core Count (SMP)
// =============================================================================

/// Number of cores (1 = single core, >1 = SMP)
/// [AMENDMENT] Currently only single-core is supported. TODO: SMP support.
pub const configNUMBER_OF_CORES: BaseType_t = 1;

// =============================================================================
// Hook Functions
// =============================================================================

/// Enable idle hook function
pub const configUSE_IDLE_HOOK: BaseType_t = 0;

/// Enable tick hook function
pub const configUSE_TICK_HOOK: BaseType_t = 0;

/// Enable malloc failed hook
pub const configUSE_MALLOC_FAILED_HOOK: BaseType_t = 0;

// =============================================================================
// Memory Allocation
// =============================================================================

/// Support static allocation (xTaskCreateStatic, etc.)
pub const configSUPPORT_STATIC_ALLOCATION: BaseType_t = 1;

/// Support dynamic allocation (xTaskCreate, etc.)
#[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
pub const configSUPPORT_DYNAMIC_ALLOCATION: BaseType_t = 1;
#[cfg(not(any(feature = "alloc", feature = "heap-4", feature = "heap-5")))]
pub const configSUPPORT_DYNAMIC_ALLOCATION: BaseType_t = 0;

/// Total heap size when using FreeRTOS heap implementations
pub const configTOTAL_HEAP_SIZE: usize = 6144; // 6KB - fits in 16KB RAM devices

// =============================================================================
// Optional Features
// =============================================================================

/// Use mutexes
pub const configUSE_MUTEXES: BaseType_t = 1;

/// Use recursive mutexes
pub const configUSE_RECURSIVE_MUTEXES: BaseType_t = 1;

/// Use counting semaphores
pub const configUSE_COUNTING_SEMAPHORES: BaseType_t = 1;

/// Use queue sets
pub const configUSE_QUEUE_SETS: BaseType_t = 0;

/// Queue registry size (for kernel-aware debugging)
/// Set to the maximum number of queues and semaphores that can be registered.
/// A value of 0 disables the registry (handled via Cargo feature `queue-registry`).
#[cfg(feature = "queue-registry")]
pub const configQUEUE_REGISTRY_SIZE: usize = 8;

/// Use task notifications
pub const configUSE_TASK_NOTIFICATIONS: BaseType_t = 1;

/// Number of task notification array entries
pub const configTASK_NOTIFICATION_ARRAY_ENTRIES: usize = 1;

/// Use timers
pub const configUSE_TIMERS: BaseType_t = 1;

/// Timer task priority
pub const configTIMER_TASK_PRIORITY: UBaseType_t = 2;

/// Timer queue length
pub const configTIMER_QUEUE_LENGTH: UBaseType_t = 10;

/// Timer task stack depth
pub const configTIMER_TASK_STACK_DEPTH: usize = configMINIMAL_STACK_SIZE;

// =============================================================================
// List Configuration
// =============================================================================

/// Use mini list items for memory optimization
/// [AMENDMENT] TODO: MiniListItem support. Currently always uses full ListItem_t.
pub const configUSE_MINI_LIST_ITEM: BaseType_t = 0;

/// Enable list data integrity checks
/// [AMENDMENT] Controlled by Cargo feature `list-data-integrity-check`
#[cfg(feature = "list-data-integrity-check")]
pub const configUSE_LIST_DATA_INTEGRITY_CHECK_BYTES: BaseType_t = 1;
#[cfg(not(feature = "list-data-integrity-check"))]
pub const configUSE_LIST_DATA_INTEGRITY_CHECK_BYTES: BaseType_t = 0;

// =============================================================================
// Debug / Assert
// =============================================================================

/// configASSERT macro equivalent
/// [AMENDMENT] In Rust, we use debug_assert! or panic! depending on context.
/// This constant controls whether asserts are active.
pub const configASSERT_DEFINED: BaseType_t = 1;

/// Macro-like function for configASSERT
#[inline(always)]
pub fn configASSERT(condition: bool) {
    if configASSERT_DEFINED != 0 {
        debug_assert!(condition, "FreeRTOS assertion failed");
    }
}

// =============================================================================
// INCLUDE_* Function Inclusion
// =============================================================================

/// Include vTaskPrioritySet
pub const INCLUDE_vTaskPrioritySet: BaseType_t = 1;

/// Include uxTaskPriorityGet
pub const INCLUDE_uxTaskPriorityGet: BaseType_t = 1;

/// Include vTaskDelete
pub const INCLUDE_vTaskDelete: BaseType_t = 1;

/// Include vTaskSuspend
pub const INCLUDE_vTaskSuspend: BaseType_t = 1;

/// Include xTaskDelayUntil
pub const INCLUDE_xTaskDelayUntil: BaseType_t = 1;

/// Include vTaskDelay
pub const INCLUDE_vTaskDelay: BaseType_t = 1;

/// Include xTaskGetIdleTaskHandle
pub const INCLUDE_xTaskGetIdleTaskHandle: BaseType_t = 0;

/// Include xTaskAbortDelay
pub const INCLUDE_xTaskAbortDelay: BaseType_t = 0;

/// Include xQueueGetMutexHolder
pub const INCLUDE_xQueueGetMutexHolder: BaseType_t = 0;

/// Include xTaskGetHandle
pub const INCLUDE_xTaskGetHandle: BaseType_t = 0;

/// Include uxTaskGetStackHighWaterMark
pub const INCLUDE_uxTaskGetStackHighWaterMark: BaseType_t = 0;

/// Include uxTaskGetStackHighWaterMark2
pub const INCLUDE_uxTaskGetStackHighWaterMark2: BaseType_t = 0;

/// Include eTaskGetState
pub const INCLUDE_eTaskGetState: BaseType_t = 1;

/// Include xTaskResumeFromISR
pub const INCLUDE_xTaskResumeFromISR: BaseType_t = 1;

/// Include xTimerPendFunctionCall
pub const INCLUDE_xTimerPendFunctionCall: BaseType_t = 0;

/// Include xTaskGetSchedulerState
pub const INCLUDE_xTaskGetSchedulerState: BaseType_t = 1;

/// Include xTaskGetCurrentTaskHandle
pub const INCLUDE_xTaskGetCurrentTaskHandle: BaseType_t = 1;

// =============================================================================
// Task Configuration (for tasks.rs)
// =============================================================================

/// Enable trace facility for debugging
pub const configUSE_TRACE_FACILITY: BaseType_t = 0;

/// Enable run-time stats
pub const configGENERATE_RUN_TIME_STATS: BaseType_t = 0;

/// Enable application task tag
pub const configUSE_APPLICATION_TASK_TAG: BaseType_t = 0;

/// Number of thread local storage pointers
#[cfg(feature = "thread-local-storage")]
pub const configNUM_THREAD_LOCAL_STORAGE_POINTERS: usize = 5;
#[cfg(not(feature = "thread-local-storage"))]
pub const configNUM_THREAD_LOCAL_STORAGE_POINTERS: usize = 0;

/// Enable POSIX errno support
pub const configUSE_POSIX_ERRNO: BaseType_t = 0;

/// Use port-optimised task selection (bit manipulation)
/// Set to 0 for generic selection, 1 for port-specific
pub const configUSE_PORT_OPTIMISED_TASK_SELECTION: BaseType_t = 0;

/// Enable core affinity (SMP only)
pub const configUSE_CORE_AFFINITY: BaseType_t = 0;

/// Enable per-task preemption disable
pub const configUSE_TASK_PREEMPTION_DISABLE: BaseType_t = 0;

/// Enable tickless idle mode
pub const configUSE_TICKLESS_IDLE: BaseType_t = 0;

/// Minimum expected idle time before entering tickless sleep (in ticks).
/// The idle task will only attempt to enter a low-power state if the
/// expected idle time is at least this many ticks.
pub const configEXPECTED_IDLE_TIME_BEFORE_SLEEP: super::types::TickType_t = 2;

/// Initial tick count value
pub const configINITIAL_TICK_COUNT: super::types::TickType_t = 0;

/// Idle task priority (always lowest)
pub const tskIDLE_PRIORITY: super::types::UBaseType_t = 0;

/// Stack overflow checking level (0=disabled, 1=simple, 2=full)
pub const configCHECK_FOR_STACK_OVERFLOW: BaseType_t = 0;

/// Record the high address of the stack
pub const configRECORD_STACK_HIGH_ADDRESS: BaseType_t = 0;

/// Kernel will provide static memory for idle/timer tasks
pub const configKERNEL_PROVIDED_STATIC_MEMORY: BaseType_t = 0;

// =============================================================================
// Port-specific Configuration (normally in portmacro.h)
// =============================================================================

// NOTE: portSTACK_GROWTH is defined in the port layer, not here.
// Each port defines its own stack growth direction.

/// Critical nesting stored in TCB (vs port layer)
pub const portCRITICAL_NESTING_IN_TCB: BaseType_t = 0;

/// Using MPU wrappers (memory protection)
pub const portUSING_MPU_WRAPPERS: BaseType_t = 0;

// =============================================================================
// Type definitions that depend on config
// =============================================================================

/// Stack depth type (configSTACK_DEPTH_TYPE)
/// [AMENDMENT] In Rust, we use usize for stack depths
pub type configSTACK_DEPTH_TYPE = usize;

/// Run-time counter type
pub type configRUN_TIME_COUNTER_TYPE = u32;

// =============================================================================
// Derived Configuration
// =============================================================================

/// Set to 1 if both static and dynamic allocation are possible
/// This affects whether ucStaticallyAllocated is stored in TCB
pub const tskSTATIC_AND_DYNAMIC_ALLOCATION_POSSIBLE: BaseType_t =
    if configSUPPORT_STATIC_ALLOCATION != 0 && configSUPPORT_DYNAMIC_ALLOCATION != 0 {
        1
    } else {
        0
    };

/// Whether to fill new stacks with known value (for high water mark)
pub const tskSET_NEW_STACKS_TO_KNOWN_VALUE: BaseType_t = if configCHECK_FOR_STACK_OVERFLOW > 1
    || configUSE_TRACE_FACILITY != 0
    || INCLUDE_uxTaskGetStackHighWaterMark != 0
    || INCLUDE_uxTaskGetStackHighWaterMark2 != 0
{
    1
} else {
    0
};

/// Byte value used to fill task stacks for high water mark detection.
/// Stacks are filled with this value and then scanned to determine
/// how much has been used.
pub const tskSTACK_FILL_BYTE: u8 = 0xA5;
