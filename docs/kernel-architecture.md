# Kernel architecture (phase A1 state)

The kernel in `kernel/` is a `no_std` Rust binary for `x86_64-unknown-none`,
booted by Limine. Goal (see `PLAN.md`): implement the Linux x86_64 syscall ABI
plus Android drivers so unmodified AOSP userspace runs on it.

## Building and running

Host needs: `qemu-desktop xorriso edk2-ovmf limine` (pacman). The toolchain is
pinned in `kernel/rust-toolchain.toml` (nightly + `rust-src`, target
`x86_64-unknown-none` installed via rustup, no `build-std` needed).

```
tools/run-qemu.sh                 # build + boot (UEFI, KVM, 4 CPUs, 2 GiB), serial on stdio, Ctrl-A X quits
tools/run-qemu.sh --test          # build + run in-kernel tests, exit status 0/1
tools/run-qemu.sh --bios --no-kvm --smp 2 --mem 512M   # other machine shapes
cd kernel && cargo run / cargo test                    # same, script is the cargo runner
```

`run-qemu.sh` builds a hybrid ISO (BIOS + UEFI) with xorriso and
`limine bios-install` next to the ELF in `kernel/target/`. If the environment
forces a toolchain through `RUSTUP_TOOLCHAIN`, unset it (the script does) so
`rust-toolchain.toml` applies.

## Source layout

```
kernel/
  rust-toolchain.toml  .cargo/config.toml (target, rustflags, runner)
  kernel/              the kernel crate (more crates join the workspace later)
    linker.ld          higher-half image at 0xffffffff80000000, W^X segments
    src/
      main.rs          kmain: boot sequence, panic handler, test entry
      boot.rs          Limine requests (base revision 6, min 3) and accessors
      logger.rs        `log` backend -> serial, "[uptime] LEVEL cpuN msg"
      time.rs          tick counter (HZ = 100), sleep_ms
      testing.rs       custom test framework + isa-debug-exit
      arch/x86_64/     gdt, cpu (per-CPU via GS base), idt, pic, pit, apic, smp
      mm/              frame (bitmap allocator), paging, heap
      drivers/         serial (COM1); later virtio, binder, dma-buf, ...
      sched/ syscall/ fs/   placeholders documenting phases B1/B2
```

Rule: generic code calls `crate::arch::*` re-exports, not `arch::x86_64::*`.

## Boot sequence (BSP)

1. `serial::init`, `logger::init`.
2. `boot::init`: check Limine base revision, log bootloader/cmdline.
3. `arch::init_bsp_early`: static BSP `PerCpu` (GDT + TSS with a double-fault
   IST stack, GS base -> `PerCpu`), load the IDT.
4. `mm::init`: log memory map -> frame allocator -> new kernel page tables ->
   heap.
5. `arch::init_bsp_late`: remap+mask the 8259 PIC, enable LAPIC, calibrate the
   LAPIC timer against PIT channel 2 (10 ms), start the periodic timer, `sti`.
6. `smp::start_aps`: per AP allocate `PerCpu`, a 64 KiB stack and an IST
   stack, `MpInfo::bootstrap`. The AP loads the kernel CR3, switches stack,
   installs GDT/TSS/GS/IDT, enables its LAPIC and parks in `hlt` with
   interrupts on. The BSP waits up to 1 s for all of them.
7. Tests (`cfg(test)`) or the idle loop.

## Memory

- **Physical**: bitmap allocator over USABLE regions only (1 bit / 4 KiB,
  ~64 KiB for 2 GiB). `alloc`, `alloc_zeroed`, `alloc_contiguous(n, align)`,
  `free`. Bootloader-reclaimable memory is *not* reclaimed yet (Limine
  responses, AP trampolines/stacks live there).
