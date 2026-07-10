/*
 * FreeRTOS Kernel <DEVELOPMENT BRANCH>
 * Copyright (C) 2021 Amazon.com, Inc. or its affiliates. All Rights Reserved.
 *
 * SPDX-License-Identifier: MIT
 *
 * [AMENDMENT] This module contains task management functionality, ported from tasks.c.
 * This is the core scheduler implementation.
 */

//! Task Management
//!
//! This module provides task creation, scheduling, and management.
//! Ported from tasks.c in the FreeRTOS kernel.
//!
//! ## Key Functions
//! - [`xTaskCreate`] / [`xTaskCreateStatic`] - Create a new task
//! - [`vTaskStartScheduler`] - Start the scheduler
//! - [`vTaskDelay`] - Delay the current task
//! - [`vTaskSuspend`] / [`vTaskResume`] - Suspend/resume tasks
//!
//! ## Scheduler Globals
//! - `pxCurrentTCB` - Pointer to the currently running task
//! - `pxReadyTasksLists` - Array of ready lists, one per priority
//! - `pxDelayedTaskList` - List of delayed tasks

#![allow(unused_variables)]
#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(dead_code)]

use core::ffi::c_void;
use core::ptr;

use crate::config::*;
use crate::kernel::list::*;
use crate::port::*;
use crate::types::*;

// =============================================================================
// Constants
// =============================================================================

/// Scheduler is suspended
pub const taskSCHEDULER_SUSPENDED: BaseType_t = 0;

/// Scheduler has not been started yet
pub const taskSCHEDULER_NOT_STARTED: BaseType_t = 1;

/// Scheduler is running
pub const taskSCHEDULER_RUNNING: BaseType_t = 2;

/// Task not waiting for notification
const taskNOT_WAITING_NOTIFICATION: u8 = 0;

/// Task waiting for notification
const taskWAITING_NOTIFICATION: u8 = 1;

/// Task received notification
const taskNOTIFICATION_RECEIVED: u8 = 2;

/// Stack fill byte for high water mark checking
const tskSTACK_FILL_BYTE: u8 = 0xa5;

/// Dynamically allocated stack and TCB
const tskDYNAMICALLY_ALLOCATED_STACK_AND_TCB: u8 = 0;

/// Statically allocated stack only
const tskSTATICALLY_ALLOCATED_STACK_ONLY: u8 = 1;

/// Statically allocated stack and TCB
const tskSTATICALLY_ALLOCATED_STACK_AND_TCB: u8 = 2;

/// Task state character - running
const tskRUNNING_CHAR: char = 'X';

/// Task state character - blocked
const tskBLOCKED_CHAR: char = 'B';

/// Task state character - ready
const tskREADY_CHAR: char = 'R';

/// Task state character - deleted
const tskDELETED_CHAR: char = 'D';

/// Task state character - suspended
const tskSUSPENDED_CHAR: char = 'S';

/// Task is not running on any core
const taskTASK_NOT_RUNNING: BaseType_t = -1;

/// Task is scheduled to yield
const taskTASK_SCHEDULED_TO_YIELD: BaseType_t = -2;

/// Bits per byte
const taskBITS_PER_BYTE: usize = 8;

/// Event list item value indicating value is in use (should not be updated)
#[cfg(not(feature = "tick-64bit"))]
const taskEVENT_LIST_ITEM_VALUE_IN_USE: TickType_t = 0x8000_0000;

#[cfg(feature = "tick-64bit")]
const taskEVENT_LIST_ITEM_VALUE_IN_USE: TickType_t = 0x8000_0000_0000_0000;

/// Idle task name
const configIDLE_TASK_NAME: &[u8] = b"IDLE\0";

/// Task attribute: is idle task
const taskATTRIBUTE_IS_IDLE: UBaseType_t = 1 << 0;

// =============================================================================
// TimeOut_t Structure
// =============================================================================

/// Used internally for timeout handling
#[repr(C)]
pub struct TimeOut_t {
    /// Overflow count at time of entry
    pub xOverflowCount: BaseType_t,
    /// Tick count when timeout started
    pub xTimeOnEntering: TickType_t,
}

impl TimeOut_t {
    /// Create a new uninitialized TimeOut_t
    pub const fn new() -> Self {
        TimeOut_t {
            xOverflowCount: 0,
            xTimeOnEntering: 0,
        }
    }
}

impl Default for TimeOut_t {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// Task State Enum
// =============================================================================

/// Task states returned by eTaskGetState
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum eTaskState {
    /// Task is running (executing on CPU)
    eRunning = 0,
    /// Task is ready to run
    eReady = 1,
    /// Task is blocked waiting for something
    eBlocked = 2,
    /// Task is suspended
    eSuspended = 3,
    /// Task has been deleted but not yet freed
    eDeleted = 4,
    /// Invalid state
    eInvalid = 5,
}

// =============================================================================
// Task Control Block (TCB)
// =============================================================================

/// Task Control Block - one per task
///
/// \[AMENDMENT\] This structure must have pxTopOfStack as the first member
/// for compatibility with assembly context switch code.
#[repr(C)]
pub struct tskTaskControlBlock {
    /// Points to the location of the last item placed on the tasks stack.
    /// THIS MUST BE THE FIRST MEMBER OF THE TCB STRUCT.
    pub pxTopOfStack: *mut StackType_t,

    /// The list that the state list item of a task is reference from
    /// denotes the state of that task (Ready, Blocked, Suspended).
    pub xStateListItem: ListItem_t,

    /// Used to reference a task from an event list.
    pub xEventListItem: ListItem_t,

    /// The priority of the task. 0 is the lowest priority.
    pub uxPriority: UBaseType_t,

    /// Points to the start of the stack.
    pub pxStack: *mut StackType_t,

    /// Number of `StackType_t` entries in the stack allocation.
    ///
    /// \[AMENDMENT\] The C kernel can scan until it reaches context bytes that
    /// replaced the fill pattern. Rust must not form an out-of-bounds read if
    /// a test or port leaves the whole stack untouched, so the allocation
    /// extent is retained explicitly.
    pub uxStackDepth: configSTACK_DEPTH_TYPE,

    /// Descriptive name given to the task when created. Facilitates debugging only.
    pub pcTaskName: [u8; configMAX_TASK_NAME_LEN],

    // --- Optional fields based on config ---
    /// Points to the highest valid address for the stack (stack grows down).
    /// Only present if portSTACK_GROWTH > 0 or configRECORD_STACK_HIGH_ADDRESS == 1.
    #[cfg(any(
        not(feature = "arch-32bit"),  // portSTACK_GROWTH > 0 check would go here
        feature = "record-stack-high-address"
    ))]
    pub pxEndOfStack: *mut StackType_t,

    /// Stores a number that increments each time a TCB is created.
    /// For debuggers to determine when a task has been deleted and recreated.
    #[cfg(feature = "trace-facility")]
    pub uxTCBNumber: UBaseType_t,

    /// Stores a number specifically for use by third party trace code.
    #[cfg(feature = "trace-facility")]
    pub uxTaskNumber: UBaseType_t,

    /// The priority last assigned to the task - used by priority inheritance.
    #[cfg(feature = "use-mutexes")]
    pub uxBasePriority: UBaseType_t,

    /// Number of mutexes held by this task.
    #[cfg(feature = "use-mutexes")]
    pub uxMutexesHeld: UBaseType_t,

    /// Application-defined task tag.
    #[cfg(feature = "application-task-tag")]
    pub pxTaskTag: Option<extern "C" fn(*mut c_void) -> BaseType_t>,

    /// Notification values array.
    pub ulNotifiedValue: [u32; configTASK_NOTIFICATION_ARRAY_ENTRIES],

    /// Notification states array.
    pub ucNotifyState: [u8; configTASK_NOTIFICATION_ARRAY_ENTRIES],

    /// Set to pdTRUE if the task is statically allocated.
    pub ucStaticallyAllocated: u8,

    /// Used to detect if a delay was aborted.
    #[cfg(feature = "abort-delay")]
    pub ucDelayAborted: u8,

    /// Thread local storage pointers array.
    #[cfg(feature = "thread-local-storage")]
    pub pvThreadLocalStoragePointers: [*mut c_void; configNUM_THREAD_LOCAL_STORAGE_POINTERS],

    /// Run-time counter for this task.
    /// Stores the total time this task has been running.
    #[cfg(feature = "generate-run-time-stats")]
    pub ulRunTimeCounter: configRUN_TIME_COUNTER_TYPE,
}

/// Type alias for TCB pointer
pub type TCB_t = tskTaskControlBlock;

impl tskTaskControlBlock {
    /// Create a new zeroed TCB
    ///
    /// \[AMENDMENT\] In C this would be memset to 0. We initialize all fields explicitly.
    pub const fn new() -> Self {
        tskTaskControlBlock {
            pxTopOfStack: ptr::null_mut(),
            xStateListItem: ListItem_t::new(),
            xEventListItem: ListItem_t::new(),
            uxPriority: 0,
            pxStack: ptr::null_mut(),
            uxStackDepth: 0,
            pcTaskName: [0u8; configMAX_TASK_NAME_LEN],

            #[cfg(any(not(feature = "arch-32bit"), feature = "record-stack-high-address"))]
            pxEndOfStack: ptr::null_mut(),

            #[cfg(feature = "trace-facility")]
            uxTCBNumber: 0,

            #[cfg(feature = "trace-facility")]
            uxTaskNumber: 0,

            #[cfg(feature = "use-mutexes")]
            uxBasePriority: 0,

            #[cfg(feature = "use-mutexes")]
            uxMutexesHeld: 0,

            #[cfg(feature = "application-task-tag")]
            pxTaskTag: None,

            ulNotifiedValue: [0u32; configTASK_NOTIFICATION_ARRAY_ENTRIES],
            ucNotifyState: [0u8; configTASK_NOTIFICATION_ARRAY_ENTRIES],

            ucStaticallyAllocated: 0,

            #[cfg(feature = "abort-delay")]
            ucDelayAborted: 0,

            #[cfg(feature = "thread-local-storage")]
            pvThreadLocalStoragePointers: [ptr::null_mut();
                configNUM_THREAD_LOCAL_STORAGE_POINTERS],

            #[cfg(feature = "generate-run-time-stats")]
            ulRunTimeCounter: 0,
        }
    }
}

// =============================================================================
// Static Task Structure (for xTaskCreateStatic)
// =============================================================================

/// Static task buffer for xTaskCreateStatic
///
/// \[AMENDMENT\] Users provide this buffer for static task allocation.
#[repr(transparent)]
pub struct StaticTask_t {
    /// Reserved storage for a TCB.
    ///
    /// \[AMENDMENT\] `MaybeUninit<TCB_t>` gives this public opaque buffer the
    /// exact size and alignment of the internal TCB. A byte array has alignment
    /// one and cannot soundly be cast to `TCB_t` in Rust.
    _storage: core::mem::MaybeUninit<TCB_t>,
}

impl StaticTask_t {
    pub const fn new() -> Self {
        StaticTask_t {
            _storage: core::mem::MaybeUninit::uninit(),
        }
    }
}

// =============================================================================
// Scheduler Globals
// =============================================================================

/// The currently running task's TCB pointer.
/// In single-core, this is a single pointer.
/// In multi-core (SMP), this would be an array of pointers.
///
/// \[AMENDMENT\] Exported with `#[no_mangle]` for assembly access in port layer.
#[no_mangle]
pub static mut pxCurrentTCB: *mut TCB_t = ptr::null_mut();

/// Prioritised ready tasks. Each priority has its own list.
/// Index 0 = lowest priority, index configMAX_PRIORITIES-1 = highest.
static mut pxReadyTasksLists: [List_t; configMAX_PRIORITIES as usize] =
    [const { List_t::new() }; configMAX_PRIORITIES as usize];

/// Delayed tasks list 1.
static mut xDelayedTaskList1: List_t = List_t::new();

/// Delayed tasks list 2 (for tick count overflow).
static mut xDelayedTaskList2: List_t = List_t::new();

/// Points to the delayed task list currently being used.
static mut pxDelayedTaskList: *mut List_t = ptr::null_mut();

/// Points to the delayed task list for overflowed tick count.
static mut pxOverflowDelayedTaskList: *mut List_t = ptr::null_mut();

/// Tasks readied while scheduler was suspended. Moved to ready list when resumed.
static mut xPendingReadyList: List_t = List_t::new();

/// Tasks that have been deleted but not yet freed.
static mut xTasksWaitingTermination: List_t = List_t::new();

/// Number of deleted tasks waiting for cleanup.
static mut uxDeletedTasksWaitingCleanUp: UBaseType_t = 0;

/// Suspended tasks list.
static mut xSuspendedTaskList: List_t = List_t::new();

/// Current number of tasks in the system.
static mut uxCurrentNumberOfTasks: UBaseType_t = 0;

/// Current tick count.
static mut xTickCount: TickType_t = configINITIAL_TICK_COUNT;

/// Highest priority with ready tasks.
static mut uxTopReadyPriority: UBaseType_t = tskIDLE_PRIORITY;

/// Flag indicating if scheduler is running.
static mut xSchedulerRunning: BaseType_t = pdFALSE;

/// Ticks that occurred while scheduler was suspended.
static mut xPendedTicks: TickType_t = 0;

/// Yield pending flag (single core).
static mut xYieldPendings: [BaseType_t; configNUMBER_OF_CORES as usize] =
    [pdFALSE; configNUMBER_OF_CORES as usize];

/// Number of tick count overflows.
static mut xNumOfOverflows: BaseType_t = 0;

/// Task number counter for unique task identification.
static mut uxTaskNumber: UBaseType_t = 0;

/// Time at which the next blocked task will unblock.
static mut xNextTaskUnblockTime: TickType_t = 0;

/// Handle to the idle task(s).
static mut xIdleTaskHandles: [TaskHandle_t; configNUMBER_OF_CORES as usize] =
    [ptr::null_mut(); configNUMBER_OF_CORES as usize];

/// Optional application-provided storage for the idle task.
static mut pxIdleTaskTCBBuffer: *mut StaticTask_t = ptr::null_mut();
static mut pxIdleTaskStackBuffer: *mut StackType_t = ptr::null_mut();
static mut uxIdleTaskStackSize: configSTACK_DEPTH_TYPE = 0;

/// Runtime boundary for FreeRTOS's required stack-overflow application hook.
#[cfg(feature = "stack-overflow-check")]
pub type StackOverflowHook_t = extern "C" fn(TaskHandle_t, *const u8);

#[cfg(feature = "stack-overflow-check")]
static mut pxApplicationStackOverflowHook: Option<StackOverflowHook_t> = None;

/// Kernel-provided fallback storage for allocator-free configurations.
#[cfg(not(any(feature = "alloc", feature = "heap-4", feature = "heap-5")))]
static mut xStaticIdleTaskTCB: StaticTask_t = StaticTask_t::new();
#[cfg(not(any(feature = "alloc", feature = "heap-4", feature = "heap-5")))]
static mut uxStaticIdleTaskStack: [StackType_t; configMINIMAL_STACK_SIZE] =
    [0; configMINIMAL_STACK_SIZE];

/// Scheduler suspension count (>0 means suspended).
static mut uxSchedulerSuspended: UBaseType_t = 0;

/// For OpenOCD debugging support.
#[no_mangle]
static uxTopUsedPriority: UBaseType_t = configMAX_PRIORITIES - 1;

// =============================================================================
// Run-time Stats Tracking Variables
// =============================================================================

/// The time at which the current task was switched in.
/// Used to calculate how long the task has been running.
#[cfg(feature = "generate-run-time-stats")]
static mut ulTaskSwitchedInTime: configRUN_TIME_COUNTER_TYPE = 0;

/// Raw counter value sampled when the scheduler started.
#[cfg(feature = "generate-run-time-stats")]
static mut ulRunTimeStart: configRUN_TIME_COUNTER_TYPE = 0;

#[cfg(feature = "generate-run-time-stats")]
#[inline(always)]
const fn prvRunTimeDelta(
    ulNow: configRUN_TIME_COUNTER_TYPE,
    ulThen: configRUN_TIME_COUNTER_TYPE,
) -> configRUN_TIME_COUNTER_TYPE {
    ulNow.wrapping_sub(ulThen)
}

#[cfg(feature = "generate-run-time-stats")]
#[inline(always)]
unsafe fn prvGetTotalRunTime() -> configRUN_TIME_COUNTER_TYPE {
    crate::port::portGET_RUN_TIME_COUNTER_VALUE().wrapping_sub(ulRunTimeStart)
}

// =============================================================================
// Task Macros as Functions
// =============================================================================

/// Record that a task with the given priority is ready.
/// Updates uxTopReadyPriority if necessary.
#[inline(always)]
unsafe fn taskRECORD_READY_PRIORITY(uxPriority: UBaseType_t) {
    if uxPriority > uxTopReadyPriority {
        uxTopReadyPriority = uxPriority;
    }
}

/// Select the highest priority task to run.
/// Sets pxCurrentTCB to the selected task.
#[inline(always)]
unsafe fn taskSELECT_HIGHEST_PRIORITY_TASK() {
    let mut uxTopPriority: UBaseType_t = uxTopReadyPriority;

    // Find the highest priority queue that contains ready tasks.
    while listLIST_IS_EMPTY(ptr::addr_of!(pxReadyTasksLists[uxTopPriority as usize])) != pdFALSE {
        configASSERT(uxTopPriority > 0);
        uxTopPriority -= 1;
    }

    // Get the next task from that priority's list (round-robin within priority).
    pxCurrentTCB =
        listGET_OWNER_OF_NEXT_ENTRY(ptr::addr_of_mut!(pxReadyTasksLists[uxTopPriority as usize]))
            as *mut TCB_t;
    uxTopReadyPriority = uxTopPriority;
}

/// Reset ready priority (for port-optimised selection).
/// No-op when not using port-optimised selection.
#[inline(always)]
unsafe fn taskRESET_READY_PRIORITY(_uxPriority: UBaseType_t) {
    // No-op when configUSE_PORT_OPTIMISED_TASK_SELECTION == 0
}

/// Switch the delayed task lists (on tick count overflow).
#[inline(always)]
unsafe fn taskSWITCH_DELAYED_LISTS() {
    // The delayed tasks list should be empty when the lists are switched.
    configASSERT(listLIST_IS_EMPTY(pxDelayedTaskList) != pdFALSE);

    let pxTemp = pxDelayedTaskList;
    pxDelayedTaskList = pxOverflowDelayedTaskList;
    pxOverflowDelayedTaskList = pxTemp;
    xNumOfOverflows += 1;
    prvResetNextTaskUnblockTime();
}

/// Add a task to the appropriate ready list.
#[inline(always)]
unsafe fn prvAddTaskToReadyList(pxTCB: *mut TCB_t) {
    crate::trace::traceMOVED_TASK_TO_READY_STATE(pxTCB as *mut c_void);
    taskRECORD_READY_PRIORITY((*pxTCB).uxPriority);
    vListInsertEnd(
        ptr::addr_of_mut!(pxReadyTasksLists[(*pxTCB).uxPriority as usize]),
        ptr::addr_of_mut!((*pxTCB).xStateListItem),
    );
    crate::trace::tracePOST_MOVED_TASK_TO_READY_STATE(pxTCB as *mut c_void);
}

/// Get TCB from handle, or current TCB if handle is null.
#[inline(always)]
unsafe fn prvGetTCBFromHandle(pxHandle: TaskHandle_t) -> *mut TCB_t {
    if pxHandle.is_null() {
        pxCurrentTCB
    } else {
        pxHandle as *mut TCB_t
    }
}

/// Read an ISR-visible task notification value with the volatile semantics of
/// the upstream TCB fields.
#[inline(always)]
unsafe fn prvReadNotifiedValue(pxTCB: *const TCB_t, uxIndex: usize) -> u32 {
    let pxValues = core::ptr::addr_of!((*pxTCB).ulNotifiedValue).cast::<u32>();
    core::ptr::read_volatile(pxValues.add(uxIndex))
}

/// Write an ISR-visible task notification value with upstream volatile semantics.
#[inline(always)]
unsafe fn prvWriteNotifiedValue(pxTCB: *mut TCB_t, uxIndex: usize, ulValue: u32) {
    let pxValues = core::ptr::addr_of_mut!((*pxTCB).ulNotifiedValue).cast::<u32>();
    core::ptr::write_volatile(pxValues.add(uxIndex), ulValue);
}

/// Read an ISR-visible task notification state with upstream volatile semantics.
#[inline(always)]
unsafe fn prvReadNotifyState(pxTCB: *const TCB_t, uxIndex: usize) -> u8 {
    let pxStates = core::ptr::addr_of!((*pxTCB).ucNotifyState).cast::<u8>();
    core::ptr::read_volatile(pxStates.add(uxIndex))
}

/// Write an ISR-visible task notification state with upstream volatile semantics.
#[inline(always)]
unsafe fn prvWriteNotifyState(pxTCB: *mut TCB_t, uxIndex: usize, ucState: u8) {
    let pxStates = core::ptr::addr_of_mut!((*pxTCB).ucNotifyState).cast::<u8>();
    core::ptr::write_volatile(pxStates.add(uxIndex), ucState);
}

/// Yield within API if using preemption.
///
/// # Safety
///
/// Call only from a live task after scheduler startup, at an upstream yield
/// point where the current kernel/list mutation is complete.
#[inline(always)]
pub unsafe fn portYIELD_WITHIN_API() {
    if configUSE_PREEMPTION != 0 {
        portYIELD();
    }
}

/// Yield if the unblocked task has higher priority.
#[inline(always)]
unsafe fn taskYIELD_ANY_CORE_IF_USING_PREEMPTION(pxTCB: *mut TCB_t) {
    if configUSE_PREEMPTION != 0 {
        if !pxCurrentTCB.is_null() && (*pxCurrentTCB).uxPriority < (*pxTCB).uxPriority {
            portYIELD_WITHIN_API();
        }
    }
}

// =============================================================================
// Private Functions
// =============================================================================

/// Initialize all the task lists.
unsafe fn prvInitialiseTaskLists() {
    // Initialize the ready lists (one per priority level).
    for uxPriority in 0..configMAX_PRIORITIES as usize {
        vListInitialise(ptr::addr_of_mut!(pxReadyTasksLists[uxPriority]));
    }

    // Initialize the delayed task lists.
    vListInitialise(ptr::addr_of_mut!(xDelayedTaskList1));
    vListInitialise(ptr::addr_of_mut!(xDelayedTaskList2));

    // Initialize pending ready list.
    vListInitialise(ptr::addr_of_mut!(xPendingReadyList));

    // Initialize terminated tasks list.
    vListInitialise(ptr::addr_of_mut!(xTasksWaitingTermination));

    // Initialize suspended tasks list.
    vListInitialise(ptr::addr_of_mut!(xSuspendedTaskList));

    // Set delayed list pointers.
    /* [AMENDMENT] The lists are self-referential after initialisation. Keep
     * their retained addresses in raw-pointer form so no later exclusive Rust
     * reference invalidates the intrusive links. */
    pxDelayedTaskList = ptr::addr_of_mut!(xDelayedTaskList1);
    pxOverflowDelayedTaskList = ptr::addr_of_mut!(xDelayedTaskList2);
}

/// Reset xNextTaskUnblockTime to the wake time of the next blocked task.
unsafe fn prvResetNextTaskUnblockTime() {
    if listLIST_IS_EMPTY(pxDelayedTaskList) != pdFALSE {
        // No tasks waiting, set to max.
        xNextTaskUnblockTime = portMAX_DELAY;
    } else {
        // Get the wake time of the first task in the delayed list.
        let pxTCB = listGET_OWNER_OF_HEAD_ENTRY(pxDelayedTaskList) as *mut TCB_t;
        xNextTaskUnblockTime = listGET_LIST_ITEM_VALUE(ptr::addr_of!((*pxTCB).xStateListItem));
    }
}

/// Add a new task to the ready list after creation.
unsafe fn prvAddNewTaskToReadyList(pxNewTCB: *mut TCB_t) {
    // Ensure interrupts don't access the task lists while they are being updated.
    taskENTER_CRITICAL();
    {
        uxCurrentNumberOfTasks += 1;

        if pxCurrentTCB.is_null() {
            // This is the first task to be created.
            pxCurrentTCB = pxNewTCB;

            if uxCurrentNumberOfTasks == 1 {
                // This is the first task, initialize the lists.
                prvInitialiseTaskLists();
            }
        } else {
            // If scheduler not running, make this the current task if it has
            // higher priority than the current task.
            if xSchedulerRunning == pdFALSE {
                if (*pxCurrentTCB).uxPriority <= (*pxNewTCB).uxPriority {
                    pxCurrentTCB = pxNewTCB;
                }
            }
        }

        uxTaskNumber = uxTaskNumber.wrapping_add(1);

        #[cfg(feature = "trace-facility")]
        {
            (*pxNewTCB).uxTCBNumber = uxTaskNumber;
        }

        crate::trace::traceTASK_CREATE(pxNewTCB as *mut c_void);

        prvAddTaskToReadyList(pxNewTCB);
    }
    taskEXIT_CRITICAL();

    if xSchedulerRunning != pdFALSE {
        // If the new task has higher priority than the current task, yield.
        if (*pxCurrentTCB).uxPriority < (*pxNewTCB).uxPriority {
            portYIELD_WITHIN_API();
        }
    }
}

