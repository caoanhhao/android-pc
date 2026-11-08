/* Higher-half kernel image for the Limine boot protocol. */
OUTPUT_FORMAT(elf64-x86-64)
ENTRY(kmain)

PHDRS
{
    requests PT_LOAD;
    text     PT_LOAD;
    rodata   PT_LOAD;
    data     PT_LOAD;
}

SECTIONS
{
    /* Top 2 GiB: required by the `kernel` code model. */
    . = 0xffffffff80000000;
    __kernel_start = .;

    /* Limine requests; the bootloader writes response pointers here (RW). */
    .requests : {
        KEEP(*(.requests_start_marker))
        KEEP(*(.requests))
        KEEP(*(.requests_end_marker))
    } :requests

    . = ALIGN(4096);
    __text_start = .;
    .text : {
        *(.text .text.*)
    } :text
    . = ALIGN(4096);
    __text_end = .;

    __rodata_start = .;
    .rodata : {
        *(.rodata .rodata.*)
    } :rodata
    . = ALIGN(4096);
    __rodata_end = .;

    __data_start = .;
    .data : {
        *(.data .data.*)
        *(.data.rel.ro .data.rel.ro.*)
        *(.got .got.*)
    } :data
    .bss : {
        *(.bss .bss.*)
        *(COMMON)
    } :data
    . = ALIGN(4096);
    __kernel_end = .;

    /DISCARD/ : {
        *(.eh_frame*)
        *(.note .note.*)
        *(.interp)
    }
}
