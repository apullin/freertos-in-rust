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

/// Scheduler has not been started yet
pub const taskSCHEDULER_NOT_STARTED: BaseType_t = 0;

/// Scheduler is running
pub const taskSCHEDULER_RUNNING: BaseType_t = 1;

/// Scheduler is suspended
pub const taskSCHEDULER_SUSPENDED: BaseType_t = 2;

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
#[cfg(feature = "tick-16bit")]
const taskEVENT_LIST_ITEM_VALUE_IN_USE: TickType_t = 0x8000;

#[cfg(feature = "tick-32bit")]
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
/// [AMENDMENT] This structure must have pxTopOfStack as the first member
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

    /// Holds the critical section nesting depth for ports that do not maintain
    /// their own count in the port layer.
    #[cfg(feature = "critical-nesting-in-tcb")]
    pub uxCriticalNesting: UBaseType_t,

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

    /// POSIX errno for this task.
    #[cfg(feature = "posix-errno")]
    pub iTaskErrno: i32,

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
    /// [AMENDMENT] In C this would be memset to 0. We initialize all fields explicitly.
    pub const fn new() -> Self {
        tskTaskControlBlock {
            pxTopOfStack: ptr::null_mut(),
            xStateListItem: ListItem_t::new(),
            xEventListItem: ListItem_t::new(),
            uxPriority: 0,
            pxStack: ptr::null_mut(),
            pcTaskName: [0u8; configMAX_TASK_NAME_LEN],

            #[cfg(any(not(feature = "arch-32bit"), feature = "record-stack-high-address"))]
            pxEndOfStack: ptr::null_mut(),

            #[cfg(feature = "critical-nesting-in-tcb")]
            uxCriticalNesting: 0,

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

            #[cfg(feature = "posix-errno")]
            iTaskErrno: 0,

            #[cfg(feature = "thread-local-storage")]
            pvThreadLocalStoragePointers: [ptr::null_mut(); configNUM_THREAD_LOCAL_STORAGE_POINTERS],

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
/// [AMENDMENT] Users provide this buffer for static task allocation.
#[repr(C)]
pub struct StaticTask_t {
    /// Reserved space for TCB - must be large enough
    _reserved: [u8; core::mem::size_of::<tskTaskControlBlock>()],
}

impl StaticTask_t {
    pub const fn new() -> Self {
        StaticTask_t {
            _reserved: [0u8; core::mem::size_of::<tskTaskControlBlock>()],
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
/// [AMENDMENT] Exported with #[no_mangle] for assembly access in port layer.
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
static mut xYieldPendings: [BaseType_t; configNUMBER_OF_CORES as usize] = [pdFALSE; configNUMBER_OF_CORES as usize];

/// Number of tick count overflows.
static mut xNumOfOverflows: BaseType_t = 0;

/// Task number counter for unique task identification.
static mut uxTaskNumber: UBaseType_t = 0;

/// Time at which the next blocked task will unblock.
static mut xNextTaskUnblockTime: TickType_t = 0;

/// Handle to the idle task(s).
static mut xIdleTaskHandles: [TaskHandle_t; configNUMBER_OF_CORES as usize] =
    [ptr::null_mut(); configNUMBER_OF_CORES as usize];

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

/// The total accumulated run time.
/// This is updated in vTaskSwitchContext() by calling portGET_RUN_TIME_COUNTER_VALUE().
#[cfg(feature = "generate-run-time-stats")]
static mut ulTotalRunTime: configRUN_TIME_COUNTER_TYPE = 0;

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
    while listLIST_IS_EMPTY(&pxReadyTasksLists[uxTopPriority as usize]) != pdFALSE {
        configASSERT(uxTopPriority > 0);
        uxTopPriority -= 1;
    }

    // Get the next task from that priority's list (round-robin within priority).
    pxCurrentTCB = listGET_OWNER_OF_NEXT_ENTRY(&mut pxReadyTasksLists[uxTopPriority as usize]) as *mut TCB_t;
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
        &mut pxReadyTasksLists[(*pxTCB).uxPriority as usize],
        &mut (*pxTCB).xStateListItem,
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

/// Yield within API if using preemption.
#[inline(always)]
pub fn portYIELD_WITHIN_API() {
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
        vListInitialise(&mut pxReadyTasksLists[uxPriority]);
    }

    // Initialize the delayed task lists.
    vListInitialise(&mut xDelayedTaskList1);
    vListInitialise(&mut xDelayedTaskList2);

    // Initialize pending ready list.
    vListInitialise(&mut xPendingReadyList);

    // Initialize terminated tasks list.
    vListInitialise(&mut xTasksWaitingTermination);

    // Initialize suspended tasks list.
    vListInitialise(&mut xSuspendedTaskList);

    // Set delayed list pointers.
    pxDelayedTaskList = &mut xDelayedTaskList1;
    pxOverflowDelayedTaskList = &mut xDelayedTaskList2;
}

/// Reset xNextTaskUnblockTime to the wake time of the next blocked task.
unsafe fn prvResetNextTaskUnblockTime() {
    if listLIST_IS_EMPTY(pxDelayedTaskList) != pdFALSE {
        // No tasks waiting, set to max.
        xNextTaskUnblockTime = portMAX_DELAY;
    } else {
        // Get the wake time of the first task in the delayed list.
        let pxTCB = listGET_OWNER_OF_HEAD_ENTRY(pxDelayedTaskList) as *mut TCB_t;
        xNextTaskUnblockTime = listGET_LIST_ITEM_VALUE(&(*pxTCB).xStateListItem);
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

        uxTaskNumber += 1;

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
    vListInitialiseItem(&mut (*pxNewTCB).xStateListItem);
    vListInitialiseItem(&mut (*pxNewTCB).xEventListItem);

    // Set the owner of the list items to this TCB.
    listSET_LIST_ITEM_OWNER(&mut (*pxNewTCB).xStateListItem, pxNewTCB as *mut c_void);

    // Event list items are stored in reverse priority order.
    listSET_LIST_ITEM_VALUE(
        &mut (*pxNewTCB).xEventListItem,
        (configMAX_PRIORITIES - 1 - uxPriority) as TickType_t,
    );
    listSET_LIST_ITEM_OWNER(&mut (*pxNewTCB).xEventListItem, pxNewTCB as *mut c_void);

    // Initialize notifications.
    for i in 0..configTASK_NOTIFICATION_ARRAY_ENTRIES {
        (*pxNewTCB).ulNotifiedValue[i] = 0;
        (*pxNewTCB).ucNotifyState[i] = taskNOT_WAITING_NOTIFICATION;
    }

    #[cfg(feature = "critical-nesting-in-tcb")]
    {
        (*pxNewTCB).uxCriticalNesting = 0;
    }

    #[cfg(feature = "application-task-tag")]
    {
        (*pxNewTCB).pxTaskTag = None;
    }

    #[cfg(feature = "abort-delay")]
    {
        (*pxNewTCB).ucDelayAborted = pdFALSE as u8;
    }

    // Initialize the stack.
    // pxPortInitialiseStack is provided by the port layer.
    (*pxNewTCB).pxTopOfStack = pxPortInitialiseStack(
        pxTopOfStack,
        pxTaskCode,
        pvParameters,
    );

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

    // Remove the task from the ready list before adding to delayed list.
    let _ux = uxListRemove(&mut (*pxCurrentTCB).xStateListItem);

    if xTicksToWait == portMAX_DELAY && xCanBlockIndefinitely != pdFALSE {
        // Add to the suspended list instead (wait forever).
        vListInsertEnd(&mut xSuspendedTaskList, &mut (*pxCurrentTCB).xStateListItem);
    } else {
        // Calculate the time at which the task should wake.
        let xTimeToWake = xConstTickCount.wrapping_add(xTicksToWait);

        // Set the wake time as the list item value.
        listSET_LIST_ITEM_VALUE(&mut (*pxCurrentTCB).xStateListItem, xTimeToWake);

        if xTimeToWake < xConstTickCount {
            // Wake time has overflowed, add to overflow list.
            vListInsert(pxOverflowDelayedTaskList, &mut (*pxCurrentTCB).xStateListItem);
        } else {
            // Add to the delayed list.
            vListInsert(pxDelayedTaskList, &mut (*pxCurrentTCB).xStateListItem);

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
        taskENTER_CRITICAL();
        {
            while uxDeletedTasksWaitingCleanUp > 0 {
                // Get the TCB of the deleted task.
                let pxTCB = listGET_OWNER_OF_HEAD_ENTRY(&xTasksWaitingTermination) as *mut TCB_t;
                let _ux = uxListRemove(&mut (*pxTCB).xStateListItem);

                uxCurrentNumberOfTasks -= 1;
                uxDeletedTasksWaitingCleanUp -= 1;

                taskEXIT_CRITICAL();

                // Free the TCB (and stack if dynamically allocated).
                prvDeleteTCB(pxTCB);

                taskENTER_CRITICAL();
            }
        }
        taskEXIT_CRITICAL();
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

        // Idle hook (if enabled).
        if configUSE_IDLE_HOOK != 0 {
            // TODO: Call vApplicationIdleHook
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
                vTaskSuspendAll();
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
                xTaskResumeAll();
            }
        }

        // Yield to allow other tasks at same priority to run.
        #[cfg(feature = "idle-yield")]
        {
            portYIELD_WITHIN_API();
        }
    }
}

/// Create the idle task(s).
unsafe fn prvCreateIdleTasks() -> BaseType_t {
    // Create the idle task.
    let xReturn: BaseType_t;

    #[cfg(any(feature = "alloc", feature = "heap-4"))]
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

    #[cfg(not(any(feature = "alloc", feature = "heap-4")))]
    {
        // Static allocation required but not yet implemented
        xReturn = pdFAIL;
    }

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
#[cfg(any(feature = "alloc", feature = "heap-4"))]
pub unsafe fn xTaskCreate(
    pxTaskCode: TaskFunction_t,
    pcName: *const u8,
    uxStackDepth: configSTACK_DEPTH_TYPE,
    pvParameters: *mut c_void,
    uxPriority: UBaseType_t,
    pxCreatedTask: *mut TaskHandle_t,
) -> BaseType_t {
    let xReturn: BaseType_t;

    // Allocate the TCB.
    let pxNewTCB = crate::memory::pvPortMalloc(
        core::mem::size_of::<TCB_t>()
    ) as *mut TCB_t;

    if !pxNewTCB.is_null() {
        // Allocate the stack.
        let pxStack = crate::memory::pvPortMalloc(
            uxStackDepth * core::mem::size_of::<StackType_t>()
        ) as *mut StackType_t;

        if !pxStack.is_null() {
            (*pxNewTCB).pxStack = pxStack;

            // Calculate top of stack based on stack growth direction.
            let pxTopOfStack: *mut StackType_t;
            if portSTACK_GROWTH < 0 {
                // Stack grows down - top is at highest address.
                pxTopOfStack = pxStack.add(uxStackDepth - 1);

                // Align the stack.
                let aligned = (pxTopOfStack as usize) & !(portBYTE_ALIGNMENT - 1);
                let pxTopOfStack = aligned as *mut StackType_t;

                // Record end of stack for stack overflow detection.
                #[cfg(any(not(feature = "arch-32bit"), feature = "record-stack-high-address"))]
                {
                    (*pxNewTCB).pxEndOfStack = pxStack.add(uxStackDepth - 1);
                }
            } else {
                // Stack grows up - top is at lowest address.
                pxTopOfStack = pxStack;

                // Record end of stack for stack overflow detection.
                #[cfg(any(not(feature = "arch-32bit"), feature = "record-stack-high-address"))]
                {
                    (*pxNewTCB).pxEndOfStack = pxStack.add(uxStackDepth - 1);
                }
            }

            // Fill stack with known value for high water mark (if enabled).
            if tskSET_NEW_STACKS_TO_KNOWN_VALUE != 0 {
                ptr::write_bytes(pxStack as *mut u8, tskSTACK_FILL_BYTE,
                    uxStackDepth * core::mem::size_of::<StackType_t>());
            }

            // Mark as dynamically allocated.
            (*pxNewTCB).ucStaticallyAllocated = tskDYNAMICALLY_ALLOCATED_STACK_AND_TCB;

            // Initialize the task.
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

            // Add to ready list.
            prvAddNewTaskToReadyList(pxNewTCB);

            xReturn = pdPASS;
        } else {
            // Stack allocation failed, free TCB.
            crate::memory::vPortFree(pxNewTCB as *mut c_void);
            xReturn = errCOULD_NOT_ALLOCATE_REQUIRED_MEMORY;
        }
    } else {
        xReturn = errCOULD_NOT_ALLOCATE_REQUIRED_MEMORY;
    }

    xReturn
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

    if !puxStackBuffer.is_null() && !pxTaskBuffer.is_null() {
        // Use the provided TCB buffer.
        let pxNewTCB = pxTaskBuffer as *mut TCB_t;

        // Zero the TCB.
        ptr::write_bytes(pxNewTCB, 0, 1);

        (*pxNewTCB).pxStack = puxStackBuffer;

        // Calculate top of stack.
        let pxTopOfStack: *mut StackType_t;
        if portSTACK_GROWTH < 0 {
            let pxTop = puxStackBuffer.add(uxStackDepth - 1);
            let aligned = (pxTop as usize) & !(portBYTE_ALIGNMENT - 1);
            pxTopOfStack = aligned as *mut StackType_t;

            #[cfg(any(not(feature = "arch-32bit"), feature = "record-stack-high-address"))]
            {
                (*pxNewTCB).pxEndOfStack = puxStackBuffer.add(uxStackDepth - 1);
            }
        } else {
            pxTopOfStack = puxStackBuffer;

            #[cfg(any(not(feature = "arch-32bit"), feature = "record-stack-high-address"))]
            {
                (*pxNewTCB).pxEndOfStack = puxStackBuffer.add(uxStackDepth - 1);
            }
        }

        // Fill stack with known value if enabled.
        if tskSET_NEW_STACKS_TO_KNOWN_VALUE != 0 {
            ptr::write_bytes(puxStackBuffer as *mut u8, tskSTACK_FILL_BYTE,
                uxStackDepth * core::mem::size_of::<StackType_t>());
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
pub fn vTaskStartScheduler() {
    unsafe {
        // Create idle task(s).
        let xReturn = prvCreateIdleTasks();

        if xReturn == pdPASS {
            // Create the timer task if timers are enabled.
            #[cfg(feature = "timers")]
            {
                crate::kernel::timers::xTimerCreateTimerTask();
            }

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
pub fn vTaskEndScheduler() {
    unsafe {
        portDISABLE_INTERRUPTS();
        xSchedulerRunning = pdFALSE;
        vPortEndScheduler();
    }
}

/// Suspend all tasks (enter scheduler suspension).
///
/// While suspended, context switches will not occur and no task will run
/// except the current one. Call xTaskResumeAll() to resume.
pub fn vTaskSuspendAll() {
    unsafe {
        // Increment suspension count atomically (no interrupts during increment).
        portDISABLE_INTERRUPTS();
        uxSchedulerSuspended += 1;
        portENABLE_INTERRUPTS();
    }
}

/// Resume all tasks after suspension.
///
/// # Returns
///
/// pdTRUE if a context switch occurred, pdFALSE otherwise.
pub fn xTaskResumeAll() -> BaseType_t {
    let mut xAlreadyYielded = pdFALSE;

    unsafe {
        // It's an error to call this before the scheduler is started.
        configASSERT(uxSchedulerSuspended > 0);

        taskENTER_CRITICAL();
        {
            uxSchedulerSuspended -= 1;

            if uxSchedulerSuspended == 0 {
                if uxCurrentNumberOfTasks > 0 {
                    // Move any tasks from pending ready list to ready list.
                    while listLIST_IS_EMPTY(&xPendingReadyList) == pdFALSE {
                        let pxTCB = listGET_OWNER_OF_HEAD_ENTRY(&xPendingReadyList) as *mut TCB_t;
                        let _ux = uxListRemove(&mut (*pxTCB).xEventListItem);
                        let _ux = uxListRemove(&mut (*pxTCB).xStateListItem);
                        prvAddTaskToReadyList(pxTCB);

                        // Yield if the moved task has higher priority.
                        if (*pxTCB).uxPriority >= (*pxCurrentTCB).uxPriority {
                            xYieldPendings[0] = pdTRUE;
                        }
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
pub fn vTaskDelay(xTicksToDelay: TickType_t) {
    if xTicksToDelay > 0 {
        unsafe {
            configASSERT(uxSchedulerSuspended == 0);

            vTaskSuspendAll();
            {
                crate::trace::traceTASK_DELAY();
                prvAddCurrentTaskToDelayedList(xTicksToDelay, pdFALSE);
            }
            xTaskResumeAll();
        }
    }

    portYIELD_WITHIN_API();
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
pub fn xTaskDelayUntil(
    pxPreviousWakeTime: *mut TickType_t,
    xTimeIncrement: TickType_t,
) -> BaseType_t {
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
                prvAddCurrentTaskToDelayedList(
                    xTimeToWake.wrapping_sub(xConstTickCount),
                    pdFALSE,
                );
            }
        }
        xTaskResumeAll();
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

/// Increment the mutex held count for the current task.
///
/// Called when a mutex is successfully taken.
#[cfg(feature = "use-mutexes")]
pub fn pvTaskIncrementMutexHeldCount() {
    unsafe {
        if !pxCurrentTCB.is_null() {
            (*pxCurrentTCB).uxMutexesHeld += 1;
        }
    }
}

/// Get the number of tasks in the system.
pub fn uxTaskGetNumberOfTasks() -> UBaseType_t {
    unsafe { uxCurrentNumberOfTasks }
}

/// Get the current tick count.
pub fn xTaskGetTickCount() -> TickType_t {
    unsafe { xTickCount }
}

/// Get the current tick count from ISR.
pub fn xTaskGetTickCountFromISR() -> TickType_t {
    unsafe { xTickCount }
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

    let _ux = uxListRemove(&mut (*pxUnblockedTCB).xEventListItem);

    if uxSchedulerSuspended == 0 {
        // Remove from delayed list and add to ready list.
        let _ux = uxListRemove(&mut (*pxUnblockedTCB).xStateListItem);
        prvAddTaskToReadyList(pxUnblockedTCB);

        // Reset event list item value.
        // (In case it was holding special values)
        listSET_LIST_ITEM_VALUE(
            &mut (*pxUnblockedTCB).xEventListItem,
            (configMAX_PRIORITIES - 1 - (*pxUnblockedTCB).uxPriority) as TickType_t,
        );
    } else {
        // Scheduler is suspended, add to pending ready list.
        vListInsertEnd(&mut xPendingReadyList, &mut (*pxUnblockedTCB).xEventListItem);
    }

    // Return pdTRUE if the unblocked task has higher priority.
    if (*pxUnblockedTCB).uxPriority > (*pxCurrentTCB).uxPriority {
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
    vListInsert(pxEventList, &mut (*pxCurrentTCB).xEventListItem);

    // Add to the delayed list.
    prvAddCurrentTaskToDelayedList(xTicksToWait, pdTRUE);
}

/// Place the current task on an event list (restricted variant).
pub unsafe fn vTaskPlaceOnEventListRestricted(
    pxEventList: *mut List_t,
    xTicksToWait: TickType_t,
    xWaitIndefinitely: BaseType_t,
) {
    configASSERT(!pxEventList.is_null());

    // Place at end (not ordered).
    vListInsertEnd(pxEventList, &mut (*pxCurrentTCB).xEventListItem);

    // Add to delayed list.
    prvAddCurrentTaskToDelayedList(xTicksToWait, xWaitIndefinitely);
}

/// Place the current task on an unordered event list.
pub unsafe fn vTaskPlaceOnUnorderedEventList(
    pxEventList: *mut List_t,
    xItemValue: TickType_t,
    xTicksToWait: TickType_t,
) {
    configASSERT(!pxEventList.is_null());

    // Store the item value.
    listSET_LIST_ITEM_VALUE(
        &mut (*pxCurrentTCB).xEventListItem,
        xItemValue | taskEVENT_LIST_ITEM_VALUE_IN_USE,
    );

    // Insert at end of list.
    vListInsertEnd(pxEventList, &mut (*pxCurrentTCB).xEventListItem);

    // Add to delayed list.
    prvAddCurrentTaskToDelayedList(xTicksToWait, pdTRUE);
}

/// Remove from an unordered event list.
pub unsafe fn xTaskRemoveFromUnorderedEventList(
    pxEventListItem: *mut ListItem_t,
    xItemValue: TickType_t,
) -> BaseType_t {
    // Update the item value (clearing the in-use marker).
    listSET_LIST_ITEM_VALUE(pxEventListItem, xItemValue);

    // Remove from event list.
    let pxUnblockedTCB = listGET_LIST_ITEM_OWNER(pxEventListItem) as *mut TCB_t;
    configASSERT(!pxUnblockedTCB.is_null());
    let _ux = uxListRemove(pxEventListItem);

    if uxSchedulerSuspended == 0 {
        let _ux = uxListRemove(&mut (*pxUnblockedTCB).xStateListItem);
        prvAddTaskToReadyList(pxUnblockedTCB);
    } else {
        vListInsertEnd(&mut xPendingReadyList, &mut (*pxUnblockedTCB).xEventListItem);
    }

    if (*pxUnblockedTCB).uxPriority > (*pxCurrentTCB).uxPriority {
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
pub fn uxTaskResetEventItemValue() -> TickType_t {
    unsafe {
        let pxCurrentTCBLocal = pxCurrentTCB;

        // Get the current value stored in the event list item
        let uxReturn = listGET_LIST_ITEM_VALUE(&(*pxCurrentTCBLocal).xEventListItem);

        // Reset the event list item value to its normal priority-based value
        // The value is set to configMAX_PRIORITIES - priority, inverted so
        // higher priorities have lower item values (for sorted list ordering)
        listSET_LIST_ITEM_VALUE(
            &mut (*pxCurrentTCBLocal).xEventListItem,
            (configMAX_PRIORITIES as TickType_t).wrapping_sub((*pxCurrentTCBLocal).uxPriority as TickType_t),
        );

        uxReturn
    }
}

// =============================================================================
// Public API - Timeout Functions
// =============================================================================

/// Initialize a TimeOut_t structure with the current time.
pub fn vTaskSetTimeOutState(pxTimeOut: *mut TimeOut_t) {
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
pub fn vTaskInternalSetTimeOutState(pxTimeOut: *mut TimeOut_t) {
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
pub fn xTaskCheckForTimeOut(
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

            if *pxTicksToWait == portMAX_DELAY {
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

/// Implement priority inheritance for mutex holder.
///
/// Called when a higher priority task blocks on a mutex.
pub fn vTaskPriorityInherit(pxMutexHolder: TaskHandle_t) {
    #[cfg(feature = "use-mutexes")]
    unsafe {
        if !pxMutexHolder.is_null() {
            let pxMutexHolderTCB = pxMutexHolder as *mut TCB_t;

            // Only inherit if holder has lower priority.
            if (*pxMutexHolderTCB).uxPriority < (*pxCurrentTCB).uxPriority {
                // Adjust event list item value if in an event list.
                if (listGET_LIST_ITEM_VALUE(&(*pxMutexHolderTCB).xEventListItem)
                    & taskEVENT_LIST_ITEM_VALUE_IN_USE) == 0
                {
                    listSET_LIST_ITEM_VALUE(
                        &mut (*pxMutexHolderTCB).xEventListItem,
                        (configMAX_PRIORITIES - 1 - (*pxCurrentTCB).uxPriority) as TickType_t,
                    );
                }

                // If holder is in ready list, move to new priority's list.
                if listIS_CONTAINED_WITHIN(
                    &pxReadyTasksLists[(*pxMutexHolderTCB).uxPriority as usize],
                    &(*pxMutexHolderTCB).xStateListItem,
                ) != pdFALSE
                {
                    let _ux = uxListRemove(&mut (*pxMutexHolderTCB).xStateListItem);

                    (*pxMutexHolderTCB).uxPriority = (*pxCurrentTCB).uxPriority;
                    prvAddTaskToReadyList(pxMutexHolderTCB);
                } else {
                    (*pxMutexHolderTCB).uxPriority = (*pxCurrentTCB).uxPriority;
                }

                crate::trace::traceTASK_PRIORITY_INHERIT(
                    pxMutexHolderTCB as *mut c_void,
                    (*pxCurrentTCB).uxPriority,
                );
            }
        }
    }

    #[cfg(not(feature = "use-mutexes"))]
    {
        let _ = pxMutexHolder;
    }
}

/// Restore priority after releasing mutex.
///
/// # Returns
///
/// pdTRUE if a context switch is required.
pub fn xTaskPriorityDisinherit(pxMutexHolder: TaskHandle_t) -> BaseType_t {
    #[cfg(feature = "use-mutexes")]
    unsafe {
        if !pxMutexHolder.is_null() {
            let pxTCB = pxMutexHolder as *mut TCB_t;

            configASSERT((*pxTCB).uxMutexesHeld > 0);
            (*pxTCB).uxMutexesHeld -= 1;

            // Only disinherit if we inherited priority.
            if (*pxTCB).uxPriority != (*pxTCB).uxBasePriority {
                // Only if no more mutexes held.
                if (*pxTCB).uxMutexesHeld == 0 {
                    // Remove from current priority's ready list.
                    let _ux = uxListRemove(&mut (*pxTCB).xStateListItem);

                    // Restore base priority.
                    (*pxTCB).uxPriority = (*pxTCB).uxBasePriority;

                    // Reset event list item value.
                    listSET_LIST_ITEM_VALUE(
                        &mut (*pxTCB).xEventListItem,
                        (configMAX_PRIORITIES - 1 - (*pxTCB).uxPriority) as TickType_t,
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
pub fn vTaskPriorityDisinheritAfterTimeout(
    pxMutexHolder: TaskHandle_t,
    uxHighestPriorityWaitingTask: UBaseType_t,
) {
    #[cfg(feature = "use-mutexes")]
    unsafe {
        if !pxMutexHolder.is_null() {
            let pxTCB = pxMutexHolder as *mut TCB_t;

            // Only matters if priority was inherited.
            if (*pxTCB).uxPriority != (*pxTCB).uxBasePriority {
                // New priority is max of base priority and highest waiting task.
                let uxPriorityToUse = if (*pxTCB).uxBasePriority > uxHighestPriorityWaitingTask {
                    (*pxTCB).uxBasePriority
                } else {
                    uxHighestPriorityWaitingTask
                };

                if uxPriorityToUse != (*pxTCB).uxPriority {
                    // Need to change priority.
                    if listIS_CONTAINED_WITHIN(
                        &pxReadyTasksLists[(*pxTCB).uxPriority as usize],
                        &(*pxTCB).xStateListItem,
                    ) != pdFALSE
                    {
                        let _ux = uxListRemove(&mut (*pxTCB).xStateListItem);

                        (*pxTCB).uxPriority = uxPriorityToUse;
                        prvAddTaskToReadyList(pxTCB);
                    } else {
                        (*pxTCB).uxPriority = uxPriorityToUse;
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
#[no_mangle]
pub extern "C" fn xTaskIncrementTick() -> BaseType_t {
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
                        let xItemValue = listGET_LIST_ITEM_VALUE(&(*pxTCB).xStateListItem);

                        if xConstTickCount < xItemValue {
                            // Task not ready yet, update next unblock time.
                            xNextTaskUnblockTime = xItemValue;
                            break;
                        }

                        // Remove from delayed list.
                        let _ux = uxListRemove(&mut (*pxTCB).xStateListItem);

                        // Remove from event list if present.
                        if listLIST_ITEM_CONTAINER(&(*pxTCB).xEventListItem) != ptr::null_mut() {
                            let _ux = uxListRemove(&mut (*pxTCB).xEventListItem);
                        }

                        // Add to ready list.
                        prvAddTaskToReadyList(pxTCB);

                        // Yield if unblocked task has higher priority.
                        if configUSE_PREEMPTION != 0 {
                            if (*pxTCB).uxPriority >= (*pxCurrentTCB).uxPriority {
                                xSwitchRequired = pdTRUE;
                            }
                        }
                    }
                }
            }

            // Time slicing within same priority.
            if configUSE_PREEMPTION != 0 {
                if listCURRENT_LIST_LENGTH(&pxReadyTasksLists[(*pxCurrentTCB).uxPriority as usize]) > 1 {
                    xSwitchRequired = pdTRUE;
                }
            }

            // Tick hook.
            if configUSE_TICK_HOOK != 0 {
                // TODO: vApplicationTickHook();
            }

            // Yield pending check.
            if xYieldPendings[0] != pdFALSE {
                xSwitchRequired = pdTRUE;
            }
        } else {
            // Scheduler suspended, pend the tick.
            xPendedTicks += 1;

            // Tick hook even when suspended.
            if configUSE_TICK_HOOK != 0 {
                // TODO: vApplicationTickHook();
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
#[no_mangle]
pub extern "C" fn vTaskSwitchContext() {
    unsafe {
        if uxSchedulerSuspended != 0 {
            // Cannot switch while suspended.
            xYieldPendings[0] = pdTRUE;
            return;
        }

        xYieldPendings[0] = pdFALSE;

        crate::trace::traceTASK_SWITCHED_OUT();

        // Check for stack overflow.
        #[cfg(feature = "stack-overflow-check")]
        {
            // TODO: taskCHECK_FOR_STACK_OVERFLOW()
        }

        // Update run-time stats for the task being switched out.
        #[cfg(feature = "generate-run-time-stats")]
        {
            // Get the current run-time counter value.
            ulTotalRunTime = crate::port::portGET_RUN_TIME_COUNTER_VALUE();

            // Add the amount of time the task has been running to the accumulated
            // time so far. The time the task started running was stored in
            // ulTaskSwitchedInTime. Note that there is no overflow protection here
            // so count values are only valid until the timer overflows. The guard
            // against negative values is to protect against suspect run time stat
            // counter implementations - which are provided by the application, not
            // the kernel.
            if ulTotalRunTime > ulTaskSwitchedInTime {
                if !pxCurrentTCB.is_null() {
                    (*pxCurrentTCB).ulRunTimeCounter += ulTotalRunTime - ulTaskSwitchedInTime;
                }
            }

            ulTaskSwitchedInTime = ulTotalRunTime;
        }

        // Select next task.
        taskSELECT_HIGHEST_PRIORITY_TASK();

        crate::trace::traceTASK_SWITCHED_IN();
    }
}

// =============================================================================
// Yield within API
// =============================================================================

/// Yield within API (trigger context switch if preemption enabled).
#[inline(always)]
pub fn vTaskYieldWithinAPI() {
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
pub fn vTaskDelete(xTaskToDelete: TaskHandle_t) {
    unsafe {
        let mut xDeleteTCBInIdleTask: BaseType_t = pdFALSE;

        taskENTER_CRITICAL();
        {
            // If null is passed in here then it is the calling task that is being deleted.
            let pxTCB = prvGetTCBFromHandle(xTaskToDelete);
            configASSERT(!pxTCB.is_null());

            // Remove task from the ready/delayed list.
            if uxListRemove(&mut (*pxTCB).xStateListItem) == 0 {
                taskRESET_READY_PRIORITY((*pxTCB).uxPriority);
            }

            // Is the task waiting on an event also?
            if !listLIST_ITEM_CONTAINER(&(*pxTCB).xEventListItem).is_null() {
                uxListRemove(&mut (*pxTCB).xEventListItem);
            }

            // Increment the uxTaskNumber so kernel aware debuggers can detect
            // that the task lists need re-generating.
            uxTaskNumber += 1;

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
                vListInsertEnd(&mut xTasksWaitingTermination, &mut (*pxTCB).xStateListItem);

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
#[cfg(feature = "task-suspend")]
pub fn vTaskSuspend(xTaskToSuspend: TaskHandle_t) {
    unsafe {
        taskENTER_CRITICAL();
        {
            let pxTCB = prvGetTCBFromHandle(xTaskToSuspend);

            crate::trace::traceTASK_SUSPEND(pxTCB as *mut c_void);

            // Remove from ready/delayed list.
            let _ux = uxListRemove(&mut (*pxTCB).xStateListItem);

            // Remove from event list if present.
            if listLIST_ITEM_CONTAINER(&(*pxTCB).xEventListItem) != ptr::null_mut() {
                let _ux = uxListRemove(&mut (*pxTCB).xEventListItem);
            }

            // Add to suspended list.
            vListInsertEnd(&mut xSuspendedTaskList, &mut (*pxTCB).xStateListItem);

            // Reset notify state.
            for i in 0..configTASK_NOTIFICATION_ARRAY_ENTRIES {
                if (*pxTCB).ucNotifyState[i] == taskWAITING_NOTIFICATION {
                    (*pxTCB).ucNotifyState[i] = taskNOT_WAITING_NOTIFICATION;
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
                if listCURRENT_LIST_LENGTH(&xSuspendedTaskList) == uxCurrentNumberOfTasks {
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
#[cfg(feature = "task-suspend")]
pub fn vTaskResume(xTaskToResume: TaskHandle_t) {
    unsafe {
        let pxTCB = xTaskToResume as *mut TCB_t;

        if !pxTCB.is_null() && pxTCB != pxCurrentTCB {
            taskENTER_CRITICAL();
            {
                if prvTaskIsTaskSuspended(pxTCB) != pdFALSE {
                    crate::trace::traceTASK_RESUME(pxTCB as *mut c_void);

                    // Remove from suspended list.
                    let _ux = uxListRemove(&mut (*pxTCB).xStateListItem);

                    // Add to ready list.
                    prvAddTaskToReadyList(pxTCB);

                    // Yield if resumed task has higher priority.
                    if (*pxTCB).uxPriority >= (*pxCurrentTCB).uxPriority {
                        portYIELD_WITHIN_API();
                    }
                }
            }
            taskEXIT_CRITICAL();
        }
    }
}

/// Check if a task is suspended.
#[cfg(feature = "task-suspend")]
unsafe fn prvTaskIsTaskSuspended(pxTCB: *const TCB_t) -> BaseType_t {
    if listIS_CONTAINED_WITHIN(&xSuspendedTaskList, &(*pxTCB).xStateListItem) != pdFALSE {
        // In suspended list.
        if listIS_CONTAINED_WITHIN(&xPendingReadyList, &(*pxTCB).xEventListItem) == pdFALSE {
            // Not in pending ready list either.
            if listLIST_ITEM_CONTAINER(&(*pxTCB).xEventListItem) == ptr::null_mut() {
                // Not waiting on any event.
                return pdTRUE;
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
pub unsafe fn xTaskNotify(
    xTaskToNotify: TaskHandle_t,
    ulValue: u32,
    eAction: i32,
) -> BaseType_t {
    xTaskGenericNotify(
        xTaskToNotify,
        tskDEFAULT_INDEX_TO_NOTIFY,
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

/// Generic task notification function.
///
/// Sends a notification to a task, optionally modifying the notification value.
pub unsafe fn xTaskGenericNotify(
    xTaskToNotify: TaskHandle_t,
    uxIndexToNotify: UBaseType_t,
    ulValue: u32,
    eAction: i32,
    pulPreviousNotificationValue: *mut u32,
) -> BaseType_t {
    configASSERT(!xTaskToNotify.is_null());
    configASSERT((uxIndexToNotify as usize) < configTASK_NOTIFICATION_ARRAY_ENTRIES);

    let pxTCB = xTaskToNotify as *mut TCB_t;
    let mut xReturn: BaseType_t = pdPASS;
    let ucOriginalNotifyState: u8;

    taskENTER_CRITICAL();
    {
        if !pulPreviousNotificationValue.is_null() {
            *pulPreviousNotificationValue = (*pxTCB).ulNotifiedValue[uxIndexToNotify as usize];
        }

        ucOriginalNotifyState = (*pxTCB).ucNotifyState[uxIndexToNotify as usize];
        (*pxTCB).ucNotifyState[uxIndexToNotify as usize] = taskNOTIFICATION_RECEIVED;

        match eAction {
            x if x == eNotifyAction::eSetBits as i32 => {
                (*pxTCB).ulNotifiedValue[uxIndexToNotify as usize] |= ulValue;
            }
            x if x == eNotifyAction::eIncrement as i32 => {
                (*pxTCB).ulNotifiedValue[uxIndexToNotify as usize] += 1;
            }
            x if x == eNotifyAction::eSetValueWithOverwrite as i32 => {
                (*pxTCB).ulNotifiedValue[uxIndexToNotify as usize] = ulValue;
            }
            x if x == eNotifyAction::eSetValueWithoutOverwrite as i32 => {
                if ucOriginalNotifyState != taskNOTIFICATION_RECEIVED {
                    (*pxTCB).ulNotifiedValue[uxIndexToNotify as usize] = ulValue;
                } else {
                    xReturn = pdFAIL;
                }
            }
            _ => {
                // eNoAction - just wake the task
            }
        }

        // If the task was waiting for a notification, unblock it.
        if ucOriginalNotifyState == taskWAITING_NOTIFICATION {
            // Remove from delayed list and add to ready list.
            let _ = uxListRemove(&mut (*pxTCB).xStateListItem);
            prvAddTaskToReadyList(pxTCB);

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
pub unsafe fn xTaskGenericNotifyWait(
    uxIndexToWait: UBaseType_t,
    ulBitsToClearOnEntry: u32,
    ulBitsToClearOnExit: u32,
    pulNotificationValue: *mut u32,
    xTicksToWait: TickType_t,
) -> BaseType_t {
    configASSERT((uxIndexToWait as usize) < configTASK_NOTIFICATION_ARRAY_ENTRIES);

    let xReturn: BaseType_t;
    let mut xShouldBlock: BaseType_t = pdFALSE;

    // Check if notification is already pending.
    if (*pxCurrentTCB).ucNotifyState[uxIndexToWait as usize] != taskNOTIFICATION_RECEIVED
        && xTicksToWait > 0
    {
        vTaskSuspendAll();
        {
            taskENTER_CRITICAL();
            {
                if (*pxCurrentTCB).ucNotifyState[uxIndexToWait as usize] != taskNOTIFICATION_RECEIVED
                {
                    // Clear bits on entry.
                    (*pxCurrentTCB).ulNotifiedValue[uxIndexToWait as usize] &=
                        !ulBitsToClearOnEntry;

                    // Mark as waiting for notification.
                    (*pxCurrentTCB).ucNotifyState[uxIndexToWait as usize] = taskWAITING_NOTIFICATION;
                    xShouldBlock = pdTRUE;
                }
            }
            taskEXIT_CRITICAL();

            if xShouldBlock == pdTRUE {
                prvAddCurrentTaskToDelayedList(xTicksToWait, pdTRUE);
            }
        }
        xTaskResumeAll();
    }

    // Check notification state after potential blocking.
    taskENTER_CRITICAL();
    {
        if !pulNotificationValue.is_null() {
            *pulNotificationValue = (*pxCurrentTCB).ulNotifiedValue[uxIndexToWait as usize];
        }

        if (*pxCurrentTCB).ucNotifyState[uxIndexToWait as usize] != taskNOTIFICATION_RECEIVED {
            xReturn = pdFALSE;
        } else {
            // Clear bits on exit.
            (*pxCurrentTCB).ulNotifiedValue[uxIndexToWait as usize] &= !ulBitsToClearOnExit;
            xReturn = pdTRUE;
        }

        (*pxCurrentTCB).ucNotifyState[uxIndexToWait as usize] = taskNOT_WAITING_NOTIFICATION;
    }
    taskEXIT_CRITICAL();

    xReturn
}

/// Generic task notification from ISR.
///
/// Sends a notification to a task from an interrupt service routine.
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

    let pxTCB = xTaskToNotify as *mut TCB_t;
    let mut xReturn: BaseType_t = pdPASS;
    let ucOriginalNotifyState: u8;

    let uxSavedInterruptStatus = portSET_INTERRUPT_MASK_FROM_ISR();
    {
        if !pulPreviousNotificationValue.is_null() {
            *pulPreviousNotificationValue = (*pxTCB).ulNotifiedValue[uxIndexToNotify as usize];
        }

        ucOriginalNotifyState = (*pxTCB).ucNotifyState[uxIndexToNotify as usize];
        (*pxTCB).ucNotifyState[uxIndexToNotify as usize] = taskNOTIFICATION_RECEIVED;

        match eAction {
            x if x == eNotifyAction::eSetBits as i32 => {
                (*pxTCB).ulNotifiedValue[uxIndexToNotify as usize] |= ulValue;
            }
            x if x == eNotifyAction::eIncrement as i32 => {
                (*pxTCB).ulNotifiedValue[uxIndexToNotify as usize] += 1;
            }
            x if x == eNotifyAction::eSetValueWithOverwrite as i32 => {
                (*pxTCB).ulNotifiedValue[uxIndexToNotify as usize] = ulValue;
            }
            x if x == eNotifyAction::eSetValueWithoutOverwrite as i32 => {
                if ucOriginalNotifyState != taskNOTIFICATION_RECEIVED {
                    (*pxTCB).ulNotifiedValue[uxIndexToNotify as usize] = ulValue;
                } else {
                    xReturn = pdFAIL;
                }
            }
            _ => {
                // eNoAction
            }
        }

        // If the task was waiting for a notification, unblock it.
        if ucOriginalNotifyState == taskWAITING_NOTIFICATION {
            // If the scheduler is suspended, add to pending ready list.
            if uxSchedulerSuspended != 0 {
                vListInsertEnd(
                    &mut xPendingReadyList,
                    &mut (*pxTCB).xEventListItem,
                );
            } else {
                let _ = uxListRemove(&mut (*pxTCB).xStateListItem);
                prvAddTaskToReadyList(pxTCB);
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
#[cfg(feature = "thread-local-storage")]
pub unsafe fn vTaskSetThreadLocalStoragePointer(
    xTaskToSet: TaskHandle_t,
    xIndex: BaseType_t,
    pvValue: *mut c_void,
) {
    let pxTCB = prvGetTCBFromHandle(xTaskToSet);

    if xIndex >= 0 && (xIndex as usize) < configNUM_THREAD_LOCAL_STORAGE_POINTERS {
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
#[cfg(feature = "thread-local-storage")]
pub unsafe fn pvTaskGetThreadLocalStoragePointer(
    xTaskToQuery: TaskHandle_t,
    xIndex: BaseType_t,
) -> *mut c_void {
    let mut pvReturn: *mut c_void = ptr::null_mut();
    let pxTCB = prvGetTCBFromHandle(xTaskToQuery);

    if xIndex >= 0 && (xIndex as usize) < configNUM_THREAD_LOCAL_STORAGE_POINTERS {
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
#[cfg(feature = "application-task-tag")]
pub unsafe fn vTaskSetApplicationTaskTag(
    xTask: TaskHandle_t,
    pxHookFunction: TaskHookFunction_t,
) {
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
#[cfg(feature = "application-task-tag")]
pub unsafe fn xTaskGetApplicationTaskTag(xTask: TaskHandle_t) -> TaskHookFunction_t {
    let pxTCB: *mut TCB_t;
    let xReturn: TaskHookFunction_t;

    // If xTask is null, use the current task.
    if xTask.is_null() {
        pxTCB = pxCurrentTCB;
    } else {
        pxTCB = xTask as *mut TCB_t;
    }

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
#[cfg(feature = "application-task-tag")]
pub unsafe fn xTaskGetApplicationTaskTagFromISR(xTask: TaskHandle_t) -> TaskHookFunction_t {
    let pxTCB: *mut TCB_t;
    let xReturn: TaskHookFunction_t;

    // If xTask is null, use the current task.
    if xTask.is_null() {
        pxTCB = pxCurrentTCB;
    } else {
        pxTCB = xTask as *mut TCB_t;
    }

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
/// This function scans from the bottom of the stack upwards looking for
/// bytes that are still set to the initial fill value (tskSTACK_FILL_BYTE).
///
/// # Safety
///
/// The stack pointer must be valid and point to a properly initialized stack.
///
/// # Arguments
///
/// * `pxStack` - Pointer to the bottom of the stack (lowest address)
///
/// # Returns
///
/// The number of bytes that appear to be unused (still contain fill byte).
#[cfg(feature = "stack-high-water-mark")]
unsafe fn prvTaskCheckFreeStackSpace(pxStack: *const u8) -> UBaseType_t {
    let mut pucStackByte = pxStack;
    let mut ulCount: UBaseType_t = 0;

    // Count consecutive fill bytes from stack bottom.
    while *pucStackByte == tskSTACK_FILL_BYTE {
        pucStackByte = pucStackByte.add(1);
        ulCount += 1;
    }

    // Return count in stack words, not bytes.
    ulCount /= core::mem::size_of::<StackType_t>() as UBaseType_t;

    ulCount
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
#[cfg(feature = "stack-high-water-mark")]
pub unsafe fn uxTaskGetStackHighWaterMark(xTask: TaskHandle_t) -> UBaseType_t {
    let pxTCB = prvGetTCBFromHandle(xTask);
    let pucStackByte: *const u8;

    // The stack grows down, so the "bottom" (unused area) is at pxStack.
    // On platforms where the stack grows up, we'd use pxEndOfStack instead.
    #[cfg(feature = "arch-32bit")]
    {
        // portSTACK_GROWTH < 0: stack grows down, bottom is at pxStack
        pucStackByte = (*pxTCB).pxStack as *const u8;
    }

    #[cfg(not(feature = "arch-32bit"))]
    {
        // portSTACK_GROWTH > 0: stack grows up, check pxEndOfStack
        // [AMENDMENT] This path requires record-stack-high-address feature.
        pucStackByte = (*pxTCB).pxStack as *const u8;
    }

    prvTaskCheckFreeStackSpace(pucStackByte)
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
#[cfg(feature = "stack-high-water-mark")]
pub unsafe fn uxTaskGetStackHighWaterMark2(xTask: TaskHandle_t) -> configSTACK_DEPTH_TYPE {
    uxTaskGetStackHighWaterMark(xTask) as configSTACK_DEPTH_TYPE
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
#[cfg(feature = "task-priority-set")]
pub unsafe fn uxTaskPriorityGet(xTask: TaskHandle_t) -> UBaseType_t {
    let pxTCB = prvGetTCBFromHandle(xTask);
    (*pxTCB).uxPriority
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
#[cfg(feature = "task-priority-set")]
pub unsafe fn uxTaskPriorityGetFromISR(xTask: TaskHandle_t) -> UBaseType_t {
    let pxTCB = prvGetTCBFromHandle(xTask);
    let uxSavedInterruptStatus = portSET_INTERRUPT_MASK_FROM_ISR();
    let uxReturn = (*pxTCB).uxPriority;
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
#[cfg(feature = "task-priority-set")]
pub unsafe fn vTaskPrioritySet(xTask: TaskHandle_t, uxNewPriority: UBaseType_t) {
    let pxTCB: *mut TCB_t;
    let uxCurrentBasePriority: UBaseType_t;
    let uxPriorityUsedOnEntry: UBaseType_t;
    let mut xYieldRequired: BaseType_t = pdFALSE;

    // Clamp priority to valid range.
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
            let xItemValue = listGET_LIST_ITEM_VALUE(&(*pxTCB).xEventListItem);
            if (xItemValue & taskEVENT_LIST_ITEM_VALUE_IN_USE) == 0 {
                listSET_LIST_ITEM_VALUE(
                    &mut (*pxTCB).xEventListItem,
                    (configMAX_PRIORITIES as TickType_t) - (uxNewPriority as TickType_t),
                );
            }

            // If task is in a ready list, move it to the correct one.
            if listIS_CONTAINED_WITHIN(
                &pxReadyTasksLists[uxPriorityUsedOnEntry as usize],
                &(*pxTCB).xStateListItem,
            ) != pdFALSE
            {
                // Remove from old ready list.
                if uxListRemove(&mut (*pxTCB).xStateListItem) == 0 {
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
/// This function is available when `abort-delay` or `trace-facility` features are enabled.
/// It determines task state by checking which list the task's state list item is in.
///
/// # Safety
/// - xTask must be a valid task handle or null (for current task is not applicable here)
/// - Must be called from a non-ISR context
#[cfg(any(feature = "abort-delay", feature = "trace-facility"))]
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
        pxStateList = listLIST_ITEM_CONTAINER(&(*pxTCB).xStateListItem);
        pxEventList = listLIST_ITEM_CONTAINER(&(*pxTCB).xEventListItem);
        pxDelayedList = pxDelayedTaskList;
        pxOverflowedDelayedList = pxOverflowDelayedTaskList;
    }
    taskEXIT_CRITICAL();

    // Check if task is on the pending ready list - if so it's ready.
    if pxEventList == &xPendingReadyList as *const _ {
        return eTaskState::eReady;
    }

    // Check if task is on one of the delayed lists.
    if pxStateList == pxDelayedList || pxStateList == pxOverflowedDelayedList {
        return eTaskState::eBlocked;
    }

    // Check if task is on the suspended list.
    #[cfg(feature = "task-suspend")]
    {
        if pxStateList == &xSuspendedTaskList as *const _ {
            // Task is on suspended list. Is it genuinely suspended or blocked indefinitely?
            if listLIST_ITEM_CONTAINER(&(*pxTCB).xEventListItem).is_null() {
                // Not waiting on an event list. Check if waiting on a notification.
                let mut is_blocked = false;
                for i in 0..configTASK_NOTIFICATION_ARRAY_ENTRIES {
                    if (*pxTCB).ucNotifyState[i] == taskWAITING_NOTIFICATION {
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
        if pxStateList == &xTasksWaitingTermination as *const _ || pxStateList.is_null() {
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
            uxListRemove(&mut (*pxTCB).xStateListItem);

            // Is the task waiting on an event also? If so remove it from
            // the event list too. Interrupts can touch the event list item,
            // even though the scheduler is suspended, so a critical section is used.
            taskENTER_CRITICAL();
            {
                if !listLIST_ITEM_CONTAINER(&(*pxTCB).xEventListItem).is_null() {
                    uxListRemove(&mut (*pxTCB).xEventListItem);

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
/// `pxTaskStatus` must point to a valid TaskStatus_t structure.
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

    // Fill in task number (for tracing)
    #[cfg(feature = "trace-facility")]
    {
        (*pxTaskStatus).xTaskNumber = (*pxTCB).uxTaskNumber;
    }
    #[cfg(not(feature = "trace-facility"))]
    {
        (*pxTaskStatus).xTaskNumber = 0;
    }

    // Fill in current priority
    (*pxTaskStatus).uxCurrentPriority = (*pxTCB).uxPriority;

    // Fill in base priority (if mutexes are in use, otherwise same as current)
    #[cfg(feature = "use-mutexes")]
    {
        (*pxTaskStatus).uxBasePriority = (*pxTCB).uxBasePriority;
    }
    #[cfg(not(feature = "use-mutexes"))]
    {
        (*pxTaskStatus).uxBasePriority = (*pxTCB).uxPriority;
    }

    // Run time counter (if run-time stats are enabled)
    #[cfg(feature = "generate-run-time-stats")]
    {
        (*pxTaskStatus).ulRunTimeCounter = (*pxTCB).ulRunTimeCounter;
    }
    #[cfg(not(feature = "generate-run-time-stats"))]
    {
        (*pxTaskStatus).ulRunTimeCounter = 0;
    }

    // Stack base pointer
    (*pxTaskStatus).pxStackBase = (*pxTCB).pxStack;

    // Stack high water mark (if requested)
    if xGetFreeStackSpace != pdFALSE {
        #[cfg(feature = "stack-high-water-mark")]
        {
            (*pxTaskStatus).usStackHighWaterMark =
                uxTaskGetStackHighWaterMark(pxTCB as TaskHandle_t) as u16;
        }
        #[cfg(not(feature = "stack-high-water-mark"))]
        {
            (*pxTaskStatus).usStackHighWaterMark = 0;
        }
    } else {
        (*pxTaskStatus).usStackHighWaterMark = 0;
    }

    // Task state - use provided state or query actual
    (*pxTaskStatus).eCurrentState = if eState != eTaskState::eInvalid {
        eState as u8
    } else {
        eTaskGetState(pxTCB as TaskHandle_t) as u8
    };
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
/// The array must be large enough for `uxArraySize` TaskStatus_t entries.
#[cfg(feature = "trace-facility")]
pub unsafe fn uxTaskGetSystemState(
    pxTaskStatusArray: *mut crate::types::TaskStatus_t,
    uxArraySize: UBaseType_t,
    pulTotalRunTime: *mut u32,
) -> UBaseType_t {
    let mut uxTask: UBaseType_t = 0;
    let mut uxQueue: UBaseType_t = configMAX_PRIORITIES;

    vTaskSuspendAll();
    {
        // Is there a space in the array for each task in the system?
        if uxArraySize >= uxCurrentNumberOfTasks {
            // Fill in a TaskStatus_t structure for each task in the Ready state
            loop {
                uxQueue -= 1;
                uxTask += prvListTasksWithinSingleList(
                    pxTaskStatusArray.add(uxTask as usize),
                    &mut pxReadyTasksLists[uxQueue as usize],
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

            // Fill in a TaskStatus_t structure for each task in the Suspended state
            #[cfg(feature = "task-suspend")]
            {
                uxTask += prvListTasksWithinSingleList(
                    pxTaskStatusArray.add(uxTask as usize),
                    &mut xSuspendedTaskList,
                    eTaskState::eSuspended,
                );
            }

            // Fill in a TaskStatus_t structure for each task that has been deleted
            // but is awaiting cleanup
            #[cfg(feature = "task-delete")]
            {
                uxTask += prvListTasksWithinSingleList(
                    pxTaskStatusArray.add(uxTask as usize),
                    &mut xTasksWaitingTermination,
                    eTaskState::eDeleted,
                );
            }

            // Fill in the run time stats if enabled
            if !pulTotalRunTime.is_null() {
                #[cfg(feature = "generate-run-time-stats")]
                {
                    *pulTotalRunTime = ulTotalRunTime;
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
/// [AMENDMENT] In C, snprintf is used for formatting. In Rust no_std,
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
        let remaining = &mut self.buf[self.pos..];
        if bytes.len() <= remaining.len() {
            remaining[..bytes.len()].copy_from_slice(bytes);
            self.pos += bytes.len();
            Ok(())
        } else {
            // Buffer full - write as much as we can
            remaining.copy_from_slice(&bytes[..remaining.len()]);
            self.pos = self.buf.len();
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
/// Buffer must be large enough for `uxBufferLength` bytes.
///
/// # Note
/// This function calls uxTaskGetSystemState which requires trace-facility.
#[cfg(all(feature = "stats-formatting", feature = "trace-facility"))]
pub unsafe fn vTaskListTasks(pcWriteBuffer: *mut u8, uxBufferLength: usize) {
    use core::fmt::Write;

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

    #[cfg(any(feature = "alloc", feature = "heap-4"))]
    {
        extern crate alloc;
        use alloc::vec::Vec;

        let mut pxTaskStatusArray: Vec<crate::types::TaskStatus_t> =
            Vec::with_capacity(uxArraySize as usize);
        pxTaskStatusArray.resize_with(uxArraySize as usize, crate::types::TaskStatus_t::new);

        let uxTasksReturned = uxTaskGetSystemState(
            pxTaskStatusArray.as_mut_ptr(),
            uxArraySize,
            ptr::null_mut(),
        );

        // Write each task
        for i in 0..uxTasksReturned as usize {
            let pxTaskStatus = &pxTaskStatusArray[i];
            let name = prvGetTaskNameFromPtr(pxTaskStatus.pcTaskName);
            let state_char = prvGetTaskStateChar(
                core::mem::transmute::<u8, eTaskState>(pxTaskStatus.eCurrentState),
            );

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

    #[cfg(not(any(feature = "alloc", feature = "heap-4")))]
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
            let name = prvGetTaskNameFromPtr(pxTaskStatus.pcTaskName);
            let state_char = prvGetTaskStateChar(
                core::mem::transmute::<u8, eTaskState>(pxTaskStatus.eCurrentState),
            );

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

    // Null-terminate the buffer if there's space
    if writer.pos < uxBufferLength {
        buffer[writer.pos] = 0;
    }
}

/// Helper to convert task name pointer to &str for formatting
#[cfg(feature = "stats-formatting")]
fn prvGetTaskNameFromPtr(pcTaskName: *const u8) -> &'static str {
    if pcTaskName.is_null() {
        return "<null>";
    }

    // Find length by scanning for null terminator or max length
    let mut len = 0;
    unsafe {
        while len < configMAX_TASK_NAME_LEN && *pcTaskName.add(len) != 0 {
            len += 1;
        }
        // Safety: we're reading from the task name which is valid for TCB lifetime
        core::str::from_utf8_unchecked(core::slice::from_raw_parts(pcTaskName, len))
    }
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
#[cfg(feature = "generate-run-time-stats")]
pub fn ulTaskGetRunTimeCounter(xTask: TaskHandle_t) -> configRUN_TIME_COUNTER_TYPE {
    unsafe {
        let pxTCB = prvGetTCBFromHandle(xTask);
        if pxTCB.is_null() {
            return 0;
        }
        (*pxTCB).ulRunTimeCounter
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
#[cfg(feature = "generate-run-time-stats")]
pub fn ulTaskGetRunTimePercent(xTask: TaskHandle_t) -> configRUN_TIME_COUNTER_TYPE {
    unsafe {
        let pxTCB = prvGetTCBFromHandle(xTask);
        if pxTCB.is_null() || ulTotalRunTime == 0 {
            return 0;
        }

        // Calculate percentage, avoiding overflow
        // percentage = (task_runtime * 100) / total_runtime
        let ulTaskRunTime = (*pxTCB).ulRunTimeCounter;

        // Use u64 for intermediate calculation to avoid overflow
        ((ulTaskRunTime as u64 * 100) / ulTotalRunTime as u64) as configRUN_TIME_COUNTER_TYPE
    }
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
        let pxIdleTCB = xIdleTaskHandle as *mut TCB_t;
        (*pxIdleTCB).ulRunTimeCounter
    }
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
    unsafe {
        if ulTotalRunTime == 0 {
            return 0;
        }

        let xIdleTaskHandle = xIdleTaskHandles[0];
        if xIdleTaskHandle.is_null() {
            return 0;
        }

        let pxIdleTCB = xIdleTaskHandle as *mut TCB_t;
        let ulIdleRunTime = (*pxIdleTCB).ulRunTimeCounter;

        // Calculate percentage, avoiding overflow
        ((ulIdleRunTime as u64 * 100) / ulTotalRunTime as u64) as configRUN_TIME_COUNTER_TYPE
    }
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
    unsafe { ulTotalRunTime }
}

/// Write run-time statistics to a buffer
///
/// Writes a human-readable table of task run-time statistics to the provided buffer.
/// Format: `TaskName        Abs. Time      % Time`
///
/// [AMENDMENT] In C, snprintf is used for formatting. In Rust no_std,
/// we use core::fmt::Write trait with a buffer wrapper instead.
///
/// # Arguments
/// * `pcWriteBuffer` - Pointer to buffer to write statistics into
/// * `uxBufferLength` - Length of the buffer in bytes
///
/// # Safety
/// Buffer must be large enough for `uxBufferLength` bytes.
#[cfg(all(
    feature = "generate-run-time-stats",
    feature = "stats-formatting",
    feature = "trace-facility"
))]
pub unsafe fn vTaskGetRunTimeStatistics(pcWriteBuffer: *mut u8, uxBufferLength: usize) {
    use core::fmt::Write;

    // Create a slice from the raw pointer
    let buffer = core::slice::from_raw_parts_mut(pcWriteBuffer, uxBufferLength);
    let mut writer = BufferWriter::new(buffer);

    // Write header
    let _ = writeln!(
        writer,
        "{:<16} {:>12} {:>8}",
        "Name", "Abs. Time", "% Time"
    );

    // Get the number of tasks
    let uxArraySize = uxTaskGetNumberOfTasks();
    if uxArraySize == 0 {
        return;
    }

    // Get the total run time
    let ulTotalTime = ulTotalRunTime;
    if ulTotalTime == 0 {
        let _ = writeln!(writer, "(No run time data collected yet)");
        return;
    }

    // Allocate array for task status
    #[cfg(any(feature = "alloc", feature = "heap-4"))]
    {
        extern crate alloc;
        use alloc::vec::Vec;

        let mut pxTaskStatusArray: Vec<crate::types::TaskStatus_t> =
            Vec::with_capacity(uxArraySize as usize);
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
            let name = prvGetTaskNameFromPtr(pcTaskName);

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

    #[cfg(not(any(feature = "alloc", feature = "heap-4")))]
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
            let name = prvGetTaskNameFromPtr(pcTaskName);

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

        #[cfg(not(feature = "port-optimised-task-selection"))]
        {
            if uxTopReadyPriority > tskIDLE_PRIORITY {
                xHigherPriorityReadyTasks = pdTRUE;
            }
        }

        #[cfg(feature = "port-optimised-task-selection")]
        {
            // When port optimised task selection is used, uxTopReadyPriority
            // is a bit map. If bits other than the least significant bit are set,
            // there are tasks above idle priority ready.
            const UX_LEAST_SIGNIFICANT_BIT: UBaseType_t = 0x01;
            if uxTopReadyPriority > UX_LEAST_SIGNIFICANT_BIT {
                xHigherPriorityReadyTasks = pdTRUE;
            }
        }

        if !pxCurrentTCB.is_null() && (*pxCurrentTCB).uxPriority > tskIDLE_PRIORITY {
            // Current task is above idle priority - cannot sleep
            xReturn = 0;
        } else if listCURRENT_LIST_LENGTH(&pxReadyTasksLists[tskIDLE_PRIORITY as usize]) > 1 {
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
#[cfg(feature = "tickless-idle")]
pub fn eTaskConfirmSleepModeStatus() -> crate::types::eSleepModeStatus {
    use crate::types::eSleepModeStatus;

    unsafe {
        // Check if a task was made ready while the scheduler was suspended.
        if listCURRENT_LIST_LENGTH(&xPendingReadyList) != 0 {
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

            if listCURRENT_LIST_LENGTH(&xSuspendedTaskList)
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
#[cfg(feature = "tickless-idle")]
pub fn vTaskStepTick(xTicksToJump: TickType_t) {
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
                xPendedTicks += 1;
            }
            taskEXIT_CRITICAL();
            xTicksToJump -= 1;
        }

        xTickCount = xTickCount.wrapping_add(xTicksToJump);

        crate::trace::traceINCREASE_TICK_COUNT(xTicksToJump);
    }
}
