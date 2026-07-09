# FreeRusTOS test driver

`xtask` supplies stable, dependency-free entry points for the host, feature,
and QEMU test matrices:

```text
cargo run --manifest-path xtask/Cargo.toml -- test host
cargo run --manifest-path xtask/Cargo.toml -- check-features
cargo run --manifest-path xtask/Cargo.toml -- qemu-test --arch cortex-m4f --suite smoke
```

QEMU firmware lives at `tests/qemu/<architecture>`. Each suite is a binary in
that architecture's `Cargo.toml`; for example, the `smoke` suite is built with
`cargo build --release --bin smoke`. The driver supplies the target triple and
a deterministic target directory.

Every guest must emit a complete TAP stream and finish with exactly one result
sentinel:

```text
TAP version 13
ok 1 - scheduler_started
1..1
FREERUSTOS_TEST_RESULT=PASS
```

The driver rejects failed, missing, duplicated, or misnumbered tests; panic and
assertion output; missing or duplicate plans and sentinels; premature non-zero
QEMU exits; and timeouts. The default per-firmware timeout is 25 seconds and can
be adjusted from 1 to 300 seconds with `FREERUSTOS_QEMU_TIMEOUT_SECS`.

Complete stdout, stderr, command, status, and timing information is retained in
`target/xtask/qemu-logs/<architecture>-<suite>.log` for CI artifact upload.
