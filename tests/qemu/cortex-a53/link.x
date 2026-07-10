/* QEMU `virt` AArch64 image layout. */

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

    .rodata ALIGN(16) :
    {
        *(.rodata*)
    } > RAM

    .data ALIGN(16) :
    {
        *(.data*)
    } > RAM

    .bss ALIGN(16) (NOLOAD) :
    {
        __bss_start = .;
        *(.bss*)
        *(COMMON)
        . = ALIGN(16);
        __bss_end = .;
    } > RAM

    /* Exception/kernel stack selected while the port runs with SPSel == 1. */
    . = ALIGN(16);
    __stack_start = .;
    . = . + 0x10000;
    __stack_end = .;

    . = ALIGN(16);
    __heap_start = .;
    . = . + 0x40000;
    __heap_end = .;

    /DISCARD/ :
    {
        *(.note*)
        *(.comment*)
        *(.ARM.*)
    }
}

