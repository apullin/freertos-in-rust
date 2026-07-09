# Upstream FreeRTOS baseline

The translation and audit-repair baseline for this repository is the
FreeRTOS-Kernel commit:

```text
3ace38969bd05558918f815ae75528cb656c64b0
```

Repository:

```text
https://github.com/FreeRTOS/FreeRTOS-Kernel.git
```

`FreeRTOS-Kernel/` is a reference checkout and is not compiled into or
published with this crate. Verify an existing checkout with:

```bash
git -C FreeRTOS-Kernel rev-parse HEAD
```

If the reference checkout is missing, recreate the exact comparison source
with:

```bash
git clone https://github.com/FreeRTOS/FreeRTOS-Kernel.git FreeRTOS-Kernel
git -C FreeRTOS-Kernel checkout 3ace38969bd05558918f815ae75528cb656c64b0
```

Do not change this baseline while fixing translation defects. First restore
behavioral parity against this commit. An upstream-version upgrade must be a
separate change with its own source comparison and validation pass.

For each translated function, compare more than the apparent algorithm. Check
statement ordering, critical-section boundaries, scheduler suspension, list
mutations, timeout-state updates, yield points, and every configuration branch
enabled by this crate.
