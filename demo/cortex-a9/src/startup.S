/*
 * ARM Cortex-A9 Startup Code for FreeRusTOS Demo
 *
 * This file provides:
 * - Exception vector table
 * - Processor mode initialization
 * - Stack setup for each mode
 * - BSS clearing
 * - Jump to Rust main
 */

    .arm
    .syntax unified

/* Processor mode constants */
    .equ MODE_USR, 0x10
    .equ MODE_FIQ, 0x11
    .equ MODE_IRQ, 0x12
    .equ MODE_SVC, 0x13
    .equ MODE_ABT, 0x17
    .equ MODE_UND, 0x1B
    .equ MODE_SYS, 0x1F

/* CPSR bits */
    .equ I_BIT, 0x80    /* IRQ disable */
    .equ F_BIT, 0x40    /* FIQ disable */

/*
 * Exception Vector Table
 * Must be at address 0x60000000 (start of RAM on vexpress-a9)
 * or configured via VBAR register
 */
    .section .vectors, "ax"
    .global _vectors
_vectors:
    ldr pc, =_start             /* Reset */
    ldr pc, =undefined_handler  /* Undefined instruction */
    ldr pc, =FreeRTOS_SWI_Handler /* Software interrupt (SVC) */
    ldr pc, =prefetch_handler   /* Prefetch abort */
    ldr pc, =data_abort_handler /* Data abort */
    nop                         /* Reserved */
    ldr pc, =FreeRTOS_IRQ_Handler /* IRQ */
    ldr pc, =fiq_handler        /* FIQ */

/*
 * Reset handler / Entry point
 */
    .section .text
    .global _start
    .type _start, %function
_start:
    /* Disable IRQ and FIQ */
    cpsid if

    /* Set VBAR to point to our vector table */
    ldr r0, =_vectors
    mcr p15, 0, r0, c12, c0, 0

    /* Initialize processor modes and stacks */

    /* IRQ mode stack */
    cps #MODE_IRQ
    ldr sp, =__irq_stack_top

    /* Abort mode stack */
    cps #MODE_ABT
    ldr sp, =__abt_stack_top

    /* Undefined mode stack */
    cps #MODE_UND
    ldr sp, =__und_stack_top

    /* SVC mode stack */
    cps #MODE_SVC
    ldr sp, =__svc_stack_top

    /* System mode (privileged, uses User mode registers) */
    cps #MODE_SYS
    ldr sp, =__sys_stack_top

    /* Clear BSS section */
    ldr r0, =__bss_start
    ldr r1, =__bss_end
    mov r2, #0
bss_clear_loop:
    cmp r0, r1
    strlt r2, [r0], #4
    blt bss_clear_loop

    /* Enable VFP/NEON (Coprocessor Access Control Register) */
    mrc p15, 0, r0, c1, c0, 2   /* Read CPACR */
    orr r0, r0, #(0xF << 20)    /* Enable CP10 and CP11 */
    mcr p15, 0, r0, c1, c0, 2   /* Write CPACR */
    isb
    vmrs r0, fpexc
    orr r0, r0, #(1 << 30)      /* Set EN bit */
    vmsr fpexc, r0

    /* Jump to Rust main (in System mode with interrupts disabled) */
    bl main

    /* If main returns, hang */
hang:
    wfi
    b hang

/*
 * Default exception handlers (weak, can be overridden)
 */
    .weak undefined_handler
    .type undefined_handler, %function
undefined_handler:
    b undefined_handler

    .weak prefetch_handler
    .type prefetch_handler, %function
prefetch_handler:
    b prefetch_handler

    .weak data_abort_handler
    .type data_abort_handler, %function
data_abort_handler:
    b data_abort_handler

    .weak fiq_handler
    .type fiq_handler, %function
fiq_handler:
    b fiq_handler

/*
 * External references to FreeRTOS handlers
 * These are defined in the port layer (cortex_a9.rs)
 */
    .extern FreeRTOS_SWI_Handler
    .extern FreeRTOS_IRQ_Handler
