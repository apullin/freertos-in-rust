# FreeRTOS-in-Rust safe veneer

FreeRTOS-in-Rust exposes two layers from one crate:

- `kernel` contains the close, C-shaped FreeRTOS translation. Raw handles,
  caller-owned storage, deletion, callbacks, and context-sensitive operations
  are `unsafe` when Rust cannot verify their contracts.
- `sync` contains the semantic Rust veneer. It validates sizes, priorities,
  event bits, timer periods, names, execution context, and object ownership.

The veneer is deliberately small. It does not try to redesign the scheduler or
hide how FreeRTOS works.

## The task-context capability

Task-only operations require `&TaskContext`. Constructing the capability is the
point where an application asserts that it is in a task, or in exclusive
single-threaded setup before the scheduler starts:

```rust
use freertos_in_rust::sync::TaskContext;

// SAFETY: no ISR can call the kernel during this startup section.
let context = unsafe { TaskContext::assume() };
```

Create a fresh capability at each task entry point. `TaskContext` is neither
`Send` nor `Sync`, so safe code cannot move one into an ISR or share it with a
different execution context.

```rust
use core::ffi::c_void;
use freertos_in_rust::{delay_ms, sync::TaskContext};

extern "C" fn worker(_: *mut c_void) {
    // SAFETY: FreeRTOS invokes this function as a task entry point.
    let context = unsafe { TaskContext::assume() };
    loop {
        // Work...
        delay_ms(&context, 100);
    }
}
```

Blocking calls also verify that the scheduler is running. A timer callback may
use zero-timeout task APIs, but the veneer rejects blocking calls from the timer
daemon because they can deadlock all software timers.

Start the scheduler by consuming the setup capability:

```rust
freertos_in_rust::start_scheduler(context)
```

The call returns only by panicking if scheduler startup fails.

## Allocation

Dynamic constructors are available with exactly one of these crate features:

- `heap-4`
- `heap-5`
- `alloc`

Static constructors remain available without an allocator. Static storage must
remain at a stable address for the complete kernel-object lifetime. The kernel
control types provide `const fn new()` and the wrapper takes a `'static` mutable
borrow, so safe code cannot reuse the storage while the object is live.

Do not create a reference to uninitialized storage. Initialize a
`StaticTask_t`, `StaticQueue_t`, `StaticTimer_t`, `StaticEventGroup_t`, or
`StaticStreamBuffer_t` with its `new()` constructor.

## Tasks

`TaskHandle` is unique control and deletion authority for a live task. It is
not `Copy`, `Clone`, or `Sync`. Dropping it relinquishes the authority but does
not delete the task. `TaskRef<'_>` is a non-deleting borrow, and
`CurrentTask<'_>` is a scoped view of the currently executing task.

Dynamic task creation copies and bounds the supplied name. Stack depth is in
`StackType_t` words, not bytes:

```rust
use freertos_in_rust::sync::TaskHandle;

let handle = TaskHandle::spawn(
    &context,
    b"worker",
    256,
    2,
    worker,
).expect("task creation failed");
```

Static creation borrows the stack and TCB for `'static`:

```rust
use freertos_in_rust::{
    kernel::tasks::StaticTask_t,
    sync::TaskHandle,
    types::StackType_t,
};

static mut STACK: [StackType_t; 256] = [0; 256];
static mut TCB: StaticTask_t = StaticTask_t::new();

// SAFETY: startup has exclusive access; both objects remain allocated forever.
let handle = unsafe {
    TaskHandle::spawn_static(
        &context,
        b"worker",
        &mut STACK,
        &mut TCB,
        2,
        worker,
    )
}.expect("task creation failed");
```

Creation rejects undersized or overflowing stacks and priorities outside
`0..configMAX_PRIORITIES`, including in release builds. Passing a raw task
parameter uses `spawn_with_param` or `spawn_static_with_param` and is `unsafe`
because the caller owns its lifetime, aliasing, and synchronization contract.

Task control follows ownership:

