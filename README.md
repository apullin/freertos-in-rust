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
| xQueueSend / Receive | ✅ | All variants (ToFront, ToBack, FromISR) |
| Binary Semaphores | ✅ | |
| Counting Semaphores | ✅ | |
| Mutexes | ✅ | Requires `use-mutexes` feature |
| Recursive Mutexes | ✅ | Requires `use-mutexes` feature |
| Priority Inheritance | ✅ | |
| Queue Sets | ✅ | Requires `queue-sets` feature |
| Queue Registry | ❌ | Not yet implemented |
| **Timers** | | |
| Software Timers | ✅ | Requires `timers` feature |
| xTimerCreate (dynamic) | ✅ | |
| xTimerCreateStatic | ✅ | |
| Timer Daemon Task | ✅ | |
| **Event Groups** | | |
| Event Groups | ⚠️ | Skeleton only, not complete |
| **Stream/Message Buffers** | | |
| Stream Buffers | ❌ | Not yet implemented |
| Message Buffers | ❌ | Not yet implemented |
| **Memory Management** | | |
| pvPortMalloc / vPortFree | ✅ | Wraps Rust's `#[global_allocator]` |
| Static Allocation | ✅ | |
| heap_1 through heap_5 | ➖ | N/A - uses Rust allocator instead |
| **Ports** | | |
| Cortex-M4F | ✅ | Tested on QEMU |
| Cortex-M0/M3/M7 | ❌ | Not yet implemented |
| RISC-V | ❌ | Not yet implemented |
| x86/x64 (Linux/Windows) | ❌ | Not yet implemented |
| **Advanced Features** | | |
| SMP / Multi-core | ➖ | Will not implement (out of scope) |
| MPU Support | ➖ | Will not implement (out of scope) |
| Co-routines | ➖ | Will not implement (deprecated in FreeRTOS) |
| Tickless Idle | ❌ | Not yet implemented |
| Run-time Stats | ❌ | Not yet implemented |

**Legend:** ✅ Supported | ❌ Not yet done | ⚠️ Partial | ➖ Will not implement

## Running the Demo on QEMU

The demo runs on an emulated Cortex-M4F (LM3S6965 board) using QEMU.

### Prerequisites

```bash
# Install Rust ARM target
rustup target add thumbv7em-none-eabihf

# Install QEMU (macOS)
brew install qemu

# Install QEMU (Ubuntu/Debian)
sudo apt install qemu-system-arm
```

### Build and Run

```bash
cd demo/cortex-m4f

# Build the demo
cargo build --release

# Run in QEMU (uses semihosting for output)
qemu-system-arm \
  -cpu cortex-m4 \
  -machine lm3s6965evb \
  -nographic \
  -semihosting-config enable=on,target=native \
  -kernel target/thumbv7em-none-eabihf/release/demo
```

You should see output like:
```
FreeRusTOS Demo Starting...
Creating tasks...
Starting scheduler...
[Blinky] LED ON (count: 1)
[Blinky] LED OFF (count: 2)
...
```

Press `Ctrl-A X` to exit QEMU.

## Memory Allocator

The kernel's `pvPortMalloc` / `vPortFree` functions wrap Rust's `#[global_allocator]` trait. This means:

- **The kernel is allocator-agnostic** - it uses whatever allocator your application provides
- **No built-in heap implementations** - unlike C FreeRTOS which has heap_1 through heap_5
- **The demo uses `embedded-alloc`** - a linked-list allocator with coalescing, suitable for embedded systems

If you need deterministic allocation timing, you can provide your own `#[global_allocator]` implementation (e.g., a pool allocator or bump allocator).

For fully static allocation (no heap), use only the `*Static` API variants (e.g., `xTaskCreateStatic`, `xQueueCreateStatic`) and disable the `alloc` feature.

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
│   ├── event_groups.rs # Event groups (event_groups.c) [skeleton]
│   └── stream_buffer.rs# Stream buffers (stream_buffer.c) [skeleton]
├── port/
│   ├── cortex_m4f.rs   # Cortex-M4F port layer
│   └── dummy.rs        # Dummy port for testing
├── memory/
│   └── mod.rs          # pvPortMalloc/vPortFree
└── trace.rs            # Trace macros (no-op by default)
```

## Configuration

Configuration is done via Cargo features rather than `FreeRTOSConfig.h`:

```toml
[dependencies]
freertos-in-rust = { version = "0.1", features = [
    "port-cortex-m4f",    # Select your port
    "alloc",              # Enable dynamic allocation
    "use-mutexes",        # Enable mutex support
    "timers",             # Enable software timers
    "task-delete",        # Enable vTaskDelete
    "task-suspend",       # Enable vTaskSuspend/Resume
] }
```

Numeric configuration values (tick rate, max priorities, etc.) are constants in `src/config.rs`.

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

- Stream buffers and message buffers
- Event groups (complete implementation)
- Additional ports (Cortex-M0/M3/M7, RISC-V)
- Tickless idle mode
- Run-time statistics

### Testing

- Add unit tests (requires test harness)
- Add integration tests running on QEMU
- CI/CD pipeline

## License

MIT License - same as FreeRTOS.

This is a clean-room port; no FreeRTOS C source code is included in the compiled output. The `FreeRTOS-Kernel/` directory (if present) is for reference only and is excluded from version control.