- **Virtual layout** (kernel half):
  - `0xffff8000_00000000 + phys`: direct map (same offset as Limine's HHDM) of
    every memory-map region except reserved/bad, using 1 GiB/2 MiB/4 KiB pages,
    RW + NX + global. MMIO is added on demand by `paging::map_mmio` (UC).
  - `0xffffe000_00000000`: free area used by the paging test; intended for
    vmalloc-style mappings / kernel stacks with guard pages.
  - `0xffffffff80000000`: kernel image; requests RW-NX, text RX, rodata R-NX,
    data/bss RW-NX. CR0.WP, EFER.NXE, CR4.PGE are enabled.
  - The lower half is empty, reserved for user space.
- **Heap**: `linked_list_allocator` on one contiguous block (1/8 of free RAM,
  clamped to 4..64 MiB) reached via the direct map. Not interrupt-safe:
  interrupt handlers must not allocate. Does not grow yet.

## CPU structures

- GDT per CPU: `0x08` kernel code, `0x10` kernel data, `0x1b` user data,
  `0x23` user code, `0x28` TSS. This order is what `syscall`/`sysret` need:
  STAR[47:32] = 0x08, STAR[63:48] = 0x10 (sysret -> SS 0x1b, CS 0x23).
- IDT shared by all CPUs. Vectors: 0x00-0x1f exceptions (all fatal except
  `#BP`; `#DF` on IST 0), 0x20 LAPIC timer, 0xe0-0xef masked PIC (spurious),
  0xff LAPIC spurious.
- LAPIC: x2APIC (MSRs) when Limine enabled it (`MP_FLAG_X2APIC`), else xAPIC
  MMIO mapped via `map_mmio`. Only the BSP runs the timer.
- `PerCpu` (GS base): index, LAPIC id, stacks, GDT, TSS; `gs:[0]` = self.

## Tests

`#[test_case]` functions anywhere in the crate; `cargo test` boots a test
kernel which runs them after full boot (SMP included) and writes to
`isa-debug-exit` (0x10 -> QEMU status 33 = pass, 0x11 -> 35 = fail); a panic
fails the run; the script times out after 120 s. Current tests: int3 returns,
all CPUs online, per-CPU area, frame alloc/free/zero/contiguous, heap Box/Vec/
1 MiB, map/unmap a page, kernel text RX-only, timer ticks advance. Verified
under UEFI+KVM, BIOS+KVM, UEFI+TCG, BIOS+TCG, and xAPIC (`-- -cpu host,-x2apic`).

## Known gaps / next step (B1: user mode, syscall entry, ELF loader)

B1 needs, roughly in order:
1. Kernel stacks with guard pages (vmalloc area) and `TSS.rsp0` per thread.
2. Per-process address spaces: new PML4 sharing entries 256..511 with the
   kernel PML4, user mappings in the lower half; TLB shootdown IPIs (ICR write
   in `apic`) once threads run on several CPUs.
3. `syscall` entry: STAR/LSTAR/SFMASK/EFER.SCE, `swapgs` with the kernel GS
   base = `PerCpu` (add a scratch slot for the user RSP and the kernel stack
   top), save a Linux-compatible `pt_regs`, dispatch by `rax`, return `-errno`.
4. Tasks + scheduler: context switch, run queues per CPU, timer preemption on
   every CPU (APs currently only `hlt`), wake APs with IPIs.
5. ELF loader for static binaries first (the `tests/` oracle programs), with
   the initial stack (argv, envp, auxv: AT_PHDR, AT_ENTRY, AT_RANDOM, AT_PAGESZ),
   then `PT_INTERP` (bionic's linker64). Programs come from a Limine module
   (initramfs cpio) until the VFS (B2) exists.
6. FPU/SSE state: user code uses SSE; enable CR4.OSFXSR/OSXSAVE and save
   XSAVE areas on context switch (the kernel itself is soft-float).
7. Page-fault handler that resolves user faults or delivers SIGSEGV.

Other gaps: no ACPI parsing yet (RSDP is requested; MADT/IOAPIC/HPET needed for
device IRQs), bootloader-reclaimable memory not reclaimed, heap does not grow,
panic does not stop other CPUs, timer only on the BSP.
