/* QEMU vexpress-a9 image layout. */

ENTRY(_start)

MEMORY
{
    RAM (rwx) : ORIGIN = 0x60000000, LENGTH = 128M
}

__irq_stack_size = 4K;
__svc_stack_size = 4K;
__abt_stack_size = 1K;
__und_stack_size = 1K;
__sys_stack_size = 8K;

SECTIONS
{
    .vectors 0x60000000 :
    {
        KEEP(*(.vectors))
        . = ALIGN(4);
    } > RAM

    .text :
    {
        *(.text .text.*)
        . = ALIGN(4);
    } > RAM

    .rodata :
    {
        *(.rodata .rodata.*)
        . = ALIGN(8);
    } > RAM

    .ARM.extab : { *(.ARM.extab*) } > RAM
    .ARM.exidx :
    {
        __exidx_start = .;
        *(.ARM.exidx*)
        __exidx_end = .;
    } > RAM

    .data :
    {
        *(.data .data.*)
        . = ALIGN(8);
    } > RAM

    .bss (NOLOAD) :
    {
        __bss_start = .;
        *(.bss .bss.*)
        *(COMMON)
        . = ALIGN(8);
        __bss_end = .;
    } > RAM

    .stacks (NOLOAD) :
    {
        . = ALIGN(8);
        . = . + __irq_stack_size;
        __irq_stack_top = .;
        . = . + __svc_stack_size;
        __svc_stack_top = .;
        . = . + __abt_stack_size;
        __abt_stack_top = .;
        . = . + __und_stack_size;
        __und_stack_top = .;
        . = . + __sys_stack_size;
        __sys_stack_top = .;
    } > RAM

    /DISCARD/ :
    {
        *(.note*)
        *(.comment*)
    }
}