/// Initialize a new task's TCB and stack.
unsafe fn prvInitialiseNewTask(
    pxTaskCode: TaskFunction_t,
    pcName: *const u8,
    uxStackDepth: configSTACK_DEPTH_TYPE,
    pvParameters: *mut c_void,
    uxPriority: UBaseType_t,
    pxCreatedTask: *mut TaskHandle_t,
    pxNewTCB: *mut TCB_t,
    pxTopOfStack: *mut StackType_t,
) {
    // Copy the task name.
    if !pcName.is_null() {
        let mut i = 0usize;
        while i < configMAX_TASK_NAME_LEN - 1 {
            let c = *pcName.add(i);
            (*pxNewTCB).pcTaskName[i] = c;
            if c == 0 {
                break;
            }
            i += 1;
        }
        (*pxNewTCB).pcTaskName[configMAX_TASK_NAME_LEN - 1] = 0;
    } else {
        (*pxNewTCB).pcTaskName[0] = 0;
    }

    // Ensure priority is within bounds.
    configASSERT(uxPriority < configMAX_PRIORITIES);
    let mut uxPriority = uxPriority;
    if uxPriority >= configMAX_PRIORITIES {
        uxPriority = configMAX_PRIORITIES - 1;
    }
    (*pxNewTCB).uxPriority = uxPriority;

    #[cfg(feature = "use-mutexes")]
    {
        (*pxNewTCB).uxBasePriority = uxPriority;
        (*pxNewTCB).uxMutexesHeld = 0;
    }

    // Initialize the list items.
    vListInitialiseItem(ptr::addr_of_mut!((*pxNewTCB).xStateListItem));
    vListInitialiseItem(ptr::addr_of_mut!((*pxNewTCB).xEventListItem));

    // Set the owner of the list items to this TCB.
    listSET_LIST_ITEM_OWNER(
        ptr::addr_of_mut!((*pxNewTCB).xStateListItem),
        pxNewTCB as *mut c_void,
    );

    // Event list items are stored in reverse priority order.
    listSET_LIST_ITEM_VALUE(
        ptr::addr_of_mut!((*pxNewTCB).xEventListItem),
        (configMAX_PRIORITIES - uxPriority) as TickType_t,
    );
    listSET_LIST_ITEM_OWNER(
        ptr::addr_of_mut!((*pxNewTCB).xEventListItem),
        pxNewTCB as *mut c_void,
    );

    // Initialize notifications.
    for i in 0..configTASK_NOTIFICATION_ARRAY_ENTRIES {
        prvWriteNotifiedValue(pxNewTCB, i, 0);
        prvWriteNotifyState(pxNewTCB, i, taskNOT_WAITING_NOTIFICATION);
    }

    #[cfg(feature = "application-task-tag")]
    {
        (*pxNewTCB).pxTaskTag = None;
    }

    #[cfg(feature = "abort-delay")]
    {
        (*pxNewTCB).ucDelayAborted = pdFALSE as u8;
    }

    (*pxNewTCB).uxStackDepth = uxStackDepth;

    // Initialize the stack.
    // pxPortInitialiseStack is provided by the port layer.
    /* SAFETY: prvInitialiseNewTask's contract supplies a unique stack of
     * uxStackDepth words. pxTopOfStack lies within that allocation, which the
     * TCB retains for the complete task lifetime; the task parameter contract
     * is inherited from xTaskCreate/xTaskCreateStatic. */
    let pxInitialisedTopOfStack: *mut StackType_t;
    #[cfg(any(
        feature = "port-cortex-m0",
        feature = "port-cortex-m3",
        feature = "port-cortex-m4f",
        feature = "port-cortex-m7"
    ))]
    {
        pxInitialisedTopOfStack =
            unsafe { pxPortInitialiseStack(pxTopOfStack, pxTaskCode, pvParameters) };
    }
    #[cfg(not(any(
        feature = "port-cortex-m0",
        feature = "port-cortex-m3",
        feature = "port-cortex-m4f",
        feature = "port-cortex-m7"
    )))]
    {
        pxInitialisedTopOfStack = pxPortInitialiseStack(pxTopOfStack, pxTaskCode, pvParameters);
    }
    (*pxNewTCB).pxTopOfStack = pxInitialisedTopOfStack;

    let uxStackBytes = uxStackDepth * core::mem::size_of::<StackType_t>();
    if portSTACK_GROWTH < 0 {
        configASSERT((pxInitialisedTopOfStack as usize) <= (pxTopOfStack as usize));
        configASSERT(
            (pxTopOfStack as usize).wrapping_sub(pxInitialisedTopOfStack as usize) < uxStackBytes,
        );
    } else {
        configASSERT((pxInitialisedTopOfStack as usize) >= (pxTopOfStack as usize));
        configASSERT(
            (pxInitialisedTopOfStack as usize).wrapping_sub(pxTopOfStack as usize) < uxStackBytes,
        );
    }

    // Return the handle if requested.
    if !pxCreatedTask.is_null() {
        *pxCreatedTask = pxNewTCB as TaskHandle_t;
    }
}

/// Add the current task to the delayed list.
unsafe fn prvAddCurrentTaskToDelayedList(
    xTicksToWait: TickType_t,
    xCanBlockIndefinitely: BaseType_t,
) {
    let xConstTickCount = xTickCount;

    #[cfg(feature = "abort-delay")]
    {
        /* About to enter a delayed list, so ensure the flag is reset. A later
         * pdTRUE value then unambiguously means this particular block was
         * aborted. */
        (*pxCurrentTCB).ucDelayAborted = pdFALSE as u8;
    }

    // Remove the task from the ready list before adding to delayed list.
    let _ux = uxListRemove(ptr::addr_of_mut!((*pxCurrentTCB).xStateListItem));

    #[cfg(feature = "task-suspend")]
    if xTicksToWait == portMAX_DELAY && xCanBlockIndefinitely != pdFALSE {
        // Add to the suspended list instead (wait forever).
        vListInsertEnd(
            ptr::addr_of_mut!(xSuspendedTaskList),
            ptr::addr_of_mut!((*pxCurrentTCB).xStateListItem),
        );
        return;
    }

    {
        // Calculate the time at which the task should wake.
        let xTimeToWake = xConstTickCount.wrapping_add(xTicksToWait);

        // Set the wake time as the list item value.
        listSET_LIST_ITEM_VALUE(
            ptr::addr_of_mut!((*pxCurrentTCB).xStateListItem),
            xTimeToWake,
        );

        if xTimeToWake < xConstTickCount {
            // Wake time has overflowed, add to overflow list.
            vListInsert(
                pxOverflowDelayedTaskList,
                ptr::addr_of_mut!((*pxCurrentTCB).xStateListItem),
            );
        } else {
            // Add to the delayed list.
            vListInsert(
                pxDelayedTaskList,
                ptr::addr_of_mut!((*pxCurrentTCB).xStateListItem),
            );

            // Update xNextTaskUnblockTime if this task wakes earliest.
            if xTimeToWake < xNextTaskUnblockTime {
                xNextTaskUnblockTime = xTimeToWake;
            }
        }
    }
}

/// Check for tasks waiting termination and delete their TCBs.
unsafe fn prvCheckTasksWaitingTermination() {
    #[cfg(feature = "task-delete")]
    {
        while uxDeletedTasksWaitingCleanUp > 0 {
            let pxTCB: *mut TCB_t;

            taskENTER_CRITICAL();
            {
                // Get the TCB of the deleted task.
                pxTCB = listGET_OWNER_OF_HEAD_ENTRY(ptr::addr_of!(xTasksWaitingTermination))
                    as *mut TCB_t;
                let _ux = uxListRemove(ptr::addr_of_mut!((*pxTCB).xStateListItem));

                uxCurrentNumberOfTasks -= 1;
                uxDeletedTasksWaitingCleanUp -= 1;
            }
            taskEXIT_CRITICAL();

            // Free the TCB (and stack if dynamically allocated).
            prvDeleteTCB(pxTCB);
        }
    }
}

/// Delete a TCB and its associated memory.
#[cfg(feature = "task-delete")]
unsafe fn prvDeleteTCB(pxTCB: *mut TCB_t) {
    // Free stack if dynamically allocated.
    if (*pxTCB).ucStaticallyAllocated == tskDYNAMICALLY_ALLOCATED_STACK_AND_TCB {
        crate::memory::vPortFree((*pxTCB).pxStack as *mut c_void);
        crate::memory::vPortFree(pxTCB as *mut c_void);
    } else if (*pxTCB).ucStaticallyAllocated == tskSTATICALLY_ALLOCATED_STACK_ONLY {
        crate::memory::vPortFree(pxTCB as *mut c_void);
    }
    // If fully statically allocated, nothing to free.
}

/// Install the application hook required by stack-overflow checking.
///
/// Call this before [`vTaskStartScheduler`] (or after the scheduler has been
/// stopped). The callback runs during a context switch with the port's
/// scheduler interrupt mask in force and must not block or attempt to resume
/// the damaged task.
///
/// # Returns
///
/// `pdPASS` when the callback was installed, or `pdFAIL` while the scheduler
/// is running.
#[cfg(feature = "stack-overflow-check")]
pub fn vTaskSetStackOverflowHook(pxHook: Option<StackOverflowHook_t>) -> BaseType_t {
    unsafe {
        taskENTER_CRITICAL();
        let xReturn = if xSchedulerRunning == pdFALSE {
            pxApplicationStackOverflowHook = pxHook;
            pdPASS
        } else {
            pdFAIL
        };
        taskEXIT_CRITICAL();
        xReturn
    }
}

#[cfg(feature = "stack-overflow-check")]
#[inline(always)]
unsafe fn prvCallApplicationStackOverflowHook(pxTCB: *mut TCB_t) {
    let pxHook = pxApplicationStackOverflowHook;
    if let Some(pxHook) = pxHook {
        pxHook(pxTCB.cast(), (*pxTCB).pcTaskName.as_ptr());
    } else {
        /* Enabling overflow detection without installing the required C hook
         * would be a link error upstream. Preserve the fail-stop behavior in
         * Rust instead of silently continuing with a damaged stack. */
        configASSERT(false);
    }
}

// =============================================================================
// Idle Task
// =============================================================================

/// The idle task function.
///
/// This is automatically created when the scheduler starts.
extern "C" fn prvIdleTask(_pvParameters: *mut c_void) {
    // The idle task runs at the lowest priority level.
    loop {
        // Check for tasks that have been deleted.
        unsafe {
            prvCheckTasksWaitingTermination();
        }

        // Yield early when another idle-priority task is ready to run.
        #[cfg(feature = "idle-yield")]
        unsafe {
            if listCURRENT_LIST_LENGTH(ptr::addr_of!(pxReadyTasksLists[0]))
                > configNUMBER_OF_CORES as UBaseType_t
            {
                portYIELD_WITHIN_API();
            }
        }

        // Idle hook (if enabled).
        if configUSE_IDLE_HOOK != 0 {
            // No idle-hook Cargo capability is currently advertised.
        }

        // Tickless idle (if enabled).
        // This uses the feature flag rather than the config constant for
        // conditional compilation.
        #[cfg(feature = "tickless-idle")]
        {
            let xExpectedIdleTime = prvGetExpectedIdleTime();

            // Only attempt to enter a low-power state if the expected idle
            // time is above the minimum threshold.
            if xExpectedIdleTime >= configEXPECTED_IDLE_TIME_BEFORE_SLEEP {
                // Safety: this is the live idle task and the matching resume
                // occurs before the loop can block, yield, or iterate.
                unsafe { vTaskSuspendAll() };
                {
                    // Now the scheduler is suspended, the expected idle time
                    // can be sampled again, and this time its value can be used.
                    unsafe {
                        configASSERT(xNextTaskUnblockTime >= xTickCount);
                    }
                    let xExpectedIdleTime = prvGetExpectedIdleTime();

                    if xExpectedIdleTime >= configEXPECTED_IDLE_TIME_BEFORE_SLEEP {
                        crate::trace::traceLOW_POWER_IDLE_BEGIN();
                        crate::port::vPortSuppressTicksAndSleep(xExpectedIdleTime);
                        crate::trace::traceLOW_POWER_IDLE_END();
                    }
                }
                // Safety: paired with the suspension immediately above in the
                // same live idle-task execution context.
                unsafe { xTaskResumeAll() };
            }
        }
    }
}

/// Create the idle task(s).
unsafe fn prvCreateIdleTasks() -> BaseType_t {
    // Create the idle task.
    let xReturn: BaseType_t;

    if !pxIdleTaskTCBBuffer.is_null() && !pxIdleTaskStackBuffer.is_null() && uxIdleTaskStackSize > 0
    {
        xIdleTaskHandles[0] = xTaskCreateStatic(
            prvIdleTask,
            configIDLE_TASK_NAME.as_ptr(),
            uxIdleTaskStackSize,
            ptr::null_mut(),
            tskIDLE_PRIORITY,
            pxIdleTaskStackBuffer,
            pxIdleTaskTCBBuffer,
        );
        return if xIdleTaskHandles[0].is_null() {
            pdFAIL
        } else {
            pdPASS
        };
    }

    #[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
    {
        xReturn = xTaskCreate(
            prvIdleTask,
            configIDLE_TASK_NAME.as_ptr(),
            configMINIMAL_STACK_SIZE,
            ptr::null_mut(),
            tskIDLE_PRIORITY,
            &mut xIdleTaskHandles[0],
        );
    }

    #[cfg(not(any(feature = "alloc", feature = "heap-4", feature = "heap-5")))]
    {
        xIdleTaskHandles[0] = xTaskCreateStatic(
            prvIdleTask,
            configIDLE_TASK_NAME.as_ptr(),
            configMINIMAL_STACK_SIZE,
            ptr::null_mut(),
            tskIDLE_PRIORITY,
            uxStaticIdleTaskStack.as_mut_ptr(),
            &mut xStaticIdleTaskTCB,
        );
        xReturn = if xIdleTaskHandles[0].is_null() {
            pdFAIL
        } else {
            pdPASS
        };
    }

    xReturn
}

/// Register application-provided storage for the statically allocated idle
/// task. This must be called before the scheduler starts.
///
/// # Safety
///
/// The TCB and stack must be uniquely owned and remain valid for the complete
/// scheduler lifetime. The stack must contain at least `uxStackSize` words.
pub unsafe fn vTaskSetIdleTaskMemory(
    pxTaskTCBBuffer: *mut StaticTask_t,
    pxTaskStackBuffer: *mut StackType_t,
    uxStackSize: configSTACK_DEPTH_TYPE,
) -> BaseType_t {
    if pxTaskTCBBuffer.is_null() || pxTaskStackBuffer.is_null() || uxStackSize == 0 {
        return pdFAIL;
    }

    taskENTER_CRITICAL();
    let xReturn = if xSchedulerRunning == pdFALSE && xIdleTaskHandles[0].is_null() {
        pxIdleTaskTCBBuffer = pxTaskTCBBuffer;
        pxIdleTaskStackBuffer = pxTaskStackBuffer;
        uxIdleTaskStackSize = uxStackSize;
        pdPASS
    } else {
        pdFAIL
    };
    taskEXIT_CRITICAL();
    xReturn
}

// =============================================================================
// Public API - Task Creation
// =============================================================================

/// Create a new task with dynamically allocated stack and TCB.
///
/// # Safety
///
/// This function allocates memory and modifies global scheduler state.
///
/// # Arguments
///
/// * `pxTaskCode` - Pointer to the task function
/// * `pcName` - Name of the task (null-terminated string)
/// * `uxStackDepth` - Stack size in words (not bytes)
/// * `pvParameters` - Parameter to pass to the task function
/// * `uxPriority` - Task priority (0 = lowest)
/// * `pxCreatedTask` - Optional pointer to receive the task handle
///
/// # Returns
///
/// pdPASS if successful, errCOULD_NOT_ALLOCATE_REQUIRED_MEMORY if allocation failed.
#[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
pub unsafe fn xTaskCreate(
    pxTaskCode: TaskFunction_t,
    pcName: *const u8,
    uxStackDepth: configSTACK_DEPTH_TYPE,
    pvParameters: *mut c_void,
    uxPriority: UBaseType_t,
    pxCreatedTask: *mut TaskHandle_t,
) -> BaseType_t {
    if uxStackDepth == 0 {
        return errCOULD_NOT_ALLOCATE_REQUIRED_MEMORY;
    }

    let stack_size = match uxStackDepth.checked_mul(core::mem::size_of::<StackType_t>()) {
        Some(size) => size,
        None => return errCOULD_NOT_ALLOCATE_REQUIRED_MEMORY,
    };

    /* Match the upstream allocation order: a downward-growing stack is
     * allocated before its TCB so it cannot grow into that TCB. */
    let (pxNewTCB, pxStack) = if portSTACK_GROWTH > 0 {
        let pxNewTCB = crate::memory::pvPortMalloc(core::mem::size_of::<TCB_t>()) as *mut TCB_t;
        if pxNewTCB.is_null() {
            return errCOULD_NOT_ALLOCATE_REQUIRED_MEMORY;
        }

        let pxStack = crate::memory::pvPortMalloc(stack_size) as *mut StackType_t;
        if pxStack.is_null() {
            crate::memory::vPortFree(pxNewTCB.cast());
            return errCOULD_NOT_ALLOCATE_REQUIRED_MEMORY;
        }

        (pxNewTCB, pxStack)
    } else {
        let pxStack = crate::memory::pvPortMalloc(stack_size) as *mut StackType_t;
        if pxStack.is_null() {
            return errCOULD_NOT_ALLOCATE_REQUIRED_MEMORY;
        }

        let pxNewTCB = crate::memory::pvPortMalloc(core::mem::size_of::<TCB_t>()) as *mut TCB_t;
        if pxNewTCB.is_null() {
            crate::memory::vPortFree(pxStack.cast());
            return errCOULD_NOT_ALLOCATE_REQUIRED_MEMORY;
        }

        (pxNewTCB, pxStack)
    };

    /* [AMENDMENT] Rust allocations are uninitialised. Initialise every TCB
     * field before field assignments below, matching the upstream memset path
     * and preventing later reads of indeterminate Rust values. */
    ptr::write(pxNewTCB, TCB_t::new());
    (*pxNewTCB).pxStack = pxStack;

    // Calculate top of stack based on stack growth direction.
    let mut pxTopOfStack: *mut StackType_t;
    if portSTACK_GROWTH < 0 {
        // Stack grows down - top is at highest address.
        pxTopOfStack = pxStack.add(uxStackDepth - 1);
        pxTopOfStack = ((pxTopOfStack as usize) & !(portBYTE_ALIGNMENT - 1)) as *mut StackType_t;

        #[cfg(any(not(feature = "arch-32bit"), feature = "record-stack-high-address"))]
        {
            (*pxNewTCB).pxEndOfStack = pxTopOfStack;
        }
    } else {
        // Stack grows up - align the lowest usable address upward.
        pxTopOfStack = (((pxStack as usize) + (portBYTE_ALIGNMENT - 1)) & !(portBYTE_ALIGNMENT - 1))
            as *mut StackType_t;

        #[cfg(any(not(feature = "arch-32bit"), feature = "record-stack-high-address"))]
        {
            (*pxNewTCB).pxEndOfStack = pxStack.add(uxStackDepth - 1);
        }
    }
    configASSERT((pxTopOfStack as usize & (portBYTE_ALIGNMENT - 1)) == 0);

    // Fill stack with known value for high water mark (if enabled).
    if tskSET_NEW_STACKS_TO_KNOWN_VALUE != 0 {
        ptr::write_bytes(pxStack.cast::<u8>(), tskSTACK_FILL_BYTE, stack_size);
    }

    (*pxNewTCB).ucStaticallyAllocated = tskDYNAMICALLY_ALLOCATED_STACK_AND_TCB;

    prvInitialiseNewTask(
        pxTaskCode,
        pcName,
        uxStackDepth,
        pvParameters,
        uxPriority,
        pxCreatedTask,
        pxNewTCB,
        pxTopOfStack,
    );
    prvAddNewTaskToReadyList(pxNewTCB);

    pdPASS
}

/// Create a new task with statically allocated stack and TCB.
///
/// # Safety
///
/// puxStackBuffer must point to a valid stack buffer of at least uxStackDepth words.
/// pxTaskBuffer must point to a valid StaticTask_t.
///
/// # Arguments
///
/// * `pxTaskCode` - Pointer to the task function
/// * `pcName` - Name of the task
/// * `uxStackDepth` - Stack size in words
/// * `pvParameters` - Parameter to pass to the task function
/// * `uxPriority` - Task priority
/// * `puxStackBuffer` - Pointer to stack buffer
/// * `pxTaskBuffer` - Pointer to TCB buffer
///
/// # Returns
///
/// Task handle, or null if parameters are invalid.
pub unsafe fn xTaskCreateStatic(
    pxTaskCode: TaskFunction_t,
    pcName: *const u8,
    uxStackDepth: configSTACK_DEPTH_TYPE,
    pvParameters: *mut c_void,
    uxPriority: UBaseType_t,
    puxStackBuffer: *mut StackType_t,
    pxTaskBuffer: *mut StaticTask_t,
) -> TaskHandle_t {
    let mut xReturn: TaskHandle_t = ptr::null_mut();

    let xStackSizeBytes = uxStackDepth.checked_mul(core::mem::size_of::<StackType_t>());

    if uxStackDepth > 0
        && xStackSizeBytes.is_some()
        && !puxStackBuffer.is_null()
        && !pxTaskBuffer.is_null()
    {
        // Use the provided TCB buffer.
        let pxNewTCB = pxTaskBuffer as *mut TCB_t;

        /* Initialise every Rust field with a valid value. A bytewise zero is
         * not a general substitute for constructing a Rust value. */
        ptr::write(pxNewTCB, TCB_t::new());

        (*pxNewTCB).pxStack = puxStackBuffer;

        // Calculate top of stack.
        let pxTopOfStack: *mut StackType_t;
        if portSTACK_GROWTH < 0 {
            let pxTop = puxStackBuffer.add(uxStackDepth - 1);
            pxTopOfStack = pxTop.map_addr(|address| address & !(portBYTE_ALIGNMENT - 1));

            #[cfg(any(not(feature = "arch-32bit"), feature = "record-stack-high-address"))]
            {
                (*pxNewTCB).pxEndOfStack = pxTopOfStack;
            }
        } else {
            pxTopOfStack = puxStackBuffer.map_addr(|address| {
                address.wrapping_add(portBYTE_ALIGNMENT - 1) & !(portBYTE_ALIGNMENT - 1)
            });

            #[cfg(any(not(feature = "arch-32bit"), feature = "record-stack-high-address"))]
            {
                (*pxNewTCB).pxEndOfStack = puxStackBuffer.add(uxStackDepth - 1);
            }
        }
        configASSERT((pxTopOfStack as usize & (portBYTE_ALIGNMENT - 1)) == 0);

        // Fill stack with known value if enabled.
        if tskSET_NEW_STACKS_TO_KNOWN_VALUE != 0 {
            ptr::write_bytes(
                puxStackBuffer as *mut u8,
                tskSTACK_FILL_BYTE,
                xStackSizeBytes.unwrap(),
            );
        }

        // Mark as statically allocated.
        (*pxNewTCB).ucStaticallyAllocated = tskSTATICALLY_ALLOCATED_STACK_AND_TCB;

        // Initialize the task.
        prvInitialiseNewTask(
            pxTaskCode,
            pcName,
            uxStackDepth,
            pvParameters,
            uxPriority,
            &mut xReturn,
            pxNewTCB,
            pxTopOfStack,
        );

        // Add to ready list.
        prvAddNewTaskToReadyList(pxNewTCB);
    }

    xReturn
}

// =============================================================================
// Public API - Scheduler Control
// =============================================================================

/// Start the FreeRTOS scheduler.
///
/// After calling this function, the scheduler will begin executing tasks.
/// This function does not return under normal operation.
///
/// # Safety
///
/// Call exactly once from exclusive pre-scheduler task context after every
/// application task and required static system-task buffer has been installed.
/// No interrupt may enter the kernel until the selected port is ready to start
/// the first task. Calling again, or with concurrently mutable kernel state,
/// can restore an invalid task context.
pub unsafe fn vTaskStartScheduler() {
    unsafe {
        // Create idle task(s).
        let xReturn = prvCreateIdleTasks();

        // Create the timer task if timers are enabled.
        #[cfg(feature = "timers")]
        let xReturn = if xReturn == pdPASS {
            /* SAFETY: vTaskStartScheduler's contract gives this exclusive
             * pre-start path ownership of task and timer module state. */
            crate::kernel::timers::xTimerCreateTimerTask()
        } else {
            xReturn
        };

        if xReturn == pdPASS {
            // Disable interrupts to ensure a tick doesn't occur before the first
            // task is switched in.
            portDISABLE_INTERRUPTS();

            xNextTaskUnblockTime = portMAX_DELAY;
            xSchedulerRunning = pdTRUE;
            xTickCount = 0;

            // Initialize tick count (may have a non-zero starting value).
            if configINITIAL_TICK_COUNT != 0 {
                xTickCount = configINITIAL_TICK_COUNT;
            }

            #[cfg(feature = "generate-run-time-stats")]
            {
                crate::port::portCONFIGURE_TIMER_FOR_RUN_TIME_STATS();
                ulRunTimeStart = crate::port::portGET_RUN_TIME_COUNTER_VALUE();
                ulTaskSwitchedInTime = ulRunTimeStart;
            }

            crate::trace::traceTASK_SWITCHED_IN();

            // Start the first task running.
            // This should not return.
            xPortStartScheduler();

            // Should not reach here.
        } else {
            // Could not create idle task.
            configASSERT(xReturn == pdPASS);
        }
    }
}

/// End the scheduler.
///
/// Stops the scheduler and restores the system to its pre-scheduler state.
///
/// # Safety
///
/// The selected port must support stopping its scheduler, and the caller must
/// own the scheduler shutdown sequence with no concurrent kernel users.
pub unsafe fn vTaskEndScheduler() {
    unsafe {
        #[cfg(feature = "task-delete")]
        {
            #[cfg(feature = "timers")]
            {
                vTaskDelete(crate::kernel::timers::xTimerGetTimerDaemonTaskHandle());
            }

            vTaskDelete(xIdleTaskHandles[0]);

            /* The idle task normally reclaims these TCBs. It has just been
             * deleted, so complete the cleanup before stopping the port. */
            prvCheckTasksWaitingTermination();
        }

        portDISABLE_INTERRUPTS();
        xSchedulerRunning = pdFALSE;
        vPortEndScheduler();
    }
}

/// Suspend all tasks (enter scheduler suspension).
///
/// While suspended, context switches will not occur and no task will run
/// except the current one. Call xTaskResumeAll() to resume.
///
/// # Safety
///
/// Call only from a live task context. Every successful call must be paired
/// with [`xTaskResumeAll`] by the same execution context before it blocks,
/// yields, exits, or allows another owner to mutate scheduler state.
pub unsafe fn vTaskSuspendAll() {
    unsafe {
        /* A critical section is not required as the variable is of type
         * UBaseType_t. Each task maintains its own context, and a context
         * switch cannot occur while the variable is non-zero.
         *
         * [AMENDMENT] The previous Rust translation disabled and then
         * unconditionally enabled interrupts here, which could clear an outer
         * critical section's mask. Compiler and port barriers preserve the
         * upstream single-core ordering without changing interrupt state. */
        core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
        uxSchedulerSuspended = uxSchedulerSuspended.wrapping_add(1);
        portMEMORY_BARRIER();
    }
}

/// Resume all tasks after suspension.
///
/// # Returns
///
/// pdTRUE if a context switch occurred, pdFALSE otherwise.
///
/// # Safety
///
/// The caller must own one outstanding [`vTaskSuspendAll`] nesting level in a
/// live task context, and all TCBs/list items reachable through pending lists
/// must remain valid for the operation.
pub unsafe fn xTaskResumeAll() -> BaseType_t {
    let mut xAlreadyYielded = pdFALSE;

    unsafe {
        // It's an error to call this before the scheduler is started.
        configASSERT(uxSchedulerSuspended > 0);

        taskENTER_CRITICAL();
        {
            uxSchedulerSuspended -= 1;

            if uxSchedulerSuspended == 0 {
                if uxCurrentNumberOfTasks > 0 {
                    let mut pxMovedTCB: *mut TCB_t = ptr::null_mut();

                    // Move any tasks from pending ready list to ready list.
                    while listLIST_IS_EMPTY(ptr::addr_of!(xPendingReadyList)) == pdFALSE {
                        let pxTCB = listGET_OWNER_OF_HEAD_ENTRY(ptr::addr_of!(xPendingReadyList))
                            as *mut TCB_t;
                        let _ux = uxListRemove(ptr::addr_of_mut!((*pxTCB).xEventListItem));
                        portMEMORY_BARRIER();
                        let _ux = uxListRemove(ptr::addr_of_mut!((*pxTCB).xStateListItem));
                        prvAddTaskToReadyList(pxTCB);
                        pxMovedTCB = pxTCB;

                        // Yield if the moved task has higher priority.
                        if (*pxTCB).uxPriority > (*pxCurrentTCB).uxPriority {
                            xYieldPendings[0] = pdTRUE;
                        }
                    }

                    if !pxMovedTCB.is_null() {
                        prvResetNextTaskUnblockTime();
                    }

                    // Process any pended ticks.
                    if xPendedTicks > 0 {
                        while xPendedTicks > 0 {
                            if xTaskIncrementTick() != pdFALSE {
                                xYieldPendings[0] = pdTRUE;
                            }
                            xPendedTicks -= 1;
                        }
                    }

                    // Yield if pending.
                    if xYieldPendings[0] != pdFALSE {
                        xAlreadyYielded = pdTRUE;
                        portYIELD_WITHIN_API();
                    }
                }
            }
        }
        taskEXIT_CRITICAL();
    }

    xAlreadyYielded
}

// =============================================================================
// Public API - Delay Functions
// =============================================================================

