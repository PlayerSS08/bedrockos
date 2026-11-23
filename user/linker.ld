/* Aeon programs: static, non-PIE, loaded at USER_BASE + 4 KiB (USER_BASE = 1 TiB). One segment per
   permission set, each page-aligned, as Aeon's loader requires (no segment both writable and executable). */
ENTRY(_start)
PHDRS {
    text   PT_LOAD FLAGS(5);
    rodata PT_LOAD FLAGS(4);
    data   PT_LOAD FLAGS(6);
}
SECTIONS {
    . = 0x10000001000;
    .text : { *(.text .text.*) } :text
    . = ALIGN(4096);
    .rodata : { *(.rodata .rodata.*) } :rodata
    . = ALIGN(4096);
    .data : { *(.data .data.*) *(.got .got.*) } :data
    .bss : { *(.bss .bss.*) *(COMMON) } :data
    /DISCARD/ : { *(.eh_frame*) *(.comment) *(.note*) }
}
