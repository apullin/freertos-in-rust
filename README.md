# FreeRTOS-in-Rust

[![CI](https://github.com/apullin/freertos-in-rust/actions/workflows/ci.yml/badge.svg)](https://github.com/apullin/freertos-in-rust/actions/workflows/ci.yml)

FreeRTOS-in-Rust is a monolithic, `no_std`, pure-Rust translation of the
FreeRTOS kernel. It links no FreeRTOS C code; only the small amount of
architecture assembly needed for context switching remains non-Rust.

The goal of this project is to provide a "simple", but fully functional, RTOS
in Rust for embedded systems. Embedded developers should be able to follow
standard FreeRTOS design patterns with 100% of the supported feature surface
available, while also gaining Rust syntax sugar.

The 0.3.x line combines two interfaces in one crate:

- `freertos_in_rust::kernel` is the close, C-shaped translation. It preserves
  FreeRTOS names, handles, storage rules, and control flow, and marks contracts
  that Rust cannot verify as `unsafe`.
- `freertos_in_rust::sync` is the safe Rust veneer. It adds owned handles,
  typed queues, stable static-storage borrows, checked parameters, and an
  explicit task-context capability without redesigning the kernel.

See [RUST_WRAPPER.md](RUST_WRAPPER.md) for details on the veneer API and its
safety model.
The crate remains under active development, so 0.3.x APIs _may_ still evolve.

## Translation baseline

This is a source translation and derivative of the MIT-licensed FreeRTOS
kernel, not a wrapper or a clean-room reimplementation. The translation target
is pinned to upstream commit
`3ace38969bd05558918f815ae75528cb656c64b0` (`git describe`:
`V10.4.3-744-g3ace38969`). Upstream notices and amendment comments are retained
in the Rust sources. The reference `FreeRTOS-Kernel/` tree is excluded from
crate packages and is not compiled.

Version 0.3.x is single-core. Separate SMP work remains experimental on the
`smp` branch. Its early boot scaffolding has run in QEMU, but it has not passed
the two-core scheduler gate and is not part of the 0.3.0 crate API or the
qualification below.

## Feature support

The supported single-core FreeRTOS feature surface is implemented. The concise
table below is backed by the symbol-by-symbol source audit in
[PARITY.md](PARITY.md); the later qualification table distinguishes compilation
coverage from code actually exercised in QEMU or on hardware.

| Feature | Status | Notes |
|---|---:|---|
| **Task management** | | |
| `xTaskCreate` (dynamic) | ✅ | Requires `alloc`, `heap-4`, or `heap-5` |
| `xTaskCreateStatic` | ✅ | No allocator required |
| `vTaskDelete` | ✅ | Requires `task-delete` |
| `vTaskDelay` / `xTaskDelayUntil` | ✅ | |
| `vTaskSuspend` / `vTaskResume` | ✅ | Requires `task-suspend` |
| `vTaskPrioritySet` / priority queries | ✅ | Mutation requires `task-priority-set` |
| `xTaskAbortDelay` | ✅ | Requires `abort-delay` |
| `eTaskGetState` | ✅ | |
| Indexed task notifications | ✅ | Task and FromISR variants |
| Thread-local storage | ✅ | Requires `thread-local-storage` |
| Application task tags | ✅ | Requires `application-task-tag` |
| Stack high-water marks | ✅ | Requires `stack-high-water-mark` |
| Stack-overflow checking | ✅ | Requires `stack-overflow-check` and an application hook |
| `vTaskGetInfo` / `uxTaskGetSystemState` | ✅ | Requires `trace-facility` |
| Task-list and run-time formatting | ✅ | Requires `stats-formatting` |
| **Queues and semaphores** | | |
| Dynamic and static queues | ✅ | Typed safe queues are also available |
| Send / receive / peek | ✅ | Front, back, overwrite, blocking, and FromISR variants |
| Binary and counting semaphores | ✅ | Dynamic and static creation |
| Mutexes and recursive mutexes | ✅ | Requires `use-mutexes` |
| Priority inheritance and disinheritance | ✅ | Including timeout paths |
| Queue sets | ✅ | Requires `queue-sets` |
| Queue registry | ✅ | Requires `queue-registry` |
| Queue trace metadata | ✅ | Requires `trace-facility` |
| **Software timers** | | |
| Timer daemon task | ✅ | Requires `timers` |
| Dynamic and static timers | ✅ | One-shot and auto-reload |
| Start / stop / reset / period changes | ✅ | Task and FromISR command variants |
| Pended function calls | ✅ | Requires `pend-function-call` |
| **Event groups** | | |
| Create / wait / set / clear / sync | ✅ | Dynamic and static creation |
| Deferred FromISR set and clear | ✅ | Uses the timer-daemon command queue |
| **Stream and message buffers** | | |
| Stream buffers | ✅ | Requires `stream-buffers` |
| Message buffers | ✅ | Message-boundary semantics preserved |
| Static and dynamic construction | ✅ | Includes callback variants |
| **Memory management** | | |
| Static-only operation | ✅ | No allocator feature required |
| Rust global-allocator bridge | ✅ | `alloc` feature |
| `heap_4` | ✅ | First-fit allocator with coalescing |
| `heap_5` | ✅ | Multiple non-contiguous regions |
| `heap_1`, `heap_2`, `heap_3` | ➖ | Intentionally omitted; use static allocation, `heap-4`/`heap-5`, or a Rust allocator |
| **Diagnostics and scheduling options** | | |
| Tickless idle | ✅ | QEMU-tested on every registered single-core port |
| Run-time statistics | ✅ | Requires `generate-run-time-stats` |
| Idle yielding | ✅ | Requires `idle-yield` |
| List integrity check bytes | ✅ | Requires `list-data-integrity-check` |
| User-replaceable trace macros | ⚠️ | Trace metadata exists; macro hooks currently use their default no-op behavior |
| **Ports** | | |
| Cortex-M0/M0+ | ✅ | QEMU-tested and run on LP-MSPM0G5187 hardware |
| Cortex-M3 | ✅ | QEMU `mps2-an385` |
| Cortex-M4F | ✅ | QEMU `mps2-an386`, including FPU context |
| Cortex-M7 | ✅ | QEMU `mps2-an500`, including FPU context |
| RISC-V RV32IMAC | ✅ | QEMU `sifive_e` |
| Cortex-A9 | ✅ | QEMU `vexpress-a9` |
| Cortex-A53 / AArch64 | ✅ | QEMU `virt` |
| **Deliberate scope boundaries** | | |
| SMP / multi-core scheduler | ⚠️ | Experimental `smp` branch; two-core scheduler gate does not yet pass |
| MPU ports | ➖ | Out of scope for the current translation |
| Co-routines | ➖ | Deprecated upstream and intentionally omitted |

**Legend:** ✅ implemented · ⚠️ partial/experimental · ➖ deliberately out of scope

## Configure a target

The default feature is only `port-dummy`, a compile-only scaffold that cannot
start a scheduler. Every production dependency must disable defaults and
select exactly one hardware port:

```toml
[dependencies]
freertos-in-rust = { version = "0.3", default-features = false, features = [
    "port-cortex-m0",
    "heap-4",
    "user-config",
    "task-delete",
    "task-suspend",
    "use-mutexes",
    "timers",
    "stream-buffers",
] }
```

The port features are:

- `port-cortex-m0`, `port-cortex-m3`, `port-cortex-m4f`, `port-cortex-m7`
- `port-riscv32`
- `port-cortex-a9`, `port-cortex-a53`

Port features are mutually exclusive and automatically select both the native
integer width and tick width. Applications should not add `arch-*` or `tick-*`
features themselves.

All 32-bit ports use `u32` `TickType_t`. Cortex-A53 uses 64-bit architecture
types and `u64` `TickType_t`. This policy avoids the unsupported 16-bit tick
combinations and lets event-group reserved bits and timer arithmetic follow the
selected tick type consistently.

Optional kernel capabilities map the corresponding FreeRTOS configuration
switches. The available names are defined in [Cargo.toml](Cargo.toml), including
`task-delete`, `task-suspend`, `abort-delay`, `task-priority-set`,
`use-mutexes`, `timers`, `stream-buffers`, `queue-sets`, `queue-registry`,
`pend-function-call`, `thread-local-storage`, `tickless-idle`, and
`generate-run-time-stats`. `all-kernel-features` is a test aggregate; it does
not select a port, width, allocator, or SMP mode.

### Hardware configuration

Enable `user-config` for production hardware and define its clock symbols in
the final application:

```rust
#[no_mangle]
pub static FREERTOS_CONFIG_CPU_CLOCK_HZ: u32 = 32_000_000;

// Required as well for port-riscv32.
#[no_mangle]
pub static FREERTOS_CONFIG_MTIME_HZ: u32 = 10_000_000;
```

Without `user-config`, the crate uses the repository's test-oriented defaults:
80 MHz for the CPU clock and, on RV32, 32.768 kHz for MTIME. Those values must
not be assumed correct for a board. Other numeric FreeRTOS configuration values
currently live in `src/config.rs`, including a 1 kHz kernel tick.

## Raw kernel and safe veneer

The raw layer is useful for direct FreeRTOS ports and for comparing behavior
with the C kernel:

```rust
use freertos_in_rust::kernel::tasks::{vTaskDelay, xTaskGetTickCount};

let before = xTaskGetTickCount();
// SAFETY: this runs in a live task with the scheduler running and unsuspended.
unsafe { vTaskDelay(10) };
```

Safe task-only operations require `&TaskContext`. Constructing the capability
is the explicit assertion that execution is in a task, or in exclusive startup
before interrupts and the scheduler can race it:

```rust
use freertos_in_rust::sync::{Queue, TaskContext};

// SAFETY: single-threaded startup; kernel interrupts are still masked.
let context = unsafe { TaskContext::assume() };
let queue = Queue::<u32>::new(&context, 8).expect("queue allocation failed");
assert!(queue.send(&context, &42));

freertos_in_rust::start_scheduler(context)
```

Create a fresh `TaskContext` at each task entry. It is neither `Send` nor
`Sync`. ISR APIs remain a separate, mostly `unsafe` boundary because Rust types
cannot prove interrupt priority, wake flags, or platform dispatch contracts.

Task deletion is also `unsafe`, even with a unique `TaskHandle`: the caller
must arrange shutdown and resource release before deletion, which skips stack
destructors. Suspending a task does not prove that it holds no mutexes or other
resources. See [the task veneer contract](RUST_WRAPPER.md#tasks).

## Allocation policy

Select at most one allocator feature:

| Selection | Behavior | Application responsibility |
|---|---|---|
| none | Static-only kernel objects | Use the `*Static` APIs and initialized `Static*_t::new()` storage. Dynamic allocation support is disabled. |
| `heap-4` | Ported first-fit/coalescing allocator over one fixed region | Size is currently `configTOTAL_HEAP_SIZE`; install `memory::FreeRtosAllocator` as the Rust global allocator if Rust allocations should use it too. |
| `heap-5` | Ported allocator over multiple non-contiguous regions | Call `vPortDefineHeapRegions` before any allocation; regions must remain valid and correctly ordered. |
| `alloc` | `pvPortMalloc`/`vPortFree` wrap Rust's allocator | The application must install and initialize a suitable `#[global_allocator]`. |

For `heap-4` or `heap-5`, the provided adapter can back Rust allocation:

```rust
#[global_allocator]
static ALLOCATOR: freertos_in_rust::memory::FreeRtosAllocator =
    freertos_in_rust::memory::FreeRtosAllocator;
```

These heaps are task-only. The `GlobalAlloc` adapter returns null for ISR
allocation/reallocation and deliberately leaves ISR-deallocated blocks allocated
to avoid corrupting an interrupted heap operation. Transfer owned allocations
back to a task for disposal; the raw heap functions retain their unsafe
task-context contract.

Static veneer constructors borrow their stack, data, and control storage for
the complete object lifetime. Do not form references to uninitialized control
blocks; initialize them with their `const fn new()` constructors.

## Port qualification

“Build” and “QEMU” below are deliberately separate. A target check proves that
a configuration cross-compiles; a bounded QEMU guest additionally exercises
the stated runtime behavior. Neither is a substitute for qualification on a
particular board, clock tree, interrupt controller, and compiler profile.

| Port / Rust target | Qualification in the 0.3 repair | Runtime evidence actually run | Important limits |
|---|---|---|---|
| `port-dummy` / host | **Build-only** | None; scheduler startup intentionally fails | Development and documentation scaffold only. |
| Cortex-M0/M0+ / `thumbv6m-none-eabi` | Target `cargo check` and Clippy with `heap-4` plus all kernel capabilities; **QEMU- and hardware-tested** | `microbit`: smoke 3/3, kernel-core 5/5, ISR 4/4, allocator 4/4, stream/event/timer 6/6, port/context 7/7, tickless 3/3; LP-MSPM0G5187 dynamic task/queue/event-group regression | QEMU does not qualify a particular board clock tree or peripheral setup. The linked hardware regression records the exact board and flash flow. |
| Cortex-M3 / `thumbv7m-none-eabi` | Target check and Clippy with `heap-4` plus all capabilities; **QEMU-tested** | `mps2-an385`: smoke 3/3, kernel-core 5/5, ISR 4/4, allocator 4/4, stream/event/timer 6/6, port/context 7/7, tickless 3/3 | Runtime evidence is for QEMU's MPS2 model, not a specific production MCU. |
| Cortex-M4F / `thumbv7em-none-eabihf` | Target check and Clippy with `heap-4` plus all capabilities; **QEMU-tested** | `mps2-an386`: smoke 3/3, kernel-core 5/5, ISR 4/4, allocator 4/4, stream/event/timer 6/6, safe API 10/10, port/FPU context 8/8, tickless 3/3; static-only 8/8 | The static-only test uses a separate no-allocator firmware. Runtime evidence is for QEMU's MPS2 model. |
| Cortex-M7 / `thumbv7em-none-eabihf` | Target check and Clippy with `heap-4` plus all capabilities; **QEMU-tested** | `mps2-an500`: smoke 3/3, kernel-core 5/5, ISR 4/4, allocator 4/4, stream/event/timer 6/6, port/FPU context 8/8, tickless 3/3 | ARM erratum 837070 handling for affected early M7 revisions is not implemented. |
| RV32IMAC / `riscv32imac-unknown-none-elf` | Minimal and maximal target checks; **QEMU-tested** | `sifive_e` composite smoke: 10/10, covering traps/ISR yield, PLIC dispatch, MTIME rollover/fractional cadence, and tickless accounting | Single-hart, machine mode, and SiFive E only. The application callback must claim and complete external interrupt-controller sources. |
| Cortex-A9 / `armv7a-none-eabi` | Focused target check with tickless/stats; **QEMU-tested** | `vexpress-a9` smoke: 7/7, including a non-tick early wake, repeated suppressed sleeps, periodic reload restoration, and WFI runtime accounting | The port assumes the vexpress-style MPCore private-peripheral/GIC mapping and SP804 integration. Validate addresses and clocks on real hardware. |
| Cortex-A53 / `aarch64-unknown-none` | Focused target check with tickless/stats; **QEMU-tested** | `virt`: smoke 3/3, port/context 14/14 (including FPCR/FPSR and IRQ SIMD preservation), tickless 4/4, event/timer 3/3 | Runtime stats read `CNTPCT_EL0`; platform firmware or a higher exception level must permit EL1 access. |

The recovered LP-MSPM0G5187 application is preserved under
[`hardware-tests/mspm0g5187`](hardware-tests/mspm0g5187/README.md) and is
cross-compiled in CI. The 0.3 repair was programmed, byte-for-byte verified,
and run through the board's onboard XDS110 with TI UniFlash 9.4. Its four-task
RGB pattern exercised dynamic allocation, queue blocking, timed delays, and an
event-group rendezvous on the MSPM0G5187.

Current QEMU qualification covers tickless idle on every registered
single-core port. Each Cortex-M tickless guest suppresses 200 ticks, wakes at
tick 180, and observes two SysTick interrupts across the test.

## Test driver

Use the repository test driver instead of assembling ad-hoc QEMU command lines:

```bash
# Deterministic host tests in debug and release modes.
cargo run --manifest-path xtask/Cargo.toml -- test host

# Feature/allocator/port matrix, optimized compiler barriers, docs, and rejections.
cargo run --manifest-path xtask/Cargo.toml -- check-features

# One bounded guest.
cargo run --manifest-path xtask/Cargo.toml -- \
  qemu-test --arch cortex-a53 --suite tickless

# The registered suites for one architecture.
cargo run --manifest-path xtask/Cargo.toml -- \
  qemu-test --arch cortex-a53 --suite all

# The smoke suite on every registered single-core architecture.
cargo run --manifest-path xtask/Cargo.toml -- \
  qemu-test --arch all --suite smoke
```

Each firmware run has a 25-second default timeout and writes a diagnostic log
under `target/xtask/qemu-logs/`. Override the bound with
`FREERUSTOS_QEMU_TIMEOUT_SECS` when necessary.

Install the Rust targets used by the ports before running the complete matrix:

```bash
rustup target add \
  thumbv6m-none-eabi \
  thumbv7m-none-eabi \
  thumbv7em-none-eabihf \
  riscv32imac-unknown-none-elf \
  armv7a-none-eabi \
  aarch64-unknown-none
```

The QEMU matrix requires `qemu-system-arm`, `qemu-system-aarch64`, and
`qemu-system-riscv32`. The `demo/` applications remain useful for manual
inspection, but passing a demo build is not counted as runtime qualification in
the table above.

## Repository layout

```text
src/
├── kernel/     # C-shaped translations of tasks, queues, lists, timers, etc.
├── sync/       # Checked Rust veneer and ownership/context types
├── port/       # Cortex-M, Cortex-A, RV32, test, and dummy ports
├── memory/     # Static stubs, heap_4, heap_5, and global-allocator bridge
├── config.rs   # FreeRTOSConfig.h equivalent
├── types.rs    # Port-selected FreeRTOS scalar and handle types
└── lib.rs
tests/qemu/     # Bounded bare-metal QEMU firmware suites
tests/codegen/  # Optimized port synchronization regressions
xtask/          # Host, feature-matrix, and QEMU test driver
```

## License

MIT, matching the upstream FreeRTOS kernel. See [LICENSE](LICENSE).
