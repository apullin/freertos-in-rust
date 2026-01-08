/* Memory layout for BBC micro:bit (nRF51822) - QEMU microbit machine */
/* Cortex-M0 with 256KB Flash and 16KB RAM */

MEMORY
{
    /* Flash memory */
    FLASH : ORIGIN = 0x00000000, LENGTH = 256K

    /* SRAM - 16KB */
    RAM : ORIGIN = 0x20000000, LENGTH = 16K
}
