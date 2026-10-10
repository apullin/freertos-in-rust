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
 * - `user-config` feature for user-provided hardware-specific values
 */

//! FreeRTOS Configuration
//!
//! This module provides the Rust equivalent of `FreeRTOSConfig.h`.
//!
//! # Hardware-Specific Configuration
//!
//! **WARNING**: By default, this crate uses a placeholder CPU clock frequency
//! (80MHz) which is almost certainly WRONG for your hardware! SysTick timing
//! will be incorrect without proper configuration.
//!
//! To provide the correct CPU clock for your hardware, enable the `user-config`
//! feature and define the required symbol in your crate:
//!
//! ```ignore
//! // In your main.rs or lib.rs:
//! #[no_mangle]
//! pub static FREERTOS_CONFIG_CPU_CLOCK_HZ: u32 = 32_000_000;  // Your CPU clock
//!
//! // For RISC-V, also provide the MTIME frequency:
//! #[no_mangle]
//! pub static FREERTOS_CONFIG_MTIME_HZ: u32 = 10_000_000;  // Your timer frequency
//! ```
//!
//! The tick rate (`configTICK_RATE_HZ`) is fixed at 1000Hz (1ms ticks), which is
//! standard for most FreeRTOS applications.

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

/// Tick rate in Hz (1000 = 1ms ticks, standard for most applications)
pub const configTICK_RATE_HZ: TickType_t = 1000;

// =============================================================================
// Hardware-Specific Configuration (user-provided or defaults)
// =============================================================================

/// CPU clock frequency in Hz
/// \[AMENDMENT\] Default: 80 MHz - almost certainly WRONG for your hardware!
/// Enable `user-config` feature and provide FREERTOS_CONFIG_CPU_CLOCK_HZ.
#[cfg(not(feature = "user-config"))]
pub const configCPU_CLOCK_HZ: u32 = 80_000_000;

// When user-config is enabled, CPU clock comes from user-provided symbol
#[cfg(feature = "user-config")]
extern "Rust" {
    /// User-provided CPU clock frequency in Hz.
    /// Define in your crate: `#[no_mangle] pub static FREERTOS_CONFIG_CPU_CLOCK_HZ: u32 = ...;`
    #[link_name = "FREERTOS_CONFIG_CPU_CLOCK_HZ"]
    pub static configCPU_CLOCK_HZ: u32;
}

/// RISC-V MTIME timer frequency in Hz
/// \[AMENDMENT\] For RISC-V ports, the CLINT timer (MTIME) may run at a
/// different frequency than the CPU clock. Default is 32768 Hz (QEMU sifive_e).
/// Enable `user-config` feature and provide FREERTOS_CONFIG_MTIME_HZ for your hardware.
#[cfg(all(feature = "port-riscv32", not(feature = "user-config")))]
pub const configMTIME_HZ: u32 = 32_768;

#[cfg(all(feature = "port-riscv32", feature = "user-config"))]
extern "Rust" {
    /// User-provided MTIME timer frequency in Hz.
    /// Define in your crate: `#[no_mangle] pub static FREERTOS_CONFIG_MTIME_HZ: u32 = ...;`
    #[link_name = "FREERTOS_CONFIG_MTIME_HZ"]
    pub static configMTIME_HZ: u32;
}

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
/// \[AMENDMENT\] The experimental `smp` branch fixes this at two Cortex-A9
/// cores. The qualified 0.3 single-core configuration remains one core.
#[cfg(not(feature = "smp"))]
pub const configNUMBER_OF_CORES: BaseType_t = 1;
#[cfg(feature = "smp")]
pub const configNUMBER_OF_CORES: BaseType_t = 2;

/// Each core runs its highest-priority eligible task, even when the two
/// selected tasks have different priorities. This is the fixed SMP policy.
#[cfg(feature = "smp")]
pub const configRUN_MULTIPLE_PRIORITIES: BaseType_t = 1;

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
#[cfg(feature = "use-mutexes")]
pub const configUSE_MUTEXES: BaseType_t = 1;
#[cfg(not(feature = "use-mutexes"))]
pub const configUSE_MUTEXES: BaseType_t = 0;

