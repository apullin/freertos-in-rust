//! Sound task-management veneer.
//!
//! A FreeRTOS task handle is only valid until that task is deleted.  This
//! module therefore treats [`TaskHandle`] as the unique deletion authority for
//! a task and deliberately does not implement `Copy` or `Clone` for it.
//!
//! Task-context-only operations require a [`TaskContext`] capability.  Creating
//! that capability is unsafe, but all operations performed through a valid
//! capability can then enforce the distinction between task and interrupt
//! context in their types.

use core::ffi::c_void;
use core::marker::PhantomData;
use core::mem::size_of;
use core::ptr::{self, NonNull};

use crate::config::{configMAX_PRIORITIES, configMAX_TASK_NAME_LEN, configMINIMAL_STACK_SIZE};
#[cfg(feature = "task-delete")]
use crate::kernel::tasks::vTaskDelete;
#[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
use crate::kernel::tasks::xTaskCreate;
use crate::kernel::tasks::{
    taskSCHEDULER_NOT_STARTED, xTaskCreateStatic, xTaskGetCurrentTaskHandle,
    xTaskGetSchedulerState, StaticTask_t,
};
#[cfg(feature = "task-suspend")]
use crate::kernel::tasks::{vTaskResume, vTaskSuspend};
use crate::types::*;

const _: () = assert!(configMAX_TASK_NAME_LEN > 0);
const _: () = assert!(configMAX_PRIORITIES > 0);

/// Function executed by a task.
///
/// The function receives the parameter supplied at creation and must not
/// return.  A task that is finished must use the task-deletion API when that
/// feature is enabled.
pub type TaskFn = extern "C" fn(*mut c_void);

/// Error returned when a task cannot be created.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaskCreateError {
    /// The supplied stack cannot contain even the configured minimum task
    /// stack.
    StackTooSmall {
        supplied_words: usize,
        minimum_words: usize,
    },
    /// Converting the requested stack depth to a byte count overflowed.
    StackByteSizeOverflow { supplied_words: usize },
    /// The requested priority is outside the configured range.
    InvalidPriority(InvalidTaskPriority),
    /// The kernel rejected creation, normally because allocation failed.
    KernelRejected,
}

/// An out-of-range task priority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidTaskPriority {
    pub requested: UBaseType_t,
    pub maximum: UBaseType_t,
}

/// Evidence that the caller is allowed to invoke task-context kernel APIs.
///
/// This value is intentionally neither `Send` nor `Sync`.  It should normally
/// be created once at a task entry point, or once during single-threaded kernel
/// setup before the scheduler starts, and retained only for that lexical
/// context.
pub struct TaskContext {
    _not_send_or_sync: PhantomData<*mut ()>,
}

impl TaskContext {
    /// Assert that the caller is executing in task context or in exclusive
    /// pre-scheduler initialization context.
    ///
    /// # Safety
    ///
    /// The caller must not be executing in an ISR.  If the scheduler has not
    /// started, the caller must have exclusive access to kernel initialization.
    /// The returned capability must not be retained after leaving that
    /// context, moved into an interrupt handler, or shared with another
    /// execution context.
    pub unsafe fn assume() -> Self {
        Self {
            _not_send_or_sync: PhantomData,
        }
    }

    /// Obtain a scoped, non-owning view of the currently executing task.
    ///
    /// This returns `None` before the scheduler starts.  A [`CurrentTask`] has
    /// no deletion operation and cannot outlive this context capability.
    pub fn current(&self) -> Option<CurrentTask<'_>> {
        if xTaskGetSchedulerState() == taskSCHEDULER_NOT_STARTED {
            return None;
        }

        NonNull::new(xTaskGetCurrentTaskHandle()).map(|handle| CurrentTask {
            handle,
            _context: PhantomData,
            _not_send_or_sync: PhantomData,
        })
    }
}

