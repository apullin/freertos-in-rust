# FreeRusTOS

A line-by-line port of the FreeRTOS kernel from C to Rust.

## Intent

The goal here is to duplicate the FreeRTOS C implementation in Rust, as closely as possible. Ideally, this gives us a package that runs fully in Rust (plus minimal assembly for context switching), while retaining FreeRTOS usage semantics and API structure.

This is **not** a Rust wrapper around FreeRTOS C code. There is no C FFI. The kernel is pure Rust.

## Feature Support

| Feature | Status | Notes |
|---------|--------|-------|
| **Task Management** | | |
| xTaskCreate (dynamic) | ✅ | Requires `alloc` feature |
| xTaskCreateStatic | ✅ | |
| vTaskDelete | ✅ | Requires `task-delete` feature |
| vTaskDelay / vTaskDelayUntil | ✅ | |
| vTaskSuspend / vTaskResume | ✅ | Requires `task-suspend` feature |
| vTaskPrioritySet / Get | ✅ | Requires `task-priority-set` feature |
| xTaskAbortDelay | ✅ | Requires `abort-delay` feature |
| eTaskGetState | ✅ | |
| Task Notifications | ✅ | |
| Thread Local Storage | ✅ | Requires `thread-local-storage` feature |
| Application Task Tags | ✅ | Requires `application-task-tag` feature |
| Stack High Water Mark | ✅ | Requires `stack-high-water-mark` feature |
| vTaskGetInfo / uxTaskGetSystemState | ✅ | Requires `trace-facility` feature |
| vTaskListTasks | ✅ | Requires `stats-formatting` feature |
| **Queues & Semaphores** | | |
| xQueueCreate (dynamic) | ✅ | Requires `alloc` feature |
| xQueueCreateStatic | ✅ | |
| xQueueSend / Receive / Peek | ✅ | All variants (ToFront, ToBack, FromISR) |
| xQueueGiveFromISR | ✅ | Optimized semaphore give from ISR |
| vQueueDelete | ✅ | Requires `alloc` feature |
| Binary Semaphores | ✅ | |
| Counting Semaphores | ✅ | xQueueCreateCountingSemaphore/Static |
| Mutexes | ✅ | Requires `use-mutexes` feature |
| Recursive Mutexes | ✅ | Requires `use-mutexes` feature |
| Priority Inheritance | ✅ | |
| Queue Sets | ✅ | Requires `queue-sets` feature |
| Queue Registry | ✅ | Requires `queue-registry` feature |
| Queue Trace Functions | ✅ | Requires `trace-facility` feature |
| **Timers** | | |
| Software Timers | ✅ | Requires `timers` feature |
| xTimerCreate (dynamic) | ✅ | |
| xTimerCreateStatic | ✅ | |
| Timer Daemon Task | ✅ | |
| **Event Groups** | | |
| Event Groups | ✅ | |
| **Stream/Message Buffers** | | |
| Stream Buffers | ✅ | Requires `stream-buffers` feature |
| Message Buffers | ✅ | Requires `stream-buffers` feature |
| **Memory Management** | | |
| pvPortMalloc / vPortFree | ✅ | Wraps Rust's `#[global_allocator]` |
| Static Allocation | ✅ | |
| heap_1 through heap_5 | ➖ | N/A - uses Rust allocator instead |
| **Ports** | | |
| Cortex-M4F | ✅ | Tested on QEMU (lm3s6965evb) |
| Cortex-M7 | ✅ <sup>[1]</sup> | Tested on QEMU |
| Cortex-M3 | ✅ | Tested on QEMU (lm3s6965evb) |
| Cortex-M0/M0+ | ✅ | Tested on QEMU (microbit) |
| RISC-V RV32 | ✅ | Tested on QEMU (sifive_e) |
| Cortex-A9 | ✅ | Tested on QEMU (vexpress-a9) |
| Cortex-A53 (AArch64) | ❌ | Not yet implemented |
| x86/x64 (Linux/Windows) | ❌ | Not yet implemented |
| **Advanced Features** | | |
| SMP / Multi-core | ➖ | Will not implement (out of scope) |
| MPU Support | ➖ | Will not implement (out of scope) |
| Co-routines | ➖ | Will not implement (deprecated in FreeRTOS) |
| Tickless Idle | ✅ <sup>[2]</sup> | Requires `tickless-idle` feature |
| Run-time Stats | ✅ | Requires `generate-run-time-stats` feature |

