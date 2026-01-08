/* Memory layout for SiFive E-series (QEMU sifive_e machine) */
/* Based on SiFive HiFive1 Rev B board */

MEMORY
{
    /* Flash memory at 0x20000000 (QEMU sifive_e uses this for code) */
    FLASH : ORIGIN = 0x20400000, LENGTH = 512M

    /* SRAM at 0x80000000 */
    RAM : ORIGIN = 0x80000000, LENGTH = 16K
}

REGION_ALIAS("REGION_TEXT", FLASH);
REGION_ALIAS("REGION_RODATA", FLASH);
REGION_ALIAS("REGION_DATA", RAM);
REGION_ALIAS("REGION_BSS", RAM);
REGION_ALIAS("REGION_HEAP", RAM);
REGION_ALIAS("REGION_STACK", RAM);