/// Unique deletion authority for a live FreeRTOS task.
///
/// The handle is intentionally neither `Copy`, `Clone`, nor `Sync`.  It is
/// `Send`, so its unique ownership may be transferred to another task or
/// protected by a synchronization primitive.  Consuming deletion and unsafe
/// raw reconstruction ensure that safe code cannot retain a second usable
/// owner after a task's TCB has been freed.  Use [`TaskHandle::borrow`] when a
/// temporary, non-deleting reference is sufficient.
///
/// Dropping this value does **not** delete the task; it merely relinquishes the
/// ability to control it through this safe veneer.
///
/// ```compile_fail
/// use freertos_in_rust::sync::TaskHandle;
///
/// fn duplicate(handle: TaskHandle) {
///     let first = handle;
///     let second = handle; // `TaskHandle` is not `Copy`.
///     drop((first, second));
/// }
/// ```
pub struct TaskHandle {
    handle: NonNull<c_void>,
    _not_send_or_sync: PhantomData<*mut ()>,
}

// Safety: TaskHandle is the unique owner of a kernel handle. Moving that
// ownership to another task cannot create aliases. Its operations still
// require a TaskContext and the type remains !Sync, so shared references cannot
// be used as cross-context control capabilities.
unsafe impl Send for TaskHandle {}

impl TaskHandle {
    // =========================================================================
    // Creation - Dynamic Allocation
    // =========================================================================

    /// Spawn a dynamically allocated task with a null parameter.
    ///
    /// `name` need not be NUL terminated.  At most
    /// `configMAX_TASK_NAME_LEN - 1` bytes are copied synchronously into a
    /// bounded NUL-terminated buffer before entering the kernel.  An embedded
    /// NUL terminates the name early.
    ///
    /// `stack_size` is measured in [`StackType_t`] words.  The request is
    /// rejected if it is smaller than `configMINIMAL_STACK_SIZE` or if its byte
    /// count overflows.
    #[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
    pub fn spawn(
        _context: &TaskContext,
        name: &[u8],
        stack_size: usize,
        priority: UBaseType_t,
        task_fn: TaskFn,
    ) -> Result<Self, TaskCreateError> {
        // A null parameter has no lifetime or aliasing obligation.
        unsafe {
            Self::spawn_with_param_inner(name, stack_size, priority, task_fn, ptr::null_mut())
        }
    }

    /// Spawn a dynamically allocated task with an untyped parameter.
    ///
    /// # Safety
    ///
    /// In addition to holding a valid task context, `param` must satisfy the
    /// validity, lifetime, aliasing, and synchronization assumptions made by
    /// `task_fn` for the entire time the task can access it.  The task function
    /// must not return.
    #[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
    pub unsafe fn spawn_with_param(
        _context: &TaskContext,
        name: &[u8],
        stack_size: usize,
        priority: UBaseType_t,
        task_fn: TaskFn,
        param: *mut c_void,
    ) -> Result<Self, TaskCreateError> {
        unsafe { Self::spawn_with_param_inner(name, stack_size, priority, task_fn, param) }
    }

    #[cfg(any(feature = "alloc", feature = "heap-4", feature = "heap-5"))]
    unsafe fn spawn_with_param_inner(
        name: &[u8],
        stack_size: usize,
        priority: UBaseType_t,
        task_fn: TaskFn,
        param: *mut c_void,
    ) -> Result<Self, TaskCreateError> {
        validate_creation(stack_size, priority)?;
        let name = PreparedTaskName::new(name);
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

        if result != pdPASS {
            return Err(TaskCreateError::KernelRejected);
        }

        Self::from_created_raw(handle)
    }

    // =========================================================================
    // Creation - Static Allocation
    // =========================================================================

