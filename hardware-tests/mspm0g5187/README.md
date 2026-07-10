# LP-MSPM0G5187 hardware regression

This is the recovered hardware application for the **TI MSPM0G5187**
(Cortex-M0+, 128 KiB flash, 32 KiB SRAM) on the **LP-MSPM0G5187 LaunchPad**.
It uses the board's default 32 MHz SYSOSC and exports
`FREERTOS_CONFIG_CPU_CLOCK_HZ = 32_000_000` for the kernel's 1 kHz tick.

The firmware dynamically creates one manager task, three RGB worker tasks, a
shared `Queue<u32>`, and an `EventGroup`. The manager queues three pseudo-random
200–1200 ms work intervals. The workers drive PA17 (red), PA24 (green), and PB13
(blue), then rendezvous through one event bit each.

## Build

Install the Rust target once, then build from this directory so Cargo reads the
board-specific target/linker configuration:

```sh
rustup target add thumbv6m-none-eabi
cd hardware-tests/mspm0g5187
cargo build --release
```

The ELF is
`target/thumbv6m-none-eabi/release/freertos-mspm0g5187-hardware-test`.

## Observable pass pattern

Each round starts with the three channels turning on as the workers consume
their queued intervals. They turn off independently after 200–1200 ms. After
the last worker reaches the event-group rendezvous, all channels remain off for
about 50 ms and the next round starts. Repeated rounds with varying channel
durations demonstrate that all four tasks, queue blocking, timed delays, and
event-group synchronization remain live.

## Previously evidenced J-Link flow

The recovered, hardware-tested application documented this SWD/GDB sequence.
It is retained as evidence, but was not rerun during this source migration:

```sh
JLinkGDBServer -device MSPM0G5187 -if SWD -speed 4000 -port 2331

arm-none-eabi-gdb \
  target/thumbv6m-none-eabi/release/freertos-mspm0g5187-hardware-test \
  -nx -batch \
  -ex "target remote localhost:2331" \
  -ex "monitor reset" \
  -ex "monitor flash erase" \
  -ex "load" \
  -ex "monitor reset" \
  -ex "continue"
```

Do not run this command without the intended board attached and permission to
erase its flash.

## Verified onboard XDS110 / TI UniFlash flow

Restore the LaunchPad's XDS110-to-target isolation jumpers before using the
onboard probe. With TI UniFlash 9.4 installed, run this from this directory:

```sh
/Applications/ti/uniflash_9.4.0/dslite.sh \
  --config=MSPM0G5187.ccxml \
  --core=0 \
  --flash \
  --verify \
  --run \
  --verbose \
  target/thumbv6m-none-eabi/release/freertos-mspm0g5187-hardware-test
```

This exact flow was run on the LP-MSPM0G5187 with its onboard XDS110. UniFlash
programmed all ELF load segments, verified them byte for byte, and started the
program. The application keeps its initialized-data load segment aligned to
the MSPM0 flash programmer's 64-bit write size so the ELF can be loaded
directly.