```rust
let mut handle = handle;
let priority = handle.priority(&context);
handle.set_priority(&context, priority + 1)?;

let mut reference = handle.borrow();
reference.resume(&context);
drop(reference);

// SAFETY: an application shutdown handshake has confirmed that the target
// has released all resources and parked without outstanding kernel references.
unsafe { handle.delete(&context) }; // consumes the unique owner
```

`TaskHandle::delete` is unsafe: unique handle ownership does not prove the
target has released its mutexes or other resources. Deletion skips the target's
stack destructors; merely suspending it is not a safe shutdown handshake.
It also rejects a handle for the currently executing task. Use
`unsafe delete_self` for that case: the caller must prove that no owning
`TaskHandle` survives and release all resources that need cleanup before
deletion. Owning deletion also requires a running task context; pre-scheduler task
retirement remains a raw, unsafe operation.

## Queues

`Queue<T>` transfers initialized `T: Copy` values. Unlike stream and message
buffers, it preserves the exact typed item boundary.

```rust
use freertos_in_rust::sync::Queue;

let queue = Queue::<u32>::new(&context, 8).expect("queue creation failed");
assert!(queue.send(&context, &42));
assert_eq!(queue.receive(&context), Some(42));
```

Static queues use initialized item storage and a control block:

```rust
use freertos_in_rust::{kernel::queue::StaticQueue_t, sync::Queue};

static mut ITEMS: [u32; 8] = [0; 8];
static mut CONTROL: StaticQueue_t = StaticQueue_t::new();

// SAFETY: setup owns both statics, which remain live for the queue lifetime.
let queue = unsafe { Queue::new_static(&context, &mut ITEMS, &mut CONTROL) }
    .expect("queue creation failed");
```

Send, receive, front-send, peek, reset, length, and timeout variants all require
task context. Separate `*_from_isr` methods are `unsafe` and nonblocking.

## Semaphores and mutexes

Binary and counting semaphores use the same task-context/ISR split:

```rust
use freertos_in_rust::sync::{BinarySemaphore, CountingSemaphore};

let ready = BinarySemaphore::new(&context).expect("semaphore creation failed");
assert!(ready.give(&context));
assert!(ready.take(&context));

let slots = CountingSemaphore::new(&context, 4, 4)
    .expect("counting semaphore creation failed");
assert_eq!(slots.count(&context), 4);
```

`Mutex<T>` adds priority inheritance and returns a guard only after the kernel
confirms acquisition:

```rust
use freertos_in_rust::sync::Mutex;

let counter = Mutex::new(&context, 0_u32).expect("mutex creation failed");
if let Some(mut guard) = counter.lock_timeout(&context, 10) {
    *guard += 1;
} else {
    // Timed out; no guard and no reference to the protected value exists.
}
```

`MutexGuard` is not `Send`; it must be released by the task that acquired it so
priority inheritance unwinds correctly. Guard release is checked in all build
modes.

## Event groups

Event-group operations reject zero masks and FreeRTOS-reserved control bits:

```rust
use freertos_in_rust::sync::EventGroup;

const READY: u32 = 1 << 0;
let events = EventGroup::new(&context).expect("event group creation failed");
events.set(&context, READY);
let observed = events.wait_any_clear_timeout(&context, READY, 100);
assert!(observed.is_some());
```

The event-bit type follows the configured tick width, so AArch64 builds retain
user bits above bit 31. From-ISR set and clear operations require the timer
pend-function facility and remain `unsafe` because interrupt priority cannot be
expressed in a Rust type.

## Stream buffers

Stream buffers transfer bytes and may perform partial transfers. They do not
reinterpret arbitrary `T: Copy` values. A buffer is single-producer,
single-consumer and is not `Sync`.

Split the owner once to obtain non-cloneable endpoints:

```rust
use freertos_in_rust::sync::StreamBuffer;

let mut stream = StreamBuffer::new(&context, 128, 1)
    .expect("stream creation failed");
let (mut sender, mut receiver) = stream.split();

assert_eq!(sender.try_send(&context, b"abc"), 3);
let mut bytes = [0_u8; 3];
assert_eq!(receiver.try_receive(&context, &mut bytes), 3);
assert_eq!(&bytes, b"abc");
```