/// Delay the current task for a number of ticks.
///
/// # Arguments
///
/// * `xTicksToDelay` - Number of ticks to delay
///
/// # Safety
///
/// Call only for the currently executing live task after the scheduler has
/// started, never from an ISR or while the scheduler is suspended.
pub unsafe fn vTaskDelay(xTicksToDelay: TickType_t) {
    let mut xAlreadyYielded = pdFALSE;

    if xTicksToDelay > 0 {
        unsafe {
            configASSERT(uxSchedulerSuspended == 0);

            vTaskSuspendAll();
            {
                crate::trace::traceTASK_DELAY();
                prvAddCurrentTaskToDelayedList(xTicksToDelay, pdFALSE);
            }
            xAlreadyYielded = xTaskResumeAll();
        }
    }

    if xAlreadyYielded == pdFALSE {
        portYIELD_WITHIN_API();
    }
}

/// Delay the current task until a specified time.
///
/// This allows a task to execute with a fixed period.
///
/// # Arguments
///
/// * `pxPreviousWakeTime` - Pointer to the last wake time (updated by this function)
/// * `xTimeIncrement` - The desired period in ticks
///
/// # Returns
///
/// pdTRUE if the task was delayed, pdFALSE if the deadline was missed.
///
/// # Safety
///
/// `pxPreviousWakeTime` must be valid and uniquely writable for the call. This
/// task-context API must not be invoked from an ISR.
pub unsafe fn xTaskDelayUntil(
    pxPreviousWakeTime: *mut TickType_t,
    xTimeIncrement: TickType_t,
) -> BaseType_t {
    let xAlreadyYielded: BaseType_t;
    let mut xShouldDelay = pdFALSE;

    unsafe {
        configASSERT(!pxPreviousWakeTime.is_null());
        configASSERT(xTimeIncrement > 0);
        configASSERT(uxSchedulerSuspended == 0);

        vTaskSuspendAll();
        {
            let xConstTickCount = xTickCount;
            let xTimeToWake = (*pxPreviousWakeTime).wrapping_add(xTimeIncrement);

            if xConstTickCount < *pxPreviousWakeTime {
                // Tick count has overflowed.
                if xTimeToWake < *pxPreviousWakeTime && xTimeToWake > xConstTickCount {
                    xShouldDelay = pdTRUE;
                }
            } else {
                // Tick count has not overflowed.
                if xTimeToWake < *pxPreviousWakeTime || xTimeToWake > xConstTickCount {
                    xShouldDelay = pdTRUE;
                }
            }

            *pxPreviousWakeTime = xTimeToWake;

            if xShouldDelay != pdFALSE {
                crate::trace::traceTASK_DELAY_UNTIL(xTimeToWake);
                prvAddCurrentTaskToDelayedList(xTimeToWake.wrapping_sub(xConstTickCount), pdFALSE);
            }
        }
        xAlreadyYielded = xTaskResumeAll();
    }

    /* Force a reschedule if xTaskResumeAll has not already done so, we may
     * have put ourselves to sleep. */
    if xAlreadyYielded == pdFALSE {
        portYIELD_WITHIN_API();
    }

    xShouldDelay
}

// =============================================================================
// Public API - Task Queries
// =============================================================================

/// Get the current scheduler state.
pub fn xTaskGetSchedulerState() -> BaseType_t {
    unsafe {
        if xSchedulerRunning == pdFALSE {
            taskSCHEDULER_NOT_STARTED
        } else if uxSchedulerSuspended == 0 {
            taskSCHEDULER_RUNNING
        } else {
            taskSCHEDULER_SUSPENDED
        }
    }
}

/// Get the current task handle.
pub fn xTaskGetCurrentTaskHandle() -> TaskHandle_t {
    unsafe { pxCurrentTCB as TaskHandle_t }
}

/// Get the current task for a particular scheduler core.
///
/// The single-core kernel accepts core zero and returns null for every other
/// core identifier, matching the shared upstream API.
pub fn xTaskGetCurrentTaskHandleForCore(xCoreID: BaseType_t) -> TaskHandle_t {
    if xCoreID == 0 {
        unsafe { pxCurrentTCB.cast() }
    } else {
        ptr::null_mut()
    }
}

/// Increment the mutex held count for the current task.
///
/// Called when a mutex is successfully taken.
///
/// # Safety
///
/// The caller must be the live current task and must hold the queue/mutex
/// exclusion required by the successful take path.
#[cfg(feature = "use-mutexes")]
pub unsafe fn pvTaskIncrementMutexHeldCount() -> TaskHandle_t {
    unsafe {
        if !pxCurrentTCB.is_null() {
            (*pxCurrentTCB).uxMutexesHeld = (*pxCurrentTCB).uxMutexesHeld.wrapping_add(1);
        }

        pxCurrentTCB as TaskHandle_t
    }
}

/// Get the number of tasks in the system.
pub fn uxTaskGetNumberOfTasks() -> UBaseType_t {
    unsafe { uxCurrentNumberOfTasks }
}

/// Return the fixed-size name storage for a task.
///
/// # Safety
///
/// A non-null handle must identify a live task. Null selects the current task.
/// The returned pointer remains valid only while that task's TCB remains live.
pub unsafe fn pcTaskGetName(xTaskToQuery: TaskHandle_t) -> *mut u8 {
    let pxTCB = prvGetTCBFromHandle(xTaskToQuery);
    configASSERT(!pxTCB.is_null());
    (*pxTCB).pcTaskName.as_mut_ptr()
}

/// Return the stack and TCB buffers when a task uses static storage.
///
/// # Safety
///
/// A non-null task handle must identify a live task. Both output pointers must
/// be non-null, aligned, uniquely writable, and valid for one pointer value.
pub unsafe fn xTaskGetStaticBuffers(
    xTask: TaskHandle_t,
    ppuxStackBuffer: *mut *mut StackType_t,
    ppxTaskBuffer: *mut *mut StaticTask_t,
) -> BaseType_t {
    configASSERT(!ppuxStackBuffer.is_null());
    configASSERT(!ppxTaskBuffer.is_null());

    let pxTCB = prvGetTCBFromHandle(xTask);
    configASSERT(!pxTCB.is_null());

    if (*pxTCB).ucStaticallyAllocated == tskSTATICALLY_ALLOCATED_STACK_AND_TCB {
        *ppuxStackBuffer = (*pxTCB).pxStack;
        *ppxTaskBuffer = pxTCB.cast();
        pdTRUE
    } else if (*pxTCB).ucStaticallyAllocated == tskSTATICALLY_ALLOCATED_STACK_ONLY {
        *ppuxStackBuffer = (*pxTCB).pxStack;
        *ppxTaskBuffer = ptr::null_mut();
        pdTRUE
    } else {
        pdFALSE
    }
}

/// Get the current tick count.
pub fn xTaskGetTickCount() -> TickType_t {
    let xTicks: TickType_t;
    taskENTER_CRITICAL();
    unsafe {
        /* [AMENDMENT] Upstream declares xTickCount volatile because the tick
         * ISR changes it asynchronously. A plain Rust load can be hoisted out
         * of a task-side polling loop, which was observed on ARMv6-M. */
        xTicks = ptr::read_volatile(ptr::addr_of!(xTickCount));
    }
    taskEXIT_CRITICAL();
    xTicks
}

/// Get the current tick count from ISR.
pub fn xTaskGetTickCountFromISR() -> TickType_t {
    let xTicks: TickType_t;
    let uxSavedInterruptStatus = taskENTER_CRITICAL_FROM_ISR();
    unsafe {
        xTicks = ptr::read_volatile(ptr::addr_of!(xTickCount));
    }
    taskEXIT_CRITICAL_FROM_ISR(uxSavedInterruptStatus);
    xTicks
}

// =============================================================================
// Public API - Event List Functions
// =============================================================================

/// Remove a task from an event list and add to ready list.
///
/// Called by queue/semaphore when unblocking a waiting task.
///
/// # Safety
///
/// pxEventList must point to a valid List_t.
///
/// # Returns
///
/// pdTRUE if the unblocked task has higher priority than the current task.
pub unsafe fn xTaskRemoveFromEventList(pxEventList: *const List_t) -> BaseType_t {
    // Remove the highest priority task from the event list.
    let pxUnblockedTCB = listGET_OWNER_OF_HEAD_ENTRY(pxEventList) as *mut TCB_t;
    configASSERT(!pxUnblockedTCB.is_null());

    let _ux = uxListRemove(ptr::addr_of_mut!((*pxUnblockedTCB).xEventListItem));

    if uxSchedulerSuspended == 0 {
        // Remove from delayed list and add to ready list.
        let _ux = uxListRemove(ptr::addr_of_mut!((*pxUnblockedTCB).xStateListItem));
        prvAddTaskToReadyList(pxUnblockedTCB);

        #[cfg(feature = "tickless-idle")]
        prvResetNextTaskUnblockTime();
    } else {
        // Scheduler is suspended, add to pending ready list.
        vListInsertEnd(
            ptr::addr_of_mut!(xPendingReadyList),
            ptr::addr_of_mut!((*pxUnblockedTCB).xEventListItem),
        );
    }

    // Return pdTRUE if the unblocked task has higher priority.
    if (*pxUnblockedTCB).uxPriority > (*pxCurrentTCB).uxPriority {
        xYieldPendings[0] = pdTRUE;
        pdTRUE
    } else {
        pdFALSE
    }
}

/// Place the current task on an event list.
///
/// Called when a task blocks on a queue/semaphore.
///
/// # Safety
///
/// pxEventList must point to a valid List_t.
pub unsafe fn vTaskPlaceOnEventList(pxEventList: *mut List_t, xTicksToWait: TickType_t) {
    configASSERT(!pxEventList.is_null());

    // Place the task on the event list (ordered by priority).
    vListInsert(
        pxEventList,
        ptr::addr_of_mut!((*pxCurrentTCB).xEventListItem),
    );

    // Add to the delayed list.
    prvAddCurrentTaskToDelayedList(xTicksToWait, pdTRUE);
}

/// Place the current task on an event list (restricted variant).
///
/// # Safety
///
/// `pxEventList` must be a live, uniquely mutable kernel list. The current TCB
/// and scheduler lists must be initialized, and the scheduler must be suspended.
pub unsafe fn vTaskPlaceOnEventListRestricted(
    pxEventList: *mut List_t,
    mut xTicksToWait: TickType_t,
    xWaitIndefinitely: BaseType_t,
) {
    configASSERT(!pxEventList.is_null());
    configASSERT(uxSchedulerSuspended != 0);

    // Place at end (not ordered).
    vListInsertEnd(
        pxEventList,
        ptr::addr_of_mut!((*pxCurrentTCB).xEventListItem),
    );

    if xWaitIndefinitely != pdFALSE {
        xTicksToWait = portMAX_DELAY;
    }

    // Add to delayed list.
    prvAddCurrentTaskToDelayedList(xTicksToWait, xWaitIndefinitely);
}

/// Place the current task on an unordered event list.
///
/// # Safety
///
/// `pxEventList` must be a live, uniquely mutable event-group list. The current
/// TCB must remain live and the caller must own scheduler suspension.
pub unsafe fn vTaskPlaceOnUnorderedEventList(
    pxEventList: *mut List_t,
    xItemValue: TickType_t,
    xTicksToWait: TickType_t,
) {
    configASSERT(!pxEventList.is_null());

    // Store the item value.
    listSET_LIST_ITEM_VALUE(
        ptr::addr_of_mut!((*pxCurrentTCB).xEventListItem),
        xItemValue | taskEVENT_LIST_ITEM_VALUE_IN_USE,
    );

    // Insert at end of list.
    vListInsertEnd(
        pxEventList,
        ptr::addr_of_mut!((*pxCurrentTCB).xEventListItem),
    );

    // Add to delayed list.
    prvAddCurrentTaskToDelayedList(xTicksToWait, pdTRUE);
}

/// Remove from an unordered event list.
///
/// # Safety
///
/// `pxEventListItem` must be linked, uniquely mutable, and owned by a live TCB.
/// The current TCB must be live and the caller must own scheduler suspension.
pub unsafe fn xTaskRemoveFromUnorderedEventList(
    pxEventListItem: *mut ListItem_t,
    xItemValue: TickType_t,
) -> BaseType_t {
    configASSERT(uxSchedulerSuspended != 0);

    // Store the event result while preserving the marker that prevents a
    // priority change from overwriting it before the task consumes the value.
    listSET_LIST_ITEM_VALUE(
        pxEventListItem,
        xItemValue | taskEVENT_LIST_ITEM_VALUE_IN_USE,
    );

    // Remove from event list.
    let pxUnblockedTCB = listGET_LIST_ITEM_OWNER(pxEventListItem) as *mut TCB_t;
    configASSERT(!pxUnblockedTCB.is_null());
    let _ux = uxListRemove(pxEventListItem);

    #[cfg(feature = "tickless-idle")]
    {
        prvResetNextTaskUnblockTime();
    }

    // Event groups are manipulated at task level with the scheduler suspended,
    // so the ready lists can be updated directly as in the upstream function.
    let _ux = uxListRemove(ptr::addr_of_mut!((*pxUnblockedTCB).xStateListItem));
    prvAddTaskToReadyList(pxUnblockedTCB);

    if (*pxUnblockedTCB).uxPriority > (*pxCurrentTCB).uxPriority {
        xYieldPendings[0] = pdTRUE;
        pdTRUE
    } else {
        pdFALSE
    }
}

// =============================================================================
// Public API - Event Item Value Functions
// =============================================================================

/// Reset the event list item value and return the previous value.
///
/// When a task is unblocked from an event list due to a bit being set,
/// the event bits are stored in the event list item value. This function
/// retrieves that value and resets the item to its normal priority-based value.
///
/// # Returns
/// The value that was in the event list item (typically containing event bits).
///
/// # Safety
///
/// A live current TCB must be installed, and the caller must own the scheduler
/// exclusion that protects its event-list item.
pub unsafe fn uxTaskResetEventItemValue() -> TickType_t {
    unsafe {
        let pxCurrentTCBLocal = pxCurrentTCB;

        // Get the current value stored in the event list item
        let uxReturn = listGET_LIST_ITEM_VALUE(ptr::addr_of!((*pxCurrentTCBLocal).xEventListItem));

        // Reset the event list item value to its normal priority-based value
        // The value is set to configMAX_PRIORITIES - priority, inverted so
        // higher priorities have lower item values (for sorted list ordering)
        listSET_LIST_ITEM_VALUE(
            ptr::addr_of_mut!((*pxCurrentTCBLocal).xEventListItem),
            (configMAX_PRIORITIES as TickType_t)
                .wrapping_sub((*pxCurrentTCBLocal).uxPriority as TickType_t),
        );

        uxReturn
    }
}

// =============================================================================
// Public API - Timeout Functions
// =============================================================================

/// Initialize a TimeOut_t structure with the current time.
///
/// # Safety
///
/// `pxTimeOut` must point to uniquely writable, initialized storage. This is a
/// task-context API.
pub unsafe fn vTaskSetTimeOutState(pxTimeOut: *mut TimeOut_t) {
    unsafe {
        configASSERT(!pxTimeOut.is_null());
        taskENTER_CRITICAL();
        {
            (*pxTimeOut).xOverflowCount = xNumOfOverflows;
            (*pxTimeOut).xTimeOnEntering = xTickCount;
        }
        taskEXIT_CRITICAL();
    }
}

/// Internal version of vTaskSetTimeOutState (called from within critical section).
///
/// # Safety
///
/// `pxTimeOut` must point to uniquely writable, initialized storage and the
/// caller must already provide the scheduler exclusion required by FreeRTOS.
pub unsafe fn vTaskInternalSetTimeOutState(pxTimeOut: *mut TimeOut_t) {
    unsafe {
        (*pxTimeOut).xOverflowCount = xNumOfOverflows;
        (*pxTimeOut).xTimeOnEntering = xTickCount;
    }
}

/// Check if a timeout has occurred.
///
/// # Returns
///
/// pdTRUE if the timeout has occurred, pdFALSE if time remains.
///
/// # Safety
///
/// Both pointers must be non-null, valid, uniquely writable for the call, and
/// refer to initialized values. This task-context API must not run in an ISR.
pub unsafe fn xTaskCheckForTimeOut(
    pxTimeOut: *mut TimeOut_t,
    pxTicksToWait: *mut TickType_t,
) -> BaseType_t {
    let xReturn: BaseType_t;

    unsafe {
        configASSERT(!pxTimeOut.is_null());
        configASSERT(!pxTicksToWait.is_null());

        taskENTER_CRITICAL();
        {
            let xConstTickCount = xTickCount;
            let xElapsedTime = xConstTickCount.wrapping_sub((*pxTimeOut).xTimeOnEntering);

            #[cfg(feature = "abort-delay")]
            {
                if (*pxCurrentTCB).ucDelayAborted != (pdFALSE as u8) {
                    (*pxCurrentTCB).ucDelayAborted = pdFALSE as u8;
                    xReturn = pdTRUE;
                    taskEXIT_CRITICAL();
                    return xReturn;
                }
            }

            if cfg!(feature = "task-suspend") && *pxTicksToWait == portMAX_DELAY {
                // Task is blocking indefinitely.
                xReturn = pdFALSE;
            } else if xNumOfOverflows != (*pxTimeOut).xOverflowCount
                && xConstTickCount >= (*pxTimeOut).xTimeOnEntering
            {
                // Tick count overflow and time has passed.
                xReturn = pdTRUE;
                *pxTicksToWait = 0;
            } else if xElapsedTime < *pxTicksToWait {
                // Time remaining.
                *pxTicksToWait -= xElapsedTime;
                // SAFETY: the enclosing contract validates pxTimeOut and the
                // task critical section remains active here.
                vTaskInternalSetTimeOutState(pxTimeOut);
                xReturn = pdFALSE;
            } else {
                // Timeout.
                *pxTicksToWait = 0;
                xReturn = pdTRUE;
            }
        }
        taskEXIT_CRITICAL();
    }

    xReturn
}

// =============================================================================
// Public API - Priority Inheritance (Mutexes)
// =============================================================================

/// Implement priority inheritance for a mutex holder.
///
/// Returns `pdTRUE` if inheritance occurred, or would have occurred had the
/// holder not already inherited an equal or higher priority.
///
/// # Safety
///
/// A non-null `pxMutexHolder` must identify a live task whose TCB and list
/// items cannot be reclaimed or concurrently mutated during the call. The
/// current task must then also be valid. The caller must provide exclusive
/// access to the affected scheduler lists and TCBs; the normal mutex-take path
/// does so with a task critical section. This is not a FromISR API.
pub unsafe fn xTaskPriorityInherit(pxMutexHolder: TaskHandle_t) -> BaseType_t {
    #[cfg(feature = "use-mutexes")]
    unsafe {
        let mut xReturn = pdFALSE;

        if !pxMutexHolder.is_null() {
            let pxMutexHolderTCB = pxMutexHolder as *mut TCB_t;

            // Only inherit if holder has lower priority.
            if (*pxMutexHolderTCB).uxPriority < (*pxCurrentTCB).uxPriority {
                // Adjust event list item value if in an event list.
                if (listGET_LIST_ITEM_VALUE(ptr::addr_of!((*pxMutexHolderTCB).xEventListItem))
                    & taskEVENT_LIST_ITEM_VALUE_IN_USE)
                    == 0
                {
                    listSET_LIST_ITEM_VALUE(
                        ptr::addr_of_mut!((*pxMutexHolderTCB).xEventListItem),
                        (configMAX_PRIORITIES - (*pxCurrentTCB).uxPriority) as TickType_t,
                    );
                }

                // If holder is in ready list, move to new priority's list.
                if listIS_CONTAINED_WITHIN(
                    ptr::addr_of!(pxReadyTasksLists[(*pxMutexHolderTCB).uxPriority as usize]),
                    ptr::addr_of!((*pxMutexHolderTCB).xStateListItem),
                ) != pdFALSE
                {
                    if uxListRemove(ptr::addr_of_mut!((*pxMutexHolderTCB).xStateListItem)) == 0 {
                        taskRESET_READY_PRIORITY((*pxMutexHolderTCB).uxPriority);
                    }

                    (*pxMutexHolderTCB).uxPriority = (*pxCurrentTCB).uxPriority;
                    prvAddTaskToReadyList(pxMutexHolderTCB);
                } else {
                    (*pxMutexHolderTCB).uxPriority = (*pxCurrentTCB).uxPriority;
                }

                crate::trace::traceTASK_PRIORITY_INHERIT(
                    pxMutexHolderTCB as *mut c_void,
                    (*pxCurrentTCB).uxPriority,
                );

                xReturn = pdTRUE;
            } else if (*pxMutexHolderTCB).uxBasePriority < (*pxCurrentTCB).uxPriority {
                /* The holder has already inherited an equal or higher
                 * priority. Remember that inheritance was nevertheless
                 * required so a later timeout can disinherit correctly. */
                xReturn = pdTRUE;
            }
        }

        xReturn
    }

    #[cfg(not(feature = "use-mutexes"))]
    {
        let _ = pxMutexHolder;
        pdFALSE
    }
}

/// Compatibility wrapper retained for existing callers of the earlier Rust
/// translation. New kernel code uses [`xTaskPriorityInherit`] to observe
/// whether inheritance occurred.
///
/// # Safety
///
/// The same handle-liveness, current-task, exclusion, and task-context
/// requirements as [`xTaskPriorityInherit`] apply.
#[inline(always)]
pub unsafe fn vTaskPriorityInherit(pxMutexHolder: TaskHandle_t) {
    // SAFETY: forwarded directly under this function's identical contract.
    let _ = unsafe { xTaskPriorityInherit(pxMutexHolder) };
}

/// Restore priority after releasing mutex.
///
/// # Returns
///
/// pdTRUE if a context switch is required.
///
/// # Safety
///
/// A non-null `pxMutexHolder` must identify the live current task, with at
/// least one mutex recorded as held. Its TCB and list items must not be
/// reclaimed or concurrently mutated. The caller must provide exclusive
/// access to the affected scheduler lists and TCB; the normal mutex-give path
/// does so with a task critical section. This is not a FromISR API.
pub unsafe fn xTaskPriorityDisinherit(pxMutexHolder: TaskHandle_t) -> BaseType_t {
    #[cfg(feature = "use-mutexes")]
    unsafe {
        if !pxMutexHolder.is_null() {
            let pxTCB = pxMutexHolder as *mut TCB_t;

            configASSERT(pxTCB == pxCurrentTCB);
            configASSERT((*pxTCB).uxMutexesHeld > 0);
            (*pxTCB).uxMutexesHeld -= 1;

            // Only disinherit if we inherited priority.
            if (*pxTCB).uxPriority != (*pxTCB).uxBasePriority {
                // Only if no more mutexes held.
                if (*pxTCB).uxMutexesHeld == 0 {
                    // Remove from current priority's ready list.
                    if uxListRemove(ptr::addr_of_mut!((*pxTCB).xStateListItem)) == 0 {
                        taskRESET_READY_PRIORITY((*pxTCB).uxPriority);
                    }

                    // Restore base priority.
                    (*pxTCB).uxPriority = (*pxTCB).uxBasePriority;

                    // Reset event list item value.
                    listSET_LIST_ITEM_VALUE(
                        ptr::addr_of_mut!((*pxTCB).xEventListItem),
                        (configMAX_PRIORITIES - (*pxTCB).uxPriority) as TickType_t,
                    );

                    // Add to correct ready list.
                    prvAddTaskToReadyList(pxTCB);

                    crate::trace::traceTASK_PRIORITY_DISINHERIT(
                        pxTCB as *mut c_void,
                        (*pxTCB).uxBasePriority,
                    );

                    return pdTRUE;
                }
            }
        }

        pdFALSE
    }

    #[cfg(not(feature = "use-mutexes"))]
    {
        let _ = pxMutexHolder;
        pdFALSE
    }
}

/// Disinherit priority after timeout on mutex wait.
///
/// # Safety
///
/// A non-null `pxMutexHolder` must identify a live, non-current task with at
/// least one mutex recorded as held. Its TCB and list items must not be
/// reclaimed or concurrently mutated. The caller must provide exclusive
/// access to the affected scheduler lists and TCB; the normal queue-timeout
/// path does so with a task critical section. This is not a FromISR API.
pub unsafe fn vTaskPriorityDisinheritAfterTimeout(
    pxMutexHolder: TaskHandle_t,
    uxHighestPriorityWaitingTask: UBaseType_t,
) {
    #[cfg(feature = "use-mutexes")]
    unsafe {
        if !pxMutexHolder.is_null() {
            let pxTCB = pxMutexHolder as *mut TCB_t;

            configASSERT((*pxTCB).uxMutexesHeld > 0);

            // New priority is max of base priority and the highest task still
            // waiting for this mutex.
            let uxPriorityToUse = if (*pxTCB).uxBasePriority > uxHighestPriorityWaitingTask {
                (*pxTCB).uxBasePriority
            } else {
                uxHighestPriorityWaitingTask
            };

            if uxPriorityToUse != (*pxTCB).uxPriority {
                /* Only disinherit if this is the sole mutex held. Other held
                 * mutexes may be the source of the inherited priority. */
                if (*pxTCB).uxMutexesHeld == 1 {
                    configASSERT(pxTCB != pxCurrentTCB);

                    let uxPriorityUsedOnEntry = (*pxTCB).uxPriority;
                    (*pxTCB).uxPriority = uxPriorityToUse;

                    if (listGET_LIST_ITEM_VALUE(ptr::addr_of!((*pxTCB).xEventListItem))
                        & taskEVENT_LIST_ITEM_VALUE_IN_USE)
                        == 0
                    {
                        listSET_LIST_ITEM_VALUE(
                            ptr::addr_of_mut!((*pxTCB).xEventListItem),
                            (configMAX_PRIORITIES - uxPriorityToUse) as TickType_t,
                        );
                    }

                    if listIS_CONTAINED_WITHIN(
                        ptr::addr_of!(pxReadyTasksLists[uxPriorityUsedOnEntry as usize]),
                        ptr::addr_of!((*pxTCB).xStateListItem),
                    ) != pdFALSE
                    {
                        if uxListRemove(ptr::addr_of_mut!((*pxTCB).xStateListItem)) == 0 {
                            taskRESET_READY_PRIORITY(uxPriorityUsedOnEntry);
                        }
                        prvAddTaskToReadyList(pxTCB);
                    }
                }
            }
        }
    }

    #[cfg(not(feature = "use-mutexes"))]
    {
        let _ = pxMutexHolder;
        let _ = uxHighestPriorityWaitingTask;
    }
}

/// Signal that a yield was missed.
pub fn vTaskMissedYield() {
    unsafe {
        xYieldPendings[0] = pdTRUE;
    }
}

// =============================================================================
// Public API - Tick Handler
// =============================================================================

