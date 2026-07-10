    .arm
    .syntax unified
    .fpu neon

    .equ MODE_IRQ, 0x12
    .equ MODE_SVC, 0x13
    .equ MODE_ABT, 0x17
    .equ MODE_UND, 0x1b
    .equ MODE_SYS, 0x1f

    .section .vectors, "ax"
    .global _vectors
_vectors:
    ldr pc, =_start
    ldr pc, =undefined_handler
    ldr pc, =FreeRTOS_SWI_Handler
    ldr pc, =prefetch_handler
    ldr pc, =data_abort_handler
    nop
    ldr pc, =FreeRTOS_IRQ_Handler
    ldr pc, =fiq_handler

    .section .text.boot, "ax"
    .global _start
    .type _start, %function
_start:
    cpsid if
    ldr r0, =_vectors
    mcr p15, 0, r0, c12, c0, 0
    isb

    cps #MODE_IRQ
    ldr sp, =__irq_stack_top
    cps #MODE_ABT
    ldr sp, =__abt_stack_top
    cps #MODE_UND
    ldr sp, =__und_stack_top
    cps #MODE_SVC
    ldr sp, =__svc_stack_top
    cps #MODE_SYS
    ldr sp, =__sys_stack_top

    ldr r0, =__bss_start
    ldr r1, =__bss_end
    mov r2, #0
1:
    cmp r0, r1
    strlo r2, [r0], #4
    blo 1b

    mrc p15, 0, r0, c1, c0, 2
    orr r0, r0, #(0xf << 20)
    mcr p15, 0, r0, c1, c0, 2
    isb
    vmrs r0, fpexc
    orr r0, r0, #(1 << 30)
    vmsr fpexc, r0

    bl main
2:
    wfi
    b 2b

undefined_handler:
    b undefined_handler
prefetch_handler:
    b prefetch_handler
data_abort_handler:
    b data_abort_handler
fiq_handler:
    b fiq_handler

    .extern FreeRTOS_SWI_Handler
    .extern FreeRTOS_IRQ_Handler
