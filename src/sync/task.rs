//! Safe Task wrapper
//!
//! Provides a safe wrapper around FreeRTOS task creation and management.
//!
//! # Task Creation
//!
//! Tasks can be created with either dynamic or static allocation:
//! - Dynamic: Stack and TCB allocated from heap
//! - Static: User provides stack and TCB buffers (common in embedded)

use core::ffi::c_void;
use core::ptr;

use crate::kernel::tasks::{
    vTaskDelete, vTaskResume, vTaskSuspend, xTaskGetCurrentTaskHandle, StaticTask_t,
};
#[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
use crate::kernel::tasks::xTaskCreate;
use crate::kernel::tasks::xTaskCreateStatic;
use crate::types::*;

/// A handle to a FreeRTOS task.
///
/// This handle can be used to suspend, resume, or delete the task.
/// The handle is `Copy` and can be freely duplicated.
#[derive(Clone, Copy)]
pub struct TaskHandle {
    handle: TaskHandle_t,
}

// Safety: Task handles can be used from any task
unsafe impl Sync for TaskHandle {}
unsafe impl Send for TaskHandle {}

/// Task function signature.
///
/// Task functions receive an optional parameter pointer and should
/// typically run forever (contain an infinite loop).
pub type TaskFn = extern "C" fn(*mut c_void);

impl TaskHandle {
    // =========================================================================
    // Creation - Dynamic Allocation
    // =========================================================================

    /// Spawns a new task with dynamically allocated stack and TCB.
    ///
    /// # Arguments
    ///
    /// * `name` - Null-terminated task name (e.g., `b"MyTask\0"`)
    /// * `stack_size` - Stack size in words (not bytes)
    /// * `priority` - Task priority (higher = more important)
    /// * `task_fn` - The task function to run
    ///
    /// Returns `None` if task creation failed (e.g., out of memory).
    #[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
    pub fn spawn(
        name: &[u8],
        stack_size: usize,
        priority: UBaseType_t,
        task_fn: TaskFn,
    ) -> Option<Self> {
        Self::spawn_with_param(name, stack_size, priority, task_fn, ptr::null_mut())
    }

    /// Spawns a new task with a parameter.
    ///
    /// The parameter pointer is passed to the task function. Ensure the
    /// pointed-to data outlives the task.
    #[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
    pub fn spawn_with_param(
        name: &[u8],
        stack_size: usize,
        priority: UBaseType_t,
        task_fn: TaskFn,
        param: *mut c_void,
    ) -> Option<Self> {
        let mut handle: TaskHandle_t = ptr::null_mut();
        let result = unsafe {
            xTaskCreate(
                task_fn,
                name.as_ptr(),
                stack_size,
                param,
                priority,
                &mut handle,
            )
        };
        if result == pdPASS && !handle.is_null() {
            Some(Self { handle })
        } else {
            None
        }
    }

    // =========================================================================
    // Creation - Static Allocation
    // =========================================================================

    /// Spawns a new task with statically allocated stack and TCB.
    ///
    /// This is the preferred method for embedded systems where dynamic
    /// allocation should be avoided.
    ///
    /// # Arguments
    ///
    /// * `name` - Null-terminated task name
    /// * `stack` - Static stack buffer (must be `'static`)
    /// * `tcb` - Static TCB buffer (must be `'static`)
    /// * `priority` - Task priority
    /// * `task_fn` - The task function to run
    ///
    /// # Example
    ///
    /// ```ignore
    /// static mut STACK: [StackType_t; 256] = [0; 256];
    /// static mut TCB: StaticTask_t = StaticTask_t::new();
    ///
    /// let handle = TaskHandle::spawn_static(
    ///     b"Worker\0",
    ///     unsafe { &mut STACK },
    ///     unsafe { &mut TCB },
    ///     2,
    ///     worker_task,
    /// )?;
    /// ```
    pub fn spawn_static(
        name: &[u8],
        stack: &'static mut [StackType_t],
        tcb: &'static mut StaticTask_t,
        priority: UBaseType_t,
        task_fn: TaskFn,
    ) -> Option<Self> {
        Self::spawn_static_with_param(name, stack, tcb, priority, task_fn, ptr::null_mut())
    }

    /// Spawns a static task with a parameter.
    pub fn spawn_static_with_param(
        name: &[u8],
        stack: &'static mut [StackType_t],
        tcb: &'static mut StaticTask_t,
        priority: UBaseType_t,
        task_fn: TaskFn,
        param: *mut c_void,
    ) -> Option<Self> {
        let handle = unsafe {
            xTaskCreateStatic(
                task_fn,
                name.as_ptr(),
                stack.len(),
                param,
                priority,
                stack.as_mut_ptr(),
                tcb as *mut StaticTask_t,
            )
        };
        if handle.is_null() {
            None
        } else {
            Some(Self { handle })
        }
    }

    // =========================================================================
    // Task Control
    // =========================================================================

    /// Suspends this task.
    ///
    /// A suspended task will not run until `resume()` is called.
    #[cfg(feature = "task-suspend")]
    pub fn suspend(&self) {
        vTaskSuspend(self.handle);
    }

    /// Resumes a suspended task.
    #[cfg(feature = "task-suspend")]
    pub fn resume(&self) {
        vTaskResume(self.handle);
    }

    /// Deletes this task.
    ///
    /// After deletion, this handle should not be used. For dynamically
    /// allocated tasks, the memory is freed. For static tasks, the
    /// stack and TCB can be reused.
    ///
    /// # Safety Note
    ///
    /// This takes `self` by value to discourage (but not prevent)
    /// using the handle after deletion.
    #[cfg(feature = "task-delete")]
    pub fn delete(self) {
        vTaskDelete(self.handle);
    }

    // =========================================================================
    // Priority
    // =========================================================================

    /// Gets this task's current priority.
    #[cfg(feature = "task-priority-set")]
    pub fn priority(&self) -> UBaseType_t {
        unsafe { crate::kernel::tasks::uxTaskPriorityGet(self.handle) }
    }

    /// Sets this task's priority.
    #[cfg(feature = "task-priority-set")]
    pub fn set_priority(&self, new_priority: UBaseType_t) {
        unsafe { crate::kernel::tasks::vTaskPrioritySet(self.handle, new_priority) }
    }

    // =========================================================================
    // Query
    // =========================================================================

    /// Gets the handle of the currently running task.
    pub fn current() -> Self {
        Self {
            handle: xTaskGetCurrentTaskHandle(),
        }
    }

    /// Returns the raw FreeRTOS handle for interop.
    pub unsafe fn raw_handle(&self) -> TaskHandle_t {
        self.handle
    }

    /// Creates a TaskHandle from a raw FreeRTOS handle.
    ///
    /// # Safety
    ///
    /// The handle must be a valid FreeRTOS task handle.
    pub unsafe fn from_raw(handle: TaskHandle_t) -> Self {
        Self { handle }
    }
}

// =========================================================================
// Current Task Operations
// =========================================================================

/// Suspends the currently running task.
///
/// The task will not run until another task calls `resume()` on it.
#[cfg(feature = "task-suspend")]
pub fn suspend_self() {
    vTaskSuspend(ptr::null_mut());
}

/// Deletes the currently running task.
///
/// This function does not return. Another task must be ready to run.
#[cfg(feature = "task-delete")]
pub fn delete_self() -> ! {
    vTaskDelete(ptr::null_mut());
    // vTaskDelete with null deletes current task and doesn't return
    unreachable!()
}