/// Increment the tick count and check for task unblocking.
///
/// Called from the tick interrupt. Must be called from a critical section
/// or with interrupts disabled.
///
/// # Returns
///
/// pdTRUE if a context switch should occur.
///
/// # Safety
///
/// Invoke only from the selected port's tick interrupt with its required
/// interrupt mask in force, after scheduler lists and the current TCB are live.
#[no_mangle]
pub unsafe extern "C" fn xTaskIncrementTick() -> BaseType_t {
    let mut xSwitchRequired = pdFALSE;

    unsafe {
        // Only increment if scheduler is not suspended.
        if uxSchedulerSuspended == 0 {
            let xConstTickCount = xTickCount.wrapping_add(1);
            xTickCount = xConstTickCount;

            if xConstTickCount == 0 {
                // Tick count overflowed, switch delayed lists.
                taskSWITCH_DELAYED_LISTS();
            }

            // Check for blocked tasks that need to wake.
            if xConstTickCount >= xNextTaskUnblockTime {
                loop {
                    if listLIST_IS_EMPTY(pxDelayedTaskList) != pdFALSE {
                        // Delayed list empty, set next unblock time to max.
                        xNextTaskUnblockTime = portMAX_DELAY;
                        break;
                    } else {
                        let pxTCB = listGET_OWNER_OF_HEAD_ENTRY(pxDelayedTaskList) as *mut TCB_t;
                        let xItemValue =
                            listGET_LIST_ITEM_VALUE(ptr::addr_of!((*pxTCB).xStateListItem));

                        if xConstTickCount < xItemValue {
                            // Task not ready yet, update next unblock time.
                            xNextTaskUnblockTime = xItemValue;
                            break;
                        }

                        // Remove from delayed list.
                        let _ux = uxListRemove(ptr::addr_of_mut!((*pxTCB).xStateListItem));

                        // Remove from event list if present.
                        if listLIST_ITEM_CONTAINER(ptr::addr_of!((*pxTCB).xEventListItem))
                            != ptr::null_mut()
                        {
                            let _ux = uxListRemove(ptr::addr_of_mut!((*pxTCB).xEventListItem));
                        }

                        // Add to ready list.
                        prvAddTaskToReadyList(pxTCB);

                        // Yield if unblocked task has higher priority.
                        if configUSE_PREEMPTION != 0 {
                            if (*pxTCB).uxPriority > (*pxCurrentTCB).uxPriority {
                                xSwitchRequired = pdTRUE;
                            }
                        }
                    }
                }
            }

            // Time slicing within same priority.
            if configUSE_PREEMPTION != 0 {
                if listCURRENT_LIST_LENGTH(ptr::addr_of!(
                    pxReadyTasksLists[(*pxCurrentTCB).uxPriority as usize]
                )) > 1
                {
                    xSwitchRequired = pdTRUE;
                }
            }

            // Tick hook.
            if configUSE_TICK_HOOK != 0 {
                // No tick-hook Cargo capability is currently advertised.
            }

            // Yield pending check.
            if xYieldPendings[0] != pdFALSE {
                xSwitchRequired = pdTRUE;
            }
        } else {
            // Scheduler suspended, pend the tick.
            xPendedTicks = xPendedTicks.wrapping_add(1);

            // Tick hook even when suspended.
            if configUSE_TICK_HOOK != 0 {
                // No tick-hook Cargo capability is currently advertised.
            }
        }
    }

    xSwitchRequired
}

// =============================================================================
// Public API - Context Switch Support
// =============================================================================

/// Called from PendSV to switch context.
///
/// Selects the next task to run and returns its TCB.
///
/// # Safety
///
/// Invoke only from the selected port's context-switch path with scheduler
/// interrupts masked and a live current TCB plus initialized ready lists.
#[no_mangle]
pub unsafe extern "C" fn vTaskSwitchContext() {
    unsafe {
        if uxSchedulerSuspended != 0 {
            // Cannot switch while suspended.
            xYieldPendings[0] = pdTRUE;
            return;
        }

        xYieldPendings[0] = pdFALSE;

        crate::trace::traceTASK_SWITCHED_OUT();

        // Update run-time stats for the task being switched out.
        #[cfg(feature = "generate-run-time-stats")]
        {
            // Get the current run-time counter value.
            let ulCurrentCounter = crate::port::portGET_RUN_TIME_COUNTER_VALUE();

            /* Treat the application timer as a wrapping unsigned counter. This
             * retains the elapsed interval across one hardware-counter wrap and
             * keeps debug and release behavior identical. */
            if !pxCurrentTCB.is_null() {
                let ulElapsed = prvRunTimeDelta(ulCurrentCounter, ulTaskSwitchedInTime);
                (*pxCurrentTCB).ulRunTimeCounter =
                    (*pxCurrentTCB).ulRunTimeCounter.wrapping_add(ulElapsed);
            }

            ulTaskSwitchedInTime = ulCurrentCounter;
        }

        // Check the outgoing task's saved stack pointer and guard bytes.
        #[cfg(feature = "stack-overflow-check")]
        taskCHECK_FOR_STACK_OVERFLOW();

        // Select next task.
        taskSELECT_HIGHEST_PRIORITY_TASK();

        crate::trace::traceTASK_SWITCHED_IN();
    }
}

// =============================================================================
// Yield within API
// =============================================================================

/// Yield within API (trigger context switch if preemption enabled).
///
/// # Safety
///
/// Call only from a live task context after scheduler startup and outside any
/// state transition that forbids a context switch.
#[inline(always)]
pub unsafe fn vTaskYieldWithinAPI() {
    portYIELD();
}

// =============================================================================
// Task Deletion (if enabled)
// =============================================================================

/// Delete a task.
///
/// The task being deleted will be removed from all ready, blocked, suspended,
/// and event lists. If the task is deleting itself, the actual freeing of the
/// TCB and stack memory is deferred to the idle task via `xTasksWaitingTermination`.
///
/// # Safety
/// - xTaskToDelete must be a valid task handle, or null to delete the calling task
/// - If deleting another task, ensure that task is not in a state where it could
///   become runnable (e.g., waiting on a semaphore that could be given)
///
/// # Parameters
/// - xTaskToDelete: Handle of the task to delete, or null to delete the calling task
#[cfg(feature = "task-delete")]
pub unsafe fn vTaskDelete(xTaskToDelete: TaskHandle_t) {
    unsafe {
        let mut xDeleteTCBInIdleTask: BaseType_t = pdFALSE;

        taskENTER_CRITICAL();
        {
            // If null is passed in here then it is the calling task that is being deleted.
            let pxTCB = prvGetTCBFromHandle(xTaskToDelete);
            configASSERT(!pxTCB.is_null());

            // Remove task from the ready/delayed list.
            if uxListRemove(ptr::addr_of_mut!((*pxTCB).xStateListItem)) == 0 {
                taskRESET_READY_PRIORITY((*pxTCB).uxPriority);
            }

            // Is the task waiting on an event also?
            if !listLIST_ITEM_CONTAINER(ptr::addr_of!((*pxTCB).xEventListItem)).is_null() {
                uxListRemove(ptr::addr_of_mut!((*pxTCB).xEventListItem));
            }

            // Increment the uxTaskNumber so kernel aware debuggers can detect
            // that the task lists need re-generating.
            uxTaskNumber = uxTaskNumber.wrapping_add(1);

            // Check if the task is running (or is the current task).
            // For single-core: task is running if it's pxCurrentTCB.
            let xTaskIsRunning = pxTCB == pxCurrentTCB;

            // If the task is running, we must add it to the termination list
            // so that the idle task can delete it when it is no longer running.
            if xSchedulerRunning != pdFALSE && xTaskIsRunning {
                // A running task is being deleted. This cannot complete when
                // the task is still running, as a context switch is required.
                // Place the task in the termination list. The idle task will
                // free up any memory allocated for the TCB and stack.
                vListInsertEnd(
                    ptr::addr_of_mut!(xTasksWaitingTermination),
                    ptr::addr_of_mut!((*pxTCB).xStateListItem),
                );

                // Increment the counter so the idle task knows there is a task
                // that has been deleted.
                uxDeletedTasksWaitingCleanUp += 1;

                crate::trace::traceTASK_DELETE(pxTCB as *mut c_void);

                // Delete the task TCB in idle task.
                xDeleteTCBInIdleTask = pdTRUE;

                // Pend a yield since the current task is being deleted.
                xYieldPendings[0] = pdTRUE;
            } else {
                // Task is not running, we can delete it immediately.
                uxCurrentNumberOfTasks -= 1;
                crate::trace::traceTASK_DELETE(pxTCB as *mut c_void);

                // Reset the next expected unblock time in case it referred to
                // the task that has just been deleted.
                prvResetNextTaskUnblockTime();
            }
        }
        taskEXIT_CRITICAL();

        // If the task is not deleting itself, call prvDeleteTCB from outside
        // of critical section. If a task deletes itself, prvDeleteTCB is called
        // from prvCheckTasksWaitingTermination which is called from the idle task.
        if xDeleteTCBInIdleTask != pdTRUE {
            prvDeleteTCB(prvGetTCBFromHandle(xTaskToDelete));
        }

        // Force a reschedule if it is the currently running task that has just
        // been deleted.
        if xSchedulerRunning != pdFALSE {
            let pxTCB = prvGetTCBFromHandle(xTaskToDelete);
            if pxTCB == pxCurrentTCB {
                configASSERT(uxSchedulerSuspended == 0);
                portYIELD_WITHIN_API();
            }
        }
    }
}

// =============================================================================
// Task Suspend/Resume (if enabled)
// =============================================================================

/// Suspend a task.
///
/// # Safety
///
/// A non-null handle must identify a live task for the duration of the call.
/// Null selects the current task. This task-context API must not be called from
/// an ISR.
#[cfg(feature = "task-suspend")]
pub unsafe fn vTaskSuspend(xTaskToSuspend: TaskHandle_t) {
    unsafe {
        taskENTER_CRITICAL();
        {
            let pxTCB = prvGetTCBFromHandle(xTaskToSuspend);
            configASSERT(!pxTCB.is_null());

            crate::trace::traceTASK_SUSPEND(pxTCB as *mut c_void);

            // Remove from ready/delayed list.
            if uxListRemove(ptr::addr_of_mut!((*pxTCB).xStateListItem)) == 0 {
                taskRESET_READY_PRIORITY((*pxTCB).uxPriority);
            }

            // Remove from event list if present.
            if listLIST_ITEM_CONTAINER(ptr::addr_of!((*pxTCB).xEventListItem)) != ptr::null_mut() {
                let _ux = uxListRemove(ptr::addr_of_mut!((*pxTCB).xEventListItem));
            }

            // Add to suspended list.
            vListInsertEnd(
                ptr::addr_of_mut!(xSuspendedTaskList),
                ptr::addr_of_mut!((*pxTCB).xStateListItem),
            );

            // Reset notify state.
            for i in 0..configTASK_NOTIFICATION_ARRAY_ENTRIES {
                if prvReadNotifyState(pxTCB, i) == taskWAITING_NOTIFICATION {
                    prvWriteNotifyState(pxTCB, i, taskNOT_WAITING_NOTIFICATION);
                }
            }
        }
        taskEXIT_CRITICAL();

        if xSchedulerRunning != pdFALSE {
            // Update next unblock time.
            taskENTER_CRITICAL();
            {
                prvResetNextTaskUnblockTime();
            }
            taskEXIT_CRITICAL();
        }

        let pxTCB = prvGetTCBFromHandle(xTaskToSuspend);
        if pxTCB == pxCurrentTCB {
            if xSchedulerRunning != pdFALSE {
                // Yield to switch to another task.
                portYIELD_WITHIN_API();
            } else {
                // Scheduler not running, select next task.
                if listCURRENT_LIST_LENGTH(ptr::addr_of!(xSuspendedTaskList))
                    == uxCurrentNumberOfTasks
                {
                    // All tasks suspended, null current TCB.
                    pxCurrentTCB = ptr::null_mut();
                } else {
                    vTaskSwitchContext();
                }
            }
        }
    }
}

/// Resume a suspended task.
///
/// # Safety
///
/// `xTaskToResume` must identify a live task for the duration of the call and
/// must not alias storage being reclaimed. This task-context API must not be
/// called from an ISR; use `xTaskResumeFromISR` there.
#[cfg(feature = "task-suspend")]
pub unsafe fn vTaskResume(xTaskToResume: TaskHandle_t) {
    unsafe {
        let pxTCB = xTaskToResume as *mut TCB_t;
        configASSERT(!pxTCB.is_null());

        if !pxTCB.is_null() && pxTCB != pxCurrentTCB {
            taskENTER_CRITICAL();
            {
                if prvTaskIsTaskSuspended(pxTCB) != pdFALSE {
                    crate::trace::traceTASK_RESUME(pxTCB as *mut c_void);

                    // Remove from suspended list.
                    let _ux = uxListRemove(ptr::addr_of_mut!((*pxTCB).xStateListItem));

                    // Add to ready list.
                    prvAddTaskToReadyList(pxTCB);

                    // Yield if resumed task has higher priority.
                    if (*pxTCB).uxPriority > (*pxCurrentTCB).uxPriority {
                        portYIELD_WITHIN_API();
                    }
                }
            }
            taskEXIT_CRITICAL();
        }
    }
}

/// Resume a suspended task from interrupt context.
///
/// Returns `pdTRUE` when the resumed task has a higher priority than the
/// interrupted task and the caller should request a context switch.
///
/// # Safety
///
/// `xTaskToResume` must be non-null and identify a live task whose TCB and list
/// items cannot be reclaimed while this operation runs. The caller must obey
/// the port's FromISR interrupt-priority rules; the handle must refer to a task
/// that may legally be resumed from this interrupt.
#[cfg(feature = "task-suspend")]
pub unsafe fn xTaskResumeFromISR(xTaskToResume: TaskHandle_t) -> BaseType_t {
    let mut xYieldRequired = pdFALSE;

    unsafe {
        let pxTCB = xTaskToResume as *mut TCB_t;
        configASSERT(!pxTCB.is_null());

        let uxSavedInterruptStatus = taskENTER_CRITICAL_FROM_ISR();
        {
            if prvTaskIsTaskSuspended(pxTCB) != pdFALSE {
                crate::trace::traceTASK_RESUME(pxTCB as *mut c_void);

                if uxSchedulerSuspended == 0 {
                    if (*pxTCB).uxPriority > (*pxCurrentTCB).uxPriority {
                        xYieldRequired = pdTRUE;
                        xYieldPendings[0] = pdTRUE;
                    }

                    let _ux = uxListRemove(ptr::addr_of_mut!((*pxTCB).xStateListItem));
                    prvAddTaskToReadyList(pxTCB);
                } else {
                    vListInsertEnd(
                        ptr::addr_of_mut!(xPendingReadyList),
                        ptr::addr_of_mut!((*pxTCB).xEventListItem),
                    );
                }
            }
        }
        taskEXIT_CRITICAL_FROM_ISR(uxSavedInterruptStatus);
    }

    xYieldRequired
}

/// Check if a task is suspended.
#[cfg(feature = "task-suspend")]
unsafe fn prvTaskIsTaskSuspended(pxTCB: *const TCB_t) -> BaseType_t {
    if listIS_CONTAINED_WITHIN(
        ptr::addr_of!(xSuspendedTaskList),
        ptr::addr_of!((*pxTCB).xStateListItem),
    ) != pdFALSE
    {
        // In suspended list.
        if listIS_CONTAINED_WITHIN(
            ptr::addr_of!(xPendingReadyList),
            ptr::addr_of!((*pxTCB).xEventListItem),
        ) == pdFALSE
        {
            // Not in pending ready list either.
            if listLIST_ITEM_CONTAINER(ptr::addr_of!((*pxTCB).xEventListItem)) == ptr::null_mut() {
                // A task blocked indefinitely on a direct notification also has
                // no event-list container, but it is not explicitly suspended.
                let mut xReturn = pdTRUE;
                let mut uxIndex = 0usize;
                while uxIndex < configTASK_NOTIFICATION_ARRAY_ENTRIES {
                    if prvReadNotifyState(pxTCB, uxIndex) == taskWAITING_NOTIFICATION {
                        xReturn = pdFALSE;
                        break;
                    }
                    uxIndex += 1;
                }
                return xReturn;
            }
        }
    }

    pdFALSE
}

// =============================================================================
// Critical Section Wrappers
// =============================================================================

/// Enter critical section.
#[inline(always)]
pub fn taskENTER_CRITICAL() {
    portENTER_CRITICAL();
}

/// Exit critical section.
#[inline(always)]
pub fn taskEXIT_CRITICAL() {
    portEXIT_CRITICAL();
}

/// Enter critical section from ISR.
#[inline(always)]
pub fn taskENTER_CRITICAL_FROM_ISR() -> UBaseType_t {
    portSET_INTERRUPT_MASK_FROM_ISR()
}

/// Exit critical section from ISR.
#[inline(always)]
pub fn taskEXIT_CRITICAL_FROM_ISR(uxSavedInterruptStatus: UBaseType_t) {
    portCLEAR_INTERRUPT_MASK_FROM_ISR(uxSavedInterruptStatus);
}

// =============================================================================
// Task Notification Functions
// =============================================================================

/// Notification action types.
#[repr(u8)]
pub enum eNotifyAction {
    eNoAction = 0,
    eSetBits = 1,
    eIncrement = 2,
    eSetValueWithOverwrite = 3,
    eSetValueWithoutOverwrite = 4,
}

/// Default notification index.
const tskDEFAULT_INDEX_TO_NOTIFY: UBaseType_t = 0;

/// Send a notification to a task.
///
/// # Arguments
///
/// * `xTaskToNotify` - Handle of the task to notify
/// * `ulValue` - Notification value
/// * `eAction` - Action to perform (as i32 for compatibility)
///
/// # Returns
///
/// pdPASS on success, pdFAIL if notification couldn't be sent
///
/// # Safety
///
/// `xTaskToNotify` must identify a live task. Call from task context with
/// scheduler state initialized; `eAction` must name a valid action.
pub unsafe fn xTaskNotify(xTaskToNotify: TaskHandle_t, ulValue: u32, eAction: i32) -> BaseType_t {
    xTaskGenericNotify(
        xTaskToNotify,
        tskDEFAULT_INDEX_TO_NOTIFY,
        ulValue,
        eAction,
        ptr::null_mut(),
    )
}

/// Send a notification to a specific notification-array index.
///
/// # Safety
///
/// The handle must identify a live task, the index and action must be valid,
/// and the call must occur from task context with initialized scheduler state.
pub unsafe fn xTaskNotifyIndexed(
    xTaskToNotify: TaskHandle_t,
    uxIndexToNotify: UBaseType_t,
    ulValue: u32,
    eAction: i32,
) -> BaseType_t {
    xTaskGenericNotify(
        xTaskToNotify,
        uxIndexToNotify,
        ulValue,
        eAction,
        ptr::null_mut(),
    )
}

/// Wait for a notification.
///
/// # Arguments
///
/// * `ulBitsToClearOnEntry` - Bits to clear on entry
/// * `ulBitsToClearOnExit` - Bits to clear on exit
/// * `pulNotificationValue` - Optional output for notification value
/// * `xTicksToWait` - Timeout in ticks
///
/// # Returns
///
/// pdPASS if notification received, pdFAIL on timeout
///
/// # Safety
///
/// Call only for the live current task outside an ISR and with the scheduler
/// unsuspended. A non-null output must be uniquely writable for one `u32`.
pub unsafe fn xTaskNotifyWait(
    ulBitsToClearOnEntry: u32,
    ulBitsToClearOnExit: u32,
    pulNotificationValue: *mut u32,
    xTicksToWait: TickType_t,
) -> BaseType_t {
    xTaskGenericNotifyWait(
        tskDEFAULT_INDEX_TO_NOTIFY,
        ulBitsToClearOnEntry,
        ulBitsToClearOnExit,
        pulNotificationValue,
        xTicksToWait,
    )
}

/// Wait for a notification at a specific notification-array index.
///
/// # Safety
///
/// The index must be valid. Call only for the live current task outside an ISR
/// with an unsuspended scheduler; a non-null output must be uniquely writable.
pub unsafe fn xTaskNotifyWaitIndexed(
    uxIndexToWait: UBaseType_t,
    ulBitsToClearOnEntry: u32,
    ulBitsToClearOnExit: u32,
    pulNotificationValue: *mut u32,
    xTicksToWait: TickType_t,
) -> BaseType_t {
    xTaskGenericNotifyWait(
        uxIndexToWait,
        ulBitsToClearOnEntry,
        ulBitsToClearOnExit,
        pulNotificationValue,
        xTicksToWait,
    )
}

/// Send a notification from an ISR.
///
/// # Arguments
///
/// * `xTaskToNotify` - Handle of the task to notify
/// * `ulValue` - Notification value
/// * `eAction` - Action to perform (as i32)
/// * `pxHigherPriorityTaskWoken` - Set to pdTRUE if a context switch is needed
///
/// # Returns
///
/// pdPASS on success
///
/// # Safety
///
/// The handle must identify a live task and the ISR must be permitted to call
/// kernel APIs. A non-null wake pointer must be uniquely writable.
pub unsafe fn xTaskNotifyFromISR(
    xTaskToNotify: TaskHandle_t,
    ulValue: u32,
    eAction: i32,
    pxHigherPriorityTaskWoken: *mut BaseType_t,
) -> BaseType_t {
    xTaskGenericNotifyFromISR(
        xTaskToNotify,
        tskDEFAULT_INDEX_TO_NOTIFY,
        ulValue,
        eAction,
        ptr::null_mut(),
        pxHigherPriorityTaskWoken,
    )
}

/// Send a notification to a specific index from interrupt context.
///
/// # Safety
///
/// The handle and index must be valid, the ISR must be permitted to call kernel
/// APIs, and a non-null wake pointer must be uniquely writable.
pub unsafe fn xTaskNotifyIndexedFromISR(
    xTaskToNotify: TaskHandle_t,
    uxIndexToNotify: UBaseType_t,
    ulValue: u32,
    eAction: i32,
    pxHigherPriorityTaskWoken: *mut BaseType_t,
) -> BaseType_t {
    xTaskGenericNotifyFromISR(
        xTaskToNotify,
        uxIndexToNotify,
        ulValue,
        eAction,
        ptr::null_mut(),
        pxHigherPriorityTaskWoken,
    )
}

/// Increment a task notification value, using it as a lightweight semaphore.
///
/// # Safety
///
/// The handle must identify a live task, the index must be valid, and this must
/// be called from task context with initialized scheduler state.
pub unsafe fn xTaskNotifyGiveIndexed(
    xTaskToNotify: TaskHandle_t,
    uxIndexToNotify: UBaseType_t,
) -> BaseType_t {
    xTaskGenericNotify(
        xTaskToNotify,
        uxIndexToNotify,
        0,
        eNotifyAction::eIncrement as i32,
        ptr::null_mut(),
    )
}

/// Increment the default notification value as a lightweight semaphore.
///
/// # Safety
///
/// The handle must identify a live task and this must be called from task
/// context with initialized scheduler state.
pub unsafe fn xTaskNotifyGive(xTaskToNotify: TaskHandle_t) -> BaseType_t {
    xTaskNotifyGiveIndexed(xTaskToNotify, tskDEFAULT_INDEX_TO_NOTIFY)
}

/// Increment a task notification value from interrupt context.
///
/// # Safety
///
/// The handle/index must be valid and the ISR must be permitted to call kernel
/// APIs. A non-null wake pointer must be uniquely writable.
pub unsafe fn vTaskNotifyGiveIndexedFromISR(
    xTaskToNotify: TaskHandle_t,
    uxIndexToNotify: UBaseType_t,
    pxHigherPriorityTaskWoken: *mut BaseType_t,
) {
    let _ = xTaskGenericNotifyFromISR(
        xTaskToNotify,
        uxIndexToNotify,
        0,
        eNotifyAction::eIncrement as i32,
        ptr::null_mut(),
        pxHigherPriorityTaskWoken,
    );
}

/// Increment the default notification value from interrupt context.
///
/// # Safety
///
/// The handle must identify a live task and the ISR must be permitted to call
/// kernel APIs. A non-null wake pointer must be uniquely writable.
pub unsafe fn vTaskNotifyGiveFromISR(
    xTaskToNotify: TaskHandle_t,
    pxHigherPriorityTaskWoken: *mut BaseType_t,
) {
    vTaskNotifyGiveIndexedFromISR(
        xTaskToNotify,
        tskDEFAULT_INDEX_TO_NOTIFY,
        pxHigherPriorityTaskWoken,
    );
}

/// Generic task notification function.
///
/// Sends a notification to a task, optionally modifying the notification value.
///
/// # Safety
///
/// The handle/index/action must be valid and the task must remain live. Call
/// from task context; a non-null previous-value pointer must be uniquely writable.
pub unsafe fn xTaskGenericNotify(
    xTaskToNotify: TaskHandle_t,
    uxIndexToNotify: UBaseType_t,
    ulValue: u32,
    eAction: i32,
    pulPreviousNotificationValue: *mut u32,
) -> BaseType_t {
    configASSERT(!xTaskToNotify.is_null());
    configASSERT((uxIndexToNotify as usize) < configTASK_NOTIFICATION_ARRAY_ENTRIES);
    configASSERT(
        eAction >= eNotifyAction::eNoAction as i32
            && eAction <= eNotifyAction::eSetValueWithoutOverwrite as i32,
    );

    let pxTCB = xTaskToNotify as *mut TCB_t;
    let uxIndex = uxIndexToNotify as usize;
    let mut xReturn: BaseType_t = pdPASS;
    let ucOriginalNotifyState: u8;

    taskENTER_CRITICAL();
    {
        if !pulPreviousNotificationValue.is_null() {
            *pulPreviousNotificationValue = prvReadNotifiedValue(pxTCB, uxIndex);
        }

        ucOriginalNotifyState = prvReadNotifyState(pxTCB, uxIndex);
        prvWriteNotifyState(pxTCB, uxIndex, taskNOTIFICATION_RECEIVED);

        match eAction {
            x if x == eNotifyAction::eSetBits as i32 => {
                prvWriteNotifiedValue(
                    pxTCB,
                    uxIndex,
                    prvReadNotifiedValue(pxTCB, uxIndex) | ulValue,
                );
            }
            x if x == eNotifyAction::eIncrement as i32 => {
                prvWriteNotifiedValue(
                    pxTCB,
                    uxIndex,
                    prvReadNotifiedValue(pxTCB, uxIndex).wrapping_add(1),
                );
            }
            x if x == eNotifyAction::eSetValueWithOverwrite as i32 => {
                prvWriteNotifiedValue(pxTCB, uxIndex, ulValue);
            }
            x if x == eNotifyAction::eSetValueWithoutOverwrite as i32 => {
                if ucOriginalNotifyState != taskNOTIFICATION_RECEIVED {
                    prvWriteNotifiedValue(pxTCB, uxIndex, ulValue);
                } else {
                    xReturn = pdFAIL;
                }
            }
            x if x == eNotifyAction::eNoAction as i32 => {
                // eNoAction - just wake the task
            }
            _ => configASSERT(false),
        }

        // If the task was waiting for a notification, unblock it.
        if ucOriginalNotifyState == taskWAITING_NOTIFICATION {
            // Remove from delayed list and add to ready list.
            let _ = uxListRemove(ptr::addr_of_mut!((*pxTCB).xStateListItem));
            prvAddTaskToReadyList(pxTCB);
            configASSERT(listLIST_ITEM_CONTAINER(ptr::addr_of!((*pxTCB).xEventListItem)).is_null());

            #[cfg(feature = "tickless-idle")]
            prvResetNextTaskUnblockTime();

            // If the notified task has higher priority, yield.
            if (*pxTCB).uxPriority > (*pxCurrentTCB).uxPriority {
                portYIELD_WITHIN_API();
            }
        }
    }
    taskEXIT_CRITICAL();

    xReturn
}

