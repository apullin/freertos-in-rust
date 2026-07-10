/* Nordic nRF51 memory map used by QEMU's microbit machine. */
MEMORY
{
    FLASH : ORIGIN = 0x00000000, LENGTH = 256K
    RAM   : ORIGIN = 0x20000000, LENGTH = 16K
}

_stack_size = 1K;
_heap_size = 0;
