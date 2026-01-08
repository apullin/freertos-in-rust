/* Linker script for ARM Cortex-A9 on QEMU vexpress-a9 */

/* Memory map for vexpress-a9:
 * RAM starts at 0x60000000, 128MB
 * We reserve:
 * - Vector table at start (must be at 0x60000000 or aligned for VBAR)
 * - Code and rodata
 * - Data and BSS
 * - Stack for each mode (IRQ, SVC, SYS, etc.)
 * - Heap at the end
 */

MEMORY
{
    RAM : ORIGIN = 0x60000000, LENGTH = 128M
}

/* Entry point */
ENTRY(_start)

/* Stack sizes for each processor mode */
__irq_stack_size = 4K;
__svc_stack_size = 4K;
__abt_stack_size = 1K;
__und_stack_size = 1K;
__sys_stack_size = 8K;

SECTIONS
{
    /* Vector table must be at start of memory (or use VBAR to relocate) */
    .vectors 0x60000000 : {
        KEEP(*(.vectors))
        . = ALIGN(4);
    } > RAM

    /* Code section */
    .text : {
        *(.text .text.*)
        *(.gnu.linkonce.t.*)
        . = ALIGN(4);
    } > RAM

    /* Read-only data */
    .rodata : {
        *(.rodata .rodata.*)
        *(.gnu.linkonce.r.*)
        . = ALIGN(4);
    } > RAM

    /* ARM exception handling tables (for C++ exceptions, not used) */
    .ARM.extab : {
        *(.ARM.extab* .gnu.linkonce.armextab.*)
    } > RAM

    .ARM.exidx : {
        __exidx_start = .;
        *(.ARM.exidx* .gnu.linkonce.armexidx.*)
        __exidx_end = .;
    } > RAM

    /* Initialized data */
    .data : {
        __data_start = .;
        *(.data .data.*)
        *(.gnu.linkonce.d.*)
        . = ALIGN(4);
        __data_end = .;
    } > RAM

    /* Uninitialized data (BSS) */
    .bss (NOLOAD) : {
        . = ALIGN(4);
        __bss_start = .;
        *(.bss .bss.*)
        *(.gnu.linkonce.b.*)
        *(COMMON)
        . = ALIGN(4);
        __bss_end = .;
    } > RAM

    /* Stack section for all processor modes
     * Stacks grow downward, so we allocate from high to low
     */
    .stacks (NOLOAD) : {
        . = ALIGN(8);

        /* IRQ mode stack */
        __irq_stack_bottom = .;
        . = . + __irq_stack_size;
        __irq_stack_top = .;

        /* SVC mode stack */
        __svc_stack_bottom = .;
        . = . + __svc_stack_size;
        __svc_stack_top = .;

        /* Abort mode stack */
        __abt_stack_bottom = .;
        . = . + __abt_stack_size;
        __abt_stack_top = .;

        /* Undefined mode stack */
        __und_stack_bottom = .;
        . = . + __und_stack_size;
        __und_stack_top = .;

        /* System/User mode stack (main stack before scheduler) */
        __sys_stack_bottom = .;
        . = . + __sys_stack_size;
        __sys_stack_top = .;

        . = ALIGN(8);
    } > RAM

    /* Heap starts after stacks */
    .heap (NOLOAD) : {
        . = ALIGN(8);
        __heap_start = .;
        /* Heap extends to end of RAM */
        . = ORIGIN(RAM) + LENGTH(RAM);
        __heap_end = .;
    } > RAM

    /* Debug information (not loaded to target) */
    /DISCARD/ : {
        *(.comment)
        *(.note*)
    }
}

/* Provide symbols for startup code */
PROVIDE(__stack = __sys_stack_top);