/// Generic task notification wait function.
///
/// Waits for a notification on the current task.
///
/// # Safety
///
/// The index must be valid. Call only for the live current task outside an ISR
/// with an unsuspended scheduler; a non-null output must be uniquely writable.
pub unsafe fn xTaskGenericNotifyWait(
    uxIndexToWait: UBaseType_t,
    ulBitsToClearOnEntry: u32,
    ulBitsToClearOnExit: u32,
    pulNotificationValue: *mut u32,
    xTicksToWait: TickType_t,
) -> BaseType_t {
    configASSERT((uxIndexToWait as usize) < configTASK_NOTIFICATION_ARRAY_ENTRIES);

    let uxIndex = uxIndexToWait as usize;
    let xReturn: BaseType_t;
    let mut xShouldBlock: BaseType_t = pdFALSE;

    // Check if notification is already pending.
    if prvReadNotifyState(pxCurrentTCB, uxIndex) != taskNOTIFICATION_RECEIVED && xTicksToWait > 0 {
        vTaskSuspendAll();
        {
            taskENTER_CRITICAL();
            {
                if prvReadNotifyState(pxCurrentTCB, uxIndex) != taskNOTIFICATION_RECEIVED {
                    // Clear bits on entry.
                    prvWriteNotifiedValue(
                        pxCurrentTCB,
                        uxIndex,
                        prvReadNotifiedValue(pxCurrentTCB, uxIndex) & !ulBitsToClearOnEntry,
                    );

                    // Mark as waiting for notification.
                    prvWriteNotifyState(pxCurrentTCB, uxIndex, taskWAITING_NOTIFICATION);
                    xShouldBlock = pdTRUE;
                }
            }
            taskEXIT_CRITICAL();

            if xShouldBlock == pdTRUE {
                prvAddCurrentTaskToDelayedList(xTicksToWait, pdTRUE);
            }
        }
        let xAlreadyYielded = xTaskResumeAll();

        // Force a reschedule if xTaskResumeAll has not already done so.
        if xShouldBlock == pdTRUE && xAlreadyYielded == pdFALSE {
            portYIELD_WITHIN_API();
        }
    }

    // Check notification state after potential blocking.
    taskENTER_CRITICAL();
    {
        if !pulNotificationValue.is_null() {
            *pulNotificationValue = prvReadNotifiedValue(pxCurrentTCB, uxIndex);
        }

        if prvReadNotifyState(pxCurrentTCB, uxIndex) != taskNOTIFICATION_RECEIVED {
            xReturn = pdFALSE;
        } else {
            // Clear bits on exit.
            prvWriteNotifiedValue(
                pxCurrentTCB,
                uxIndex,
                prvReadNotifiedValue(pxCurrentTCB, uxIndex) & !ulBitsToClearOnExit,
            );
            xReturn = pdTRUE;
        }

        prvWriteNotifyState(pxCurrentTCB, uxIndex, taskNOT_WAITING_NOTIFICATION);
    }
    taskEXIT_CRITICAL();

    xReturn
}

/// Generic task notification from ISR.
///
/// Sends a notification to a task from an interrupt service routine.
///
/// # Safety
///
/// The target/index/action must be valid and the ISR must be permitted to call
/// kernel APIs. Non-null output pointers must be uniquely writable.
pub unsafe fn xTaskGenericNotifyFromISR(
    xTaskToNotify: TaskHandle_t,
    uxIndexToNotify: UBaseType_t,
    ulValue: u32,
    eAction: i32,
    pulPreviousNotificationValue: *mut u32,
    pxHigherPriorityTaskWoken: *mut BaseType_t,
) -> BaseType_t {
    configASSERT(!xTaskToNotify.is_null());
    configASSERT((uxIndexToNotify as usize) < configTASK_NOTIFICATION_ARRAY_ENTRIES);
    configASSERT(
        eAction >= eNotifyAction::eNoAction as i32
            && eAction <= eNotifyAction::eSetValueWithoutOverwrite as i32,
    );

    let pxTCB = xTaskToNotify as *mut TCB_t;
    let uxIndex = uxIndexToNotify as usize;
    let mut xReturn: BaseType_t = pdPASS;
    let ucOriginalNotifyState: u8;

    let uxSavedInterruptStatus = portSET_INTERRUPT_MASK_FROM_ISR();
    {
        if !pulPreviousNotificationValue.is_null() {
            *pulPreviousNotificationValue = prvReadNotifiedValue(pxTCB, uxIndex);
        }

        ucOriginalNotifyState = prvReadNotifyState(pxTCB, uxIndex);
        prvWriteNotifyState(pxTCB, uxIndex, taskNOTIFICATION_RECEIVED);

        match eAction {
            x if x == eNotifyAction::eSetBits as i32 => {
                prvWriteNotifiedValue(
                    pxTCB,
                    uxIndex,
                    prvReadNotifiedValue(pxTCB, uxIndex) | ulValue,
                );
            }
            x if x == eNotifyAction::eIncrement as i32 => {
                prvWriteNotifiedValue(
                    pxTCB,
                    uxIndex,
                    prvReadNotifiedValue(pxTCB, uxIndex).wrapping_add(1),
                );
            }
            x if x == eNotifyAction::eSetValueWithOverwrite as i32 => {
                prvWriteNotifiedValue(pxTCB, uxIndex, ulValue);
            }
            x if x == eNotifyAction::eSetValueWithoutOverwrite as i32 => {
                if ucOriginalNotifyState != taskNOTIFICATION_RECEIVED {
                    prvWriteNotifiedValue(pxTCB, uxIndex, ulValue);
                } else {
                    xReturn = pdFAIL;
                }
            }
            x if x == eNotifyAction::eNoAction as i32 => {
                // eNoAction
            }
            _ => configASSERT(false),
        }

        // If the task was waiting for a notification, unblock it.
        if ucOriginalNotifyState == taskWAITING_NOTIFICATION {
            configASSERT(listLIST_ITEM_CONTAINER(ptr::addr_of!((*pxTCB).xEventListItem)).is_null());

            // If the scheduler is suspended, add to pending ready list.
            if uxSchedulerSuspended != 0 {
                vListInsertEnd(
                    ptr::addr_of_mut!(xPendingReadyList),
                    ptr::addr_of_mut!((*pxTCB).xEventListItem),
                );
            } else {
                let _ = uxListRemove(ptr::addr_of_mut!((*pxTCB).xStateListItem));
                prvAddTaskToReadyList(pxTCB);

                #[cfg(feature = "tickless-idle")]
                prvResetNextTaskUnblockTime();
            }

            if (*pxTCB).uxPriority > (*pxCurrentTCB).uxPriority {
                if !pxHigherPriorityTaskWoken.is_null() {
                    *pxHigherPriorityTaskWoken = pdTRUE;
                }
                xYieldPendings[0] = pdTRUE;
            }
        }
    }
    portCLEAR_INTERRUPT_MASK_FROM_ISR(uxSavedInterruptStatus);

    xReturn
}

/// Take a task notification as a counting or binary semaphore.
///
/// # Safety
///
/// The index must be valid. Call only for the live current task outside an ISR
/// with initialized, unsuspended scheduler state.
pub unsafe fn ulTaskNotifyTakeIndexed(
    uxIndexToWait: UBaseType_t,
    xClearCountOnExit: BaseType_t,
    xTicksToWait: TickType_t,
) -> u32 {
    configASSERT((uxIndexToWait as usize) < configTASK_NOTIFICATION_ARRAY_ENTRIES);

    let uxIndex = uxIndexToWait as usize;
    let mut xShouldBlock = pdFALSE;

    if prvReadNotifiedValue(pxCurrentTCB, uxIndex) == 0 && xTicksToWait > 0 {
        vTaskSuspendAll();
        {
            taskENTER_CRITICAL();
            {
                if prvReadNotifiedValue(pxCurrentTCB, uxIndex) == 0 {
                    prvWriteNotifyState(pxCurrentTCB, uxIndex, taskWAITING_NOTIFICATION);
                    xShouldBlock = pdTRUE;
                }
            }
            taskEXIT_CRITICAL();

            if xShouldBlock == pdTRUE {
                prvAddCurrentTaskToDelayedList(xTicksToWait, pdTRUE);
            }
        }
        let xAlreadyYielded = xTaskResumeAll();
        if xShouldBlock == pdTRUE && xAlreadyYielded == pdFALSE {
            portYIELD_WITHIN_API();
        }
    }

    let ulReturn: u32;
    taskENTER_CRITICAL();
    {
        ulReturn = prvReadNotifiedValue(pxCurrentTCB, uxIndex);
        if ulReturn != 0 {
            if xClearCountOnExit != pdFALSE {
                prvWriteNotifiedValue(pxCurrentTCB, uxIndex, 0);
            } else {
                prvWriteNotifiedValue(pxCurrentTCB, uxIndex, ulReturn - 1);
            }
        }
        prvWriteNotifyState(pxCurrentTCB, uxIndex, taskNOT_WAITING_NOTIFICATION);
    }
    taskEXIT_CRITICAL();

    ulReturn
}

/// Take the default task notification as a counting or binary semaphore.
///
/// # Safety
///
/// Call only for the live current task outside an ISR with initialized,
/// unsuspended scheduler state.
pub unsafe fn ulTaskNotifyTake(xClearCountOnExit: BaseType_t, xTicksToWait: TickType_t) -> u32 {
    ulTaskNotifyTakeIndexed(tskDEFAULT_INDEX_TO_NOTIFY, xClearCountOnExit, xTicksToWait)
}

/// Clear the pending state of a task notification at a specific index.
///
/// # Safety
///
/// A non-null handle must identify a live task; null selects the current task.
/// The index must be valid and the call must occur from task context.
pub unsafe fn xTaskNotifyStateClearIndexed(
    xTask: TaskHandle_t,
    uxIndexToClear: UBaseType_t,
) -> BaseType_t {
    configASSERT((uxIndexToClear as usize) < configTASK_NOTIFICATION_ARRAY_ENTRIES);
    let pxTCB = prvGetTCBFromHandle(xTask);
    configASSERT(!pxTCB.is_null());

    let xReturn: BaseType_t;
    taskENTER_CRITICAL();
    {
        if prvReadNotifyState(pxTCB, uxIndexToClear as usize) == taskNOTIFICATION_RECEIVED {
            prvWriteNotifyState(pxTCB, uxIndexToClear as usize, taskNOT_WAITING_NOTIFICATION);
            xReturn = pdPASS;
        } else {
            xReturn = pdFAIL;
        }
    }
    taskEXIT_CRITICAL();
    xReturn
}

/// Clear the pending state of the default task notification.
///
/// # Safety
///
/// A non-null handle must identify a live task; null selects the current task.
/// Call from task context while that task cannot be reclaimed.
pub unsafe fn xTaskNotifyStateClear(xTask: TaskHandle_t) -> BaseType_t {
    xTaskNotifyStateClearIndexed(xTask, tskDEFAULT_INDEX_TO_NOTIFY)
}

/// Clear selected bits in a task notification value and return its old value.
///
/// # Safety
///
/// A non-null handle must identify a live task; null selects the current task.
/// The index must be valid and the call must occur from task context.
pub unsafe fn ulTaskNotifyValueClearIndexed(
    xTask: TaskHandle_t,
    uxIndexToClear: UBaseType_t,
    ulBitsToClear: u32,
) -> u32 {
    configASSERT((uxIndexToClear as usize) < configTASK_NOTIFICATION_ARRAY_ENTRIES);
    let pxTCB = prvGetTCBFromHandle(xTask);
    configASSERT(!pxTCB.is_null());

    let ulReturn: u32;
    taskENTER_CRITICAL();
    {
        ulReturn = prvReadNotifiedValue(pxTCB, uxIndexToClear as usize);
        prvWriteNotifiedValue(pxTCB, uxIndexToClear as usize, ulReturn & !ulBitsToClear);
    }
    taskEXIT_CRITICAL();
    ulReturn
}

/// Clear selected bits in the default task notification value.
///
/// # Safety
///
/// A non-null handle must identify a live task; null selects the current task.
/// Call from task context while that task cannot be reclaimed.
pub unsafe fn ulTaskNotifyValueClear(xTask: TaskHandle_t, ulBitsToClear: u32) -> u32 {
    ulTaskNotifyValueClearIndexed(xTask, tskDEFAULT_INDEX_TO_NOTIFY, ulBitsToClear)
}

// =============================================================================
// Thread Local Storage (configNUM_THREAD_LOCAL_STORAGE_POINTERS)
// =============================================================================

/// Set a thread local storage pointer for a task.
///
/// Thread local storage allows storing task-specific data without global variables.
/// Each task has configNUM_THREAD_LOCAL_STORAGE_POINTERS slots available.
///
/// # Arguments
///
/// * `xTaskToSet` - Handle of the task, or NULL for the calling task
/// * `xIndex` - Index into the TLS array (0 to configNUM_THREAD_LOCAL_STORAGE_POINTERS-1)
/// * `pvValue` - Value to store
///
/// # Safety
///
/// A non-null handle must identify a live, mutable task; null selects the
/// current task. The stored pointer's lifetime is managed by the caller.
#[cfg(feature = "thread-local-storage")]
pub unsafe fn vTaskSetThreadLocalStoragePointer(
    xTaskToSet: TaskHandle_t,
    xIndex: BaseType_t,
    pvValue: *mut c_void,
) {
    if xIndex >= 0 && (xIndex as usize) < configNUM_THREAD_LOCAL_STORAGE_POINTERS {
        let pxTCB = prvGetTCBFromHandle(xTaskToSet);
        configASSERT(!pxTCB.is_null());
        (*pxTCB).pvThreadLocalStoragePointers[xIndex as usize] = pvValue;
    }
}

/// Get a thread local storage pointer from a task.
///
/// # Arguments
///
/// * `xTaskToQuery` - Handle of the task, or NULL for the calling task
/// * `xIndex` - Index into the TLS array (0 to configNUM_THREAD_LOCAL_STORAGE_POINTERS-1)
///
/// # Returns
///
/// The stored pointer value, or NULL if index is out of range.
///
/// # Safety
///
/// A non-null handle must identify a live task; null selects the current task.
/// The returned pointer retains the lifetime and aliasing contract set by its owner.
#[cfg(feature = "thread-local-storage")]
pub unsafe fn pvTaskGetThreadLocalStoragePointer(
    xTaskToQuery: TaskHandle_t,
    xIndex: BaseType_t,
) -> *mut c_void {
    let mut pvReturn: *mut c_void = ptr::null_mut();

    if xIndex >= 0 && (xIndex as usize) < configNUM_THREAD_LOCAL_STORAGE_POINTERS {
        let pxTCB = prvGetTCBFromHandle(xTaskToQuery);
        configASSERT(!pxTCB.is_null());
        pvReturn = (*pxTCB).pvThreadLocalStoragePointers[xIndex as usize];
    }

    pvReturn
}

// =============================================================================
// Application Task Tag (configUSE_APPLICATION_TASK_TAG)
// =============================================================================

/// Task hook function type for application task tags.
/// Returns a BaseType_t and takes a single void pointer parameter.
#[cfg(feature = "application-task-tag")]
pub type TaskHookFunction_t = Option<extern "C" fn(*mut c_void) -> BaseType_t>;

/// Set the application task tag for a task.
///
/// Task tags allow storing an application-defined hook function pointer
/// in each task's TCB. This can be used for tracing, debugging, or
/// implementing custom task-specific behavior.
///
/// # Arguments
///
/// * `xTask` - Handle of the task, or NULL for the calling task
/// * `pxHookFunction` - Function pointer to store, or None to clear
///
/// # Safety
///
/// A non-null handle must identify a live task; null selects the current task.
/// The callback must remain callable for as long as it remains installed.
#[cfg(feature = "application-task-tag")]
pub unsafe fn vTaskSetApplicationTaskTag(xTask: TaskHandle_t, pxHookFunction: TaskHookFunction_t) {
    let pxTCB: *mut TCB_t;

    // If xTask is null, use the current task.
    if xTask.is_null() {
        pxTCB = pxCurrentTCB;
    } else {
        pxTCB = xTask as *mut TCB_t;
    }

    taskENTER_CRITICAL();
    (*pxTCB).pxTaskTag = pxHookFunction;
    taskEXIT_CRITICAL();
}

/// Get the application task tag from a task.
///
/// # Arguments
///
/// * `xTask` - Handle of the task, or NULL for the calling task
///
/// # Returns
///
/// The stored hook function pointer, or None if not set.
///
/// # Safety
///
/// A non-null handle must identify a live task; null selects the current task.
/// Call from task context while that TCB cannot be reclaimed.
#[cfg(feature = "application-task-tag")]
pub unsafe fn xTaskGetApplicationTaskTag(xTask: TaskHandle_t) -> TaskHookFunction_t {
    let pxTCB: *mut TCB_t = prvGetTCBFromHandle(xTask);
    let xReturn: TaskHookFunction_t;
    configASSERT(!pxTCB.is_null());

    taskENTER_CRITICAL();
    xReturn = (*pxTCB).pxTaskTag;
    taskEXIT_CRITICAL();

    xReturn
}

/// Get the application task tag from a task (ISR-safe version).
///
/// # Arguments
///
/// * `xTask` - Handle of the task, or NULL for the calling task
///
/// # Returns
///
/// The stored hook function pointer, or None if not set.
///
/// # Safety
///
/// A non-null handle must identify a live task; null selects the interrupted
/// task. The ISR must be permitted to call kernel APIs.
#[cfg(feature = "application-task-tag")]
pub unsafe fn xTaskGetApplicationTaskTagFromISR(xTask: TaskHandle_t) -> TaskHookFunction_t {
    let pxTCB: *mut TCB_t = prvGetTCBFromHandle(xTask);
    let xReturn: TaskHookFunction_t;
    configASSERT(!pxTCB.is_null());

    let uxSavedInterruptStatus = portSET_INTERRUPT_MASK_FROM_ISR();
    xReturn = (*pxTCB).pxTaskTag;
    portCLEAR_INTERRUPT_MASK_FROM_ISR(uxSavedInterruptStatus);

    xReturn
}

/// Call the application task hook function.
///
/// Calls the hook function stored in the task's tag with the provided parameter.
///
/// # Arguments
///
/// * `xTask` - Handle of the task whose hook to call, or NULL for the calling task
/// * `pvParameter` - Parameter to pass to the hook function
///
/// # Returns
///
/// The return value from the hook function, or pdFAIL if no hook is set.
///
/// # Safety
///
/// A non-null handle must identify a live task; null selects the current task.
/// `pvParameter` must satisfy the installed callback's contract for the call.
#[cfg(feature = "application-task-tag")]
pub unsafe fn xTaskCallApplicationTaskHook(
    xTask: TaskHandle_t,
    pvParameter: *mut c_void,
) -> BaseType_t {
    let pxTCB: *mut TCB_t;
    let xReturn: BaseType_t;

    // If xTask is null, use the current task.
    if xTask.is_null() {
        pxTCB = pxCurrentTCB;
    } else {
        pxTCB = xTask as *mut TCB_t;
    }

    if let Some(hook) = (*pxTCB).pxTaskTag {
        xReturn = hook(pvParameter);
    } else {
        xReturn = pdFAIL;
    }

    xReturn
}

// =============================================================================
// Stack High Water Mark (INCLUDE_uxTaskGetStackHighWaterMark)
// =============================================================================

/// Check how much free stack space remains by scanning for the fill byte.
///
/// Unlike the C helper, this scan is bounded by the allocation extent retained
/// in the TCB. That matters for ports such as `port-test` whose stack setup can
/// legitimately leave every byte equal to the fill value.
///
/// # Safety
///
/// `pxStack` must reference `uxStackDepth` initialized stack words.
#[cfg(any(feature = "trace-facility", feature = "stack-high-water-mark"))]
unsafe fn prvTaskCheckFreeStackSpace(
    pxStack: *const StackType_t,
    uxStackDepth: configSTACK_DEPTH_TYPE,
) -> configSTACK_DEPTH_TYPE {
    if pxStack.is_null() || uxStackDepth == 0 {
        return 0;
    }

    let Some(xStackBytes) = uxStackDepth.checked_mul(core::mem::size_of::<StackType_t>()) else {
        return 0;
    };

    let pucStackBase = pxStack.cast::<u8>();
    let mut uxCount = 0usize;

    if portSTACK_GROWTH < 0 {
        while uxCount < xStackBytes && *pucStackBase.add(uxCount) == tskSTACK_FILL_BYTE {
            uxCount += 1;
        }
    } else {
        while uxCount < xStackBytes
            && *pucStackBase.add(xStackBytes - 1 - uxCount) == tskSTACK_FILL_BYTE
        {
            uxCount += 1;
        }
    }

    uxCount / core::mem::size_of::<StackType_t>()
}

/// FreeRTOS `taskCHECK_FOR_STACK_OVERFLOW()` translated as a bounded function.
#[cfg(feature = "stack-overflow-check")]
#[inline(always)]
unsafe fn taskCHECK_FOR_STACK_OVERFLOW() {
    let pxTCB = pxCurrentTCB;
    if pxTCB.is_null() {
        return;
    }

    let pxStack = (*pxTCB).pxStack;
    let uxStackDepth = (*pxTCB).uxStackDepth;
    let Some(xStackBytes) = uxStackDepth.checked_mul(core::mem::size_of::<StackType_t>()) else {
        prvCallApplicationStackOverflowHook(pxTCB);
        return;
    };

    if pxStack.is_null() || xStackBytes == 0 || (*pxTCB).pxTopOfStack.is_null() {
        prvCallApplicationStackOverflowHook(pxTCB);
        return;
    }

    let uxBase = pxStack as usize;
    let Some(uxEndExclusive) = uxBase.checked_add(xStackBytes) else {
        prvCallApplicationStackOverflowHook(pxTCB);
        return;
    };
    let uxTop = (*pxTCB).pxTopOfStack as usize;

    let mut xOverflowed = if portSTACK_GROWTH < 0 {
        uxTop <= uxBase
    } else {
        uxTop >= uxEndExclusive.saturating_sub(core::mem::size_of::<StackType_t>())
    };

    if configCHECK_FOR_STACK_OVERFLOW > 1 {
        let pucStack = pxStack.cast::<u8>();
        let uxGuardBytes = if portSTACK_GROWTH < 0 { 16 } else { 20 }.min(xStackBytes);
        let mut uxIndex = 0usize;

        while uxIndex < uxGuardBytes {
            let uxOffset = if portSTACK_GROWTH < 0 {
                uxIndex
            } else {
                xStackBytes - 1 - uxIndex
            };
            if *pucStack.add(uxOffset) != tskSTACK_FILL_BYTE {
                xOverflowed = true;
                break;
            }
            uxIndex += 1;
        }
    }

    if xOverflowed {
        prvCallApplicationStackOverflowHook(pxTCB);
    }
}

/// Get the high water mark for a task's stack.
///
/// Returns the minimum amount of remaining stack space that was available
/// to the task since the task started executing. This is the amount of
/// stack that remained unused when the task stack was at its greatest
/// (deepest) value.
///
/// The value is in words (StackType_t units), not bytes.
///
/// # Arguments
///
/// * `xTask` - Handle of the task, or NULL for the calling task
///
/// # Returns
///
/// The minimum free stack space (in words) since the task started.
///
/// # Safety
///
/// A non-null handle must identify a live task whose entire recorded stack
/// allocation remains readable; null selects the current task.
#[cfg(feature = "stack-high-water-mark")]
pub unsafe fn uxTaskGetStackHighWaterMark(xTask: TaskHandle_t) -> UBaseType_t {
    let pxTCB = prvGetTCBFromHandle(xTask);
    configASSERT(!pxTCB.is_null());
    if pxTCB.is_null() {
        return 0;
    }

    let uxFree = prvTaskCheckFreeStackSpace((*pxTCB).pxStack, (*pxTCB).uxStackDepth);
    uxFree.min(UBaseType_t::MAX as usize) as UBaseType_t
}

/// Get the high water mark for a task's stack (returning configSTACK_DEPTH_TYPE).
///
/// Same as uxTaskGetStackHighWaterMark but returns configSTACK_DEPTH_TYPE
/// instead of UBaseType_t, for compatibility with configSTACK_DEPTH_TYPE
/// configurations that use larger types.
///
/// # Arguments
///
/// * `xTask` - Handle of the task, or NULL for the calling task
///
/// # Returns
///
/// The minimum free stack space (in words) since the task started.
///
/// # Safety
///
/// A non-null handle must identify a live task whose entire recorded stack
/// allocation remains readable; null selects the current task.
#[cfg(feature = "stack-high-water-mark")]
pub unsafe fn uxTaskGetStackHighWaterMark2(xTask: TaskHandle_t) -> configSTACK_DEPTH_TYPE {
    let pxTCB = prvGetTCBFromHandle(xTask);
    configASSERT(!pxTCB.is_null());
    if pxTCB.is_null() {
        return 0;
    }

    prvTaskCheckFreeStackSpace((*pxTCB).pxStack, (*pxTCB).uxStackDepth)
}

// =============================================================================
// Task Priority Get/Set (INCLUDE_vTaskPrioritySet, INCLUDE_uxTaskPriorityGet)
// =============================================================================

/// Get the priority of a task.
///
/// # Arguments
///
/// * `xTask` - Handle of the task, or NULL for the calling task
///
/// # Returns
///
/// The priority of the task.
///
/// # Safety
///
/// A non-null handle must identify a live task; null selects the current task.
/// Call from task context while the TCB cannot be reclaimed.
#[cfg(feature = "task-priority-set")]
pub unsafe fn uxTaskPriorityGet(xTask: TaskHandle_t) -> UBaseType_t {
    let uxReturn: UBaseType_t;

    taskENTER_CRITICAL();
    {
        let pxTCB = prvGetTCBFromHandle(xTask);
        configASSERT(!pxTCB.is_null());
        uxReturn = (*pxTCB).uxPriority;
    }
    taskEXIT_CRITICAL();

    uxReturn
}

/// Get the priority of a task (ISR-safe version).
///
/// # Arguments
///
/// * `xTask` - Handle of the task, or NULL for the calling task
///
/// # Returns
///
/// The priority of the task.
///
/// # Safety
///
/// A non-null handle must identify a live task; null selects the interrupted
/// task. The ISR must be permitted to call kernel APIs.
#[cfg(feature = "task-priority-set")]
pub unsafe fn uxTaskPriorityGetFromISR(xTask: TaskHandle_t) -> UBaseType_t {
    let pxTCB = prvGetTCBFromHandle(xTask);
    configASSERT(!pxTCB.is_null());
    if pxTCB.is_null() {
        return 0;
    }
    let uxSavedInterruptStatus = portSET_INTERRUPT_MASK_FROM_ISR();
    let uxReturn = (*pxTCB).uxPriority;
    portCLEAR_INTERRUPT_MASK_FROM_ISR(uxSavedInterruptStatus);
    uxReturn
}

/// Get the priority last assigned to a task, before mutex inheritance.
///
/// # Safety
///
/// A non-null handle must identify a live task; null selects the current task.
/// Call from task context while the TCB cannot be reclaimed.
#[cfg(all(feature = "task-priority-set", feature = "use-mutexes"))]
pub unsafe fn uxTaskBasePriorityGet(xTask: TaskHandle_t) -> UBaseType_t {
    let uxReturn: UBaseType_t;

    taskENTER_CRITICAL();
    {
        let pxTCB = prvGetTCBFromHandle(xTask);
        configASSERT(!pxTCB.is_null());
        uxReturn = (*pxTCB).uxBasePriority;
    }
    taskEXIT_CRITICAL();

    uxReturn
}

/// Get the pre-inheritance base priority from interrupt context.
///
/// # Safety
///
/// A non-null handle must identify a live task; null selects the interrupted
/// task. The ISR must be permitted to call kernel APIs.
#[cfg(all(feature = "task-priority-set", feature = "use-mutexes"))]
pub unsafe fn uxTaskBasePriorityGetFromISR(xTask: TaskHandle_t) -> UBaseType_t {
    let uxSavedInterruptStatus = portSET_INTERRUPT_MASK_FROM_ISR();
    let pxTCB = prvGetTCBFromHandle(xTask);
    configASSERT(!pxTCB.is_null());
    let uxReturn = (*pxTCB).uxBasePriority;
    portCLEAR_INTERRUPT_MASK_FROM_ISR(uxSavedInterruptStatus);
    uxReturn
}

