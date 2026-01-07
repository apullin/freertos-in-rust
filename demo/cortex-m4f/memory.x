/* Generic memory layout for Cortex-M4F
 *
 * Adjust these values for your specific MCU:
 * - STM32F407: FLASH 1024K, RAM 128K (CCM) + 112K (SRAM)
 * - STM32L476: FLASH 1024K, RAM 96K + 32K
 * - TM4C123:   FLASH 256K,  RAM 32K
 * - nRF52840:  FLASH 1024K, RAM 256K
 *
 * This default is conservative and should work on most parts.
 */

MEMORY
{
    /* Adjust these for your specific MCU */
    FLASH : ORIGIN = 0x08000000, LENGTH = 256K
    RAM   : ORIGIN = 0x20000000, LENGTH = 32K
}

/* Stack size - adjust based on your needs and available RAM */
_stack_size = 4K;

/* Heap size for dynamic allocation */
_heap_size = 8K;