    /// Spawn a task using caller-owned static stack and TCB storage.
    ///
    /// The storage is borrowed for `'static`, which prevents safe code from
    /// reusing it while the task might still exist.  As with [`TaskHandle::spawn`],
    /// `name` is copied into bounded temporary C-compatible storage.
    pub fn spawn_static(
        _context: &TaskContext,
        name: &[u8],
        stack: &'static mut [StackType_t],
        tcb: &'static mut StaticTask_t,
        priority: UBaseType_t,
        task_fn: TaskFn,
    ) -> Result<Self, TaskCreateError> {
        // A null parameter has no lifetime or aliasing obligation.
        unsafe {
            Self::spawn_static_with_param_inner(
                name,
                stack,
                tcb,
                priority,
                task_fn,
                ptr::null_mut(),
            )
        }
    }

    /// Spawn a statically allocated task with an untyped parameter.
    ///
    /// # Safety
    ///
    /// In addition to holding a valid task context, `param` must satisfy the
    /// validity, lifetime, aliasing, and synchronization assumptions made by
    /// `task_fn` for the entire time the task can access it.  The task function
    /// must not return.
    pub unsafe fn spawn_static_with_param(
        _context: &TaskContext,
        name: &[u8],
        stack: &'static mut [StackType_t],
        tcb: &'static mut StaticTask_t,
        priority: UBaseType_t,
        task_fn: TaskFn,
        param: *mut c_void,
    ) -> Result<Self, TaskCreateError> {
        unsafe { Self::spawn_static_with_param_inner(name, stack, tcb, priority, task_fn, param) }
    }

    unsafe fn spawn_static_with_param_inner(
        name: &[u8],
        stack: &'static mut [StackType_t],
        tcb: &'static mut StaticTask_t,
        priority: UBaseType_t,
        task_fn: TaskFn,
        param: *mut c_void,
    ) -> Result<Self, TaskCreateError> {
        validate_creation(stack.len(), priority)?;
        let name = PreparedTaskName::new(name);
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

        Self::from_created_raw(handle)
    }

    fn from_created_raw(handle: TaskHandle_t) -> Result<Self, TaskCreateError> {
        NonNull::new(handle)
            .map(Self::from_non_null)
            .ok_or(TaskCreateError::KernelRejected)
    }

    fn from_non_null(handle: NonNull<c_void>) -> Self {
        Self {
            handle,
            _not_send_or_sync: PhantomData,
        }
    }

    // =========================================================================
    // Task Control
    // =========================================================================