/// Set the priority of a task.
///
/// A context switch will occur if the priority being set is higher than the
/// currently executing task and preemption is enabled.
///
/// # Arguments
///
/// * `xTask` - Handle of the task, or NULL for the calling task
/// * `uxNewPriority` - The new priority (0 to configMAX_PRIORITIES-1)
///
/// # Safety
///
/// A non-null handle must identify a live, mutable task; null selects the
/// current task. Call from task context with initialized scheduler lists.
#[cfg(feature = "task-priority-set")]
pub unsafe fn vTaskPrioritySet(xTask: TaskHandle_t, uxNewPriority: UBaseType_t) {
    let pxTCB: *mut TCB_t;
    let uxCurrentBasePriority: UBaseType_t;
    let uxPriorityUsedOnEntry: UBaseType_t;
    let mut xYieldRequired: BaseType_t = pdFALSE;

    configASSERT(uxNewPriority < configMAX_PRIORITIES);

    // Clamp priority to valid range after asserting, as upstream does.
    let uxNewPriority = if uxNewPriority >= configMAX_PRIORITIES {
        configMAX_PRIORITIES - 1
    } else {
        uxNewPriority
    };

    taskENTER_CRITICAL();
    {
        pxTCB = prvGetTCBFromHandle(xTask);
        configASSERT(!pxTCB.is_null());

        // Get current base priority (considering mutex inheritance).
        #[cfg(feature = "use-mutexes")]
        {
            uxCurrentBasePriority = (*pxTCB).uxBasePriority;
        }
        #[cfg(not(feature = "use-mutexes"))]
        {
            uxCurrentBasePriority = (*pxTCB).uxPriority;
        }

        if uxCurrentBasePriority != uxNewPriority {
            // Check if we need to yield after changing priority.
            if uxNewPriority > uxCurrentBasePriority {
                // Priority is being raised.
                if pxTCB != pxCurrentTCB {
                    // Another task's priority is being raised.
                    if uxNewPriority > (*pxCurrentTCB).uxPriority {
                        xYieldRequired = pdTRUE;
                    }
                }
                // If raising current task's priority, no yield needed.
            } else if pxTCB == pxCurrentTCB {
                // Lowering the running task's priority - may need to yield.
                xYieldRequired = pdTRUE;
            }

            // Remember old priority for list manipulation.
            uxPriorityUsedOnEntry = (*pxTCB).uxPriority;

            // Update priority.
            #[cfg(feature = "use-mutexes")]
            {
                // Only change effective priority if not using inherited priority
                // or if new priority is higher than inherited.
                if (*pxTCB).uxBasePriority == (*pxTCB).uxPriority
                    || uxNewPriority > (*pxTCB).uxPriority
                {
                    (*pxTCB).uxPriority = uxNewPriority;
                }
                // Base priority always gets updated.
                (*pxTCB).uxBasePriority = uxNewPriority;
            }
            #[cfg(not(feature = "use-mutexes"))]
            {
                (*pxTCB).uxPriority = uxNewPriority;
            }

            // Update event list item value if not in use for something else.
            let xItemValue = listGET_LIST_ITEM_VALUE(ptr::addr_of!((*pxTCB).xEventListItem));
            if (xItemValue & taskEVENT_LIST_ITEM_VALUE_IN_USE) == 0 {
                listSET_LIST_ITEM_VALUE(
                    ptr::addr_of_mut!((*pxTCB).xEventListItem),
                    (configMAX_PRIORITIES as TickType_t) - (uxNewPriority as TickType_t),
                );
            }

            // If task is in a ready list, move it to the correct one.
            if listIS_CONTAINED_WITHIN(
                ptr::addr_of!(pxReadyTasksLists[uxPriorityUsedOnEntry as usize]),
                ptr::addr_of!((*pxTCB).xStateListItem),
            ) != pdFALSE
            {
                // Remove from old ready list.
                if uxListRemove(ptr::addr_of_mut!((*pxTCB).xStateListItem)) == 0 {
                    // List is now empty, reset the priority bit.
                    taskRESET_READY_PRIORITY(uxPriorityUsedOnEntry);
                }
                // Add to new ready list.
                prvAddTaskToReadyList(pxTCB);
            }

            if xYieldRequired != pdFALSE {
                portYIELD_WITHIN_API();
            }
        }
    }
    taskEXIT_CRITICAL();
}

// =============================================================================
// eTaskGetState - Get the state of a task
// =============================================================================

/// Returns the state of the specified task.
///
/// This determines task state by checking which list contains the task's state item.
///
/// # Safety
/// - xTask must be a valid task handle or null (for current task is not applicable here)
/// - Must be called from a non-ISR context
pub unsafe fn eTaskGetState(xTask: TaskHandle_t) -> eTaskState {
    let pxTCB = xTask as *const TCB_t;
    configASSERT(!pxTCB.is_null());

    // If this is the current task, it must be running.
    if pxTCB == pxCurrentTCB {
        return eTaskState::eRunning;
    }

    // Get the list containers in a critical section.
    let pxStateList: *const List_t;
    let pxEventList: *const List_t;
    let pxDelayedList: *const List_t;
    let pxOverflowedDelayedList: *const List_t;

    taskENTER_CRITICAL();
    {
        pxStateList = listLIST_ITEM_CONTAINER(ptr::addr_of!((*pxTCB).xStateListItem));
        pxEventList = listLIST_ITEM_CONTAINER(ptr::addr_of!((*pxTCB).xEventListItem));
        pxDelayedList = pxDelayedTaskList;
        pxOverflowedDelayedList = pxOverflowDelayedTaskList;
    }
    taskEXIT_CRITICAL();

    // Check if task is on the pending ready list - if so it's ready.
    if pxEventList == ptr::addr_of!(xPendingReadyList) {
        return eTaskState::eReady;
    }

    // Check if task is on one of the delayed lists.
    if pxStateList == pxDelayedList || pxStateList == pxOverflowedDelayedList {
        return eTaskState::eBlocked;
    }

    // Check if task is on the suspended list.
    #[cfg(feature = "task-suspend")]
    {
        if pxStateList == ptr::addr_of!(xSuspendedTaskList) {
            // Task is on suspended list. Is it genuinely suspended or blocked indefinitely?
            if pxEventList.is_null() {
                // Not waiting on an event list. Check if waiting on a notification.
                let mut is_blocked = false;
                for i in 0..configTASK_NOTIFICATION_ARRAY_ENTRIES {
                    if prvReadNotifyState(pxTCB, i) == taskWAITING_NOTIFICATION {
                        is_blocked = true;
                        break;
                    }
                }
                if is_blocked {
                    return eTaskState::eBlocked;
                } else {
                    return eTaskState::eSuspended;
                }
            } else {
                // Waiting on an event list while on suspended list means blocked indefinitely.
                return eTaskState::eBlocked;
            }
        }
    }

    // Check if task is on the deleted list (or has no list).
    #[cfg(feature = "task-delete")]
    {
        if pxStateList == ptr::addr_of!(xTasksWaitingTermination) || pxStateList.is_null() {
            return eTaskState::eDeleted;
        }
    }

    // If not in any other state, it must be Ready.
    eTaskState::eReady
}

// =============================================================================
// xTaskAbortDelay - Abort a task's delay
// =============================================================================

/// Forces a task out of the Blocked state and into the Ready state.
///
/// A task will enter the Blocked state when it is waiting for an event (such as
/// a timeout, semaphore, or queue). xTaskAbortDelay() can be used to force the
/// task out of the Blocked state before the event occurs.
///
/// # Returns
/// - pdPASS if the task was removed from the Blocked state
/// - pdFAIL if the task was not in the Blocked state
///
/// # Safety
/// - xTask must be a valid task handle
/// - Must be called from a task context (not ISR)
#[cfg(feature = "abort-delay")]
pub unsafe fn xTaskAbortDelay(xTask: TaskHandle_t) -> BaseType_t {
    let pxTCB = xTask as *mut TCB_t;
    configASSERT(!pxTCB.is_null());

    let xReturn: BaseType_t;

    vTaskSuspendAll();
    {
        // A task can only be prematurely removed from the Blocked state if
        // it is actually in the Blocked state.
        if eTaskGetState(xTask) == eTaskState::eBlocked {
            xReturn = pdPASS;

            // Remove the reference to the task from the blocked list.
            // An interrupt won't touch the xStateListItem because the scheduler is suspended.
            uxListRemove(ptr::addr_of_mut!((*pxTCB).xStateListItem));

            // Is the task waiting on an event also? If so remove it from
            // the event list too. Interrupts can touch the event list item,
            // even though the scheduler is suspended, so a critical section is used.
            taskENTER_CRITICAL();
            {
                if !listLIST_ITEM_CONTAINER(ptr::addr_of!((*pxTCB).xEventListItem)).is_null() {
                    uxListRemove(ptr::addr_of_mut!((*pxTCB).xEventListItem));

                    // This lets the task know it was forcibly removed from the
                    // blocked state so it should not re-evaluate its block time
                    // and then block again.
                    (*pxTCB).ucDelayAborted = pdTRUE as u8;
                }
            }
            taskEXIT_CRITICAL();

            // Place the unblocked task into the appropriate ready list.
            prvAddTaskToReadyList(pxTCB);

            // A task being unblocked cannot cause an immediate context switch
            // if preemption is turned off.
            if configUSE_PREEMPTION != 0 {
                // Preemption is on, but a context switch should only be
                // performed if the unblocked task has a priority that is
                // higher than the currently executing task.
                if (*pxTCB).uxPriority > (*pxCurrentTCB).uxPriority {
                    // Pend the yield to be performed when the scheduler is unsuspended.
                    xYieldPendings[0] = pdTRUE;
                }
            }
        } else {
            xReturn = pdFAIL;
        }
    }
    xTaskResumeAll();

    xReturn
}

// =============================================================================
// Task Information APIs (configUSE_TRACE_FACILITY)
// =============================================================================

/// Return the application-defined trace number for a task.
///
/// # Safety
///
/// A non-null handle must identify a live task for the duration of the call.
#[cfg(feature = "trace-facility")]
pub unsafe fn uxTaskGetTaskNumber(xTask: TaskHandle_t) -> UBaseType_t {
    if xTask.is_null() {
        0
    } else {
        (*(xTask as *mut TCB_t)).uxTaskNumber
    }
}

/// Set the application-defined trace number for a task.
///
/// # Safety
///
/// A non-null handle must identify a live, uniquely mutable task for the call.
#[cfg(feature = "trace-facility")]
pub unsafe fn vTaskSetTaskNumber(xTask: TaskHandle_t, uxHandle: UBaseType_t) {
    if !xTask.is_null() {
        (*(xTask as *mut TCB_t)).uxTaskNumber = uxHandle;
    }
}

/// Get information about a task
///
/// Populates a TaskStatus_t structure with information about the task.
///
/// # Arguments
/// * `xTask` - Handle of the task to query, or NULL for the current task
/// * `pxTaskStatus` - Pointer to TaskStatus_t struct to populate
/// * `xGetFreeStackSpace` - If pdTRUE, also calculate stack high water mark
/// * `eState` - The state to use for the task, or eInvalid to query actual state
///
/// # Safety
///
/// A non-null `xTask` must identify a live task whose TCB and stack cannot be
/// reclaimed during the call; null selects the live current task.
/// `pxTaskStatus` must be non-null, aligned, and uniquely writable for one
/// `TaskStatus_t`. If stack space is requested, the task's recorded stack
/// extent must still be readable. Raw pointers copied into the result remain
/// valid only while the corresponding task remains live.
#[cfg(feature = "trace-facility")]
pub unsafe fn vTaskGetInfo(
    xTask: TaskHandle_t,
    pxTaskStatus: *mut crate::types::TaskStatus_t,
    xGetFreeStackSpace: BaseType_t,
    eState: eTaskState,
) {
    // [AMENDMENT] In C, eState can be passed in to avoid recomputing it.
    // If eInvalid is passed, we query the actual state.

    // Get the TCB from handle or use current task
    let pxTCB: *mut TCB_t = if xTask.is_null() {
        pxCurrentTCB
    } else {
        xTask as *mut TCB_t
    };

    if pxTCB.is_null() || pxTaskStatus.is_null() {
        return;
    }

    // Fill in the task handle
    (*pxTaskStatus).xHandle = pxTCB as TaskHandle_t;

    // Copy task name pointer
    (*pxTaskStatus).pcTaskName = (*pxTCB).pcTaskName.as_ptr();

    /* TaskStatus_t reports the automatically assigned TCB generation number.
     * uxTaskNumber is a separate user-editable value for third-party tools. */
    (*pxTaskStatus).xTaskNumber = (*pxTCB).uxTCBNumber;

    // Fill in current priority
    (*pxTaskStatus).uxCurrentPriority = (*pxTCB).uxPriority;

    // Fill in base priority (if mutexes are in use, otherwise same as current)
    #[cfg(feature = "use-mutexes")]
    {
        (*pxTaskStatus).uxBasePriority = (*pxTCB).uxBasePriority;
    }
    #[cfg(not(feature = "use-mutexes"))]
    {
        (*pxTaskStatus).uxBasePriority = 0;
    }

    // Run time counter (if run-time stats are enabled)
    #[cfg(feature = "generate-run-time-stats")]
    {
        // SAFETY: vTaskGetInfo's contract keeps pxTCB live for the call.
        (*pxTaskStatus).ulRunTimeCounter = unsafe { ulTaskGetRunTimeCounter(pxTCB.cast()) };
    }
    #[cfg(not(feature = "generate-run-time-stats"))]
    {
        (*pxTaskStatus).ulRunTimeCounter = 0;
    }

    // Stack base pointer
    (*pxTaskStatus).pxStackBase = (*pxTCB).pxStack;

    // Stack high water mark (if requested)
    if xGetFreeStackSpace != pdFALSE {
        let uxFree = prvTaskCheckFreeStackSpace((*pxTCB).pxStack, (*pxTCB).uxStackDepth);
        (*pxTaskStatus).usStackHighWaterMark = uxFree.min(u16::MAX as usize) as u16;
    } else {
        (*pxTaskStatus).usStackHighWaterMark = 0;
    }

    // Task state - use provided state or query actual.
    if eState != eTaskState::eInvalid {
        if pxTCB == pxCurrentTCB {
            (*pxTaskStatus).eCurrentState = eTaskState::eRunning as u8;
        } else {
            (*pxTaskStatus).eCurrentState = eState as u8;

            #[cfg(feature = "task-suspend")]
            if eState == eTaskState::eSuspended {
                vTaskSuspendAll();
                {
                    let mut xNotificationWaiting = pdFALSE;
                    let mut uxIndex = 0usize;
                    while uxIndex < configTASK_NOTIFICATION_ARRAY_ENTRIES {
                        if prvReadNotifyState(pxTCB, uxIndex) == taskWAITING_NOTIFICATION {
                            xNotificationWaiting = pdTRUE;
                            break;
                        }
                        uxIndex += 1;
                    }

                    if !listLIST_ITEM_CONTAINER(ptr::addr_of!((*pxTCB).xEventListItem)).is_null()
                        || xNotificationWaiting != pdFALSE
                    {
                        (*pxTaskStatus).eCurrentState = eTaskState::eBlocked as u8;
                    }
                }
                let _ = xTaskResumeAll();
            }

            /* A task in the pending-ready list is ready irrespective of the
             * state list that still contains its state item. */
            taskENTER_CRITICAL();
            {
                if listIS_CONTAINED_WITHIN(
                    ptr::addr_of!(xPendingReadyList),
                    ptr::addr_of!((*pxTCB).xEventListItem),
                ) != pdFALSE
                {
                    (*pxTaskStatus).eCurrentState = eTaskState::eReady as u8;
                }
            }
            taskEXIT_CRITICAL();
        }
    } else {
        (*pxTaskStatus).eCurrentState = eTaskGetState(pxTCB as TaskHandle_t) as u8;
    }
}

/// Helper to add tasks from a list to the status array
///
/// Iterates through a list, calling vTaskGetInfo for each task found.
///
/// # Returns
/// Number of tasks added to the array
#[cfg(feature = "trace-facility")]
unsafe fn prvListTasksWithinSingleList(
    pxTaskStatusArray: *mut crate::types::TaskStatus_t,
    pxList: *mut List_t,
    eState: eTaskState,
) -> UBaseType_t {
    let mut uxTask: UBaseType_t = 0;

    if listCURRENT_LIST_LENGTH(pxList) > 0 {
        // Get the first item in the list
        let pxListEnd = listGET_END_MARKER(pxList) as *mut ListItem_t;
        let mut pxNextListItem = listGET_HEAD_ENTRY(pxList);

        // Iterate through the list
        while pxNextListItem != pxListEnd {
            let pxTCB = listGET_LIST_ITEM_OWNER(pxNextListItem) as *mut TCB_t;

            // Fill in task info
            vTaskGetInfo(
                pxTCB as TaskHandle_t,
                pxTaskStatusArray.add(uxTask as usize),
                pdTRUE,
                eState,
            );

            uxTask += 1;
            pxNextListItem = listGET_NEXT(pxNextListItem);
        }
    }

    uxTask
}

/// Get system state - information about all tasks
///
/// Populates an array of TaskStatus_t structures, one for each task in the
/// system. This function is intended for debugging aid only.
///
/// # Arguments
/// * `pxTaskStatusArray` - Pointer to array of TaskStatus_t structures
/// * `uxArraySize` - Size of the array (max number of tasks to report)
/// * `pulTotalRunTime` - If not NULL, filled with total run time (if stats enabled)
///
/// # Returns
/// Number of tasks populated in the array, or 0 if array too small
///
/// # Safety
///
/// `pxTaskStatusArray` must be aligned and writable for `uxArraySize`
/// `TaskStatus_t` entries (unless the size and current task count are both
/// zero). If non-null, `pulTotalRunTime` must be aligned and writable for one
/// counter. This task-context API requires all reported TCBs and stacks to
/// remain live through the call. Raw pointers copied into the returned entries
/// remain valid only while their corresponding tasks remain live.
#[cfg(feature = "trace-facility")]
pub unsafe fn uxTaskGetSystemState(
    pxTaskStatusArray: *mut crate::types::TaskStatus_t,
    uxArraySize: UBaseType_t,
    pulTotalRunTime: *mut configRUN_TIME_COUNTER_TYPE,
) -> UBaseType_t {
    let mut uxTask: UBaseType_t = 0;
    let mut uxQueue: UBaseType_t = configMAX_PRIORITIES;

    if pxTaskStatusArray.is_null() {
        if uxCurrentNumberOfTasks != 0 {
            return 0;
        }

        if !pulTotalRunTime.is_null() {
            #[cfg(feature = "generate-run-time-stats")]
            {
                *pulTotalRunTime = prvGetTotalRunTime();
            }
            #[cfg(not(feature = "generate-run-time-stats"))]
            {
                *pulTotalRunTime = 0;
            }
        }

        return 0;
    }

    vTaskSuspendAll();
    {
        // Is there a space in the array for each task in the system?
        if uxArraySize >= uxCurrentNumberOfTasks {
            // Fill in a TaskStatus_t structure for each task in the Ready state
            loop {
                uxQueue -= 1;
                uxTask += prvListTasksWithinSingleList(
                    pxTaskStatusArray.add(uxTask as usize),
                    ptr::addr_of_mut!(pxReadyTasksLists[uxQueue as usize]),
                    eTaskState::eReady,
                );

                if uxQueue == 0 {
                    break;
                }
            }

            // Fill in a TaskStatus_t structure for each task in the Blocked state
            uxTask += prvListTasksWithinSingleList(
                pxTaskStatusArray.add(uxTask as usize),
                pxDelayedTaskList,
                eTaskState::eBlocked,
            );

            uxTask += prvListTasksWithinSingleList(
                pxTaskStatusArray.add(uxTask as usize),
                pxOverflowDelayedTaskList,
                eTaskState::eBlocked,
            );

            // Fill in a TaskStatus_t structure for each task that has been deleted
            // but is awaiting cleanup
            #[cfg(feature = "task-delete")]
            {
                uxTask += prvListTasksWithinSingleList(
                    pxTaskStatusArray.add(uxTask as usize),
                    ptr::addr_of_mut!(xTasksWaitingTermination),
                    eTaskState::eDeleted,
                );
            }

            // Fill in a TaskStatus_t structure for each task in the Suspended state
            #[cfg(feature = "task-suspend")]
            {
                uxTask += prvListTasksWithinSingleList(
                    pxTaskStatusArray.add(uxTask as usize),
                    ptr::addr_of_mut!(xSuspendedTaskList),
                    eTaskState::eSuspended,
                );
            }

            // Fill in the run time stats if enabled
            if !pulTotalRunTime.is_null() {
                #[cfg(feature = "generate-run-time-stats")]
                {
                    *pulTotalRunTime = prvGetTotalRunTime();
                }
                #[cfg(not(feature = "generate-run-time-stats"))]
                {
                    *pulTotalRunTime = 0;
                }
            }
        } else {
            // Array not large enough, return 0
            uxTask = 0;
        }
    }
    xTaskResumeAll();

    uxTask
}

// =============================================================================
// Task List Formatting (configUSE_STATS_FORMATTING_FUNCTIONS)
// =============================================================================

/// Buffer writer for formatting task list output
///
/// \[AMENDMENT\] In C, snprintf is used for formatting. In Rust no_std,
/// we use core::fmt::Write trait with a buffer wrapper instead.
/// This avoids heap allocation and works on bare metal.
#[cfg(feature = "stats-formatting")]
pub struct BufferWriter<'a> {
    buf: &'a mut [u8],
    pos: usize,
}

#[cfg(feature = "stats-formatting")]
impl<'a> BufferWriter<'a> {
    /// Create a new BufferWriter wrapping a byte slice
    pub fn new(buf: &'a mut [u8]) -> Self {
        if !buf.is_empty() {
            buf[0] = 0;
        }
        BufferWriter { buf, pos: 0 }
    }

    /// Returns the number of bytes written
    pub fn len(&self) -> usize {
        self.pos
    }

    /// Returns true if nothing has been written
    pub fn is_empty(&self) -> bool {
        self.pos == 0
    }
}

#[cfg(feature = "stats-formatting")]
impl<'a> core::fmt::Write for BufferWriter<'a> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let bytes = s.as_bytes();
        let available = self.buf.len().saturating_sub(1).saturating_sub(self.pos);
        let copied = bytes.len().min(available);

        if copied > 0 {
            self.buf[self.pos..self.pos + copied].copy_from_slice(&bytes[..copied]);
            self.pos += copied;
        }

        if !self.buf.is_empty() {
            self.buf[self.pos] = 0;
        }

        if copied == bytes.len() {
            Ok(())
        } else {
            Err(core::fmt::Error)
        }
    }
}

/// Convert task state to a single character for display
#[cfg(feature = "stats-formatting")]
fn prvGetTaskStateChar(eState: eTaskState) -> char {
    match eState {
        eTaskState::eRunning => 'X',   // eXecuting
        eTaskState::eReady => 'R',     // Ready
        eTaskState::eBlocked => 'B',   // Blocked
        eTaskState::eSuspended => 'S', // Suspended
        eTaskState::eDeleted => 'D',   // Deleted
        eTaskState::eInvalid => '?',   // Invalid
    }
}

/// Write task list to a buffer
///
/// Writes a human-readable table of task information to the provided buffer.
/// Format: `TaskName        State  Prio  Stack  Num`
///
/// # Arguments
/// * `pcWriteBuffer` - Pointer to buffer to write task list into
/// * `uxBufferLength` - Length of the buffer in bytes
///
/// # Safety
///
/// `pcWriteBuffer` must be non-null, aligned, and uniquely writable for
/// `uxBufferLength` bytes. This task-context API requires every task reported
/// by the snapshot, including its fixed-size name and stack storage, to remain
/// live until formatting finishes; callers must coordinate against concurrent
/// task deletion.
///
/// # Note
/// This function calls uxTaskGetSystemState which requires trace-facility.
#[cfg(all(feature = "stats-formatting", feature = "trace-facility"))]
pub unsafe fn vTaskListTasks(pcWriteBuffer: *mut u8, uxBufferLength: usize) {
    use core::fmt::Write;

    configASSERT(!pcWriteBuffer.is_null());
    configASSERT(uxBufferLength > 0);

    // Create a slice from the raw pointer
    let buffer = core::slice::from_raw_parts_mut(pcWriteBuffer, uxBufferLength);
    let mut writer = BufferWriter::new(buffer);

    // Write header
    let _ = writeln!(
        writer,
        "{:<16} {:>5} {:>4} {:>6} {:>4}",
        "Name", "State", "Prio", "Stack", "Num"
    );

    // Get the number of tasks
    let uxArraySize = uxTaskGetNumberOfTasks();
    if uxArraySize == 0 {
        return;
    }

    // Allocate array on stack for small systems, or use alloc if available
    // [AMENDMENT] In C, malloc is used. Here we use a fixed-size stack array
    // or alloc if available. For simplicity, we limit to 16 tasks on stack.
    const MAX_STACK_TASKS: usize = 16;

    #[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
    {
        extern crate alloc;
        use alloc::vec::Vec;

        let mut pxTaskStatusArray: Vec<crate::types::TaskStatus_t> = Vec::new();
        if pxTaskStatusArray
            .try_reserve_exact(uxArraySize as usize)
            .is_err()
        {
            return;
        }
        pxTaskStatusArray.resize_with(uxArraySize as usize, crate::types::TaskStatus_t::new);

        let uxTasksReturned =
            uxTaskGetSystemState(pxTaskStatusArray.as_mut_ptr(), uxArraySize, ptr::null_mut());

        // Write each task
        for i in 0..uxTasksReturned as usize {
            let pxTaskStatus = &pxTaskStatusArray[i];
            // SAFETY: vTaskListTasks' contract keeps reported tasks and their
            // fixed-size name storage live through formatting.
            let name = unsafe { prvGetTaskNameFromPtr(pxTaskStatus.pcTaskName) };
            let state_char = prvGetTaskStateChar(core::mem::transmute::<u8, eTaskState>(
                pxTaskStatus.eCurrentState,
            ));

            let _ = writeln!(
                writer,
                "{:<16} {:>5} {:>4} {:>6} {:>4}",
                name,
                state_char,
                pxTaskStatus.uxCurrentPriority,
                pxTaskStatus.usStackHighWaterMark,
                pxTaskStatus.xTaskNumber
            );
        }
    }

    #[cfg(not(any(feature = "alloc", feature = "heap-4", feature = "heap-5")))]
    {
        // Stack-allocated version for no-alloc builds
        let mut pxTaskStatusArray: [crate::types::TaskStatus_t; MAX_STACK_TASKS] =
            [const { crate::types::TaskStatus_t::new() }; MAX_STACK_TASKS];

        let uxArraySizeClamped = if uxArraySize as usize > MAX_STACK_TASKS {
            MAX_STACK_TASKS as UBaseType_t
        } else {
            uxArraySize
        };

        let uxTasksReturned = uxTaskGetSystemState(
            pxTaskStatusArray.as_mut_ptr(),
            uxArraySizeClamped,
            ptr::null_mut(),
        );

        // Write each task
        for i in 0..uxTasksReturned as usize {
            let pxTaskStatus = &pxTaskStatusArray[i];
            // SAFETY: vTaskListTasks' contract keeps reported tasks and their
            // fixed-size name storage live through formatting.
            let name = unsafe { prvGetTaskNameFromPtr(pxTaskStatus.pcTaskName) };
            let state_char = prvGetTaskStateChar(core::mem::transmute::<u8, eTaskState>(
                pxTaskStatus.eCurrentState,
            ));

            let _ = writeln!(
                writer,
                "{:<16} {:>5} {:>4} {:>6} {:>4}",
                name,
                state_char,
                pxTaskStatus.uxCurrentPriority,
                pxTaskStatus.usStackHighWaterMark,
                pxTaskStatus.xTaskNumber
            );
        }
    }

    // BufferWriter preserves a trailing NUL after every write, including
    // truncation and all early-return paths.
}

