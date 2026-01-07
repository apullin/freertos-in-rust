/* Memory layout for QEMU ARM targets
 *
 * Works with:
 * - mps2-an386 (ARM MPS2, Cortex-M4) - recommended for Cortex-M4F code
 * - lm3s6965evb (TI Stellaris, Cortex-M3)
 *
 * Run with:
 *   qemu-system-arm -machine mps2-an386 -nographic \
 *     -semihosting-config enable=on,target=native \
 *     -kernel target/thumbv7em-none-eabihf/debug/demo
 */

MEMORY
{
    FLASH : ORIGIN = 0x00000000, LENGTH = 256K
    RAM   : ORIGIN = 0x20000000, LENGTH = 64K
}

/* Stack size - adjust based on your needs and available RAM */
_stack_size = 4K;

/* Heap size for dynamic allocation */
_heap_size = 8K;