    /// Borrow this live task without granting deletion authority.
    ///
    /// The returned reference may query or resume the task.  Its lifetime
    /// prevents this owner from being consumed by [`TaskHandle::delete`] or
    /// [`TaskHandle::into_raw`] until the borrow ends.
    ///
    /// ```compile_fail
    /// use freertos_in_rust::sync::TaskHandle;
    ///
    /// fn cannot_consume_while_borrowed(handle: TaskHandle) {
    ///     let borrowed = handle.borrow();
    ///     let raw = handle.into_raw(); // cannot move out while borrowed
    ///     drop(borrowed);
    ///     let _ = raw;
    /// }
    /// ```
    pub fn borrow(&self) -> TaskRef<'_> {
        TaskRef {
            handle: self.handle,
            _owner: PhantomData,
            _not_sync: PhantomData,
        }
    }

    /// Suspend this task until another task or ISR resumes it.
    #[cfg(feature = "task-suspend")]
    pub fn suspend(&mut self, _context: &TaskContext) {
        // Safety: unique TaskHandle ownership keeps the handle live; the
        // TaskContext proves this is a task-context call.
        unsafe { vTaskSuspend(self.handle.as_ptr()) };
    }

    /// Resume this suspended task from task context.
    ///
    /// # Panics
    ///
    /// Panics during pre-scheduler setup if every created task is suspended and
    /// FreeRTOS therefore has no selected task against which to compare the
    /// resumed task's priority.
    #[cfg(feature = "task-suspend")]
    pub fn resume(&mut self, _context: &TaskContext) {
        assert_has_selected_task("TaskHandle::resume", xTaskGetCurrentTaskHandle());
        // Safety: unique TaskHandle ownership keeps the handle live; the
        // TaskContext proves this is a task-context call, and the check above
        // satisfies FreeRTOS's current-task priority comparison.
        unsafe { vTaskResume(self.handle.as_ptr()) };
    }

    /// Delete another task and consume its unique handle.
    ///
    /// For a dynamically allocated task its TCB and stack may be freed before
    /// this method returns.  The by-value receiver prevents safe use afterward.
    /// Deletion does not run the target's Rust stack destructors. Deleting the
    /// executing task must use [`delete_self`] instead.
    /// Deletion is unavailable during pre-scheduler setup because FreeRTOS may
    /// retain the selected task in its internal current-task pointer there.
    ///
    /// # Safety
    ///
    /// The caller must coordinate shutdown so that the target cannot acquire
    /// resources or start new operations before deletion. It must hold no
    /// mutexes, leave no outstanding kernel references (including stream/message
    /// buffer waiters), and have no stack values whose destruction is required
    /// for memory safety. All raw aliases must cease being used. Suspending a
    /// task alone does not establish these conditions.
    ///
    /// Unique handle ownership prevents stale safe handles, but cannot prove
    /// that the target has released its resources:
    ///
    /// ```compile_fail,E0133
    /// use freertos_in_rust::sync::{TaskContext, TaskHandle};
    /// fn cannot_cancel_safely(handle: TaskHandle, context: &TaskContext) {
    ///     handle.delete(context);
    /// }
    /// ```
    ///
    /// # Panics
    ///
    /// Panics before the scheduler starts, or if this handle identifies the
    /// currently executing task.
    #[cfg(feature = "task-delete")]
    pub unsafe fn delete(self, context: &TaskContext) {
        let current = context.current().map(|task| task.as_raw());
        assert_deletable_task(self.handle.as_ptr(), current);
        // Safety: ownership is consumed; the caller establishes target quiescence
        // and that skipping its stack destructors leaves no dangling resources.
        unsafe { vTaskDelete(self.handle.as_ptr()) };
    }

    // =========================================================================
    // Priority
    // =========================================================================

    /// Return this live task's current priority.
    #[cfg(feature = "task-priority-set")]
    pub fn priority(&self, _context: &TaskContext) -> UBaseType_t {
        // Safety: unique TaskHandle ownership keeps the TCB live.
        unsafe { crate::kernel::tasks::uxTaskPriorityGet(self.handle.as_ptr()) }
    }

    /// Set this task's priority, rejecting rather than clamping invalid input.
    ///
    /// # Panics
    ///
    /// Panics during pre-scheduler setup if every created task is suspended and
    /// FreeRTOS therefore has no selected task for its priority comparison.
    #[cfg(feature = "task-priority-set")]
    pub fn set_priority(
        &mut self,
        _context: &TaskContext,
        new_priority: UBaseType_t,
    ) -> Result<(), InvalidTaskPriority> {
        validate_priority(new_priority)?;
        assert_has_selected_task("TaskHandle::set_priority", xTaskGetCurrentTaskHandle());
        // Safety: unique TaskHandle ownership keeps the TCB live.
        unsafe { crate::kernel::tasks::vTaskPrioritySet(self.handle.as_ptr(), new_priority) };
        Ok(())
    }

    // =========================================================================
    // Raw interoperation
    // =========================================================================

    /// Borrow the opaque raw kernel handle.
    ///
    /// Merely obtaining the pointer is safe.  Dereferencing it or passing it to
    /// a raw kernel operation remains subject to that operation's safety
    /// contract.  Code must not use the pointer after this owner is deleted.
    pub fn as_raw(&self) -> TaskHandle_t {
        self.handle.as_ptr()
    }

    /// Relinquish safe ownership and return the opaque raw kernel handle.
    pub fn into_raw(self) -> TaskHandle_t {
        self.handle.as_ptr()
    }

    /// Reconstruct unique deletion authority from a raw task handle.
    ///
    /// # Safety
    ///
    /// `handle` must be non-null and identify a live task.  No other
    /// [`TaskHandle`] may own the same task, and the task must not independently
    /// delete itself while the returned owner is usable.  The caller must also
    /// arrange that all raw aliases stop being used before deletion.
    pub unsafe fn from_raw_owned(handle: TaskHandle_t) -> Option<Self> {
        NonNull::new(handle).map(Self::from_non_null)
    }
}

