//! Linux x86_64 syscall layer (not implemented yet).
//!
//! Planned (phase B1): `syscall`/`sysret` entry via the STAR/LSTAR/SFMASK
//! MSRs (the GDT layout in `arch::x86_64::gdt` already matches what `sysret`
//! expects), a table indexed by Linux syscall number returning `-errno`,
//! and per-subsystem handlers (process, memory, VFS, signals, futex).
