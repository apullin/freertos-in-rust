//! Safe synchronization primitives for FreeRTOS
//!
//! This module provides Rust-idiomatic wrappers around FreeRTOS synchronization
//! primitives. These wrappers use RAII patterns and provide memory safety
//! guarantees on top of the raw FreeRTOS API.
//!
//! # Example
//!
//! ```ignore
//! use freertos_in_rust::sync::Mutex;
//!
//! // Create a mutex protecting a counter
//! let counter: Mutex<u32> = Mutex::new(0).unwrap();
//!
//! // Lock and modify - automatically released when guard drops
//! {
//!     let mut guard = counter.lock();
//!     *guard += 1;
//! } // mutex released here
//! ```

mod mutex;
mod queue;
mod semaphore;
#[cfg(feature = "timers")]
mod timer;

pub use mutex::{Mutex, MutexGuard};
pub use queue::Queue;
pub use semaphore::{BinarySemaphore, CountingSemaphore};
#[cfg(feature = "timers")]
pub use timer::Timer;