/// Use recursive mutexes
#[cfg(feature = "use-mutexes")]
pub const configUSE_RECURSIVE_MUTEXES: BaseType_t = 1;
#[cfg(not(feature = "use-mutexes"))]
pub const configUSE_RECURSIVE_MUTEXES: BaseType_t = 0;

/// Use counting semaphores
pub const configUSE_COUNTING_SEMAPHORES: BaseType_t = 1;

/// Use queue sets
#[cfg(feature = "queue-sets")]
pub const configUSE_QUEUE_SETS: BaseType_t = 1;
#[cfg(not(feature = "queue-sets"))]
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
#[cfg(feature = "timers")]
pub const configUSE_TIMERS: BaseType_t = 1;
#[cfg(not(feature = "timers"))]
pub const configUSE_TIMERS: BaseType_t = 0;

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
/// \[AMENDMENT\] TODO: MiniListItem support. Currently always uses full ListItem_t.
pub const configUSE_MINI_LIST_ITEM: BaseType_t = 0;

/// Enable list data integrity checks
/// \[AMENDMENT\] Controlled by Cargo feature `list-data-integrity-check`
#[cfg(feature = "list-data-integrity-check")]
pub const configUSE_LIST_DATA_INTEGRITY_CHECK_BYTES: BaseType_t = 1;
#[cfg(not(feature = "list-data-integrity-check"))]
pub const configUSE_LIST_DATA_INTEGRITY_CHECK_BYTES: BaseType_t = 0;

// =============================================================================
// Debug / Assert
// =============================================================================

/// configASSERT macro equivalent
/// \[AMENDMENT\] Assertions remain active in release builds because FreeRTOS
/// relies on them to enforce API and kernel invariants.
pub const configASSERT_DEFINED: BaseType_t = 1;

/// Macro-like function for configASSERT
#[inline(always)]
#[track_caller]
pub fn configASSERT(condition: bool) {
    if configASSERT_DEFINED != 0 {
        assert!(condition, "FreeRTOS assertion failed");
    }
}

#[cfg(test)]
mod config_assert_tests {
    use super::*;

    #[test]
    fn config_assert_accepts_true() {
        configASSERT(true);
    }

    #[test]
    #[should_panic(expected = "FreeRTOS assertion failed")]
    fn config_assert_rejects_false_in_release_too() {
        configASSERT(false);
    }

    #[test]
    fn cargo_capabilities_match_exported_configuration_constants() {
        assert_eq!(configUSE_MUTEXES != 0, cfg!(feature = "use-mutexes"));
        assert_eq!(configUSE_QUEUE_SETS != 0, cfg!(feature = "queue-sets"));
        assert_eq!(
            INCLUDE_vTaskPrioritySet != 0,
            cfg!(feature = "task-priority-set")
        );
        assert_eq!(
            INCLUDE_uxTaskPriorityGet != 0,
            cfg!(feature = "task-priority-set")
        );
        assert_eq!(INCLUDE_vTaskDelete != 0, cfg!(feature = "task-delete"));
        assert_eq!(INCLUDE_vTaskSuspend != 0, cfg!(feature = "task-suspend"));
        assert_eq!(
            INCLUDE_xTaskResumeFromISR != 0,
            cfg!(feature = "task-suspend")
        );
        assert_eq!(
            INCLUDE_xQueueGetMutexHolder != 0,
            cfg!(feature = "use-mutexes")
        );
        assert_eq!(
            configGENERATE_RUN_TIME_STATS != 0,
            cfg!(feature = "generate-run-time-stats")
        );
        assert_eq!(
            configCHECK_FOR_STACK_OVERFLOW != 0,
            cfg!(feature = "stack-overflow-check")
        );

        /* Unsupported context-switch modes are not advertised as Cargo
         * capabilities. */
        assert_eq!(configUSE_POSIX_ERRNO, 0);
        assert_eq!(portCRITICAL_NESTING_IN_TCB, 0);
    }
}

