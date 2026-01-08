/*
 * Linker script for Cortex-A53 on QEMU virt machine
 *
 * QEMU virt memory map:
 * - RAM: 0x40000000 (1GB default, -m to change)
 * - GIC Distributor: 0x08000000
 * - GIC CPU Interface: 0x08010000
 * - UART (PL011): 0x09000000
 */

ENTRY(_start)

MEMORY
{
    RAM (rwx) : ORIGIN = 0x40000000, LENGTH = 128M
}

SECTIONS
{
    .text 0x40000000 :
    {
        KEEP(*(.text.boot))
        *(.text*)
    } > RAM

    .vectors ALIGN(2048) :
    {
        KEEP(*(.vectors))
    } > RAM

    .rodata ALIGN(8) :
    {
        *(.rodata*)
    } > RAM

    .data ALIGN(8) :
    {
        *(.data*)
    } > RAM

    .bss ALIGN(8) (NOLOAD) :
    {
        __bss_start = .;
        *(.bss*)
        *(COMMON)
        __bss_end = .;
    } > RAM

    /* Stack for EL1 (kernel) */
    . = ALIGN(16);
    __stack_start = .;
    . = . + 0x10000; /* 64KB stack */
    __stack_end = .;

    /* Heap */
    . = ALIGN(8);
    __heap_start = .;
    . = . + 0x20000; /* 128KB heap */
    __heap_end = .;

    /DISCARD/ :
    {
        *(.note*)
        *(.comment*)
        *(.ARM.*)
    }
}