**Legend:** ✅ Supported | ❌ Not yet done | ⚠️ Partial | ➖ Will not implement

<sup>[1]</sup> *Cortex-M7 reuses the CM4F port, per FreeRTOS's recommendation. Early CM7 silicon (r0p0/r0p1) requires ARM Errata 837070 workaround, which is not currently implemented. Most modern CM7 chips do not need this workaround.*

<sup>[2]</sup> *Tickless idle support is per-port. It has been tested and appears to work correctly in QEMU for all ports included in this repository.*

## Running the Demos on QEMU

Demos are available for all supported ports.

### Prerequisites

```bash
# Install Rust ARM targets
rustup target add thumbv7em-none-eabihf  # CM4F, CM7
rustup target add thumbv7m-none-eabi     # CM3
rustup target add thumbv6m-none-eabi     # CM0

# Install Rust RISC-V target
rustup target add riscv32imac-unknown-none-elf  # RISC-V RV32

# Install Rust ARM Cortex-A target
rustup target add armv7a-none-eabi  # Cortex-A9

# Install QEMU (macOS)
brew install qemu

# Install QEMU (Ubuntu/Debian)
sudo apt install qemu-system-arm qemu-system-misc
```

### Cortex-M4F Demo

```bash
cd demo/cortex-m4f
cargo build --release
qemu-system-arm \
  -cpu cortex-m4 \
  -machine lm3s6965evb \
  -nographic \
  -semihosting-config enable=on,target=native \
  -kernel target/thumbv7em-none-eabihf/release/demo
```

### Cortex-M3 Demo

```bash
cd demo/cortex-m3
cargo build --release
qemu-system-arm \
  -cpu cortex-m3 \
  -machine lm3s6965evb \
  -nographic \
  -semihosting-config enable=on,target=native \
  -kernel target/thumbv7m-none-eabi/release/demo
```

### Cortex-M0 Demo

```bash
cd demo/cortex-m0
cargo build --release
qemu-system-arm \
  -cpu cortex-m0 \
  -machine microbit \
  -nographic \
  -semihosting-config enable=on,target=native \
  -kernel target/thumbv6m-none-eabi/release/demo
```

### RISC-V RV32 Demo

```bash
cd demo/riscv32
cargo build --release
qemu-system-riscv32 \
  -machine sifive_e \
  -nographic \
  -kernel target/riscv32imac-unknown-none-elf/release/demo
```

### Cortex-A9 Demo

```bash
cd demo/cortex-a9
cargo build --release
qemu-system-arm \
  -machine vexpress-a9 \
  -cpu cortex-a9 \
  -m 128M \
  -nographic \
  -semihosting-config enable=on,target=native \
  -kernel target/armv7a-none-eabi/release/demo
```

You should see output like:
```
========================================
   FreeRusTOS Demo - Cortex-M0
========================================

[Init] Creating mutex...
[Init] Mutex created successfully
...
[Main] Starting scheduler...
```

Press `Ctrl-A X` to exit QEMU.

## Memory Allocator

FreeRusTOS supports dynamic memory allocation via Cargo feature flags. This is a project-level configuration—select the allocator that fits your needs.

### Option 1: `heap-4` — Ported FreeRTOS Allocator

FreeRTOS's `heap_4.c` is one of the most commonly used allocators in embedded FreeRTOS projects. We've ported it to Rust:

- **First-fit allocation** with block splitting
- **Coalescing on free** — merges adjacent free blocks to reduce fragmentation
- **Fixed heap size** — configured via `configTOTAL_HEAP_SIZE` in `config.rs`
- **Deterministic timing** — important for real-time systems

```toml
freertos-in-rust = { version = "0.1", features = ["heap-4"] }
```

```rust
use freertos_in_rust::memory::FreeRtosAllocator;

#[global_allocator]
static ALLOCATOR: FreeRtosAllocator = FreeRtosAllocator;
```

### Option 2: `alloc` — Rust-Native Allocator

