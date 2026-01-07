/*
 * FreeRTOS Kernel <DEVELOPMENT BRANCH>
 * Copyright (C) 2021 Amazon.com, Inc. or its affiliates. All Rights Reserved.
 *
 * SPDX-License-Identifier: MIT
 *
 * [AMENDMENT] This module contains the core FreeRTOS kernel components,
 * translated from the original C source files:
 * - list.rs   (from list.c)
 * - queue.rs  (from queue.c)
 * - tasks.rs  (from tasks.c)
 * - timers.rs (from timers.c)
 * - event_groups.rs (from event_groups.c)
 * - stream_buffer.rs (from stream_buffer.c)
 */

//! FreeRTOS Kernel Core
//!
//! This module contains the translated kernel components:
//!
//! - [`list`] - Linked list implementation used by the scheduler
//! - `queue` - Queue, semaphore, and mutex implementation (TODO)
//! - `tasks` - Task management and scheduler (TODO)
//! - `timers` - Software timer service (TODO)
//! - `event_groups` - Event group synchronization (TODO)
//! - `stream_buffer` - Stream and message buffers (TODO)

pub mod list;
pub mod queue;
pub mod tasks;
pub mod timers;
pub mod event_groups;
#[cfg(feature = "stream-buffers")]
pub mod stream_buffer;