/// A borrowed reference to a live task.
///
/// `TaskRef` carries no deletion authority.  Its lifetime keeps the owning
/// [`TaskHandle`] alive, so it cannot become stale through the safe API.
/// A reference can be moved to another task (`Send`) but is not `Sync`; transfer
/// it through a synchronization primitive instead of sharing one instance.
pub struct TaskRef<'task> {
    handle: NonNull<c_void>,
    _owner: PhantomData<&'task TaskHandle>,
    _not_sync: PhantomData<*mut ()>,
}

// Safety: the lifetime prevents deletion through the unique owner while this
// reference exists. Moving the non-owning reference transfers its permission;
// it is deliberately not Sync and exposes no deletion operation.
unsafe impl Send for TaskRef<'_> {}

impl TaskRef<'_> {
    /// Resume this suspended task from task context.
    ///
    /// # Panics
    ///
    /// Panics during pre-scheduler setup when FreeRTOS has no selected task;
    /// see [`TaskHandle::resume`].
    #[cfg(feature = "task-suspend")]
    pub fn resume(&mut self, _context: &TaskContext) {
        assert_has_selected_task("TaskRef::resume", xTaskGetCurrentTaskHandle());
        // Safety: the owner cannot be deleted for this borrow's lifetime, and
        // TaskContext excludes ISR use of the task-context API. The check above
        // satisfies FreeRTOS's current-task priority comparison.
        unsafe { vTaskResume(self.handle.as_ptr()) };
    }

    /// Return this live task's current priority.
    #[cfg(feature = "task-priority-set")]
    pub fn priority(&self, _context: &TaskContext) -> UBaseType_t {
        // Safety: the owner cannot be deleted for this borrow's lifetime.
        unsafe { crate::kernel::tasks::uxTaskPriorityGet(self.handle.as_ptr()) }
    }

    /// Borrow the opaque raw kernel handle.
    pub fn as_raw(&self) -> TaskHandle_t {
        self.handle.as_ptr()
    }
}

/// A scoped, non-owning view of the task executing in a [`TaskContext`].
///
/// This type cannot delete the current task and cannot outlive or be sent away
/// from the context that produced it.
///
/// ```compile_fail
/// use freertos_in_rust::sync::{CurrentTask, TaskContext};
///
/// fn cannot_delete(current: CurrentTask<'_>, context: &TaskContext) {
///     current.delete(context); // a current-task view has no deletion authority
/// }
/// ```
pub struct CurrentTask<'context> {
    handle: NonNull<c_void>,
    _context: PhantomData<&'context TaskContext>,
    _not_send_or_sync: PhantomData<*mut ()>,
}

impl CurrentTask<'_> {
    /// Suspend the current task.  This call returns after another context
    /// resumes it.
    #[cfg(feature = "task-suspend")]
    pub fn suspend(self) {
        // Safety: this view is tied to a valid TaskContext and names the
        // currently executing, therefore live, task.
        unsafe { vTaskSuspend(self.handle.as_ptr()) };
    }

    /// Return the current task's priority.
    #[cfg(feature = "task-priority-set")]
    pub fn priority(&self) -> UBaseType_t {
        // Safety: a CurrentTask cannot outlive its live task context.
        unsafe { crate::kernel::tasks::uxTaskPriorityGet(self.handle.as_ptr()) }
    }

    /// Set the current task's priority, rejecting invalid input.
    #[cfg(feature = "task-priority-set")]
    pub fn set_priority(&mut self, new_priority: UBaseType_t) -> Result<(), InvalidTaskPriority> {
        validate_priority(new_priority)?;
        // Safety: a CurrentTask cannot outlive its live task context.
        unsafe { crate::kernel::tasks::vTaskPrioritySet(self.handle.as_ptr(), new_priority) };
        Ok(())
    }

    /// Borrow the current task's opaque raw kernel handle.
    pub fn as_raw(&self) -> TaskHandle_t {
        self.handle.as_ptr()
    }
}

