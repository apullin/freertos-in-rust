/* ARM MPS2 AN386 memory map used by qemu-system-arm. */
MEMORY
{
    FLASH : ORIGIN = 0x00000000, LENGTH = 4M
    RAM   : ORIGIN = 0x20000000, LENGTH = 4M
}

_stack_size = 8K;
_heap_size = 0;
