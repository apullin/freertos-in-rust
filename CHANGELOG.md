# Changelog

## 0.3.0 - 2026-07-10

This release combines the corrected single-core kernel and the Rust safe veneer
in one crate. It is API-breaking relative to the experimental 0.2 safe-shim
line.

### Kernel correctness

- Restored task, notification, delay, timeout, ready-list, queue-lock, mutex
  inheritance, event-group, stream/message-buffer, and timer-daemon behavior
  against the pinned FreeRTOS source baseline.
- Corrected static control-block layout and lifetime initialization, queue
  pointer wrapping, checked allocation arithmetic, allocator alignment, and
  heap-4/heap-5 edge cases.
- Preserved ISR-visible task state and tick reads with the volatile/atomic
  semantics required by the translated C implementation.
- Removed Rust aliasing violations from intrusive list and static-object paths;
  the maximal host configuration now passes Miri.

### Safe Rust veneer

- Added explicit task-context capabilities and separate task/ISR operations.
- Made task deletion authority unique and non-copyable, with checked creation,
  names, priorities, stacks, scheduler state, and self-deletion rules.
- Added checked static and dynamic queues, mutexes, semaphores, event groups,
  timers, stream buffers, and message buffers with storage and callback
  lifetime contracts.
- Added compile-fail documentation for ownership, variance, endpoint, context,
  and static-storage invariants.

### Ports and qualification

- Repaired and qualified Cortex-M0/M3/M4F/M7, RV32IMAC, Cortex-A9, and
  Cortex-A53 single-core ports.
- Added bounded host and QEMU test drivers. The release gate runs 36 firmware
  suites, including register/FPU context, real ISR wakeups, allocators,
  tickless idle, static-only scheduling, and the safe API.
- Recovered the LP-MSPM0G5187 application and verified the 0.3 repair on the
  physical board through its onboard XDS110 and TI UniFlash.

### Branch policy

- The former `safe_shim` work is integrated into the single-core release and
  no longer needs a separate long-lived branch.
- SMP remains experimental on the `smp` branch. It is kept current with the
  single-core line but is not included in 0.3.0 unless its two-core gate passes.

Crates.io publication is a separate operation and is not performed by this
repository update.