/// Suspend the currently executing task.
///
/// Returns `false` if called during pre-scheduler initialization, when no task
/// is actually executing.  Otherwise it returns `true` after the task is
/// resumed.
#[cfg(feature = "task-suspend")]
pub fn suspend_self(context: &TaskContext) -> bool {
    match context.current() {
        Some(current) => {
            current.suspend();
            true
        }
        None => false,
    }
}

/// Delete the currently executing task without leaving a stale safe owner.
///
/// # Safety
///
/// The context must represent an executing task, not pre-scheduler setup.  No
/// live [`TaskHandle`] may own the current task, because that handle would
/// become stale.  This operation does not run destructors for values on the
/// task's stack. The caller must first release all mutexes and other resources
/// whose cleanup is required for memory safety, and leave no outstanding
/// references to this task in kernel objects or raw callers.
#[cfg(feature = "task-delete")]
pub unsafe fn delete_self(context: TaskContext) -> ! {
    assert!(
        context.current().is_some(),
        "delete_self requires a running task"
    );

    // Safety: upheld by this function's contract. Null selects the caller.
    unsafe { vTaskDelete(ptr::null_mut()) };
    unreachable!("a deleted FreeRTOS task became runnable again")
}

struct PreparedTaskName {
    bytes: [u8; configMAX_TASK_NAME_LEN],
}

impl PreparedTaskName {
    fn new(name: &[u8]) -> Self {
        let mut bytes = [0; configMAX_TASK_NAME_LEN];
        let source_len = name
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(name.len());
        let copy_len = core::cmp::min(source_len, configMAX_TASK_NAME_LEN - 1);
        bytes[..copy_len].copy_from_slice(&name[..copy_len]);
        Self { bytes }
    }

    fn as_ptr(&self) -> *const u8 {
        self.bytes.as_ptr()
    }
}

fn validate_creation(stack_words: usize, priority: UBaseType_t) -> Result<(), TaskCreateError> {
    if stack_words < configMINIMAL_STACK_SIZE {
        return Err(TaskCreateError::StackTooSmall {
            supplied_words: stack_words,
            minimum_words: configMINIMAL_STACK_SIZE,
        });
    }

    if stack_words.checked_mul(size_of::<StackType_t>()).is_none() {
        return Err(TaskCreateError::StackByteSizeOverflow {
            supplied_words: stack_words,
        });
    }

    validate_priority(priority).map_err(TaskCreateError::InvalidPriority)
}

fn validate_priority(priority: UBaseType_t) -> Result<(), InvalidTaskPriority> {
    if priority >= configMAX_PRIORITIES {
        return Err(InvalidTaskPriority {
            requested: priority,
            maximum: configMAX_PRIORITIES - 1,
        });
    }

    Ok(())
}

#[cfg(any(feature = "task-suspend", feature = "task-priority-set"))]
fn assert_has_selected_task(operation: &str, current: TaskHandle_t) {
    assert!(
        !current.is_null(),
        "{operation} requires FreeRTOS to have a selected current task"
    );
}