If you prefer a simpler Rust-native solution, use the `alloc` feature with an external allocator like [`embedded-alloc`](https://crates.io/crates/embedded-alloc). This is a well-maintained linked-list allocator from the embedded Rust ecosystem:

```toml
freertos-in-rust = { version = "0.1", features = ["alloc"] }

[dependencies]
embedded-alloc = "0.6"
```

```rust
use embedded_alloc::LlffHeap as Heap;

#[global_allocator]
static HEAP: Heap = Heap::empty();

fn main() {
    // Initialize heap (required for embedded-alloc)
    static mut HEAP_MEM: [MaybeUninit<u8>; 16384] = [MaybeUninit::uninit(); 16384];
    unsafe { HEAP.init(HEAP_MEM.as_ptr() as usize, 16384) }
}
```

### Static-only (No heap)

For fully static allocation, don't enable any heap feature:
- Use only `*Static` API variants (e.g., `xTaskCreateStatic`, `xQueueCreateStatic`)
- `pvPortMalloc` will panic if called

## Project Structure

```
src/
├── lib.rs              # Crate root, re-exports
├── config.rs           # FreeRTOSConfig.h equivalent
├── types.rs            # Base types (BaseType_t, TickType_t, etc.)
├── kernel/
│   ├── tasks.rs        # Task management (tasks.c)
│   ├── queue.rs        # Queues & semaphores (queue.c)
│   ├── list.rs         # Linked list implementation (list.c)
│   ├── timers.rs       # Software timers (timers.c)
│   ├── event_groups.rs # Event groups (event_groups.c)
│   └── stream_buffer.rs# Stream buffers (stream_buffer.c)
├── port/
│   ├── cortex_m4f.rs   # Cortex-M4F port (ARMv7E-M with FPU)
│   ├── cortex_m7.rs    # Cortex-M7 port (reexports CM4F)
│   ├── cortex_m3.rs    # Cortex-M3 port (ARMv7-M, no FPU)
│   ├── cortex_m0.rs    # Cortex-M0/M0+ port (ARMv6-M, Thumb-1)
│   ├── cortex_a9.rs    # Cortex-A9 port (ARMv7-A, GIC, SWI)
│   ├── riscv32.rs      # RISC-V RV32 port (RV32I/RV32IMAC)
│   └── dummy.rs        # Dummy port for testing
├── memory/
│   └── mod.rs          # pvPortMalloc/vPortFree
└── trace.rs            # Trace macros (no-op by default)
```

## Configuration

Rust doesn't have header files, so `FreeRTOSConfig.h` is replaced by two mechanisms:

**Boolean flags** (e.g., `#define configUSE_MUTEXES 1`) become Cargo features:

```toml
[dependencies]
freertos-in-rust = { version = "0.1", features = [
    "port-cortex-m4f",    # Select your port (or port-cortex-m3, port-cortex-m0, port-cortex-m7, port-riscv32, port-cortex-a9)
    "heap-4",             # Allocator: ported heap_4 (or "alloc" for external)
    "use-mutexes",        # configUSE_MUTEXES
    "timers",             # configUSE_TIMERS
    "task-delete",        # INCLUDE_vTaskDelete
    "task-suspend",       # INCLUDE_vTaskSuspend
] }
```

See [Memory Allocator](#memory-allocator) for choosing between `heap-4` and `alloc`.

**Numeric constants** (e.g., `#define configTOTAL_HEAP_SIZE 10240`) live in `src/config.rs` and are referenced as `crate::config::configTOTAL_HEAP_SIZE`:

```rust
// src/config.rs
pub const configMAX_PRIORITIES: UBaseType_t = 5;
pub const configTICK_RATE_HZ: TickType_t = 1000;
pub const configTOTAL_HEAP_SIZE: usize = 10240;
pub const configTIMER_TASK_PRIORITY: UBaseType_t = 2;
// ... etc
```

## TODOs

### Crate Structure

This is currently a monolithic single-crate repository. Future work includes:

- Split into workspace with separate crates:
  - `freertos-kernel` - Core kernel
  - `freertos-port-cortex-m` - Cortex-M ports
  - `freertos-port-riscv` - RISC-V ports (future)
- Publish to crates.io
- Add `#![no_std]` examples for various boards

### Missing Features

- Additional ports:
  - Cortex-A53 (ARMv8-A/AArch64, 64-bit) - in progress
  - RISC-V 64-bit
  - x86/x64

### Testing

- Add unit tests (requires test harness)
- Add integration tests running on QEMU
- CI/CD pipeline

## License

MIT License - same as FreeRTOS.

This is a clean-room port; no FreeRTOS C source code is included in the compiled output. The `FreeRTOS-Kernel/` directory (if present) is for reference only and is excluded from version control.