// =============================================================================
// INCLUDE_* Function Inclusion
// =============================================================================

/// Include vTaskPrioritySet
#[cfg(feature = "task-priority-set")]
pub const INCLUDE_vTaskPrioritySet: BaseType_t = 1;
#[cfg(not(feature = "task-priority-set"))]
pub const INCLUDE_vTaskPrioritySet: BaseType_t = 0;

/// Include uxTaskPriorityGet
#[cfg(feature = "task-priority-set")]
pub const INCLUDE_uxTaskPriorityGet: BaseType_t = 1;
#[cfg(not(feature = "task-priority-set"))]
pub const INCLUDE_uxTaskPriorityGet: BaseType_t = 0;

/// Include vTaskDelete
#[cfg(feature = "task-delete")]
pub const INCLUDE_vTaskDelete: BaseType_t = 1;
#[cfg(not(feature = "task-delete"))]
pub const INCLUDE_vTaskDelete: BaseType_t = 0;

/// Include vTaskSuspend
#[cfg(feature = "task-suspend")]
pub const INCLUDE_vTaskSuspend: BaseType_t = 1;
#[cfg(not(feature = "task-suspend"))]
pub const INCLUDE_vTaskSuspend: BaseType_t = 0;

/// Include xTaskDelayUntil
pub const INCLUDE_xTaskDelayUntil: BaseType_t = 1;

/// Include vTaskDelay
pub const INCLUDE_vTaskDelay: BaseType_t = 1;

/// Include xTaskGetIdleTaskHandle
pub const INCLUDE_xTaskGetIdleTaskHandle: BaseType_t = 0;

/// Include xTaskAbortDelay
#[cfg(feature = "abort-delay")]
pub const INCLUDE_xTaskAbortDelay: BaseType_t = 1;
#[cfg(not(feature = "abort-delay"))]
pub const INCLUDE_xTaskAbortDelay: BaseType_t = 0;

/// Include xQueueGetMutexHolder
#[cfg(feature = "use-mutexes")]
pub const INCLUDE_xQueueGetMutexHolder: BaseType_t = 1;
#[cfg(not(feature = "use-mutexes"))]
pub const INCLUDE_xQueueGetMutexHolder: BaseType_t = 0;

/// Include xTaskGetHandle
pub const INCLUDE_xTaskGetHandle: BaseType_t = 0;

/// Include uxTaskGetStackHighWaterMark
#[cfg(feature = "stack-high-water-mark")]
pub const INCLUDE_uxTaskGetStackHighWaterMark: BaseType_t = 1;
#[cfg(not(feature = "stack-high-water-mark"))]
pub const INCLUDE_uxTaskGetStackHighWaterMark: BaseType_t = 0;

/// Include uxTaskGetStackHighWaterMark2
#[cfg(feature = "stack-high-water-mark")]
pub const INCLUDE_uxTaskGetStackHighWaterMark2: BaseType_t = 1;
#[cfg(not(feature = "stack-high-water-mark"))]
pub const INCLUDE_uxTaskGetStackHighWaterMark2: BaseType_t = 0;

/// Include eTaskGetState
pub const INCLUDE_eTaskGetState: BaseType_t = 1;

/// Include xTaskResumeFromISR
#[cfg(feature = "task-suspend")]
pub const INCLUDE_xTaskResumeFromISR: BaseType_t = 1;
#[cfg(not(feature = "task-suspend"))]
pub const INCLUDE_xTaskResumeFromISR: BaseType_t = 0;

/// Include xTimerPendFunctionCall
#[cfg(feature = "pend-function-call")]
pub const INCLUDE_xTimerPendFunctionCall: BaseType_t = 1;
#[cfg(not(feature = "pend-function-call"))]
pub const INCLUDE_xTimerPendFunctionCall: BaseType_t = 0;

/// Include xTaskGetSchedulerState
pub const INCLUDE_xTaskGetSchedulerState: BaseType_t = 1;

/// Include xTaskGetCurrentTaskHandle
pub const INCLUDE_xTaskGetCurrentTaskHandle: BaseType_t = 1;

