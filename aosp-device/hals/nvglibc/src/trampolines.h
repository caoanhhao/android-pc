// Phase L2 (docs/nvidia-native.md): assembler macros for nvglibc's soname libraries.
//
// A soname library defines glibc's names and nothing else, so a glibc library's imports bind to
// it (its symbols carry no version, which bionic's linker matches against any GLIBC_2.x
// version) while libnvglibc.so itself keeps calling bionic under the same names.
//
// FWD(name, target): `name` jumps through a slot that nvglibc_bind() points at bionic's `target`
//   (looked up in libc.so, libm.so, libdl.so). Until then, or if bionic has no `target`, the slot
//   points at a trap that calls nvglibc_missing("name"). A plain jump keeps the caller's frame,
//   so this is right for variadic functions and for _setjmp/vfork too.
// WRAP(name): `name` jumps to nvg_<name> in libnvglibc.so.
// DATA(name): an 8-byte variable that nvglibc_bind_data() fills in.
// ZERO(name): returns 0 (the X11 stubs).
//
// Each FWD/DATA adds a {target, slot} pair to the nvglibc_fwd/nvglibc_data section; the
// library's constructor (NVGLIBC_BIND_INIT) hands the section bounds to libnvglibc.so.

#define FWD(name, target)                                                        \
    .text; .globl name; .type name, @function; .p2align 4;                       \
    name: jmp *nvg_slot_##name(%rip);                                            \
    nvg_trap_##name: leaq nvg_str_##name(%rip), %rdi; jmp nvglibc_missing@PLT;   \
    .size name, . - name;                                                        \
    .pushsection .rodata.nvglibc_str, "a";                                       \
    nvg_str_##name: .asciz #name; nvg_tgt_##name: .asciz #target;                \
    .popsection;                                                                 \
    .pushsection .data.nvglibc_slot, "aw"; .p2align 3;                           \
    nvg_slot_##name: .quad nvg_trap_##name;                                      \
    .popsection;                                                                 \
    .pushsection nvglibc_fwd, "awR"; .p2align 3;                                 \
    .quad nvg_tgt_##name, nvg_slot_##name;                                       \
    .popsection

#define WRAP(name)                                                               \
    .text; .globl name; .type name, @function; .p2align 4;                       \
    name: jmp nvg_##name@PLT;                                                    \
    .size name, . - name

#define DATA(name)                                                               \
    .pushsection .data.nvglibc_var, "aw"; .p2align 3;                            \
    .globl name; .type name, @object; .size name, 8;                             \
    name: .quad 0;                                                               \
    .popsection;                                                                 \
    .pushsection .rodata.nvglibc_str, "a";                                       \
    nvg_str_##name: .asciz #name;                                                \
    .popsection;                                                                 \
    .pushsection nvglibc_data, "awR"; .p2align 3;                                \
    .quad nvg_str_##name, name;                                                  \
    .popsection

#define ZERO(name)                                                               \
    .text; .globl name; .type name, @function; .p2align 4;                       \
    name: xorl %eax, %eax; ret;                                                  \
    .size name, . - name

#define ZERO_DATA(name)                                                          \
    .pushsection .data.nvglibc_var, "aw"; .p2align 3;                            \
    .globl name; .type name, @object; .size name, 8;                             \
    name: .quad 0;                                                               \
    .popsection

// The constructor: nvglibc_bind(fwd_begin, fwd_end, data_begin, data_end). Both sections are
// declared here, so that their __start_/__stop_ symbols exist even when a library has no entry.
#define NVGLIBC_BIND_INIT                                                        \
    .pushsection nvglibc_fwd, "awR"; .popsection;                                \
    .pushsection nvglibc_data, "awR"; .popsection;                               \
    .text; .p2align 4; .type nvg_bind_init, @function;                           \
    nvg_bind_init:                                                               \
    leaq __start_nvglibc_fwd(%rip), %rdi; leaq __stop_nvglibc_fwd(%rip), %rsi;   \
    leaq __start_nvglibc_data(%rip), %rdx; leaq __stop_nvglibc_data(%rip), %rcx; \
    jmp nvglibc_bind@PLT;                                                        \
    .size nvg_bind_init, . - nvg_bind_init;                                      \
    .pushsection .init_array, "aw", @init_array; .p2align 3;                     \
    .quad nvg_bind_init;                                                         \
    .popsection;                                                                 \
    .section .note.GNU-stack, "", @progbits
