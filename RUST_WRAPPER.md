# FreeRusTOS Safe Wrappers Guide

This guide documents the safe Rust wrappers around FreeRTOS primitives. These wrappers provide idiomatic Rust APIs with RAII patterns, type safety, and reduced `unsafe` usage.

## Table of Contents

- [Convenience Functions](#convenience-functions)
- [Static Allocation](#static-allocation)
- [Task](#task)
- [Mutex](#mutex)
- [Semaphores](#semaphores)
- [Queue](#queue)
- [Timer](#timer)
- [EventGroup](#eventgroup)
- [StreamBuffer](#streambuffer)
- [MessageBuffer](#messagebuffer)

---

## Convenience Functions

Simple helpers available at the crate root for common operations.

```rust
use freertos_in_rust::{delay, delay_ms, start_scheduler};

// Delay by raw ticks
delay(100);

// Delay by milliseconds (converts using configured tick rate)
delay_ms(500);

// Start the scheduler (does not return)
start_scheduler();
```

**Replaces:**
```rust
// Old way
vTaskDelay(pdMS_TO_TICKS(500));
vTaskStartScheduler();
```

---

## Static Allocation

All wrappers support static allocation, allowing you to pre-allocate all FreeRTOS objects at compile time. This pattern is ideal for embedded systems where:
- You want deterministic memory usage
- You're avoiding heap allocation entirely
- You need to account for all resources at compile time

### Pattern

Each wrapper provides a `new_static()` method (or `new_periodic_static()`/`new_oneshot_static()` for timers) that takes static storage buffers:

```rust
use core::mem::MaybeUninit;

// 1. Declare static storage
static mut BUFFER: MaybeUninit<StaticType_t> = MaybeUninit::uninit();

// 2. Create wrapper with static storage
let wrapper = Wrapper::new_static(
    unsafe { BUFFER.assume_init_mut() },
)?;

// 3. Use normally - no difference in API
wrapper.method();
```

### Why MaybeUninit?

FreeRTOS kernel types like `StaticQueue_t`, `StaticTimer_t`, etc. don't have const `new()` constructors in Rust. Using `MaybeUninit<T>` allows declaring uninitialized static storage that FreeRTOS will initialize.

### Static Types by Wrapper

| Wrapper | Static Type | Additional Storage |
|---------|-------------|-------------------|
| `Task` | `StaticTask_t` | `[StackType_t; N]` |
| `Mutex<T>` | `StaticQueue_t` | - |
| `BinarySemaphore` | `StaticQueue_t` | - |
| `CountingSemaphore` | `StaticQueue_t` | - |
| `Queue<T>` | `StaticQueue_t` | `[T; N]` |
| `Timer` | `StaticTimer_t` | - |
| `EventGroup` | `StaticEventGroup_t` | - |
| `StreamBuffer` | `StaticStreamBuffer_t` | `[u8; N]` |
| `MessageBuffer` | `StaticStreamBuffer_t` | `[u8; N]` |

See individual wrapper sections for complete examples.

---

## Task

Safe wrapper for FreeRTOS task creation and management.

### Dynamic Allocation (Heap)

When using heap features (`alloc`, `heap-4`, `heap-5`), tasks can be spawned with automatic memory allocation:

```rust
use freertos_in_rust::sync::TaskHandle;

extern "C" fn my_task(_param: *mut c_void) {
    loop {
        // Task work here
        delay_ms(1000);
    }
}

// Spawn a task with 256-word stack at priority 2
let handle = TaskHandle::spawn(
    b"MyTask\0",    // null-terminated name
    256,            // stack size in words
    2,              // priority
    my_task,
).expect("Failed to create task");
```

### Static Allocation (Embedded)

For embedded systems avoiding dynamic allocation, provide your own stack and TCB:

```rust
use freertos_in_rust::sync::TaskHandle;
use freertos_in_rust::kernel::tasks::StaticTask_t;
use freertos_in_rust::types::StackType_t;

// Static storage - must be 'static lifetime
static mut WORKER_STACK: [StackType_t; 256] = [0; 256];
static mut WORKER_TCB: StaticTask_t = StaticTask_t::new();

extern "C" fn worker_task(_param: *mut c_void) {
    loop {
        // Do work
        delay_ms(100);
    }
}

// Spawn with static storage
let handle = TaskHandle::spawn_static(
    b"Worker\0",
    unsafe { &mut WORKER_STACK },
    unsafe { &mut WORKER_TCB },
    2,
    worker_task,
).expect("Failed to create task");
```

### Passing Parameters

Pass data to tasks via the parameter pointer:

```rust
static TASK_CONFIG: TaskConfig = TaskConfig { ... };

extern "C" fn configurable_task(param: *mut c_void) {
    let config = unsafe { &*(param as *const TaskConfig) };
    loop {
        // Use config
    }
}

let handle = TaskHandle::spawn_with_param(
    b"Configured\0",
    256,
    2,
    configurable_task,
    &TASK_CONFIG as *const _ as *mut c_void,
)?;
```

### Task Control

```rust
// Suspend a task (requires feature "task-suspend")
handle.suspend();

// Resume a suspended task
handle.resume();

// Delete a task (requires feature "task-delete")
handle.delete();  // Takes ownership, can't use handle after

// Delete the current task
delete_self();  // Does not return
```

### Priority Management

```rust
// Get current priority (requires feature "task-priority-set")
let prio = handle.priority();

// Change priority
handle.set_priority(3);
```

### Query

```rust
// Get handle to currently running task
let current = TaskHandle::current();

// Get raw handle for interop
let raw = unsafe { handle.raw_handle() };

// Create from raw handle
let handle = unsafe { TaskHandle::from_raw(raw) };
```

### Feature Flags

| Operation | Required Feature |
|-----------|------------------|
| `spawn()` | heap feature |
| `spawn_static()` | (always available) |
| `suspend()` / `resume()` | `task-suspend` |
| `delete()` | `task-delete` |
| `priority()` / `set_priority()` | `task-priority-set` |

**Replaces:**
```rust
// Old way
static mut STACK: [StackType_t; 256] = [0; 256];
static mut TCB: StaticTask_t = StaticTask_t::new();

unsafe {
    let handle = xTaskCreateStatic(
        my_task,
        b"MyTask\0".as_ptr(),
        256,
        ptr::null_mut(),
        2,
        STACK.as_mut_ptr(),
        &mut TCB as *mut StaticTask_t,
    );
    if handle.is_null() {
        panic!("Failed to create task");
    }
}
```

---

## Mutex

A mutual exclusion primitive with RAII guard pattern and priority inheritance.

**Feature:** `use-mutexes`

### Basic Usage

```rust
use freertos_in_rust::sync::Mutex;

// Create a mutex protecting shared data
let counter: Mutex<u32> = Mutex::new(0).expect("Failed to create mutex");

// Lock and modify - automatically released when guard drops
{
    let mut guard = counter.lock();
    *guard += 1;
} // mutex released here

// Try to lock without blocking
if let Some(mut guard) = counter.try_lock() {
    *guard += 1;
}

// Lock with timeout (100 ticks)
if let Some(mut guard) = counter.lock_timeout(100) {
    *guard += 1;
}
```

### Sharing Between Tasks

```rust
use freertos_in_rust::sync::Mutex;

// Mutex<T> is Send + Sync, so it can be shared via static
static SHARED: Mutex<u32> = Mutex::new(0).unwrap();

// Task 1
{
    let mut guard = SHARED.lock();
    *guard = 42;
}

// Task 2
{
    let value = *SHARED.lock();
}
```

### Priority Inheritance

When a high-priority task blocks on a mutex held by a low-priority task, the low-priority task's priority is temporarily boosted. This happens automatically - no extra code needed.

### Static Allocation

For embedded systems avoiding heap allocation:

```rust
use freertos_in_rust::sync::Mutex;
use freertos_in_rust::kernel::queue::StaticQueue_t;
use core::mem::MaybeUninit;

// Static storage
static mut MUTEX_BUF: MaybeUninit<StaticQueue_t> = MaybeUninit::uninit();

// Create mutex with static storage
let counter: Mutex<u32> = Mutex::new_static(
    0,  // initial value
    unsafe { MUTEX_BUF.assume_init_mut() },
).expect("Failed to create mutex");

// Usage is identical to heap-allocated mutex
let mut guard = counter.lock();
*guard += 1;
```

**Replaces:**
```rust
// Old way
unsafe {
    let mutex = xQueueCreateMutex(queueQUEUE_TYPE_MUTEX);
    xSemaphoreTake(mutex, portMAX_DELAY);
    // ... do work ...
    xSemaphoreGive(mutex);
}
```

---

## Semaphores

### BinarySemaphore

A semaphore with only two states: available or taken. Useful for signaling between tasks or from ISRs. Does **not** have priority inheritance.

```rust
use freertos_in_rust::sync::BinarySemaphore;

let sem = BinarySemaphore::new().expect("Failed to create semaphore");

// Task 1: Wait for signal
sem.take();  // Blocks until signaled
// ... handle event ...

// Task 2 (or ISR): Send signal
sem.give();
```

### Non-blocking and Timeout Variants

```rust
// Try without blocking
if sem.try_take() {
    // Got it
}

// Wait with timeout
if sem.take_timeout(100) {
    // Got it within 100 ticks
}
```

### CountingSemaphore

Tracks a count of available resources.

```rust
use freertos_in_rust::sync::CountingSemaphore;

// Create with max 5, initially 3 available
let pool = CountingSemaphore::new(5, 3).expect("Failed to create");

// Acquire a resource (decrements count, blocks if 0)
pool.take();

// Release a resource (increments count)
pool.give();

// Non-blocking try
if pool.try_take() {
    // Got a resource
}
```

### Static Allocation

For embedded systems avoiding heap allocation:

```rust
use freertos_in_rust::sync::{BinarySemaphore, CountingSemaphore};
use freertos_in_rust::kernel::queue::StaticQueue_t;
use core::mem::MaybeUninit;

// Static storage for binary semaphore
static mut BINARY_SEM_BUF: MaybeUninit<StaticQueue_t> = MaybeUninit::uninit();

let sem = BinarySemaphore::new_static(
    unsafe { BINARY_SEM_BUF.assume_init_mut() },
).expect("Failed to create binary semaphore");

// Static storage for counting semaphore
static mut COUNT_SEM_BUF: MaybeUninit<StaticQueue_t> = MaybeUninit::uninit();

let pool = CountingSemaphore::new_static(
    5,   // max count
    3,   // initial count
    unsafe { COUNT_SEM_BUF.assume_init_mut() },
).expect("Failed to create counting semaphore");

// Usage is identical to heap-allocated semaphores
sem.give();
pool.take();
```

**Replaces:**
```rust
// Old way
unsafe {
    let sem = xQueueGenericCreate(1, 0, queueQUEUE_TYPE_BINARY_SEMAPHORE);
    xQueueSemaphoreTake(sem, portMAX_DELAY);
    xQueueGenericSend(sem, ptr::null(), 0, queueSEND_TO_BACK);
}
```

---

## Queue

A type-safe FIFO queue for inter-task communication.

**Feature:** `new()` requires heap (`alloc`, `heap-4`, or `heap-5`). `new_static()` works without heap.

### Basic Usage

```rust
use freertos_in_rust::sync::Queue;

// Create a queue holding up to 10 u32 values
let queue: Queue<u32> = Queue::new(10).expect("Failed to create queue");

// Send a value (blocks if full)
queue.send(&42);

// Receive a value (blocks if empty)
if let Some(value) = queue.receive() {
    println!("Got: {}", value);
}
```

### Non-blocking and Timeout

```rust
// Try to send without blocking
if queue.try_send(&42) {
    // Sent successfully
}

// Send with timeout
if queue.send_timeout(&42, 100) {
    // Sent within 100 ticks
}

// Try to receive without blocking
if let Some(value) = queue.try_receive() {
    // Got a value
}

// Receive with timeout
if let Some(value) = queue.receive_timeout(100) {
    // Got a value within 100 ticks
}
```

### Send to Front (Priority)

```rust
// Send to front of queue (received before other items)
queue.send_to_front(&urgent_value);
```

### Peek Without Removing

```rust
// Look at front item without removing it
if let Some(value) = queue.peek() {
    // value is a copy, original stays in queue
}
```

### Query State

```rust
queue.len();        // Items currently in queue
queue.available();  // Free slots
queue.is_empty();
queue.is_full();
queue.reset();      // Remove all items
```

### Typed Messages

```rust
#[derive(Copy, Clone)]
struct Command {
    id: u8,
    value: i32,
}

let cmd_queue: Queue<Command> = Queue::new(5)?;

cmd_queue.send(&Command { id: 1, value: 100 });

if let Some(cmd) = cmd_queue.receive() {
    match cmd.id {
        1 => { /* handle */ },
        _ => {},
    }
}
```

### Static Allocation

For embedded systems avoiding heap allocation:

```rust
use freertos_in_rust::sync::Queue;
use freertos_in_rust::kernel::queue::StaticQueue_t;
use core::mem::MaybeUninit;

// Static storage - both for the queue control block and item storage
static mut QUEUE_BUF: MaybeUninit<StaticQueue_t> = MaybeUninit::uninit();
static mut QUEUE_STORAGE: [u32; 10] = [0; 10];

let queue: Queue<u32> = Queue::new_static(
    unsafe { &mut QUEUE_STORAGE },
    unsafe { QUEUE_BUF.assume_init_mut() },
).expect("Failed to create queue");

// Usage is identical to heap-allocated queue
queue.send(&42);
if let Some(value) = queue.receive() {
    // Got value
}
```

**Replaces:**
```rust
// Old way
unsafe {
    let queue = xQueueGenericCreate(10, size_of::<u32>(), queueQUEUE_TYPE_BASE);
    xQueueGenericSend(queue, &value as *const _ as *const c_void, timeout, queueSEND_TO_BACK);
    xQueueReceive(queue, &mut value as *mut _ as *mut c_void, timeout);
}
```

---

## Timer

Software timers that execute callbacks in the timer daemon task.

**Feature:** `timers`

### Periodic Timer

```rust
use freertos_in_rust::sync::Timer;

extern "C" fn on_tick(_timer: TimerHandle_t) {
    // Called every 1000 ticks
    println!("Timer fired!");
}

let timer = Timer::new_periodic(
    b"MyTimer\0",
    1000,  // period in ticks
    on_tick,
).expect("Failed to create timer");

timer.start();
```

### One-Shot Timer

```rust
extern "C" fn on_timeout(_timer: TimerHandle_t) {
    println!("Timeout!");
}

let timer = Timer::new_oneshot(
    b"Timeout\0",
    5000,  // delay in ticks
    on_timeout,
)?;

timer.start();  // Fires once after 5000 ticks
```

### Timer Control

```rust
timer.start();              // Start the timer
timer.stop();               // Stop the timer
timer.reset();              // Restart period from now
timer.set_period(2000);     // Change period

// With command queue timeout
timer.start_timeout(100);
timer.stop_timeout(100);
```

### Query State

```rust
timer.is_active();    // Is timer running?
timer.period();       // Current period in ticks
timer.expiry_time();  // Tick count when timer will next fire
```

### Timer ID (Context Passing)

Pass context to timer callbacks using the timer ID:

```rust
struct TimerContext {
    counter: u32,
    threshold: u32,
}

static mut CTX: TimerContext = TimerContext { counter: 0, threshold: 10 };

extern "C" fn callback(timer_handle: TimerHandle_t) {
    let timer = unsafe { Timer::from_raw(timer_handle) };
    if let Some(ctx) = timer.get_id::<TimerContext>() {
        unsafe {
            (*ctx).counter += 1;
            if (*ctx).counter >= (*ctx).threshold {
                timer.stop();
            }
        }
    }
}

let timer = Timer::new_periodic(b"Counter\0", 100, callback)?;
timer.set_id(unsafe { &mut CTX });
timer.start();
```

```rust
// Set typed context
timer.set_id(&mut my_context);

// Get typed context (returns Option)
if let Some(ctx) = timer.get_id::<MyContext>() {
    // ctx is *mut MyContext
}

// Get raw pointer
let raw = timer.get_id_raw();  // *mut c_void
```

### Static Allocation

For embedded systems avoiding heap allocation:

```rust
use freertos_in_rust::sync::Timer;
use freertos_in_rust::kernel::timer::StaticTimer_t;
use core::mem::MaybeUninit;

// Static storage
static mut TIMER_BUF: MaybeUninit<StaticTimer_t> = MaybeUninit::uninit();

extern "C" fn on_tick(_timer: TimerHandle_t) {
    // Timer callback
}

// Create periodic timer with static storage
let timer = Timer::new_periodic_static(
    b"MyTimer\0",
    1000,  // period in ticks
    on_tick,
    unsafe { TIMER_BUF.assume_init_mut() },
).expect("Failed to create timer");

// Create one-shot timer with static storage
static mut ONESHOT_BUF: MaybeUninit<StaticTimer_t> = MaybeUninit::uninit();

let oneshot = Timer::new_oneshot_static(
    b"Timeout\0",
    5000,  // delay in ticks
    on_timeout,
    unsafe { ONESHOT_BUF.assume_init_mut() },
)?;

// Usage is identical to heap-allocated timers
timer.start();
```

**Replaces:**
```rust
// Old way
unsafe {
    let timer = xTimerCreate(name, period, auto_reload, id, callback);
    xTimerStart(timer, 0);
    xTimerStop(timer, 0);
}
```

---

## EventGroup

Event groups allow tasks to wait on combinations of event bits.

**Feature:** `new()` requires heap (`alloc`, `heap-4`, or `heap-5`). `new_static()` works without heap.

### Basic Usage

```rust
use freertos_in_rust::sync::EventGroup;

const DATA_READY: u32 = 0x01;
const ACK: u32 = 0x02;
const ERROR: u32 = 0x04;

let events = EventGroup::new().expect("Failed to create event group");

// Task 1: Signal data ready
events.set(DATA_READY);

// Task 2: Wait for data ready
events.wait_any(DATA_READY);  // Blocks until DATA_READY is set
```

### Wait for ANY Bit (OR)

```rust
// Wait for any of these bits (doesn't clear)
let bits = events.wait_any(DATA_READY | ERROR);

// Wait for any, clear the matched bits on exit
let bits = events.wait_any_clear(DATA_READY | ERROR);

// With timeout
if let Some(bits) = events.wait_any_timeout(DATA_READY | ERROR, 1000) {
    // Got at least one bit within 1000 ticks
}
```

### Wait for ALL Bits (AND)

```rust
// Wait for all bits to be set
let bits = events.wait_all(DATA_READY | ACK);

// Wait for all, clear them on exit
let bits = events.wait_all_clear(DATA_READY | ACK);

// With timeout
if let Some(bits) = events.wait_all_timeout(DATA_READY | ACK, 1000) {
    // All bits were set within 1000 ticks
}
```

### Synchronization (Rendezvous)

```rust
// Multiple tasks synchronize at a barrier
const TASK1_BIT: u32 = 0x01;
const TASK2_BIT: u32 = 0x02;
const TASK3_BIT: u32 = 0x04;
const ALL_TASKS: u32 = TASK1_BIT | TASK2_BIT | TASK3_BIT;

// Each task sets its bit and waits for all
// Task 1:
events.sync(TASK1_BIT, ALL_TASKS);

// Task 2:
events.sync(TASK2_BIT, ALL_TASKS);

// Task 3:
events.sync(TASK3_BIT, ALL_TASKS);

// All tasks unblock when the last one arrives
```

### Set/Clear/Get

```rust
events.set(DATA_READY);           // Set bits, returns bits after
events.clear(DATA_READY);         // Clear bits, returns bits before
let current = events.get();       // Get current bits
```

### Static Allocation

For embedded systems avoiding heap allocation:

```rust
use freertos_in_rust::sync::EventGroup;
use freertos_in_rust::kernel::event_groups::StaticEventGroup_t;
use core::mem::MaybeUninit;

// Static storage
static mut EVENT_BUF: MaybeUninit<StaticEventGroup_t> = MaybeUninit::uninit();

let events = EventGroup::new_static(
    unsafe { EVENT_BUF.assume_init_mut() },
).expect("Failed to create event group");

// Usage is identical to heap-allocated event group
events.set(DATA_READY);
events.wait_any(DATA_READY);
```

**Replaces:**
```rust
// Old way
unsafe {
    let eg = xEventGroupCreate();
    xEventGroupSetBits(eg, bits);
    xEventGroupWaitBits(eg, bits, clear_on_exit, wait_for_all, timeout);
}
```

---

## StreamBuffer

Byte stream transfers between tasks. Optimized for continuous data flow.

**Feature:** `stream-buffers`

**Important:** Single producer, single consumer only.

### Basic Usage

```rust
use freertos_in_rust::sync::StreamBuffer;

// Create 128-byte buffer, wake reader when any data arrives (trigger=1)
let stream = StreamBuffer::new(128, 1).expect("Failed to create");

// Producer: send bytes
stream.send(b"Hello, world!");

// Consumer: receive bytes
let mut buf = [0u8; 64];
let n = stream.receive(&mut buf);
// buf[..n] contains received data
```

### Non-blocking and Timeout

```rust
// Try to send without blocking (returns bytes sent)
let sent = stream.try_send(b"data");

// Send with timeout
let sent = stream.send_timeout(b"data", 100);

// Try to receive without blocking
let n = stream.try_receive(&mut buf);

// Receive with timeout
let n = stream.receive_timeout(&mut buf, 100);
```

### Typed Send/Receive

Send and receive structs directly:

```rust
#[derive(Copy, Clone)]
#[repr(C)]
struct Packet {
    seq: u32,
    data: [u8; 16],
}

let stream = StreamBuffer::new(256, 1)?;

// Send a struct (returns true if all bytes sent)
let packet = Packet { seq: 1, data: [0; 16] };
if stream.send_val(&packet) {
    // All bytes sent
}

// Receive a struct (returns Some if all bytes received)
if let Some(p) = stream.receive_val::<Packet>() {
    println!("Got packet {}", p.seq);
}

// Non-blocking
stream.try_send_val(&packet);
let p = stream.try_receive_val::<Packet>();

// With timeout
stream.send_val_timeout(&packet, 100);
let p = stream.receive_val_timeout::<Packet>(100);
```

**Note:** Unlike MessageBuffer, stream buffers can do partial transfers. The typed methods return `bool`/`Option` to indicate if ALL bytes were transferred.

### Query State

```rust
stream.available();   // Bytes ready to read
stream.spaces();      // Free space in bytes
stream.is_empty();
stream.is_full();
stream.reset();       // Clear buffer (fails if tasks blocked)
```

### Trigger Level

The trigger level (set at creation) determines when a blocked reader wakes:
- `trigger=1`: Wake as soon as any data arrives
- `trigger=64`: Wake when 64+ bytes are available

```rust
// Wake reader only when 64+ bytes available
let stream = StreamBuffer::new(256, 64)?;
```

### Static Allocation

For embedded systems avoiding heap allocation:

```rust
use freertos_in_rust::sync::StreamBuffer;
use freertos_in_rust::kernel::stream_buffer::StaticStreamBuffer_t;
use core::mem::MaybeUninit;

// Static storage - both for control block and data buffer
static mut STREAM_BUF: MaybeUninit<StaticStreamBuffer_t> = MaybeUninit::uninit();
static mut STREAM_STORAGE: [u8; 128] = [0; 128];

let stream = StreamBuffer::new_static(
    unsafe { &mut STREAM_STORAGE },
    1,  // trigger level
    unsafe { STREAM_BUF.assume_init_mut() },
).expect("Failed to create stream buffer");

// Usage is identical to heap-allocated stream buffer
stream.send(b"data");
let mut buf = [0u8; 32];
let n = stream.receive(&mut buf);
```

**Replaces:**
```rust
// Old way
unsafe {
    let sb = xStreamBufferCreate(128, 1);
    xStreamBufferSend(sb, data.as_ptr() as *const c_void, data.len(), timeout);
    xStreamBufferReceive(sb, buf.as_mut_ptr() as *mut c_void, buf.len(), timeout);
}
```

---

## MessageBuffer

Discrete message transfers. Unlike StreamBuffer, messages are atomic - you receive complete messages or nothing.

**Feature:** `stream-buffers`

**Important:** Single producer, single consumer only.

### Basic Usage (Raw Bytes)

```rust
use freertos_in_rust::sync::MessageBuffer;

let msgs = MessageBuffer::new(256).expect("Failed to create");

// Send a message
msgs.send(b"hello");

// Receive a message
let mut buf = [0u8; 64];
let n = msgs.receive(&mut buf);
// buf[..n] contains the complete message
```

### Typed Messages (Recommended)

Send and receive structs directly without manual serialization:

```rust
#[derive(Copy, Clone)]
#[repr(C)]
struct SensorReading {
    sensor_id: u8,
    temperature: i16,
    humidity: u8,
}

let msgs = MessageBuffer::new(256)?;

// Send a struct
let reading = SensorReading {
    sensor_id: 1,
    temperature: 250,  // 25.0 C
    humidity: 60,
};
msgs.send_val(&reading);

// Receive a struct
if let Some(r) = msgs.receive_val::<SensorReading>() {
    println!("Sensor {}: {}C, {}%", r.sensor_id, r.temperature, r.humidity);
}

// With timeout
if let Some(r) = msgs.receive_val_timeout::<SensorReading>(1000) {
    // Got reading within 1000 ticks
}
```

### Non-blocking Variants

```rust
// Try to send without blocking
if msgs.try_send_val(&reading) {
    // Sent successfully
}

// Try to receive without blocking
if let Some(r) = msgs.try_receive_val::<SensorReading>() {
    // Got a message
}
```

### Check Message Size Before Receiving

```rust
// Peek at next message size (useful for variable-length messages)
let size = msgs.next_message_size();
if size > 0 {
    let mut buf = vec![0u8; size];
    msgs.receive(&mut buf);
}
```

### Query State

```rust
msgs.next_message_size();  // Size of next message (0 if empty)
msgs.bytes_used();         // Total bytes used (including length prefixes)
msgs.spaces();             // Free space
msgs.is_empty();
msgs.is_full();
msgs.reset();              // Clear buffer
```

### Message Size Overhead

Each message has a 4-byte length prefix. When sizing your buffer:
- Buffer must fit: `message_size + 4` bytes minimum
- For N messages of size S: buffer should be `N * (S + 4)` bytes

### Static Allocation

For embedded systems avoiding heap allocation:

```rust
use freertos_in_rust::sync::MessageBuffer;
use freertos_in_rust::kernel::stream_buffer::StaticStreamBuffer_t;
use core::mem::MaybeUninit;

// Static storage - both for control block and data buffer
static mut MSG_BUF: MaybeUninit<StaticStreamBuffer_t> = MaybeUninit::uninit();
static mut MSG_STORAGE: [u8; 256] = [0; 256];

let msgs = MessageBuffer::new_static(
    unsafe { &mut MSG_STORAGE },
    unsafe { MSG_BUF.assume_init_mut() },
).expect("Failed to create message buffer");

// Usage is identical to heap-allocated message buffer
msgs.send(b"hello");
let mut buf = [0u8; 64];
let n = msgs.receive(&mut buf);
```

**Replaces:**
```rust
// Old way
unsafe {
    let mb = xMessageBufferCreate(256);
    xStreamBufferSend(mb, &msg as *const _ as *const c_void, size_of_val(&msg), timeout);

    let mut msg = MaybeUninit::uninit();
    xStreamBufferReceive(mb, msg.as_mut_ptr() as *mut c_void, size, timeout);
    msg.assume_init()
}
```

---

## Feature Flags Summary

| Wrapper | `new()` (heap) | `new_static()` |
|---------|----------------|----------------|
| `Mutex<T>` | `use-mutexes` + heap | `use-mutexes` |
| `BinarySemaphore` | heap feature | (always) |
| `CountingSemaphore` | heap feature | (always) |
| `Queue<T>` | heap feature | (always) |
| `Timer` | `timers` + heap | `timers` |
| `EventGroup` | heap feature | (always) |
| `StreamBuffer` | `stream-buffers` + heap | `stream-buffers` |
| `MessageBuffer` | `stream-buffers` + heap | `stream-buffers` |

Heap features: `alloc`, `heap-4`, or `heap-5`

**Note:** Static allocation methods (`new_static()`, `new_periodic_static()`, etc.) don't require heap features. They're ideal for embedded systems with strict memory requirements.

---

## Migration Guide

### From Raw FreeRTOS API

1. **Replace handle statics** with wrapper types
2. **Remove `unsafe` blocks** around primitive operations
3. **Use RAII guards** instead of manual take/give pairs
4. **Use typed APIs** instead of void pointer casts

### Example Migration

**Before:**
```rust
static mut MUTEX: QueueHandle_t = ptr::null_mut();
static mut COUNTER: u32 = 0;

unsafe {
    MUTEX = xQueueCreateMutex(queueQUEUE_TYPE_MUTEX);
}

// In task:
unsafe {
    xSemaphoreTake(MUTEX, portMAX_DELAY);
    COUNTER += 1;
    xSemaphoreGive(MUTEX);
}
```

**After:**
```rust
static COUNTER: Mutex<u32> = Mutex::new(0).unwrap();

// In task:
{
    let mut guard = COUNTER.lock();
    *guard += 1;
} // automatically released
```