// =============================================================================
// Task Configuration (for tasks.rs)
// =============================================================================

/// Enable trace facility for debugging
#[cfg(feature = "trace-facility")]
pub const configUSE_TRACE_FACILITY: BaseType_t = 1;
#[cfg(not(feature = "trace-facility"))]
pub const configUSE_TRACE_FACILITY: BaseType_t = 0;

/// Enable run-time stats
#[cfg(feature = "generate-run-time-stats")]
pub const configGENERATE_RUN_TIME_STATS: BaseType_t = 1;
#[cfg(not(feature = "generate-run-time-stats"))]
pub const configGENERATE_RUN_TIME_STATS: BaseType_t = 0;

/// Enable application task tag
#[cfg(feature = "application-task-tag")]
pub const configUSE_APPLICATION_TASK_TAG: BaseType_t = 1;
#[cfg(not(feature = "application-task-tag"))]
pub const configUSE_APPLICATION_TASK_TAG: BaseType_t = 0;

/// Number of thread local storage pointers
#[cfg(feature = "thread-local-storage")]
pub const configNUM_THREAD_LOCAL_STORAGE_POINTERS: usize = 5;
#[cfg(not(feature = "thread-local-storage"))]
pub const configNUM_THREAD_LOCAL_STORAGE_POINTERS: usize = 0;

/// Enable POSIX errno context switching.
///
/// \[AMENDMENT\] No supported port saves/restores a process errno value, so this
/// configuration is not exposed as a Cargo capability.
pub const configUSE_POSIX_ERRNO: BaseType_t = 0;

/// Use port-optimised task selection (bit manipulation)
/// Set to 0 for generic selection, 1 for port-specific
pub const configUSE_PORT_OPTIMISED_TASK_SELECTION: BaseType_t = 0;

/// Enable core affinity (SMP only)
#[cfg(feature = "core-affinity")]
pub const configUSE_CORE_AFFINITY: BaseType_t = 1;
#[cfg(not(feature = "core-affinity"))]
pub const configUSE_CORE_AFFINITY: BaseType_t = 0;

/// Enable per-task preemption disable
pub const configUSE_TASK_PREEMPTION_DISABLE: BaseType_t = 0;

/// Enable tickless idle mode
#[cfg(feature = "tickless-idle")]
pub const configUSE_TICKLESS_IDLE: BaseType_t = 1;
#[cfg(not(feature = "tickless-idle"))]
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
#[cfg(feature = "stack-overflow-check")]
pub const configCHECK_FOR_STACK_OVERFLOW: BaseType_t = 2;
#[cfg(not(feature = "stack-overflow-check"))]
pub const configCHECK_FOR_STACK_OVERFLOW: BaseType_t = 0;

/// Record the high address of the stack
#[cfg(feature = "record-stack-high-address")]
pub const configRECORD_STACK_HIGH_ADDRESS: BaseType_t = 1;
#[cfg(not(feature = "record-stack-high-address"))]
pub const configRECORD_STACK_HIGH_ADDRESS: BaseType_t = 0;

/// Kernel will provide static memory for idle/timer tasks
pub const configKERNEL_PROVIDED_STATIC_MEMORY: BaseType_t = 0;

// =============================================================================
// Port-specific Configuration (normally in portmacro.h)
// =============================================================================

// NOTE: portSTACK_GROWTH is defined in the port layer, not here.
// Each port defines its own stack growth direction.

/// Critical nesting stored in TCB (vs port layer).
///
/// \[AMENDMENT\] Supported ports save their port-global nesting state in the
/// context frame, so this alternative storage mode is not exposed as a Cargo
/// capability.
pub const portCRITICAL_NESTING_IN_TCB: BaseType_t = 0;

/// Using MPU wrappers (memory protection)
pub const portUSING_MPU_WRAPPERS: BaseType_t = 0;

// =============================================================================
// Type definitions that depend on config
// =============================================================================

/// Stack depth type (configSTACK_DEPTH_TYPE)
/// \[AMENDMENT\] In Rust, we use usize for stack depths
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