/// Helper to convert a task name pointer to `&str` for formatting.
///
/// # Safety
///
/// A non-null `pcTaskName` must remain readable through a NUL terminator within
/// `configMAX_TASK_NAME_LEN` bytes, or for that complete maximum length, for
/// the returned lifetime. The bytes may be arbitrary; invalid UTF-8 is
/// reported as `"<invalid>"`.
#[cfg(feature = "stats-formatting")]
unsafe fn prvGetTaskNameFromPtr<'a>(pcTaskName: *const u8) -> &'a str {
    if pcTaskName.is_null() {
        return "<null>";
    }

    // Find length by scanning for null terminator or max length
    let mut len = 0;
    while len < configMAX_TASK_NAME_LEN && unsafe { *pcTaskName.add(len) } != 0 {
        len += 1;
    }
    // SAFETY: the caller guarantees this fixed-size name storage is readable.
    let bytes = unsafe { core::slice::from_raw_parts(pcTaskName, len) };
    core::str::from_utf8(bytes).unwrap_or("<invalid>")
}

// =============================================================================
// Run-time Statistics Functions (configGENERATE_RUN_TIME_STATS)
// =============================================================================

/// Get the run-time counter value for a task
///
/// Returns the total time the task has spent in the Running state.
/// This is only available if `generate-run-time-stats` feature is enabled.
///
/// # Arguments
/// * `xTask` - Handle of the task to query, or NULL for the current task
///
/// # Returns
/// The run-time counter value for the task
///
/// # Safety
///
/// A non-null `xTask` must identify a live task whose TCB cannot be reclaimed
/// during the call. Null selects the current task. Call from task context.
#[cfg(feature = "generate-run-time-stats")]
pub unsafe fn ulTaskGetRunTimeCounter(xTask: TaskHandle_t) -> configRUN_TIME_COUNTER_TYPE {
    unsafe {
        let pxTCB = prvGetTCBFromHandle(xTask);
        if pxTCB.is_null() {
            return 0;
        }

        taskENTER_CRITICAL();
        let mut ulTaskRunTime = (*pxTCB).ulRunTimeCounter;
        if pxTCB == pxCurrentTCB && xSchedulerRunning != pdFALSE {
            let ulNow = crate::port::portGET_RUN_TIME_COUNTER_VALUE();
            ulTaskRunTime =
                ulTaskRunTime.wrapping_add(prvRunTimeDelta(ulNow, ulTaskSwitchedInTime));
        }
        taskEXIT_CRITICAL();
        ulTaskRunTime
    }
}

/// Get the percentage of total run time that a task has used
///
/// Returns the percentage of total run time that the task has consumed.
/// This is only available if `generate-run-time-stats` feature is enabled.
///
/// # Arguments
/// * `xTask` - Handle of the task to query, or NULL for the current task
///
/// # Returns
/// The percentage of total run time (0-100)
///
/// # Safety
///
/// A non-null `xTask` must identify a live task whose TCB cannot be reclaimed
/// during the call. Null selects the current task. Call from task context.
#[cfg(feature = "generate-run-time-stats")]
pub unsafe fn ulTaskGetRunTimePercent(xTask: TaskHandle_t) -> configRUN_TIME_COUNTER_TYPE {
    let ulTaskRunTime = unsafe { ulTaskGetRunTimeCounter(xTask) };
    let ulTotalTime = unsafe { prvGetTotalRunTime() };
    if ulTotalTime == 0 {
        return 0;
    }

    (((ulTaskRunTime as u64 * 100) / ulTotalTime as u64) as configRUN_TIME_COUNTER_TYPE).min(100)
}

/// Get the run-time counter value for the idle task
///
/// Returns the total time the idle task has spent in the Running state.
/// This is only available if `generate-run-time-stats` feature is enabled.
///
/// # Returns
/// The run-time counter value for the idle task
#[cfg(feature = "generate-run-time-stats")]
pub fn ulTaskGetIdleRunTimeCounter() -> configRUN_TIME_COUNTER_TYPE {
    unsafe {
        let xIdleTaskHandle = xIdleTaskHandles[0];
        if xIdleTaskHandle.is_null() {
            return 0;
        }
    }

    unsafe { ulTaskGetRunTimeCounter(xIdleTaskHandles[0]) }
}

/// Get the percentage of total run time that the idle task has used
///
/// Returns the percentage of total run time that the idle task has consumed.
/// This is a measure of CPU idle time - higher values indicate more idle time.
/// This is only available if `generate-run-time-stats` feature is enabled.
///
/// # Returns
/// The percentage of total run time (0-100)
#[cfg(feature = "generate-run-time-stats")]
pub fn ulTaskGetIdleRunTimePercent() -> configRUN_TIME_COUNTER_TYPE {
    let xIdleTaskHandle = unsafe { xIdleTaskHandles[0] };
    if xIdleTaskHandle.is_null() {
        return 0;
    }

    // SAFETY: the scheduler owns the idle handle for its complete lifetime.
    unsafe { ulTaskGetRunTimePercent(xIdleTaskHandle) }
}

/// Get the total run time
///
/// Returns the total run time since the scheduler started.
/// This is only available if `generate-run-time-stats` feature is enabled.
///
/// # Returns
/// The total run time counter value
#[cfg(feature = "generate-run-time-stats")]
pub fn ulTaskGetTotalRunTime() -> configRUN_TIME_COUNTER_TYPE {
    unsafe { prvGetTotalRunTime() }
}

/// Write run-time statistics to a buffer
///
/// Writes a human-readable table of task run-time statistics to the provided buffer.
/// Format: `TaskName        Abs. Time      % Time`
///
/// \[AMENDMENT\] In C, snprintf is used for formatting. In Rust no_std,
/// we use core::fmt::Write trait with a buffer wrapper instead.
///
/// # Arguments
/// * `pcWriteBuffer` - Pointer to buffer to write statistics into
/// * `uxBufferLength` - Length of the buffer in bytes
///
/// # Safety
///
/// `pcWriteBuffer` must be non-null, aligned, and uniquely writable for
/// `uxBufferLength` bytes. This task-context API requires every task reported
/// by the snapshot, including its fixed-size name storage, to remain live until
/// formatting finishes; callers must coordinate against concurrent deletion.
#[cfg(all(
    feature = "generate-run-time-stats",
    feature = "stats-formatting",
    feature = "trace-facility"
))]
pub unsafe fn vTaskGetRunTimeStatistics(pcWriteBuffer: *mut u8, uxBufferLength: usize) {
    use core::fmt::Write;

    configASSERT(!pcWriteBuffer.is_null());
    configASSERT(uxBufferLength > 0);

    // Create a slice from the raw pointer
    let buffer = core::slice::from_raw_parts_mut(pcWriteBuffer, uxBufferLength);
    let mut writer = BufferWriter::new(buffer);

    // Write header
    let _ = writeln!(writer, "{:<16} {:>12} {:>8}", "Name", "Abs. Time", "% Time");

    // Get the number of tasks
    let uxArraySize = uxTaskGetNumberOfTasks();
    if uxArraySize == 0 {
        return;
    }

    // Get the total run time
    let ulTotalTime = prvGetTotalRunTime();
    if ulTotalTime == 0 {
        let _ = writeln!(writer, "(No run time data collected yet)");
        return;
    }

    // Allocate array for task status
    #[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
    {
        extern crate alloc;
        use alloc::vec::Vec;

        let mut pxTaskStatusArray: Vec<crate::types::TaskStatus_t> = Vec::new();
        if pxTaskStatusArray
            .try_reserve_exact(uxArraySize as usize)
            .is_err()
        {
            return;
        }
        pxTaskStatusArray.resize_with(uxArraySize as usize, crate::types::TaskStatus_t::new);

        let mut ulTotalRunTimeReturned: u32 = 0;
        let uxTasksReturned = uxTaskGetSystemState(
            pxTaskStatusArray.as_mut_ptr(),
            uxArraySize,
            &mut ulTotalRunTimeReturned,
        );

        // Generate output for each task
        for i in 0..uxTasksReturned as usize {
            let pxTaskStatus = &pxTaskStatusArray[i];

            // Calculate percentage
            let ulStatsAsPercentage = if ulTotalTime > 0 {
                ((pxTaskStatus.ulRunTimeCounter as u64 * 100) / ulTotalTime as u64) as u32
            } else {
                0
            };

            // Get task name as string
            let pcTaskName = pxTaskStatus.pcTaskName;
            // SAFETY: vTaskGetRunTimeStatistics' contract keeps reported task
            // name storage live through formatting.
            let name = unsafe { prvGetTaskNameFromPtr(pcTaskName) };

            // Write the task line
            if ulStatsAsPercentage > 0 {
                let _ = writeln!(
                    writer,
                    "{:<16} {:>12} {:>7}%",
                    name, pxTaskStatus.ulRunTimeCounter, ulStatsAsPercentage
                );
            } else {
                // If percentage is less than 1%, show "<1%"
                let _ = writeln!(
                    writer,
                    "{:<16} {:>12} {:>7}",
                    name, pxTaskStatus.ulRunTimeCounter, "<1%"
                );
            }
        }
    }

    #[cfg(not(any(feature = "alloc", feature = "heap-4", feature = "heap-5")))]
    {
        // Without dynamic allocation, use a fixed-size array on the stack
        const MAX_STACK_TASKS: usize = 16;
        let mut pxTaskStatusArray: [crate::types::TaskStatus_t; MAX_STACK_TASKS] =
            [const { crate::types::TaskStatus_t::new() }; MAX_STACK_TASKS];

        let uxActualArraySize = if uxArraySize as usize > MAX_STACK_TASKS {
            MAX_STACK_TASKS as UBaseType_t
        } else {
            uxArraySize
        };

        let mut ulTotalRunTimeReturned: u32 = 0;
        let uxTasksReturned = uxTaskGetSystemState(
            pxTaskStatusArray.as_mut_ptr(),
            uxActualArraySize,
            &mut ulTotalRunTimeReturned,
        );

        // Generate output for each task
        for i in 0..uxTasksReturned as usize {
            let pxTaskStatus = &pxTaskStatusArray[i];

            // Calculate percentage
            let ulStatsAsPercentage = if ulTotalTime > 0 {
                ((pxTaskStatus.ulRunTimeCounter as u64 * 100) / ulTotalTime as u64) as u32
            } else {
                0
            };

            // Get task name as string
            let pcTaskName = pxTaskStatus.pcTaskName;
            // SAFETY: vTaskGetRunTimeStatistics' contract keeps reported task
            // name storage live through formatting.
            let name = unsafe { prvGetTaskNameFromPtr(pcTaskName) };

            // Write the task line
            if ulStatsAsPercentage > 0 {
                let _ = writeln!(
                    writer,
                    "{:<16} {:>12} {:>7}%",
                    name, pxTaskStatus.ulRunTimeCounter, ulStatsAsPercentage
                );
            } else {
                // If percentage is less than 1%, show "<1%"
                let _ = writeln!(
                    writer,
                    "{:<16} {:>12} {:>7}",
                    name, pxTaskStatus.ulRunTimeCounter, "<1%"
                );
            }
        }
    }
}

// =============================================================================
// Tickless Idle Functions (configUSE_TICKLESS_IDLE)
// =============================================================================

/// Calculate the expected idle time.
///
/// Returns the number of ticks until the next task needs to wake up.
/// This is used to determine if it's worth entering a low-power sleep mode.
///
/// This function should only be called from the idle task.
#[cfg(feature = "tickless-idle")]
fn prvGetExpectedIdleTime() -> TickType_t {
    unsafe {
        let mut xReturn: TickType_t;
        let mut xHigherPriorityReadyTasks = pdFALSE;

        // xHigherPriorityReadyTasks takes care of the case where
        // configUSE_PREEMPTION is 0, so there may be tasks above the idle priority
        // task that are in the Ready state, even though the idle task is running.

        // uxTopReadyPriority stores the highest ready priority.  The bitmap
        // representation used by port-optimised selection is not implemented.
        if uxTopReadyPriority > tskIDLE_PRIORITY {
            xHigherPriorityReadyTasks = pdTRUE;
        }

        if !pxCurrentTCB.is_null() && (*pxCurrentTCB).uxPriority > tskIDLE_PRIORITY {
            // Current task is above idle priority - cannot sleep
            xReturn = 0;
        } else if listCURRENT_LIST_LENGTH(ptr::addr_of!(
            pxReadyTasksLists[tskIDLE_PRIORITY as usize]
        )) > 1
        {
            // There are other idle priority tasks in the ready state.
            // If time slicing is used, the next tick interrupt must be processed.
            xReturn = 0;
        } else if xHigherPriorityReadyTasks != pdFALSE {
            // There are tasks in the Ready state that have a priority above the
            // idle priority. This path can only be reached if configUSE_PREEMPTION is 0.
            xReturn = 0;
        } else {
            xReturn = xNextTaskUnblockTime;
            xReturn = xReturn.wrapping_sub(xTickCount);
        }

        xReturn
    }
}

/// Confirm that it is still safe to enter a sleep mode.
///
/// This function is called from the port layer after disabling interrupts
/// but before entering the sleep mode. It checks for conditions that would
/// require aborting the sleep.
///
/// Must be called from a critical section.
///
/// # Safety
///
/// The scheduler must be suspended with its lists initialized, and the caller
/// must hold the port interrupt mask required by the tickless-idle protocol.
#[cfg(feature = "tickless-idle")]
pub unsafe fn eTaskConfirmSleepModeStatus() -> crate::types::eSleepModeStatus {
    use crate::types::eSleepModeStatus;

    unsafe {
        // Check if a task was made ready while the scheduler was suspended.
        if listCURRENT_LIST_LENGTH(ptr::addr_of!(xPendingReadyList)) != 0 {
            return eSleepModeStatus::eAbortSleep;
        }

        // Check if a yield was pended while the scheduler was suspended.
        if xYieldPendings[0] != pdFALSE {
            return eSleepModeStatus::eAbortSleep;
        }

        // Check if a tick interrupt has already occurred but was held pending
        // because the scheduler is suspended.
        if xPendedTicks != 0 {
            return eSleepModeStatus::eAbortSleep;
        }

        // Check if all tasks are suspended (can enter deep sleep)
        #[cfg(feature = "task-suspend")]
        {
            // The idle task exists in addition to the application tasks.
            let uxNonApplicationTasks: UBaseType_t = configNUMBER_OF_CORES as UBaseType_t;

            if listCURRENT_LIST_LENGTH(ptr::addr_of!(xSuspendedTaskList))
                == (uxCurrentNumberOfTasks - uxNonApplicationTasks)
            {
                // All tasks are in the suspended list (which might mean they
                // have an infinite block time rather than actually being suspended).
                // It is safe to enter a sleep mode that can only be exited by
                // an external interrupt.
                return eSleepModeStatus::eNoTasksWaitingTimeout;
            }
        }

        // Standard sleep - enter a sleep that will end at the next tick
        eSleepModeStatus::eStandardSleep
    }
}

/// Step the tick count forward after sleeping.
///
/// Called from the port layer after waking from tickless sleep to advance
/// the tick count by the number of ticks that elapsed during sleep.
///
/// # Arguments
/// * `xTicksToJump` - Number of ticks to advance the tick count by
///
/// # Safety
///
/// Call only from a selected port's tickless-idle path while the scheduler is
/// suspended and tick interrupts are masked. `xTicksToJump` must not advance
/// beyond the next unblock deadline under the port's final-tick convention.
#[cfg(feature = "tickless-idle")]
pub unsafe fn vTaskStepTick(xTicksToJump: TickType_t) {
    unsafe {
        let mut xTicksToJump = xTicksToJump;

        // Correct the tick count value after a period during which the tick
        // was suppressed. Note this does *not* call the tick hook function for
        // each stepped tick.
        let xUpdatedTickCount = xTickCount.wrapping_add(xTicksToJump);

        configASSERT(xUpdatedTickCount <= xNextTaskUnblockTime);

        if xUpdatedTickCount == xNextTaskUnblockTime {
            // Arrange for xTickCount to reach xNextTaskUnblockTime in
            // xTaskIncrementTick() when the scheduler resumes. This ensures
            // that any delayed tasks are resumed at the correct time.
            configASSERT(uxSchedulerSuspended != 0);
            configASSERT(xTicksToJump != 0);

            // Prevent the tick interrupt modifying xPendedTicks simultaneously.
            taskENTER_CRITICAL();
            {
                xPendedTicks = xPendedTicks.wrapping_add(1);
            }
            taskEXIT_CRITICAL();
            xTicksToJump -= 1;
        }

        xTickCount = xTickCount.wrapping_add(xTicksToJump);

        crate::trace::traceINCREASE_TICK_COUNT(xTicksToJump);
    }
}

/// Process ticks that elapsed while the application temporarily stopped the
/// kernel tick source.
///
/// # Safety
///
/// Call only from task context with a running, unsuspended scheduler. The
/// caller must ensure the elapsed count is authoritative and is not also
/// delivered by the port's normal tick interrupt.
pub unsafe fn xTaskCatchUpTicks(xTicksToCatchUp: TickType_t) -> BaseType_t {
    configASSERT(uxSchedulerSuspended == 0);

    vTaskSuspendAll();
    taskENTER_CRITICAL();
    {
        xPendedTicks = xPendedTicks.wrapping_add(xTicksToCatchUp);
    }
    taskEXIT_CRITICAL();

    xTaskResumeAll()
}