#[cfg(feature = "task-delete")]
fn assert_deletable_task(target: TaskHandle_t, current: Option<TaskHandle_t>) {
    let current = current.expect("TaskHandle::delete requires a running task context");
    assert!(
        current != target,
        "TaskHandle::delete cannot delete the current task; use unsafe delete_self"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_send<T: Send>() {}

    #[test]
    fn unique_and_borrowed_task_authorities_can_be_transferred() {
        assert_send::<TaskHandle>();
        assert_send::<TaskRef<'static>>();
    }

    #[test]
    fn task_names_are_bounded_and_nul_terminated() {
        let short = PreparedTaskName::new(b"worker");
        assert_eq!(&short.bytes[..7], b"worker\0");

        let terminated = PreparedTaskName::new(b"one\0ignored");
        assert_eq!(&terminated.bytes[..4], b"one\0");
        assert!(terminated.bytes[4..].iter().all(|byte| *byte == 0));

        let long = PreparedTaskName::new(&[b'x'; configMAX_TASK_NAME_LEN + 8]);
        assert!(long.bytes[..configMAX_TASK_NAME_LEN - 1]
            .iter()
            .all(|byte| *byte == b'x'));
        assert_eq!(long.bytes[configMAX_TASK_NAME_LEN - 1], 0);

        let empty = PreparedTaskName::new(b"");
        assert!(empty.bytes.iter().all(|byte| *byte == 0));
    }

    #[test]
    fn creation_validation_rejects_invalid_stacks_in_release_builds() {
        assert_eq!(
            validate_creation(0, 0),
            Err(TaskCreateError::StackTooSmall {
                supplied_words: 0,
                minimum_words: configMINIMAL_STACK_SIZE,
            })
        );
        assert_eq!(
            validate_creation(configMINIMAL_STACK_SIZE - 1, 0),
            Err(TaskCreateError::StackTooSmall {
                supplied_words: configMINIMAL_STACK_SIZE - 1,
                minimum_words: configMINIMAL_STACK_SIZE,
            })
        );
        assert_eq!(
            validate_creation(usize::MAX, 0),
            Err(TaskCreateError::StackByteSizeOverflow {
                supplied_words: usize::MAX,
            })
        );
        assert_eq!(validate_creation(configMINIMAL_STACK_SIZE, 0), Ok(()));
    }

    #[test]
    fn creation_validation_rejects_priority_instead_of_clamping() {
        let invalid = InvalidTaskPriority {
            requested: configMAX_PRIORITIES,
            maximum: configMAX_PRIORITIES - 1,
        };
        assert_eq!(
            validate_creation(configMINIMAL_STACK_SIZE, configMAX_PRIORITIES),
            Err(TaskCreateError::InvalidPriority(invalid))
        );
        assert_eq!(validate_priority(configMAX_PRIORITIES - 1), Ok(()));
    }

    #[cfg(feature = "task-suspend")]
    #[test]
    #[should_panic(expected = "requires FreeRTOS to have a selected current task")]
    fn resume_rejects_setup_with_every_task_suspended() {
        assert_has_selected_task("test resume", ptr::null_mut());
    }

    #[cfg(feature = "task-suspend")]
    #[test]
    fn resume_accepts_a_selected_task() {
        let mut selected = 0_u8;
        assert_has_selected_task("test resume", core::ptr::addr_of_mut!(selected).cast());
    }

    #[cfg(feature = "task-priority-set")]
    #[test]
    #[should_panic(expected = "requires FreeRTOS to have a selected current task")]
    fn priority_change_rejects_setup_with_every_task_suspended() {
        assert_has_selected_task("test priority change", ptr::null_mut());
    }

    #[cfg(feature = "task-delete")]
    #[test]
    #[should_panic(expected = "TaskHandle::delete cannot delete the current task")]
    fn owning_delete_rejects_the_current_task() {
        let mut task_storage = 0_u8;
        let task = core::ptr::addr_of_mut!(task_storage).cast();
        assert_deletable_task(task, Some(task));
    }

    #[cfg(feature = "task-delete")]
    #[test]
    #[should_panic(expected = "TaskHandle::delete requires a running task context")]
    fn owning_delete_rejects_pre_scheduler_setup() {
        let mut task_storage = 0_u8;
        let task = core::ptr::addr_of_mut!(task_storage).cast();
        assert_deletable_task(task, None);
    }

    #[cfg(feature = "task-delete")]
    #[test]
    fn owning_delete_allows_another_task() {
        let mut task_storage = 0_u8;
        let mut other_storage = 0_u8;
        let task = core::ptr::addr_of_mut!(task_storage).cast();
        let other = core::ptr::addr_of_mut!(other_storage).cast();
        assert_deletable_task(task, Some(other));
    }
}
