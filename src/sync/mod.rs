//! Safe synchronization primitives for FreeRTOS
//!
//! This module provides Rust-idiomatic wrappers around FreeRTOS synchronization
//! primitives. These wrappers use RAII patterns and provide memory safety
//! guarantees on top of the raw FreeRTOS API.
//!
//! # Example
//!
//! ```no_run
//! use freertos_in_rust::kernel::queue::StaticQueue_t;
//! use freertos_in_rust::sync::{Queue, TaskContext};
//!
//! let context = unsafe { TaskContext::assume() };
//! let storage = Box::leak(Box::new([0_u32; 4]));
//! let control = Box::leak(Box::new(StaticQueue_t::new()));
//! let queue = Queue::new_static(&context, storage, control).expect("queue creation failed");
//!
//! assert!(queue.try_send(&context, &42));
//! assert_eq!(queue.try_receive(&context), Some(42));
//! ```

mod event_group;
#[cfg(feature = "stream-buffers")]
mod message_buffer;
#[cfg(feature = "use-mutexes")]
mod mutex;
mod queue;
mod semaphore;
#[cfg(feature = "stream-buffers")]
mod stream_buffer;
mod task;
#[cfg(feature = "timers")]
mod timer;

pub use event_group::EventGroup;
#[cfg(feature = "stream-buffers")]
pub use message_buffer::{MessageBuffer, MessageBufferReceiver, MessageBufferSender};
#[cfg(feature = "use-mutexes")]
pub use mutex::{Mutex, MutexGuard};
pub use queue::Queue;
pub use semaphore::{BinarySemaphore, CountingSemaphore};
#[cfg(feature = "stream-buffers")]
pub use stream_buffer::{StreamBuffer, StreamBufferReceiver, StreamBufferSender};
#[cfg(feature = "task-delete")]
pub use task::delete_self;
#[cfg(feature = "task-suspend")]
pub use task::suspend_self;
pub use task::{
    CurrentTask, InvalidTaskPriority, TaskContext, TaskCreateError, TaskFn, TaskHandle, TaskRef,
};
#[cfg(feature = "timers")]
pub use timer::{Timer, TimerCallback};

#[inline(always)]
pub(crate) fn is_in_isr() -> bool {
    crate::port::xPortIsInsideInterrupt() != crate::types::pdFALSE
}

#[inline(always)]
pub(crate) fn assert_task_context(operation: &str) {
    assert!(
        !is_in_isr(),
        "{operation} is a task-context API and cannot be called from an ISR"
    );
}

#[inline(always)]
pub(crate) fn assert_can_block(operation: &str, ticks: crate::types::TickType_t) {
    assert_task_context(operation);

    if ticks != 0 {
        assert!(
            crate::kernel::tasks::xTaskGetSchedulerState()
                == crate::kernel::tasks::taskSCHEDULER_RUNNING,
            "{operation} cannot block unless the scheduler is running"
        );

        #[cfg(feature = "timers")]
        assert!(
            crate::kernel::tasks::xTaskGetCurrentTaskHandle()
                != crate::kernel::timers::xTimerGetTimerDaemonTaskHandle(),
            "{operation} cannot block the timer daemon task"
        );
    }
}