Only the sender can send and only the receiver can receive. Each endpoint is
`Send` but not `Sync`, so it may be transferred to its one owning task without
creating a second producer or consumer.

Static construction additionally takes a byte ring and
`StaticStreamBuffer_t`. The ring must have space beyond the trigger level and
all size arithmetic is checked.

## Message buffers

Message buffers also transfer bytes, but preserve complete message boundaries.
A send succeeds only when the whole message fits, and receive returns one whole
message or zero. This avoids reconstructing a Rust value from fragments or from
an invalid byte pattern.

```rust
use freertos_in_rust::sync::MessageBuffer;

let mut messages = MessageBuffer::new(&context, 128)
    .expect("message buffer creation failed");
let (mut sender, mut receiver) = messages.split();

assert!(sender.try_send(&context, b"command"));
let mut output = [0_u8; 32];
let length = receiver.try_receive(&context, &mut output);
assert_eq!(&output[..length], b"command");
```

If the output slice is too small for the next message, receive returns zero and
leaves the message queued.

Stream and message buffers are not implicitly deleted on `Drop`, because an
arbitrary drop site cannot prove that no task or ISR is using the SPSC object.
Explicit deletion is therefore `unsafe` and consumes the owner.

## Software timers

Timer names are `&'static CStr` because FreeRTOS retains the pointer. Periods
must be nonzero. The safe callback type is a safe `extern "C"` function pointer
and cannot impose hidden caller preconditions:

```rust
use freertos_in_rust::{sync::Timer, types::TimerHandle_t};

extern "C" fn expired(_: TimerHandle_t) {
    // Timer-daemon context: keep this short and do not block.
}

let timer = Timer::new_periodic(&context, c"heartbeat", 1000, expired)
    .expect("timer creation failed");
assert!(timer.start(&context));
```

Start, stop, reset, period change, state queries, ID access, and deletion use a
`TaskContext`. Timer IDs set through the safe API must point to a shared
`Sync + 'static` value; arbitrary ID pointers require `unsafe`.

`Timer::delete` returns the owner on command-queue failure so deletion can be
retried. Dropping an owned timer attempts a nonblocking delete in task context;
if that is impossible it leaks rather than invoke a task API from an ISR.

## Interrupt APIs

All veneer methods ending in `_from_isr` are `unsafe`. The caller must prove:

- execution is really in an interrupt;
- the interrupt priority may call FreeRTOS;
- the object is live and no forbidden producer/consumer race exists;
- the port's yield-from-ISR operation is performed when a higher-priority task
  was woken.

Task-context methods cannot be called without a `TaskContext`, and they also
perform runtime context checks. This keeps an accidental ISR/task API mix-up
from becoming a release-only bug.

## Raw interoperation and deletion

`as_raw` or `raw_handle` exposes an opaque FreeRTOS handle only where the
wrapper can state a sound borrowing contract. `into_raw` consumes ownership and
transfers all lifetime and deletion duties to the caller. Reconstructing an
owner with `from_raw*` is `unsafe`: there must be exactly one owner and the
object must remain live.

Deleting a kernel object while another task is blocked on it, while an ISR can
access it, or while a raw alias survives is outside the safe veneer. Use
application shutdown coordination before explicit deletion.

## Feature map

| Veneer | Required kernel feature | Dynamic constructor |
| --- | --- | --- |
| `TaskHandle` | none for static creation | allocator feature |
| `Queue<T>` | none | allocator feature |
| binary/counting semaphore | none | allocator feature |
| `Mutex<T>` | `use-mutexes` | allocator feature |
| `EventGroup` | none | allocator feature |
| stream/message buffer | `stream-buffers` | allocator feature |
| `Timer` | `timers` | allocator feature |

Enable exactly one port and at most one allocator. `all-kernel-features` adds
kernel capabilities only; it deliberately does not select a port, integer
width, tick width, allocator, or SMP mode.

For lower-level functionality, use the matching `kernel` module and follow the
function's `# Safety` contract. The raw layer intentionally retains FreeRTOS
naming and control flow for side-by-side comparison with the pinned upstream
source.
