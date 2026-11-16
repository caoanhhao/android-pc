// Phase L2 (docs/nvidia-native.md): the glibc sonames whose symbols all live in libc.so.6 (as in
// glibc 2.34+): libpthread.so.0, libdl.so.2, libm.so.6, librt.so.1, ld-linux-x86-64.so.2,
// libXext.so.6. Each is this empty library under its own soname, for DT_NEEDED and verneed.
.section .note.GNU-stack, "", @progbits