/// Reset task-module globals before restarting a scheduler that has been
/// stopped and whose kernel-created tasks have been deleted.
///
/// # Safety
///
/// The scheduler must be stopped, no task or ISR may access kernel state, and
/// every application task from the prior scheduler instance must already have
/// been deleted or otherwise made permanently unreachable.
pub unsafe fn vTaskResetState() {
    pxCurrentTCB = ptr::null_mut();

    #[cfg(feature = "task-delete")]
    {
        uxDeletedTasksWaitingCleanUp = 0;
    }

    uxCurrentNumberOfTasks = 0;
    xTickCount = configINITIAL_TICK_COUNT;
    uxTopReadyPriority = tskIDLE_PRIORITY;
    xSchedulerRunning = pdFALSE;
    xPendedTicks = 0;
    xYieldPendings[0] = pdFALSE;
    xNumOfOverflows = 0;
    uxTaskNumber = 0;
    xNextTaskUnblockTime = 0;
    uxSchedulerSuspended = 0;

    #[cfg(feature = "generate-run-time-stats")]
    {
        ulTaskSwitchedInTime = 0;
        ulRunTimeStart = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "stack-overflow-check")]
    use core::sync::atomic::{AtomicUsize, Ordering};

    extern "C" fn test_task(_parameter: *mut c_void) {}

    #[cfg(feature = "stack-overflow-check")]
    static STACK_OVERFLOW_CALLS: AtomicUsize = AtomicUsize::new(0);

    #[cfg(feature = "stack-overflow-check")]
    extern "C" fn stack_overflow_hook(_task: TaskHandle_t, _name: *const u8) {
        STACK_OVERFLOW_CALLS.fetch_add(1, Ordering::SeqCst);
    }

    #[cfg(feature = "port-test")]
    unsafe fn initialise_synthetic_tcb(tcb: *mut TCB_t, priority: UBaseType_t) {
        (*tcb).uxPriority = priority;
        /* [AMENDMENT] Synthetic TCBs obey the same intrusive-list provenance
         * rule as production TCBs: derive retained pointers directly, then do
         * not recreate references to the TCB or either list item. */
        vListInitialiseItem(ptr::addr_of_mut!((*tcb).xStateListItem));
        vListInitialiseItem(ptr::addr_of_mut!((*tcb).xEventListItem));
        listSET_LIST_ITEM_OWNER(ptr::addr_of_mut!((*tcb).xStateListItem), tcb.cast());
        listSET_LIST_ITEM_OWNER(ptr::addr_of_mut!((*tcb).xEventListItem), tcb.cast());
        listSET_LIST_ITEM_VALUE(
            ptr::addr_of_mut!((*tcb).xEventListItem),
            (configMAX_PRIORITIES - priority) as TickType_t,
        );
    }

    #[cfg(feature = "port-test")]
    unsafe fn reset_synthetic_scheduler_state() {
        prvInitialiseTaskLists();
        pxCurrentTCB = ptr::null_mut();
        uxCurrentNumberOfTasks = 0;
        uxTopReadyPriority = tskIDLE_PRIORITY;
        xSchedulerRunning = pdFALSE;
        xTickCount = configINITIAL_TICK_COUNT;
        xPendedTicks = 0;
        xYieldPendings[0] = pdFALSE;
        xNumOfOverflows = 0;
        xNextTaskUnblockTime = portMAX_DELAY;
        uxSchedulerSuspended = 0;
    }

    #[cfg(all(feature = "port-test", feature = "use-mutexes"))]
    unsafe fn initialise_synthetic_mutex_tcb(tcb: *mut TCB_t, priority: UBaseType_t) {
        initialise_synthetic_tcb(tcb, priority);
        (*tcb).uxBasePriority = priority;
    }

    #[test]
    fn static_task_storage_matches_tcb_layout() {
        assert_eq!(
            core::mem::size_of::<StaticTask_t>(),
            core::mem::size_of::<TCB_t>()
        );
        assert_eq!(
            core::mem::align_of::<StaticTask_t>(),
            core::mem::align_of::<TCB_t>()
        );
    }

    #[test]
    fn scheduler_state_values_match_the_public_freertos_contract() {
        assert_eq!(taskSCHEDULER_SUSPENDED, 0);
        assert_eq!(taskSCHEDULER_NOT_STARTED, 1);
        assert_eq!(taskSCHEDULER_RUNNING, 2);
    }

    #[cfg(all(
        feature = "port-test",
        feature = "record-stack-high-address",
        not(feature = "arch-64bit")
    ))]
    #[test]
    fn static_task_records_the_aligned_high_stack_address() {
        #[repr(align(8))]
        struct AlignedStack([StackType_t; 2]);

        crate::port::test_port_reset();
        let mut stack = AlignedStack([0; 2]);
        let mut task_buffer = StaticTask_t::new();
        let task_buffer_ptr = ptr::addr_of_mut!(task_buffer);

        unsafe {
            pxCurrentTCB = ptr::null_mut();
            uxCurrentNumberOfTasks = 0;
            xSchedulerRunning = pdFALSE;
            uxTopReadyPriority = tskIDLE_PRIORITY;

            let handle = xTaskCreateStatic(
                test_task,
                b"aligned\0".as_ptr(),
                stack.0.len(),
                ptr::null_mut(),
                1,
                stack.0.as_mut_ptr(),
                task_buffer_ptr,
            );
            assert!(!handle.is_null());

            let tcb = handle as *mut TCB_t;
            assert_eq!((*tcb).pxEndOfStack, (*tcb).pxTopOfStack);
            assert_eq!((*tcb).pxEndOfStack as usize % portBYTE_ALIGNMENT, 0);

            let mut returned_stack = ptr::null_mut();
            let mut returned_tcb = ptr::null_mut();
            assert_eq!(
                xTaskGetStaticBuffers(handle, &mut returned_stack, &mut returned_tcb),
                pdTRUE
            );
            assert_eq!(returned_stack, stack.0.as_mut_ptr());
            assert!(returned_tcb == task_buffer_ptr);

            prvInitialiseTaskLists();
            pxCurrentTCB = ptr::null_mut();
            uxCurrentNumberOfTasks = 0;
            uxTopReadyPriority = tskIDLE_PRIORITY;
        }
    }

    #[cfg(feature = "stats-formatting")]
    #[test]
    fn diagnostic_buffer_writer_always_reserves_a_nul() {
        use core::fmt::Write;

        let mut untouched = [0xa5; 4];
        let writer = BufferWriter::new(&mut untouched);
        assert!(writer.is_empty());
        assert_eq!(untouched[0], 0);

        let mut truncated = [0xa5; 4];
        let mut writer = BufferWriter::new(&mut truncated);
        assert!(writer.write_str("abcdef").is_err());
        assert_eq!(&truncated, b"abc\0");
    }

    #[cfg(feature = "port-test")]
    #[test]
    fn resume_all_drains_pending_ready_and_recomputes_unblock_time() {
        crate::port::test_port_reset();
        let mut current = TCB_t::new();
        let mut pending = TCB_t::new();
        let mut later = TCB_t::new();
        let current_ptr = ptr::addr_of_mut!(current);
        let pending_ptr = ptr::addr_of_mut!(pending);
        let later_ptr = ptr::addr_of_mut!(later);

        unsafe {
            reset_synthetic_scheduler_state();
            initialise_synthetic_tcb(current_ptr, 2);
            initialise_synthetic_tcb(pending_ptr, 2);
            initialise_synthetic_tcb(later_ptr, 0);

            prvAddTaskToReadyList(current_ptr);
            listSET_LIST_ITEM_VALUE(ptr::addr_of_mut!((*pending_ptr).xStateListItem), 10);
            listSET_LIST_ITEM_VALUE(ptr::addr_of_mut!((*later_ptr).xStateListItem), 20);
            vListInsert(
                pxDelayedTaskList,
                ptr::addr_of_mut!((*pending_ptr).xStateListItem),
            );
            vListInsert(
                pxDelayedTaskList,
                ptr::addr_of_mut!((*later_ptr).xStateListItem),
            );
            vListInsertEnd(
                ptr::addr_of_mut!(xPendingReadyList),
                ptr::addr_of_mut!((*pending_ptr).xEventListItem),
            );

            pxCurrentTCB = current_ptr;
            uxCurrentNumberOfTasks = 3;
            xSchedulerRunning = pdTRUE;
            xNextTaskUnblockTime = 10;
            uxSchedulerSuspended = 1;

            assert_eq!(xTaskResumeAll(), pdFALSE);
            assert_eq!(xNextTaskUnblockTime, 20);
            assert_eq!(xYieldPendings[0], pdFALSE);
            assert!(
                listIS_CONTAINED_WITHIN(
                    ptr::addr_of!(pxReadyTasksLists[(*pending_ptr).uxPriority as usize]),
                    ptr::addr_of!((*pending_ptr).xStateListItem),
                ) != pdFALSE
            );
            assert_eq!(crate::port::test_port_snapshot().yield_count, 0);

            reset_synthetic_scheduler_state();
        }
        crate::port::test_port_reset();
    }

    #[cfg(feature = "port-test")]
    #[test]
    fn delay_does_not_yield_twice_when_resume_all_already_yielded() {
        crate::port::test_port_reset();
        let mut current = TCB_t::new();
        let mut pending = TCB_t::new();
        let current_ptr = ptr::addr_of_mut!(current);
        let pending_ptr = ptr::addr_of_mut!(pending);

        unsafe {
            reset_synthetic_scheduler_state();
            initialise_synthetic_tcb(current_ptr, 1);
            initialise_synthetic_tcb(pending_ptr, 3);

            prvAddTaskToReadyList(current_ptr);
            listSET_LIST_ITEM_VALUE(ptr::addr_of_mut!((*pending_ptr).xStateListItem), 20);
            vListInsert(
                pxDelayedTaskList,
                ptr::addr_of_mut!((*pending_ptr).xStateListItem),
            );
            vListInsertEnd(
                ptr::addr_of_mut!(xPendingReadyList),
                ptr::addr_of_mut!((*pending_ptr).xEventListItem),
            );

            pxCurrentTCB = current_ptr;
            uxCurrentNumberOfTasks = 2;
            xSchedulerRunning = pdTRUE;
            xNextTaskUnblockTime = 20;

            vTaskDelay(5);

            assert_eq!(crate::port::test_port_snapshot().yield_count, 1);
            assert_eq!(xNextTaskUnblockTime, 5);
            assert!(
                listIS_CONTAINED_WITHIN(
                    pxDelayedTaskList,
                    ptr::addr_of!((*current_ptr).xStateListItem),
                ) != pdFALSE
            );

            reset_synthetic_scheduler_state();
        }
        crate::port::test_port_reset();
    }

    #[cfg(all(feature = "port-test", feature = "tickless-idle"))]
    #[test]
    fn event_wakeup_latches_yield_and_recomputes_tickless_deadline() {
        crate::port::test_port_reset();
        let mut current = TCB_t::new();
        let mut unblocked = TCB_t::new();
        let mut later = TCB_t::new();
        let mut event_list = List_t::new();
        let current_ptr = ptr::addr_of_mut!(current);
        let unblocked_ptr = ptr::addr_of_mut!(unblocked);
        let later_ptr = ptr::addr_of_mut!(later);
        let event_list_ptr = ptr::addr_of_mut!(event_list);

        unsafe {
            reset_synthetic_scheduler_state();
            initialise_synthetic_tcb(current_ptr, 1);
            initialise_synthetic_tcb(unblocked_ptr, 3);
            initialise_synthetic_tcb(later_ptr, 0);
            vListInitialise(event_list_ptr);

            prvAddTaskToReadyList(current_ptr);
            listSET_LIST_ITEM_VALUE(ptr::addr_of_mut!((*unblocked_ptr).xStateListItem), 10);
            listSET_LIST_ITEM_VALUE(ptr::addr_of_mut!((*later_ptr).xStateListItem), 20);
            vListInsert(
                pxDelayedTaskList,
                ptr::addr_of_mut!((*unblocked_ptr).xStateListItem),
            );
            vListInsert(
                pxDelayedTaskList,
                ptr::addr_of_mut!((*later_ptr).xStateListItem),
            );
            vListInsert(
                event_list_ptr,
                ptr::addr_of_mut!((*unblocked_ptr).xEventListItem),
            );

            pxCurrentTCB = current_ptr;
            uxCurrentNumberOfTasks = 3;
            xSchedulerRunning = pdTRUE;
            xNextTaskUnblockTime = 10;

            taskENTER_CRITICAL();
            let xYieldRequired = xTaskRemoveFromEventList(event_list_ptr);
            taskEXIT_CRITICAL();

            assert_eq!(xYieldRequired, pdTRUE);
            assert_eq!(xYieldPendings[0], pdTRUE);
            assert_eq!(xNextTaskUnblockTime, 20);
            assert!(
                listIS_CONTAINED_WITHIN(
                    ptr::addr_of!(pxReadyTasksLists[(*unblocked_ptr).uxPriority as usize]),
                    ptr::addr_of!((*unblocked_ptr).xStateListItem),
                ) != pdFALSE
            );

            reset_synthetic_scheduler_state();
        }
        crate::port::test_port_reset();
    }

    #[cfg(all(feature = "port-test", feature = "tickless-idle"))]
    #[test]
    fn task_notification_increment_wraps_and_updates_tickless_deadline() {
        crate::port::test_port_reset();
        let mut current = TCB_t::new();
        let mut notified = TCB_t::new();
        let mut later = TCB_t::new();
        let current_ptr = ptr::addr_of_mut!(current);
        let notified_ptr = ptr::addr_of_mut!(notified);
        let later_ptr = ptr::addr_of_mut!(later);

        unsafe {
            reset_synthetic_scheduler_state();
            initialise_synthetic_tcb(current_ptr, 1);
            initialise_synthetic_tcb(notified_ptr, 3);
            initialise_synthetic_tcb(later_ptr, 0);

            prvAddTaskToReadyList(current_ptr);
            listSET_LIST_ITEM_VALUE(ptr::addr_of_mut!((*notified_ptr).xStateListItem), 10);
            listSET_LIST_ITEM_VALUE(ptr::addr_of_mut!((*later_ptr).xStateListItem), 20);
            vListInsert(
                pxDelayedTaskList,
                ptr::addr_of_mut!((*notified_ptr).xStateListItem),
            );
            vListInsert(
                pxDelayedTaskList,
                ptr::addr_of_mut!((*later_ptr).xStateListItem),
            );
            prvWriteNotifiedValue(notified_ptr, 0, u32::MAX);
            prvWriteNotifyState(notified_ptr, 0, taskWAITING_NOTIFICATION);

            pxCurrentTCB = current_ptr;
            uxCurrentNumberOfTasks = 3;
            xSchedulerRunning = pdTRUE;
            xNextTaskUnblockTime = 10;

            assert_eq!(
                xTaskGenericNotify(
                    notified_ptr.cast(),
                    0,
                    0,
                    eNotifyAction::eIncrement as i32,
                    ptr::null_mut(),
                ),
                pdPASS
            );
            assert_eq!(prvReadNotifiedValue(notified_ptr, 0), 0);
            assert_eq!(xNextTaskUnblockTime, 20);
            assert_eq!(crate::port::test_port_snapshot().yield_count, 1);

            reset_synthetic_scheduler_state();
        }
        crate::port::test_port_reset();
    }

    #[cfg(all(feature = "port-test", feature = "tickless-idle"))]
    #[test]
    fn isr_notification_wakeup_updates_deadline_and_yield_outputs() {
        crate::port::test_port_reset();
        let mut current = TCB_t::new();
        let mut notified = TCB_t::new();
        let mut later = TCB_t::new();
        let mut xHigherPriorityTaskWoken = pdFALSE;
        let current_ptr = ptr::addr_of_mut!(current);
        let notified_ptr = ptr::addr_of_mut!(notified);
        let later_ptr = ptr::addr_of_mut!(later);

        unsafe {
            reset_synthetic_scheduler_state();
            initialise_synthetic_tcb(current_ptr, 1);
            initialise_synthetic_tcb(notified_ptr, 3);
            initialise_synthetic_tcb(later_ptr, 0);

            prvAddTaskToReadyList(current_ptr);
            listSET_LIST_ITEM_VALUE(ptr::addr_of_mut!((*notified_ptr).xStateListItem), 10);
            listSET_LIST_ITEM_VALUE(ptr::addr_of_mut!((*later_ptr).xStateListItem), 20);
            vListInsert(
                pxDelayedTaskList,
                ptr::addr_of_mut!((*notified_ptr).xStateListItem),
            );
            vListInsert(
                pxDelayedTaskList,
                ptr::addr_of_mut!((*later_ptr).xStateListItem),
            );
            prvWriteNotifiedValue(notified_ptr, 0, u32::MAX);
            prvWriteNotifyState(notified_ptr, 0, taskWAITING_NOTIFICATION);

            pxCurrentTCB = current_ptr;
            uxCurrentNumberOfTasks = 3;
            xSchedulerRunning = pdTRUE;
            xNextTaskUnblockTime = 10;
            crate::port::test_port_set_inside_interrupt(true);

            assert_eq!(
                xTaskGenericNotifyFromISR(
                    notified_ptr.cast(),
                    0,
                    0,
                    eNotifyAction::eIncrement as i32,
                    ptr::null_mut(),
                    &mut xHigherPriorityTaskWoken,
                ),
                pdPASS
            );
            crate::port::test_port_set_inside_interrupt(false);

            assert_eq!(prvReadNotifiedValue(notified_ptr, 0), 0);
            assert_eq!(xHigherPriorityTaskWoken, pdTRUE);
            assert_eq!(xYieldPendings[0], pdTRUE);
            assert_eq!(xNextTaskUnblockTime, 20);

            reset_synthetic_scheduler_state();
        }
        crate::port::test_port_reset();
    }

    #[cfg(all(feature = "port-test", feature = "task-suspend"))]
    #[test]
    fn maximum_timeout_is_indefinite_only_with_task_suspend_enabled() {
        crate::port::test_port_reset();
        let mut current = TCB_t::new();
        let mut timeout = TimeOut_t {
            xOverflowCount: 0,
            xTimeOnEntering: 5,
        };
        let mut ticks = portMAX_DELAY;
        let current_ptr = ptr::addr_of_mut!(current);

        unsafe {
            reset_synthetic_scheduler_state();
            initialise_synthetic_tcb(current_ptr, 1);
            pxCurrentTCB = current_ptr;
            xTickCount = 10;
            xNumOfOverflows = 1;

            assert_eq!(xTaskCheckForTimeOut(&mut timeout, &mut ticks), pdFALSE);
            assert_eq!(ticks, portMAX_DELAY);

            reset_synthetic_scheduler_state();
        }
        crate::port::test_port_reset();
    }

    #[cfg(all(feature = "port-test", not(feature = "task-suspend")))]
    #[test]
    fn maximum_timeout_remains_finite_without_task_suspend() {
        crate::port::test_port_reset();
        let mut current = TCB_t::new();
        let mut timeout = TimeOut_t {
            xOverflowCount: 0,
            xTimeOnEntering: 5,
        };
        let mut ticks = portMAX_DELAY;
        let current_ptr = ptr::addr_of_mut!(current);

        unsafe {
            reset_synthetic_scheduler_state();
            initialise_synthetic_tcb(current_ptr, 1);
            pxCurrentTCB = current_ptr;
            xTickCount = 10;
            xNumOfOverflows = 1;

            assert_eq!(xTaskCheckForTimeOut(&mut timeout, &mut ticks), pdTRUE);
            assert_eq!(ticks, 0);

            reset_synthetic_scheduler_state();
        }
        crate::port::test_port_reset();
    }

    #[cfg(feature = "stats-formatting")]
    #[test]
    fn invalid_utf8_task_names_are_formatted_without_unchecked_str() {
        let name = [0xff_u8, 0];
        // SAFETY: name remains readable through its NUL terminator for the
        // returned borrow, which is consumed by this assertion.
        assert_eq!(unsafe { prvGetTaskNameFromPtr(name.as_ptr()) }, "<invalid>");
    }

    #[cfg(all(feature = "port-test", feature = "stack-high-water-mark"))]
    #[test]
    fn high_water_scan_is_bounded_and_reports_whole_unused_stack() {
        let mut stack = [0 as StackType_t; 8];
        unsafe {
            ptr::write_bytes(
                stack.as_mut_ptr().cast::<u8>(),
                tskSTACK_FILL_BYTE,
                core::mem::size_of_val(&stack),
            );
        }

        let stack_ptr = stack.as_mut_ptr();
        let mut tcb = TCB_t::new();
        tcb.pxStack = stack_ptr;
        tcb.uxStackDepth = stack.len();

        assert_eq!(
            unsafe { uxTaskGetStackHighWaterMark2((&mut tcb as *mut TCB_t).cast()) },
            stack.len()
        );

        unsafe {
            *stack_ptr
                .cast::<u8>()
                .add(3 * core::mem::size_of::<StackType_t>()) = 0;
        }
        assert_eq!(
            unsafe { uxTaskGetStackHighWaterMark2((&mut tcb as *mut TCB_t).cast()) },
            3
        );
    }

    #[cfg(all(feature = "port-test", feature = "stack-overflow-check"))]
    #[test]
    fn corrupted_stack_guard_calls_registered_application_hook() {
        crate::port::test_port_reset();
        STACK_OVERFLOW_CALLS.store(0, Ordering::SeqCst);

        let mut stack = [0 as StackType_t; 8];
        let stack_ptr = stack.as_mut_ptr();
        unsafe {
            ptr::write_bytes(
                stack_ptr.cast::<u8>(),
                tskSTACK_FILL_BYTE,
                core::mem::size_of_val(&stack),
            );
        }

        let mut tcb = TCB_t::new();
        tcb.pxStack = stack_ptr;
        tcb.uxStackDepth = stack.len();
        tcb.pxTopOfStack = unsafe { stack_ptr.add(stack.len() - 1) };

        unsafe {
            xSchedulerRunning = pdFALSE;
            pxCurrentTCB = &mut tcb;
        }
        assert_eq!(vTaskSetStackOverflowHook(Some(stack_overflow_hook)), pdPASS);

        unsafe {
            *stack_ptr.cast::<u8>() = 0;
            taskCHECK_FOR_STACK_OVERFLOW();
        }

        assert_eq!(STACK_OVERFLOW_CALLS.load(Ordering::SeqCst), 1);
        assert_eq!(vTaskSetStackOverflowHook(None), pdPASS);
        unsafe {
            pxCurrentTCB = ptr::null_mut();
        }
    }

    #[cfg(all(feature = "port-test", feature = "generate-run-time-stats"))]
    #[test]
    fn runtime_delta_handles_counter_wrap_and_current_interval() {
        assert_eq!(prvRunTimeDelta(3, u32::MAX - 1), 5);

        crate::port::test_port_reset();
        let mut tcb = TCB_t::new();
        tcb.ulRunTimeCounter = 7;
        unsafe {
            pxCurrentTCB = &mut tcb;
            xSchedulerRunning = pdTRUE;
            ulRunTimeStart = 0;
            ulTaskSwitchedInTime = 0;
        }
        crate::port::portINCREMENT_RUN_TIME_COUNTER();
        crate::port::portINCREMENT_RUN_TIME_COUNTER();

        // SAFETY: the synthetic TCB remains live and uniquely owned here.
        assert_eq!(
            unsafe { ulTaskGetRunTimeCounter((&mut tcb as *mut TCB_t).cast()) },
            9
        );
        assert_eq!(ulTaskGetTotalRunTime(), 2);

        unsafe {
            xSchedulerRunning = pdFALSE;
            pxCurrentTCB = ptr::null_mut();
        }
    }

    #[cfg(all(feature = "port-test", feature = "trace-facility"))]
    #[test]
    fn task_status_uses_unique_tcb_number_and_reports_current_task_prestart() {
        let mut stack = [0 as StackType_t; 1];
        unsafe {
            ptr::write_bytes(
                stack.as_mut_ptr().cast::<u8>(),
                tskSTACK_FILL_BYTE,
                core::mem::size_of_val(&stack),
            );
        }

        let mut tcb = TCB_t::new();
        tcb.pxStack = stack.as_mut_ptr();
        tcb.uxStackDepth = stack.len();
        tcb.uxTCBNumber = 41;
        tcb.uxTaskNumber = 99;
        let mut status = crate::types::TaskStatus_t::new();

        unsafe {
            xSchedulerRunning = pdFALSE;
            pxCurrentTCB = &mut tcb;
            vTaskGetInfo(
                (&mut tcb as *mut TCB_t).cast(),
                &mut status,
                pdTRUE,
                eTaskState::eReady,
            );
            pxCurrentTCB = ptr::null_mut();
        }

        assert_eq!(status.xTaskNumber, 41);
        assert_eq!(status.usStackHighWaterMark, 1);
        assert_eq!(status.eCurrentState, eTaskState::eRunning as u8);
    }

    #[cfg(all(
        feature = "port-test",
        feature = "trace-facility",
        feature = "task-suspend"
    ))]
    #[test]
    fn task_info_corrects_indefinite_block_and_pending_ready_states() {
        crate::port::test_port_reset();
        let mut current = TCB_t::new();
        let mut target = TCB_t::new();
        let mut status = crate::types::TaskStatus_t::new();
        let current_ptr = ptr::addr_of_mut!(current);
        let target_ptr = ptr::addr_of_mut!(target);

        unsafe {
            reset_synthetic_scheduler_state();
            initialise_synthetic_tcb(current_ptr, 3);
            initialise_synthetic_tcb(target_ptr, 1);
            prvAddTaskToReadyList(current_ptr);
            vListInsertEnd(
                ptr::addr_of_mut!(xSuspendedTaskList),
                ptr::addr_of_mut!((*target_ptr).xStateListItem),
            );
            prvWriteNotifyState(target_ptr, 0, taskWAITING_NOTIFICATION);

            pxCurrentTCB = current_ptr;
            uxCurrentNumberOfTasks = 2;
            xSchedulerRunning = pdTRUE;

            vTaskGetInfo(
                target_ptr.cast(),
                &mut status,
                pdFALSE,
                eTaskState::eSuspended,
            );
            assert_eq!(status.eCurrentState, eTaskState::eBlocked as u8);

            prvWriteNotifyState(target_ptr, 0, taskNOT_WAITING_NOTIFICATION);
            vListInsertEnd(
                ptr::addr_of_mut!(xPendingReadyList),
                ptr::addr_of_mut!((*target_ptr).xEventListItem),
            );
            uxSchedulerSuspended = 1;
            vTaskGetInfo(
                target_ptr.cast(),
                &mut status,
                pdFALSE,
                eTaskState::eSuspended,
            );
            assert_eq!(status.eCurrentState, eTaskState::eReady as u8);
            let _ = xTaskResumeAll();

            reset_synthetic_scheduler_state();
        }
        crate::port::test_port_reset();
    }

    #[cfg(feature = "port-test")]
    #[test]
    fn catch_up_ticks_processes_each_elapsed_tick() {
        crate::port::test_port_reset();
        let mut current = TCB_t::new();
        let current_ptr = ptr::addr_of_mut!(current);

        unsafe {
            reset_synthetic_scheduler_state();
            initialise_synthetic_tcb(current_ptr, 1);
            prvAddTaskToReadyList(current_ptr);
            pxCurrentTCB = current_ptr;
            uxCurrentNumberOfTasks = 1;
            xSchedulerRunning = pdTRUE;

            assert_eq!(xTaskCatchUpTicks(3), pdFALSE);
            assert_eq!(xTickCount, configINITIAL_TICK_COUNT.wrapping_add(3));
            assert_eq!(xPendedTicks, 0);

            reset_synthetic_scheduler_state();
        }
        crate::port::test_port_reset();
    }

    #[cfg(all(feature = "port-test", feature = "task-delete"))]
    #[test]
    fn idle_cleanup_skips_critical_section_when_nothing_is_pending() {
        crate::port::test_port_reset();
        unsafe {
            reset_synthetic_scheduler_state();
            prvCheckTasksWaitingTermination();
        }
        let snapshot = crate::port::test_port_snapshot();
        assert_eq!(snapshot.interrupt_disable_count, 0);
        assert_eq!(snapshot.interrupt_enable_count, 0);
    }

    #[cfg(all(feature = "port-test", feature = "trace-facility"))]
    #[test]
    fn empty_system_state_accepts_null_zero_length_array() {
        crate::port::test_port_reset();
        let mut total = 123;
        unsafe {
            reset_synthetic_scheduler_state();
            assert_eq!(uxTaskGetSystemState(ptr::null_mut(), 0, &mut total), 0);
        }
    }

    #[cfg(feature = "port-test")]
    #[test]
    fn scheduler_suspend_preserves_preexisting_interrupt_mask() {
        crate::port::test_port_reset();
        unsafe {
            uxSchedulerSuspended = 0;
        }

        portDISABLE_INTERRUPTS();
        let before = crate::port::test_port_snapshot();
        assert!(before.interrupts_masked);

        // Safety: the test exclusively owns synthetic scheduler state. It
        // deliberately inspects the suspended state, then restores the global
        // nesting value below without allowing any task or ISR to run.
        unsafe { vTaskSuspendAll() };

        let after = crate::port::test_port_snapshot();
        assert!(after.interrupts_masked);
        assert_eq!(after.interrupt_enable_count, before.interrupt_enable_count);
        unsafe {
            uxSchedulerSuspended = 0;
        }
    }

    #[cfg(all(feature = "port-test", feature = "task-suspend"))]
    #[test]
    fn resume_from_isr_moves_a_live_suspended_task_to_ready() {
        crate::port::test_port_reset();
        let mut current = TCB_t::new();
        current.uxPriority = 1;
        let mut resumed = TCB_t::new();
        resumed.uxPriority = 3;
        let current_ptr = ptr::addr_of_mut!(current);
        let resumed_ptr = ptr::addr_of_mut!(resumed);

        // SAFETY: both synthetic TCBs remain live and uniquely owned for the
        // whole operation. Their list items and global lists are initialized
        // before use, and the test port is explicitly placed in ISR context.
        unsafe {
            prvInitialiseTaskLists();
            vListInitialiseItem(ptr::addr_of_mut!((*resumed_ptr).xStateListItem));
            vListInitialiseItem(ptr::addr_of_mut!((*resumed_ptr).xEventListItem));
            listSET_LIST_ITEM_OWNER(
                ptr::addr_of_mut!((*resumed_ptr).xStateListItem),
                resumed_ptr.cast(),
            );
            listSET_LIST_ITEM_OWNER(
                ptr::addr_of_mut!((*resumed_ptr).xEventListItem),
                resumed_ptr.cast(),
            );
            vListInsertEnd(
                ptr::addr_of_mut!(xSuspendedTaskList),
                ptr::addr_of_mut!((*resumed_ptr).xStateListItem),
            );
            pxCurrentTCB = current_ptr;
            uxSchedulerSuspended = 0;

            crate::port::test_port_set_inside_interrupt(true);
            let xYieldRequired = xTaskResumeFromISR(resumed_ptr.cast());
            crate::port::test_port_set_inside_interrupt(false);

            assert_eq!(xYieldRequired, pdTRUE);
            assert!(
                listIS_CONTAINED_WITHIN(
                    ptr::addr_of!(pxReadyTasksLists[(*resumed_ptr).uxPriority as usize]),
                    ptr::addr_of!((*resumed_ptr).xStateListItem),
                ) != pdFALSE
            );
            assert!(
                listIS_CONTAINED_WITHIN(
                    ptr::addr_of!(xSuspendedTaskList),
                    ptr::addr_of!((*resumed_ptr).xStateListItem),
                ) == pdFALSE
            );

            // Detach stack-owned list items before their backing TCBs drop.
            prvInitialiseTaskLists();
            pxCurrentTCB = ptr::null_mut();
            xYieldPendings[0] = pdFALSE;
        }
        crate::port::test_port_reset();
    }

    #[cfg(feature = "port-test")]
    #[test]
    fn event_list_keys_are_unique_and_priority_ordered() {
        crate::port::test_port_reset();
        let mut previous_key: Option<TickType_t> = None;

        let mut uxPriority = 0;
        while uxPriority < configMAX_PRIORITIES {
            let mut tcb = TCB_t::new();
            let mut stack_word: StackType_t = 0;
            let tcb_ptr = ptr::addr_of_mut!(tcb);

            unsafe {
                prvInitialiseNewTask(
                    test_task,
                    b"test\0".as_ptr(),
                    1,
                    ptr::null_mut(),
                    uxPriority,
                    ptr::null_mut(),
                    tcb_ptr,
                    &mut stack_word,
                );
            }

            let key = unsafe { listGET_LIST_ITEM_VALUE(ptr::addr_of!((*tcb_ptr).xEventListItem)) };
            assert_eq!(key, (configMAX_PRIORITIES - uxPriority) as TickType_t);
            if let Some(previous) = previous_key {
                assert!(key < previous);
            }
            previous_key = Some(key);
            uxPriority += 1;
        }
    }

    #[cfg(all(feature = "port-test", feature = "use-mutexes"))]
    #[test]
    fn priority_inheritance_reports_both_new_and_existing_inheritance() {
        crate::port::test_port_reset();

        let mut holder = TCB_t::new();
        holder.uxMutexesHeld = 1;

        let mut waiter = TCB_t::new();
        let holder_ptr = ptr::addr_of_mut!(holder);
        let waiter_ptr = ptr::addr_of_mut!(waiter);

        // SAFETY: these synthetic TCBs and list items remain live and uniquely
        // owned throughout the test; pxCurrentTCB is set to each synthetic
        // waiter before invoking the inheritance helper.
        unsafe {
            initialise_synthetic_mutex_tcb(holder_ptr, 1);
            initialise_synthetic_mutex_tcb(waiter_ptr, 3);
            pxCurrentTCB = waiter_ptr;
            assert_eq!(xTaskPriorityInherit(holder_ptr.cast()), pdTRUE);
            assert_eq!((*holder_ptr).uxPriority, 3);
            assert_eq!(
                listGET_LIST_ITEM_VALUE(ptr::addr_of!((*holder_ptr).xEventListItem)),
                (configMAX_PRIORITIES - (*waiter_ptr).uxPriority) as TickType_t
            );

            /* A second waiter below an already inherited priority still needs
             * a true result so timeout disinheritance is performed later. */
            let mut second_waiter = TCB_t::new();
            let second_waiter_ptr = ptr::addr_of_mut!(second_waiter);
            initialise_synthetic_mutex_tcb(second_waiter_ptr, 2);
            pxCurrentTCB = second_waiter_ptr;
            assert_eq!(xTaskPriorityInherit(holder_ptr.cast()), pdTRUE);
            assert_eq!((*holder_ptr).uxPriority, 3);
            pxCurrentTCB = ptr::null_mut();
        }
    }

    #[cfg(all(feature = "port-test", feature = "use-mutexes"))]
    #[test]
    fn timeout_disinherits_only_sole_mutex_and_repositions_ready_task() {
        crate::port::test_port_reset();

        let mut holder = TCB_t::new();
        holder.uxMutexesHeld = 2;
        let holder_ptr = ptr::addr_of_mut!(holder);

        let mut waiter = TCB_t::new();
        let waiter_ptr = ptr::addr_of_mut!(waiter);

        // SAFETY: the synthetic holder and waiter remain live and uniquely
        // owned; the ready lists are initialized before the raw priority/list
        // helper is invoked under the test's exclusive kernel state.
        unsafe {
            initialise_synthetic_mutex_tcb(holder_ptr, 4);
            (*holder_ptr).uxBasePriority = 1;
            initialise_synthetic_mutex_tcb(waiter_ptr, 3);
            prvInitialiseTaskLists();
            prvAddTaskToReadyList(holder_ptr);
            pxCurrentTCB = waiter_ptr;

            vTaskPriorityDisinheritAfterTimeout(holder_ptr.cast(), 2);
            assert_eq!((*holder_ptr).uxPriority, 4);
            assert!(
                listIS_CONTAINED_WITHIN(
                    ptr::addr_of!(pxReadyTasksLists[4]),
                    ptr::addr_of!((*holder_ptr).xStateListItem),
                ) != pdFALSE
            );

            (*holder_ptr).uxMutexesHeld = 1;
            vTaskPriorityDisinheritAfterTimeout(holder_ptr.cast(), 2);
            assert_eq!((*holder_ptr).uxPriority, 2);
            assert_eq!(
                listGET_LIST_ITEM_VALUE(ptr::addr_of!((*holder_ptr).xEventListItem)),
                (configMAX_PRIORITIES - 2) as TickType_t
            );
            assert!(
                listIS_CONTAINED_WITHIN(
                    ptr::addr_of!(pxReadyTasksLists[2]),
                    ptr::addr_of!((*holder_ptr).xStateListItem),
                ) != pdFALSE
            );
            pxCurrentTCB = ptr::null_mut();
        }
    }

    #[cfg(all(feature = "port-test", feature = "use-mutexes"))]
    #[test]
    fn giving_last_of_multiple_mutexes_restores_base_priority() {
        crate::port::test_port_reset();

        let mut holder = TCB_t::new();
        holder.uxMutexesHeld = 2;
        let holder_ptr = ptr::addr_of_mut!(holder);

        // SAFETY: the synthetic holder is installed as the current task and
        // remains live, uniquely owned, and correctly linked for both calls.
        unsafe {
            initialise_synthetic_mutex_tcb(holder_ptr, 4);
            (*holder_ptr).uxBasePriority = 1;
            prvInitialiseTaskLists();
            prvAddTaskToReadyList(holder_ptr);
            pxCurrentTCB = holder_ptr;

            assert_eq!(xTaskPriorityDisinherit(holder_ptr.cast()), pdFALSE);
            assert_eq!((*holder_ptr).uxMutexesHeld, 1);
            assert_eq!((*holder_ptr).uxPriority, 4);

            assert_eq!(xTaskPriorityDisinherit(holder_ptr.cast()), pdTRUE);
            assert_eq!((*holder_ptr).uxMutexesHeld, 0);
            assert_eq!((*holder_ptr).uxPriority, 1);
            assert!(
                listIS_CONTAINED_WITHIN(
                    ptr::addr_of!(pxReadyTasksLists[1]),
                    ptr::addr_of!((*holder_ptr).xStateListItem),
                ) != pdFALSE
            );
            pxCurrentTCB = ptr::null_mut();
        }
    }
}
