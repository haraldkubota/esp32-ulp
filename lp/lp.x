/* Memory layout of the ESP32-C5 LP core firmware.
 *
 * Mirrors ESP-IDF's lp_core_riscv.ld for chips without an LP ROM: the LP core
 * starts executing at 0x5000_0080, right after 0x80 bytes reserved for a
 * vector table at the start of the 16 KiB LP SRAM (filled with a nop sled
 * here, see src/main.rs). The stack grows down from the end of LP SRAM.
 */

MEMORY
{
    VECTORS (rx)  : ORIGIN = 0x50000000, LENGTH = 0x80
    LP_RAM  (rwx) : ORIGIN = 0x50000080, LENGTH = 16K - 0x80
}

ENTRY(_reset)

PROVIDE(__stack_top = ORIGIN(LP_RAM) + LENGTH(LP_RAM));
__stack_size = 1K;

SECTIONS
{
    .vectors :
    {
        KEEP(*(.init.vector))
    } > VECTORS

    .text :
    {
        KEEP(*(.init.reset))
        *(.text .text.*)
    } > LP_RAM

    /* mtvec needs 256-byte alignment. A section of its own, so this alignment
     * doesn't move .text (and with it _reset) as well. */
    .trap_table : ALIGN(256)
    {
        KEEP(*(.trap_table))
    } > LP_RAM

    .rodata : ALIGN(4)
    {
        *(.srodata .srodata.*)
        *(.rodata .rodata.*)
    } > LP_RAM

    .data : ALIGN(4)
    {
        *(.sdata .sdata.*)
        *(.data .data.*)
    } > LP_RAM

    /* Not initialised by the LP core: the HP core zeroes all of LP SRAM
     * before copying the image in. */
    .bss (NOLOAD) : ALIGN(4)
    {
        *(.sbss .sbss.*)
        *(.bss .bss.*)
        *(COMMON)
        __bss_end = .;
    } > LP_RAM

    /DISCARD/ :
    {
        *(.eh_frame .eh_frame_hdr)
    }
}

ASSERT(SIZEOF(.vectors) == 0x80, "LP boot sled must be exactly 0x80 bytes")
ASSERT(_reset == 0x50000080, "LP reset handler must be at 0x50000080")
ASSERT(__bss_end + __stack_size <= __stack_top, "LP firmware leaves too little room for the stack")
