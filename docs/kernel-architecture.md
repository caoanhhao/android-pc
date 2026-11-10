# Kernel architecture (phase D3b state: AOSP boots to sys.boot_completed=1 and Launcher3 draws; an APK built from source installs with `adb install` and its Activity runs on screen; zygote runs ART's userfaultfd GC; SurfaceFlinger composites on virtio-gpu; virtio-input devices reach getevent; E1: NVMe, AHCI and xHCI USB keyboard/mouse drivers, AOSP boots from NVMe; E2: a simpledrm-style DRM driver on the firmware framebuffer, AOSP boots to the launcher on it under UEFI; E3: USB mass storage and hubs, ACPI tables, timer calibration without the PIT, the kernel log on the framebuffer; AOSP boots from a USB stick image under OVMF, docs/usb-boot.md)

The kernel in `kernel/` is a `no_std` Rust binary for `x86_64-unknown-none`,
booted by Limine. Goal (see `PLAN.md`): implement the Linux x86_64 syscall ABI
plus Android drivers so unmodified AOSP userspace runs on it.

## Building and running

Host needs: `qemu-desktop xorriso edk2-ovmf limine` (pacman). The toolchain is
pinned in `kernel/rust-toolchain.toml` (nightly + `rust-src`, target
`x86_64-unknown-none` installed via rustup, no `build-std` needed).

```
tools/run-qemu.sh                 # build + boot (UEFI, KVM, 4 CPUs, 2 GiB), serial on stdio, Ctrl-A X quits;
                                  # boots out/ktest-initramfs.cpio when it exists
tools/run-qemu.sh --test          # build + run in-kernel tests, exit status 0/1
tools/run-qemu.sh --ktest b1      # build the test initramfs, run ktest-init in the guest
                                  # (FILTER prefixes optional), exit 0 iff "END ktest-init ... fail=0"
tools/run-qemu.sh --aosp          # AOSP mksh + toybox (dynamically linked against bionic) on the
                                  # serial console, see aosp-shell.md
tools/run-qemu.sh --aosp-sh-test  # the same, with scripted commands; exit 0 iff all checks pass
tools/run-qemu.sh --aosp-disk-test  # AOSP super.img as a read-only virtio disk: dmctl maps
                                  # system_a/vendor_a, erofs mounts, toybox runs from the image,
                                  # every file's md5 checked against the host
tools/run-qemu.sh --aosp-init     # unmodified AOSP /init (init_boot ramdisk + super.img): first and
                                  # second stage, rc triggers, console shell; see android-init.md
tools/run-qemu.sh --aosp-init-test  # the same, boot milestones and getprop/setprop checked
tools/run-qemu.sh --aosp-gfx-test  # the same boot with a virtio-gpu: graphics HALs, surfaceflinger,
                                  # adb screencap and a QEMU screendump checked (see android-init.md)
tools/run-qemu.sh --aosp-boot-test  # zygote, system_server, sys.boot_completed=1 (D3a)
tools/run-qemu.sh --aosp-apk-test  # builds apps/hello, boots, adb install, am start: the Activity
                                  # resumed and on the screen (D3b, see android-init.md)
tools/run-qemu.sh --aosp-fb-test  # E2: the boot test with no virtio-gpu, only a VGA's linear
                                  # framebuffer set up by OVMF's GOP (simpledrm): launcher on
                                  # screencap and in the framebuffer (QEMU screendump)
tools/run-qemu.sh --gpu [--gpu-size WxH] [--display vnc=:1] [--monitor SOCK]
                                  # virtio-gpu-pci at 00:02.0 (default for --ktest / --aosp-gfx-test)
tools/run-qemu.sh --fb[=std|bochs] [--gpu-size WxH]
                                  # a VGA / bochs-display at 00:02.0 instead: Limine asks the
                                  # firmware for that mode and the kernel drives it (simpledrm)
tools/run-qemu.sh --disk IMG[,rw|,snapshot][,legacy]   # attach raw images as virtio-blk disks
                                  # (PCI slots 00:04.0, 00:05.0, ... in order)
tools/run-qemu.sh --nvme ...      # the same disks (and the AOSP boots' super/frp/misc) as NVMe
                                  # controllers: nvme0n1, nvme1n1, ... (E1)
tools/run-qemu.sh --hw-test       # E1: C2 disk on NVMe + AHCI + usb-storage, qemu-xhci with a usb-hub
                                  # (usb-kbd behind it) and usb-mouse; ktest c2_fs + e1_hw, keys/mouse
                                  # sent through the monitor
tools/run-qemu.sh --usb-boot-test # E3: AOSP from a USB stick image (tools/mkusb.sh + tools/mkusbimg.py)
                                  # under OVMF, no virtio disk/GPU/input; checked by aosp-fb-test.py --usb
tools/mkusb.sh --esp /run/media/$USER/ANDROIDPC   # E3: fill a real stick's ESP (docs/usb-boot.md)
tools/run-qemu.sh --initrd out/b1_hello.cpio --append "strace"   # other initramfs, kernel cmdline
tools/run-qemu.sh --bios --no-kvm --smp 2 --mem 512M   # other machine shapes (UEFI through
                                  # OVMF is the default; --bios boots the same ISO via SeaBIOS)
cd kernel && cargo run / cargo test                    # same, script is the cargo runner
```

Wrap every build or QEMU run in `tools/limited-build.sh` (see `CLAUDE.md`).

Kernel command line (`--append`): `init=PATH` (default `/init`), `strace`
(log every syscall), `strace=COMM` (only processes whose name starts with
COMM once, and their later children; write/writev payloads are shown, e.g.
`strace=adbd`), `abortdump=COMM` (keep the last 48 syscalls of each thread
of such processes and log them when a thread abort()s itself: SIGABRT to
its own tid), `dtfstab=` (Android device-tree fstab entries, see
android-init.md), `simpledrm.name=NAME` (the DRM driver name of the boot
framebuffer device, see Graphics), and everything after `--` becomes init's argv[1..]
(`--ktest b1` passes `-- b1`, so ktest-init only runs the B1 tests).

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
      main.rs          kmain: boot sequence, cmdline, panic handler, test entry
      boot.rs          Limine requests (base revision 6, min 3): memmap, HHDM, MP,
                       modules (initramfs), cmdline, boot date
      logger.rs        `log` backend -> serial, "[uptime] LEVEL cpuN msg", and the
                       kernel log buffer (fs/kmsg)
      time.rs          ticks (HZ = 100), TSC clock (monotonic/realtime ns)
      sync.rs          IrqLock (spinlock with local interrupts off)
      errno.rs         Errno / KResult
      random.rs        RDRAND + xorshift (getrandom, AT_RANDOM, /dev/urandom)
      security.rs      SELinux stub: enforce/policy state, per-process contexts (attr/*),
                       exec transitions, labels for new files
      testing.rs       custom test framework + isa-debug-exit
      arch/x86_64/     gdt, cpu (per-CPU via GS base, syscall MSRs, FPU/FSGSBASE),
                       entry (syscall/trap asm, TrapFrame, context switch),
                       trap (exceptions -> signals), idt, fpu (FXSAVE), tlb
                       (shootdown/kick IPIs), pic, pit, apic, smp
      mm/              frame (bitmap allocator + refcounts), paging, heap (size
                       classes + linked list), kstack (guarded kernel stacks), user
                       (address spaces, VMAs, user copies), shmem, userfaultfd
      sched/           run queue, context switch, sleep/wake, idle loop;
                       timer (kernel timer callbacks from the tick)
      process/         Task/Process/process table, lifecycle (clone, exec, exit,
                       wait4), elf (loader + initial stack), signal, futex (+ PI),
                       pidfd, ptimer (POSIX timers, ITIMER_REAL)
      block/           block devices: registry, /dev/block nodes, sysfs, block ioctls
                       (mod), per-device page cache (cache), GPT partitions (gpt),
                       loop devices (loop_dev), device mapper (dm), RAM disks (brd)
      syscall/         dispatch table (Linux numbers), fs, time, poll (poll/select,
                       epoll, eventfd, timerfd, signalfd syscalls), net (socket
                       syscalls, msghdr/cmsg handling, socket options), sys
                       (rlimits, rusage, sched_*, priorities, ids, caps,
                       personality, sigqueueinfo, unshare/setns, syslog,
                       hostname, ioprio), keys (add_key/keyctl)
      bpf/             eBPF objects: maps (map.rs: array, hash, LRU, LPM trie,
                       per-CPU, ring buffer storage), programs (kept, never run),
                       BTF, links, attachments, ids, bpffs pinning; syscall/bpf.rs
                       is the bpf(2) front end
      net/             sockets: constants, credentials; unix (AF_UNIX sockets,
                       also behind local AF_VSOCK); packet (AF_PACKET + classic
                       BPF); tun (/dev/net/tun);
                       netlink (AF_NETLINK: kernel uevents, stub protocols);
                       rtnetlink (NETLINK_ROUTE); ifreq (interface ioctls);
                       inet/ (AF_INET/AF_INET6: mod.rs the stack under smoltcp,
                       links, routes, loopback + ARP device; socket.rs TCP/UDP
                       sockets; netfilter.rs stored iptables tables)
      fs/              vfs (inodes, dentries, mounts, path walk), file (open
                       files, fd tables), tmpfs (also rootfs/devtmpfs/sysfs),
                       dev (character devices), tty (console line discipline),
                       procfs, pipe (+ FIFOs), poll
                       (event bits, wait queues, poll loop), epoll, eventfd,
                       timerfd, signalfd; erofs (+ lz4), ext4 (read-only disk
                       file systems); sysfs (device tree, uevent files), sysctl
                       (/proc/sys table), selinuxfs, cgroupfs (cgroup v1/v2),
                       inotify, pty (ptmx + devpts), kmsg (printk ring: /dev/kmsg,
                       /proc/kmsg, syslog), lock (OFD byte-range locks); mod.rs: cpio unpacking,
                       open, mountable file system types
      drivers/         serial (COM1); binder/ (binder IPC + binderfs: abi, state,
                       protocol, mod); ashmem; hvc (host-less virtio-console
                       ports); pci (config space, enumeration, sysfs dirs, DMA
                       and MMIO helpers); nvme; ahci; usb/ (xHCI host + hid boot
                       keyboards/mice); virtio/
                       (transports + virtqueues, blk, net, gpu); drm/ (DRM device
                       on virtio-gpu or the boot framebuffer: mod = files,
                       GEM, ioctls; kms = mode objects, properties, atomic
                       state, outputs); dma_buf/ (buffers,
                       dma-buf files, the system dma-heap; sync_file = fences,
                       sync files, sw_sync)
```

Rule: generic code calls `crate::arch::*` re-exports, not `arch::x86_64::*`.

## Boot sequence (BSP)

1. `serial::init` (if a UART answers), `fbcon::init` (kernel log on the
   boot framebuffer), `serial=off` honoured, `logger::init` (`loglevel=`).
2. `boot::init`: check Limine base revision, log bootloader/cmdline and
   where the kernel was loaded from (GPT disk GUID).
3. `arch::init_bsp_early`: static BSP `PerCpu` (GDT + TSS with a double-fault
   IST stack, GS base -> `PerCpu`), load the IDT, enable SSE/FXSR (+FSGSBASE)
   for user code and program STAR/LSTAR/SFMASK.
4. `mm::init`: log memory map -> frame allocator -> new kernel page tables
   (all 256 kernel-half PDPTs pre-allocated) -> heap.
5. `arch::init_bsp_late`: remap+mask the 8259 PIC, parse the ACPI tables,
   enable LAPIC, calibrate the LAPIC timer and the TSC against the HPET or
   ACPI PM timer (20 ms; else PIT channel 2, 10 ms; else CPUID's crystal
   clock), start the periodic timer, `sti`.
6. `smp::start_aps`: per AP allocate `PerCpu`, a 64 KiB stack and an IST
   stack, `MpInfo::bootstrap`. The AP loads the kernel CR3, switches stack,
   installs GDT/TSS/GS/IDT, user-mode support, its LAPIC and timer, then
   enters the scheduler's idle loop. The BSP waits up to 1 s for all of them.
7. Tests (`cfg(test)`), or: unpack the initramfs (first Limine module)
   into a tmpfs that becomes the root mount, fill in sysfs (mount
   points, memory/tty/misc character devices), create loop0-7 and the
   RAM disks asked for with `brd.rd_nr=` (`block::init`), enumerate PCI and bring up the virtio
   devices, NVMe and AHCI disks with their partitions and xHCI USB
   keyboards/mice/hubs/disks (`drivers::init`), fill in
   `androidboot.*=auto` from the boot disk (`block::resolve_boot_disk`),
   start `/init` as pid 1 (cwd
   `/`, umask 022) with fds 0-2 on the serial console, and enter the idle
   loop. When init exits the kernel logs its status and powers off.

## Memory

- **Physical**: bitmap allocator over USABLE regions only (1 bit / 4 KiB,
  ~64 KiB for 2 GiB) plus a `u32` reference count per frame (~2 MiB for
  2 GiB, stored next to the bitmap). `alloc`, `alloc_zeroed`,
  `alloc_contiguous(n, align)` return frames with count 1; `get` adds a
  reference, `free` drops one and frees the frame at 0, `refcount` reads it.
  Only user pages are ever shared; everything else keeps count 1. Bootloader-reclaimable memory is *not* reclaimed yet (Limine
  responses, AP trampolines/stacks live there).
- **Virtual layout** (kernel half):
  - `0xffff8000_00000000 + phys`: direct map (same offset as Limine's HHDM) of
    every memory-map region except reserved/bad, using 1 GiB/2 MiB/4 KiB pages,
    RW + NX + global. MMIO is added on demand by `paging::map_mmio` (UC).
  - `0xffffe000_00000000`: page used by the paging test.
  - `0xffffe000_10000000..0xfffff000_00000000`: task kernel stacks (64 KiB +
    an unmapped guard page each). Slots are never reused, so freeing a stack
    needs no TLB shootdown.
  - `0xffffffff80000000`: kernel image; requests RW-NX, text RX, rodata R-NX,
    data/bss RW-NX. CR0.WP, EFER.NXE, CR4.PGE are enabled.
  - The lower half is user space (one PML4 per process, see below).
- **Heap**: one contiguous block (1/8 of free RAM, clamped to 4..256 MiB)
  reached via the direct map. Allocations up to 4 KiB come from nine
  power-of-two size classes (free lists, one lock each, refilled with
  16 KiB chunks that are never returned); larger ones from a
  `linked_list_allocator` over the block (D3a: the linked list alone,
  O(free blocks) per call under one lock, dominated kernel time once
  system_server ran). Interrupts are off while a heap lock is held, so
  interrupt handlers may allocate. Does not grow yet.
- **User address spaces** (`mm/user.rs`): a PML4 whose entries 256..511 are
  copied from the kernel PML4 (shared kernel PDPTs). VMAs (start, end, prot,
  shared, maywrite, optional backing = shmem object + page offset + the
  open file) in a BTreeMap. ELF segments are copied in at exec; anonymous memory
  (stack, brk heap, mmap) is zero-filled on first touch, from the page-fault
  handler or from kernel user-copies. PROT_NONE pages keep their frame in a
  not-present PTE marked BIT_9.
  - **Copy-on-write fork**: every present private page gets a second frame
    reference and its PTE loses WRITABLE in parent and child; the parent's
    TLBs are shot down. A PTE that is read-only inside a writable private VMA
    *is* a COW page (no extra PTE bit). A write to it — page fault with
    P=1, or `copy_out` from the kernel — copies the frame, or, if the
    refcount is back to 1, just sets WRITABLE again; the old frame is
    released after a TLB shootdown. `mprotect` and ELF segment merging keep
    still-shared pages read-only. Frames return to the allocator only when
    the last address space drops them (munmap, exit, exec).
  - **Shmem objects / page cache** (`mm/shmem.rs`): a sparse
    `BTreeMap<page index, frame>` + size + memfd seals. Every tmpfs regular
    file (memfd included) keeps its data in one, and every MAP_SHARED
    anonymous mapping gets one at mmap time. The object holds one frame
    reference per page, each PTE another. `read`/`write` copy through the
    direct map, so they and every MAP_SHARED mapping see the same frames.
    Files from the initramfs keep pointing into the boot module until a page
    is first touched. Files of disk file systems (erofs, ext4) get a
    read-only object with a `PageSource` (the file system's read path) that
    fills missing pages; write/truncate/fallocate on it give EROFS. No size cap (sparse, up to 2^63-1); `write` and
    `fallocate` return ENOSPC rather than use the last 16 MiB of RAM.
  - **File and shared mappings**: a fault in a backed VMA asks the object
    for page `pgoff + (va - start) / 4096` (EFAULT past EOF -> SIGBUS
    BUS_ADRERR; EFAULT for kernel copies). MAP_SHARED maps it writable per
    prot; MAP_PRIVATE maps it read-only, which the COW rule above turns into
    a copy on write (the object's reference keeps the count above 1), or
    copies at once for a write fault. fork copies the PTEs (shared ones stay
    writable). Each object keeps weak references to the address spaces that
    map it: truncate unmaps the dropped pages from all of them (private
    copies too, like Linux), a hole punch unmaps only the object's own
    frames, and F_SEAL_WRITE fails with EBUSY while a shared VMA with
    `maywrite` (fd open for writing, no write seal) exists. Lock order:
    address space -> shmem -> frame allocator; an object never calls into
    an address space under its own lock. At process exit the mappings are
    dropped before the parent is notified (Linux's exit_mm), unless a vfork
    parent shares the address space: `AddressSpace::users` counts the live
    tasks using it (Linux's mm_users; exited tasks not yet freed still hold
    `Arc`s), and the last one to exit (or exec away) unmaps everything,
    closing mapped files such as a binder buffer's.
  - **userfaultfd** (`mm/userfaultfd.rs`, D3a): a context (syscall 323
    or /dev/userfaultfd's USERFAULTFD_IOC_NEW) belongs to the caller's
    address space; UFFDIO_REGISTER puts a `UffdReg` (context + mode) on
    the VMAs of the range (private anonymous memory, or writable shmem;
    MINOR needs shmem). A fault on a registered page without a PTE
    (MISSING: no page; MINOR: the object has it, this mapping not) either
    returns `FaultError::Bus` (UFFD_FEATURE_SIGBUS: the trap handler
    raises SIGBUS/BUS_ADRERR at the address, ART's mutators fill the page
    from their handler) or `FaultError::Userfault`: the thread queues a
    `uffd_msg` (read/poll on the fd) and sleeps until UFFDIO_COPY /
    ZEROPAGE / CONTINUE / MOVE (unless DONTWAKE) / WAKE covers the page,
    the range is unregistered or the fd closed, then retries the
    instruction. Kernel copies to such a page wait the same way (or
    EFAULT in SIGBUS and UFFD_USER_MODE_ONLY contexts). COPY/ZEROPAGE put
    a fresh frame in the PTE (shared shmem: into the object), CONTINUE
    maps the object's page, MOVE moves PTEs (a COW-shared source page is
    copied); results follow Linux (bytes done, EEXIST/ENOENT, EAGAIN when
    partial). Registrations are not inherited by fork children or by
    the new range of an mremap; MREMAP_DONTUNMAP leaves the old range
    registered (ART registers, then moves the pages to a shadow mapping).
    Unregister, close and mprotect re-merge VMAs that match again
    (`Inner::remerge`), so a later mremap sees one VMA.
  - **madvise** DONTNEED/FREE unmap the pages of the range (backed VMAs
    refault from their object), REMOVE punches a hole in the object;
    **mremap** shrinks, grows in place when the following range is free, or
    moves the PTEs (not the data) to a new range (MAYMOVE, FIXED,
    DONTUNMAP), keeping a backed VMA's object at the right offset;
    old_size 0 maps a shared mapping's pages a second time.
  - **Futex keys**: (object address, byte offset) inside MAP_SHARED backed
    VMAs unless FUTEX_PRIVATE_FLAG, else (address space, address).
  - **Stack**: the 8 MiB stack VMA is demand-faulted, which covers normal
    growth; MAP_GROWSDOWN and RLIMIT_STACK-driven growth are not done.
  Layout: ELF at its link address (static-PIE at 0x5555_5555_4000), brk after
  it, mmap top-down below 0x7f00_0000_0000, 8 MiB stack VMA below
  0x7fff_ffff_f000. The kernel never dereferences user pointers: `copy_in` /
  `copy_out` walk the page tables under the address-space lock and use the
  direct map, so bad pointers are `EFAULT`. Each address space tracks the CPUs
  that have it loaded; munmap/mprotect/mremap/madvise, fork (parent PTEs made
  read-only) and COW copies send a TLB shootdown IPI (0xf0) to
  them and wait for the acknowledgements.

## CPU structures

- GDT per CPU: `0x08` kernel code, `0x10` kernel data, `0x1b` user data,
  `0x23` user code, `0x28` TSS. This order is what `syscall`/`sysret` need:
  STAR[47:32] = 0x08, STAR[63:48] = 0x10 (sysret -> SS 0x1b, CS 0x23).
- IDT shared by all CPUs. Vectors: 0x00-0x1f exceptions, 0x20 LAPIC timer,
  0xe0-0xef masked PIC (spurious), 0xf0 TLB shootdown IPI, 0xf1 reschedule
  IPI (wakes an idle CPU), 0xff LAPIC spurious. Everything that can come from
  ring 3 goes through assembly stubs (`entry.rs`) that build a `TrapFrame` and
  call `trap::trap_handler`; `int3`/`into` have DPL 3. NMI, #DF (IST 0) and
  #MC use `x86-interrupt` handlers and are fatal. User exceptions become
  signals; kernel exceptions panic (except `int3`).
- LAPIC: x2APIC (MSRs) when Limine enabled it (`MP_FLAG_X2APIC`), else xAPIC
  MMIO mapped via `map_mmio`. Every CPU runs the periodic timer at HZ = 100;
  the BSP's also drives the tick count and sleep timeouts. `apic::send_ipi`
  writes the ICR.
- `PerCpu` (GS base): `gs:[0]` = self, `gs:[8]` user RSP scratch, `gs:[16]`
  top of the running task's kernel stack, then index, LAPIC id, stacks, GDT,
  TSS (RSP0 = same kernel stack top).
- FPU: CR0.MP, CR4.OSFXSR/OSXMMEXCPT (and FSGSBASE when supported). The
  kernel is soft-float, so a task's user FPU state stays live in the CPU
  while it is in the kernel; it is saved/restored with FXSAVE/FXRSTOR on
  every context switch and stored in signal frames. (XSAVE/AVX state later.)

## Kernel entry and exit

- `syscall` (LSTAR = `__syscall_entry`, SFMASK clears IF/TF/DF/AC/NT):
  `swapgs`, save the user RSP in `gs:[8]`, load `gs:[16]`, push a frame in
  the order of Linux `pt_regs` (`TrapFrame`: r15..rdi, vector, error
  code / orig_rax, rip, cs, rflags, rsp, ss; 176 bytes), enable interrupts,
  `syscall::dispatch` (number in RAX, args in RDI, RSI, RDX, R10, R8, R9,
  result or `-errno` in RAX).
- Interrupts/exceptions: the stub pushes a 0 error code if the CPU did not,
  the vector, `swapgs` only when coming from ring 3, then the same registers.
- Exit (`sched::exit_to_user`, run before every return to ring 3 with
  interrupts disabled at the end): reschedule if requested, deliver pending
  signals (one per iteration, building signal frames), restart or fail
  interrupted syscalls (ERESTARTSYS -> SA_RESTART or EINTR). Then `sysretq`
  when the frame allows it (RCX = RIP, R11 = RFLAGS, canonical user RIP, no
  TF/RF), else `iretq` (after exec, sigreturn, interrupts).
- GS: in the kernel GS base = `PerCpu` and KERNEL_GS_BASE = the user's GS
  base; arch_prctl(ARCH_SET_GS) and the context switch work on
  KERNEL_GS_BASE.

## Scheduler (`sched/`)

- CPU time (D3a): every timer tick charges the task it interrupted, as
  user or kernel time (`Task::utime`/`stime`, /proc/<pid>[/task/<tid>]/stat).
  Ticks that interrupt kernel code also sample the RIP; `echo p >
  /proc/sysrq-trigger` logs the 30 most frequent addresses ("profile:")
  and starts over (resolve them with addr2line on the kernel ELF).

- One global run queue (`IrqLock<VecDeque<Arc<Task>>>`) shared by all CPUs,
  FIFO round robin. The timer interrupt on each CPU requests a reschedule
  when other tasks are waiting; preemption happens on the way back to user
  mode (the kernel itself is not preemptible: tasks block only inside
  `schedule`, never holding a lock).
- Context switch: callee-saved registers + RSP on the kernel stack
  (`__switch_stacks`). The run-queue lock is held across the switch and
  released by the next context in `finish_switch`, which also clears the
  previous task's `on_cpu` flag and drops the reference to it (freeing a dead
  task's kernel stack and address space once it is off the CPU). `on_cpu`
  keeps a task that was woken while still running elsewhere from being
  picked by a second CPU. The switch saves/loads FPU state, FS base, user GS
  base, CR3 (with the per-address-space CPU mask), TSS.RSP0 and `gs:[16]`.
- Each CPU's idle context is its boot stack: it loads the kernel page
  tables, marks itself idle and `hlt`s; enqueueing a task kicks one idle CPU
  with the reschedule IPI.
- Sleeping: a single global queue of (key, deadline, task) entries.
  `wait_until(key, deadline, mode, ready)` re-checks a condition after each
  wake-up (wait4, pipes, vfork, sleeps, sigsuspend/sigtimedwait);
  `wait_on(key, deadline, check)` is the futex variant (check under
  registration, woken only by `wake_n`). Interruptible waits end on any
  deliverable signal, killable ones only on SIGKILL. The BSP's timer tick
  expires deadlines (10 ms granularity; times are read from the TSC so sleeps
  never end early).
- Kernel timers (`sched/timer.rs`): `arm(deadline, Weak<dyn TimerCallback>)`
  / `cancel(id)`; the BSP's tick runs expired callbacks in interrupt
  context (so they only take `IrqLock`s). timerfd uses them.

## Processes (`process/`)

- `Task` = a thread: tid, kernel stack, saved RSP, state
  (RUNNING/SLEEPING/DEAD), FS/GS bases, FPU area, address space
  (`Arc<AddressSpace>`, shared by CLONE_VM), per-thread signal state (mask,
  pending, altstack), clear_child_tid, robust list, credentials (`Creds`:
  real/effective/saved/fs uid and gid, supplementary groups; per thread
  like Linux, copied by clone, set by the set*id calls and by exec of a
  set-user/group-ID file; reported, never checked: see
  [syscalls.md](syscalls.md#credentials-c3b)).
- `Process` = thread group: pid (= leader tid), signal actions and
  process-wide pending signals, fd table (`Arc`, shared by CLONE_FILES),
  `FsContext` (cwd as a `PathRef`, umask; copied by fork), the executable
  and dynamic linker paths and argv (for /proc), exit signal, name, and
  `Attrs` (rlimits, nice, personality, sched policy, prctl flags; copied
  by fork, reported back by their syscalls but not acted on), and the
  mount namespace (`Option<Arc<MntNs>>`, None = the initial one).
- `TABLE`: tid -> task map and pid -> node (parent, children, live threads,
  pgid/sid, zombie status, group exit status) under one lock. Orphans are
  reparented to pid 1; a parent with SIGCHLD set to SIG_IGN (or
  SA_NOCLDWAIT) gets its children reaped automatically.
- `clone` covers fork, vfork (parent sleeps until exec/exit) and bionic-style
  threads; `exit` ends a thread (clear_child_tid + futex wake); the process
  becomes a zombie with its last thread (status from exit_group / a fatal
  signal, else the leader's exit code), closes its files, sends its exit
  signal and wakes `wait4` sleepers. `exit_group` and SIGKILL mark every
  thread with a pending SIGKILL and wake it.
- `execve` resolves the path through the VFS (relative to the cwd,
  following symlinks), loads the new image first (errors return normally), then kills
  the other threads and waits for them, swaps the address space, releases a
  vfork parent, resets handled signals to SIG_DFL (ignored stay ignored, the
  mask stays), clears the altstack, closes CLOEXEC fds, clears FS/GS and the
  FPU.
- ELF loader (`elf.rs`): ET_EXEC and ET_DYN (PIE at 0x555555554000),
  PT_LOAD segments copied into anonymous memory (pages shared by two
  segments get both protections), bss zeroed, brk after the last segment.
  A PT_INTERP program (bionic's `/system/bin/linker64`, an ET_DYN) is read
  through the VFS by `lifecycle::load_executable`, mapped at
  0x7fc000000000 (between the top-down mmap area, below 0x7f0000000000, and
  the stack) and entered first, with AT_BASE = its base and AT_PHDR/
  AT_PHNUM/AT_ENTRY describing the program, as Linux's binfmt_elf does.
  `#!interp [arg]` scripts re-run the lookup with the interpreter (up to 5
  levels). /proc/<pid>/maps names the linker's range by its path. Initial stack: argc, argv, envp, auxv
  (AT_PHDR, AT_PHENT, AT_PHNUM, AT_PAGESZ, AT_BASE, AT_FLAGS, AT_ENTRY,
  AT_UID/EUID/GID/EGID, AT_HWCAP, AT_HWCAP2, AT_CLKTCK, AT_SECURE,
  AT_MINSIGSTKSZ, AT_RANDOM, AT_EXECFN, AT_PLATFORM), strings at the top,
  RSP 16-byte aligned at argc.
- Signals and futexes: see [syscalls.md](syscalls.md). PI futexes
  (LOCK_PI, LOCK_PI2, TRYLOCK_PI, UNLOCK_PI, D3a) follow the futex word
  protocol (owner tid, FUTEX_WAITERS, FUTEX_OWNER_DIED for a dead owner)
  without boosting priorities; UNLOCK_PI frees the word and wakes one
  waiter, which re-takes it with FUTEX_WAITERS while others sleep.
- Thread names (D3a): each `Task` has its own `comm` (PR_SET_NAME,
  /proc/<pid>/task/<tid>/comm and stat); the leader's is the process's.
- pidfds (`process/pidfd.rs`, D3a): pidfd_open and CLONE_PIDFD give a
  file naming a thread-group leader; POLLIN once it exited (a global wait
  queue woken at every process exit), pidfd_send_signal, pidfd_getfd,
  process_madvise (hints only) and process_mrelease (no-op) take one.
- POSIX timers and ITIMER_REAL (`process/ptimer.rs`, D3a): kernel timers
  fire in the timer interrupt, which cannot take the process table lock,
  so an expired timer is queued and its signal (SI_TIMER with timer id,
  overrun and sigev_value, or SIGEV_THREAD_ID to one thread) is sent from
  task context: on the way back to user mode (`exit_to_user`) or from the
  idle loop.
- /proc/<pid>/cmdline reads the argument area from the process's memory
  (Linux semantics: zygote's argv[0] rewrite to `system_server` or the
  app's package shows); another task's address space is taken under its
  `mm_lock`, which exec holds while swapping it.

## Files (`fs/`)

Mount namespaces share one global list of mounts; a namespace is the set
of mounts whose mount-point chain leads to its root mount. The pieces:

- **Inodes** (`vfs::Inode` trait): what a file system implements — lookup,
  create (file/dir/device node/FIFO by mode), symlink, link, unlink,
  rename (with RENAME_NOREPLACE/EXCHANGE), entries, readlink,
  read_at/write_at/append/truncate, and an optional `open` hook returning
  custom file operations (pseudo files). Every inode has a `Meta`: ino,
  dev, rdev and the attributes (mode, uid, gid, nlink, a/m/ctime ns).
- **Dentries** (`vfs::Dentry`): name, strong parent pointer, inode and a
  cache of children as weak references. Every namespace change goes
  through `vfs::{create, symlink, link, unlink, rename}`, which update the
  cache: unlinked dentries are marked deleted (paths get " (deleted)",
  getcwd gives ENOENT), renamed ones move, so cwd and open files keep the
  right path. Dynamic directories (procfs) opt out of caching.
- **Mounts** (`vfs::Mount`): root dentry, mount point (`PathRef` = mount +
  dentry), type, source, flags. `child_of` crosses into the most recent
  mount on a dentry (each dentry keeps weak references to the mounts on
  it, so path walks never scan the mount list; D3a); `parent_of` handles `..` at a mount root and never goes
  above `/`. `path_of` builds absolute paths the same way. MS_MOVE
  changes a mount's mount point. The root directory is whatever is
  mounted (moved) on top of the namespace's root mount (the initial
  rootfs or its copy), so after Android's switch_root every path walk
  starts in the system image; mounts not reachable from it drop out of
  /proc/mounts. A generation counter and wait queue let /proc/mounts
  readers poll for changes.
- **Namespaces and propagation** (`vfs::MntNs`, `copy_ns`,
  `mount_in_group`): unshare/clone(CLONE_NEWNS) copy every mount of the
  namespace (same root dentries, so the copies show the same trees); when
  the last process leaves a namespace (`MntNs` dropped) its mounts leave
  the list (zygote unshares one per app; D3a);
  setns switches through /proc/<pid>/ns/mnt. Each mount has a peer group
  (0 = private; MS_SHARED, binds of shared mounts and namespace copies
  share one): a new mount below one member is copied onto the same dentry
  below the others (bind mounts share dentries, so the dentry is valid
  there), an unmount removes the copies.
- **Magic links**: an inode may name its target directly
  (`Inode::magic_link`: /proc/<pid>/fd/N, exe, cwd, ns/*); the walk
  follows that instead of the link text.
- **Path walk** (`vfs::lookup`, `lookup_parent`): absolute or relative to a
  start `PathRef` (cwd or a dirfd), `.`/`..`, symlinks followed for every
  intermediate component and optionally the last one (at most 40, else
  ELOOP), ENOTDIR through files, trailing slashes require directories,
  ENAMETOOLONG for components > 255 bytes or paths >= 4096.
- **Open files** (`file::OpenFile`): inode, the `PathRef` it was opened
  by, status flags, position, and boxed `FileOps` (regular file, directory,
  O_PATH, generated `SeqFile`, device, pipe end, epoll, eventfd, timerfd,
  signalfd). `FileOps` also has `poll` (readiness bits; default "always
  ready"), `wait_queue` (see Readiness below), `anon_name` (for
  `anon_inode:[...]` names) and `fdinfo`. `FdTable` holds
  `(OpenFile, cloexec)` slots (1024 max); dup shares the description.
- **File systems**:
  - `tmpfs`: directories as `BTreeMap`s (so getdents order is by name),
    file data in a shmem object (`mm/shmem.rs`, see above), which `mmap`
    maps directly. memfds are unnamed tmpfs inodes on their own device;
    `/proc/<pid>/fd` and `maps` show them as `/memfd:NAME (deleted)`. The root file system ("rootfs") is a tmpfs filled from the
    cpio archive (files, directories, symlinks, device nodes, owners,
    mtimes). `mount -t tmpfs|ramfs` makes new instances (`mode=` option).
  - devtmpfs (one shared instance): null, zero, full, random, urandom,
    kmsg, tty, console, ttyS0, ptmx nodes plus `pts/` and `shm/`. Device nodes
    on any file system open the driver in `dev.rs` by major:minor. The
    console (/dev/console, /dev/tty, /dev/ttyS0) is the tty in `tty.rs`,
    below. /dev/kmsg is the kernel log buffer (`fs/kmsg.rs`: a static ring
    of 1024 records filled by the logger and by /dev/kmsg writes, read by
    /dev/kmsg, /proc/kmsg and syslog; readers woken from the timer tick).
    /dev/hw_random (misc 10:183) gives random bytes.
  - procfs: `self`, `thread-self`, `<pid>/{stat,status,cmdline,comm,maps,
    mounts,fd/,fdinfo/,exe,cwd,root}`, `meminfo`, `cpuinfo`, `uptime`, `mounts`,
    `filesystems`, `cmdline`, `version`, `loadavg`, `stat`, `net/unix`,
    `sys/kernel/random/{boot_id,uuid}` (libbinder aborts without boot_id). Contents are
    generated at open. `maps` shows `s`/`p`, file offset, device and inode,
    and names file mappings by path (`/dev/zero (deleted)` for anonymous
    shared memory). `maps`/`VmSize` are only filled in for the calling
    process (reading another task's address space pointer would race with
    its execve). `fd/N` are symlinks whose target is the open file's path
    (`pipe:[ino]` for pipes, `socket:[ino]` for sockets, `anon_inode:[eventfd]`/`[eventpoll]`/
    `[timerfd]`/`[signalfd]` for event files, which share one 0600
    anonymous inode); they are not "magic" links yet. `fdinfo/N`: pos,
    flags (with O_CLOEXEC), mnt_id (device), ino, then per-type lines.
  - sysfs (`fs/sysfs.rs`): one shared tmpfs. Devices live below
    /sys/devices (PCI functions, `virtioN`, `virtual/<class>/<name>`,
    `platform/serial8250`), each with a `uevent` pseudo file and a
    `subsystem` link; /sys/class, /sys/block, /sys/dev and /sys/bus hold
    relative symlinks into that tree. Writing an action to `uevent`
    broadcasts it on NETLINK_KOBJECT_UEVENT; registering or removing a
    device sends add/remove (hotplug). Plus mount points (fs/selinux,
    fs/cgroup, kernel/tracing, ...) and a few kernel attributes.
  - tmpfs extras: *pseudo files* (a `PseudoFile` object produces the
    contents at open and handles writes, optionally answering on the
    same fd: sysfs `uevent`, selinuxfs nodes) and extended attributes
    stored per inode (`security.selinux` from restorecon and fscreate).
  - selinuxfs, cgroupfs, devpts, /proc/sys: see [syscalls.md](syscalls.md)
    (SELinux stub, cgroups, pseudo-terminals, /proc and /sys for init)
    and [android-init.md](android-init.md).
  - pipes: 64 KiB, blocking and O_NONBLOCK, atomic writes up to PIPE_BUF,
    EOF when writers are gone, EPIPE + SIGPIPE when readers are gone.
    FIFO nodes (mknod S_IFIFO on any file system) get a pipe on first open,
    looked up by inode in a global table of weak references (dropped with
    the last open end). Opening blocks for the other side unless O_NONBLOCK
    (a nonblocking writer with no reader gets ENXIO); O_RDWR never blocks.
- Everything runs as root: no permission checks except the execute bit
  (execve, access X_OK). chown/chmod are stored; MS_RDONLY is recorded only.

## Block devices and disk file systems (C2)

- **PCI** (`drivers/pci.rs`): configuration space through ports
  0xCF8/0xCFC, brute-force scan of all buses at boot, BAR sizing, capability
  lists. Devices keep the BARs the firmware assigned.
- **virtio** (`drivers/virtio/`): modern transport (vendor capabilities:
  common/notify/ISR/device config in MMIO BARs, mapped with `map_mmio`)
  with a fallback to the legacy I/O-port transport; split virtqueues in
  physically contiguous frames (legacy 4 KiB layout, which modern devices
  accept too). Requests are polled: VRING_AVAIL_F_NO_INTERRUPT, then the
  caller spins on the used ring (no IOAPIC/MSI-X yet; QEMU completes
  requests on its own threads). virtio-blk: one queue, one request in
  flight under the device lock, data through a 128 KiB bounce buffer (so
  any kernel buffer works), negotiates RO, BLK_SIZE, FLUSH, SEG_MAX,
  SIZE_MAX, VERSION_1. Disks are vda, vdb, ... in PCI order.
- **Block layer** (`block/mod.rs`): a `BlockDev` = name, device number,
  BLKROSET flag, open/holder counts and a `Backend` (size, sector-aligned
  read/write, flush, read-only, device ioctls). Backends: virtio-blk,
  GPT partition (an offset into the disk's backend), loop, dm. Stacked
  devices call the lower *backend*, not its cache, so every layer caches
  its own view (as Linux's per-bdev page cache). `register` creates the
  devtmpfs node (devtmpfs and sysfs are created on first use, so drivers
  can register before user space mounts them), by-name links and the sysfs
  directory; `open_blk` gives a `BlockFile` (byte-granular read/write via
  the cache, block ioctls, device ioctls passed to the backend).
  `resolve` turns a path or "major:minor" into a device (mount sources, dm
  tables). Holders (mounted file systems via a `Holder` guard, dm tables
  via their targets) and opens make dm/loop teardown return EBUSY or, for
  AUTOCLEAR loops, defer it to the last release.
- **Cache** (`block/cache.rs`): 4 KiB pages of the device in frames from
  the frame allocator (the heap is too small for disk data), write-through
  (partial sectors read-modify-written), so pages are never dirty and
  uncached reads stay coherent. At most 8192 pages per device (fewer under
  memory pressure); when full the least recently used quarter goes.
  Metadata reads go through it; file data headed for a file's own page
  cache uses `read_direct` (whole aligned blocks, no double caching).
- **GPT** (`block/gpt.rs`): primary header and entry array (no CRC
  checks), partition number = slot + 1, UTF-16 names become PARTNAME and
  `/dev/block/by-name/` links (first disk wins on duplicates).
- **RAM disks** (`block/brd.rs`, Linux's brd): `ram0..N-1`, major 1, from
  `brd.rd_nr=N brd.rd_size=KiB`; sparse pages allocated on first write.
  The AOSP boot only uses them as the block devices of its tmpfs
  /metadata and /data fstab entries.
- **Loop** (`block/loop_dev.rs`) reads and writes its backing `OpenFile`
  (any file system or another block device) at the offset; **device
  mapper** (`block/dm.rs`) keeps a live and an inactive table per device,
  maps I/O by walking the live table, and implements the dm ioctl buffer
  protocol (header + data area, BUFFER_FULL). Resuming a table links
  `slaves/<lower>` under the dm device and `holders/dm-N` under each
  lower device in sysfs (D3b; removed with the table or the device), as
  apexd needs to map /apex mounts back to loop devices. Details of both in
  [syscalls.md](syscalls.md#block-devices-c2).
- **erofs** (`fs/erofs.rs`): superblock, compact/extended inodes (nid =
  inode number, st_dev = the block device), FLAT_PLAIN and tail-packed
  FLAT_INLINE data, directories (sorted dirent blocks), symlinks, device
  nodes, inline and shared xattrs, and LZ4 files with full or compact
  (4B/2B) lcluster indexes following Linux's zmap.c: an offset maps to its
  extent (lookback through NONHEAD lclusters), whose single-block pcluster
  is decompressed (`fs/lz4.rs`, partial decoding; 0-padded or not) or, for
  plain pclusters, copied (interlaced or shifted). The 32 most recently
  decompressed pclusters (of all inodes) are kept for sequential reads; a
  buffer per inode used to fill the 64 MiB kernel heap once zygote read
  hundreds of files. That covers AOSP's
  system/vendor/product images (lz4hc, 4 KiB pclusters, compact indexes).
- **ext4** (`fs/ext4.rs`, read-only): group descriptors (32/64 bytes),
  inode table lookup, extent trees and indirect maps (holes and
  uninitialised extents read as zeros), linear scan of directory blocks
  (covers htree: index blocks look like empty entries), fast/slow
  symlinks, inline data, xattrs (in-inode, block, EA inodes). No journal
  replay, no writes yet.
- Both file systems keep an inode cache (weak references by inode
  number), give each regular file a read-only page cache (`Inode::shmem`,
  so mmap and exec work) and refuse namespace changes with EROFS; mounts
  are forced read-only.

## eBPF (`bpf/`, `syscall/bpf.rs`, C3c)

What AOSP's loaders (netbpfload with libbpf, the platform bpfloader in
Rust over libbpf-rs, uprobestatsbpfload) and their users (netd's
libnetd_updatable, libbpf_android) need, without running any program:

- **Objects**: `BpfMap`, `BpfProg`, `Btf`, `BpfLink`, each with an id from
  its own registry (weak references: an object lives while a file, a
  bpffs node, or for maps a program using them refers to it). Files are
  `anon_inode:bpf-map` / `bpf-prog` / `btf` / `bpf_link` on the shared
  anonymous inode (netd's OFD locks rely on that), always close-on-exec;
  BPF_F_RDONLY / WRONLY become the file's access mode.
- **Maps** (`bpf/map.rs`): arrays live in a shmem object (elements of
  `round_up(value_size, 8)` bytes, per-CPU arrays one such value per
  possible CPU), so BPF_F_MMAPABLE arrays mmap through
  `FileOps::mmap_shmem` and share pages with lookup/update. Hash-like
  types (hash, per-CPU and LRU variants, map-in-map, devmap, sockmap,
  local storage, ...) are ordered maps of keys (GET_NEXT_KEY walks them in
  key order; an unknown key restarts at the first), LRU maps evict the
  least recently used key when full, LPM tries store (prefix length,
  masked prefix) and look up the longest match. fd arrays (prog array,
  perf event array, ...) are sparse arrays. Ring buffers are a shmem
  object laid out like Linux's mapping (consumer page, producer page,
  data) that stays empty; their fds never poll readable.
- **Programs**: BPF_PROG_LOAD checks the type, length, license (for the
  GPL bit) and the maps referenced by ld_imm64 (BPF_PSEUDO_MAP_FD /
  MAP_VALUE: EBADF for a bad fd) and keeps the instructions. There is no
  verifier, JIT or interpreter: OBJ_GET_INFO_BY_FD reports the
  instructions as both the translated and the "JITed" image (netbpfload
  insists on a non-zero JIT length), the tag is a hash over them with map
  fds cleared (not Linux's SHA-1), the verifier log is empty.
- **Attaching**: BPF_PROG_ATTACH/DETACH record (target inode, attach
  type) -> programs (ALLOW_MULTI keeps several, otherwise one is
  replaced), BPF_PROG_QUERY reports them (netd aborts unless its cgroup
  programs show up); LINK_CREATE and RAW_TRACEPOINT_OPEN give link fds.
  Nothing runs: socket filters, cgroup skb/sock hooks, tracepoints and
  uprobes never fire.
- **BTF**: BPF_BTF_LOAD checks the header and section bounds and keeps
  the blob (libbpf's feature probes and netbpfload require a BTF fd).
  There is no kernel BTF (`/sys/kernel/btf/vmlinux`), so CO-RE objects
  (timeInState.bpf) fail to load; nothing critical needs them.
- **bpffs**: a tmpfs instance whose nodes can hold an object
  (`TmpInode::add_object`); BPF_OBJ_PIN works only in a bpffs (EPERM
  elsewhere, EEXIST for an existing name), the node is a regular file
  (mode 0600 & ~umask) that chmod, chown, rename and unlink handle like
  any tmpfs file; open(2) of it is EIO. BPF_OBJ_GET looks the path up
  (BPF_F_PATH_FD supported) and opens the object.
- **OFD locks** (`fs/lock.rs`): F_OFD_SETLK/SETLKW/GETLK byte-range locks
  per inode, owned by the open file description and dropped with it
  (libbpf_android's `bpfLock` locks byte `map id` of the shared bpf
  inode). Classic F_SETLK locks are still always granted.

## Readiness and event files (`fs/poll.rs`, `fs/epoll.rs`, ...)

- **Wait queues** (`poll::WaitQueue`, always in an `Arc`): a unique sleep
  key plus a list of `Waiter` callbacks. `wake(events)` wakes tasks
  sleeping on the key (`sched::wait_until(queue.key(), ...)`: blocked
  reads/writes on the object itself) and then calls each callback outside
  the queue lock with a hint of what changed (0 = unknown). Callbacks can
  run in interrupt context (timerfd expiry), so the queue and everything a
  callback touches use `IrqLock`. Pipes, the console, eventfd, timerfd,
  epoll and the per-process `signalfd_wq` (woken by every queued signal)
  have one; files without one (regular files, memory devices) are always
  ready and refused by epoll_ctl with EPERM.
- **poll/select** (`poll::do_poll`): a `Poller` registers one callback that
  wakes its own sleep key on every watched queue, then sleeps in
  `wait_until` re-scanning all files' `poll` after each wake-up (register
  then re-check, so no wake-up is lost). select is poll with the fd_set
  bits mapped to POLLIN/POLLOUT/POLLPRI sets. ppoll/pselect6/epoll_pwait
  install a temporary signal mask through `ThreadSignals::saved_mask` (the
  rt_sigsuspend mechanism): on EINTR the handler frame gets the old mask,
  on any other return it is restored at once.
- **epoll**: items `(fd, file address) -> {Weak<OpenFile>, events, data}`,
  a ready list, and an `ItemWaiter` callback on each file's queue that
  queues the item (when its events match the hint) and wakes the epoll's
  queue. `epoll_wait` drains the ready list, polls each file without the
  epoll lock, reports up to maxevents, re-queues level-triggered items,
  leaves EPOLLET items off until the next wake-up and disables EPOLLONESHOT
  items. ADD/MOD queue an item that is already ready. Items hold files
  weakly, so closing the last reference removes them (lazily: dead items
  are purged on the next ctl/wait). The epoll fd polls readable when a
  queued item has events; nesting is allowed up to 4 levels, loops are
  ELOOP.
- **eventfd**: a counter under a mutex with blocking/EAGAIN reads and
  writes. **timerfd**: deadline on the monotonic clock (realtime absolute
  times converted with the boot epoch), interval and unread expirations
  are computed from the clock on every read/poll; a kernel timer per
  expiry wakes the queue. **signalfd**: a mask; reads dequeue the reading
  thread's pending signals in the mask (`signal::dequeue`).

## Sockets (`net/`)

- AF_NETLINK (`net/netlink.rs`): a socket per open, with a port id,
  multicast groups and a message queue; a global list of live sockets.
  The kernel's uevents are queued on every NETLINK_KOBJECT_UEVENT socket
  in group 1 (see [syscalls.md](syscalls.md#netlink-c3a)).
- AF_INET / AF_INET6: see [Network](#network-netinet-c3d) below.
- AF_UNIX (`net/unix.rs`); other families get EAFNOSUPPORT. A
  socket file is an `OpenFile` whose ops are `SocketFile(Arc<UnixSocket>)`
  and whose inode is a fresh anonymous `S_IFSOCK | 0777` inode (fstat,
  `socket:[ino]`). Dropping the `SocketFile` (last close) *releases* the
  socket. Syscalls find the socket by downcasting the ops (ENOTSOCK
  otherwise); read/write/readv/writev go through the same send/recv.
- `UnixSocket`: type, wait queue, and under one lock: state
  (unconnected / listening / connected), bound address, peer
  (`Arc`, cleared on release so pairs do not leak), peer credentials,
  pending connections (listener), receive queue of `Message`s (data,
  `Arc<OpenFile>`s from SCM_RIGHTS, sender credentials and address),
  shutdown bits, pending error, options (passcred, buffers, timeouts).
- Sending appends to the *receiver's* queue and sleeps on the receiver's
  wait queue while it is full; receiving sleeps on its own and afterwards
  wakes its own queue (blocked senders) and the peer's (POLLOUT). Streams
  are a queue of chunks with an offset into the first; dgram/seqpacket
  messages are taken whole. Locks: one socket at a time (a listener's
  lock is held around the still-private embryo's); name tables are taken
  under a socket lock, never the reverse; queued files are dropped only
  after unlocking, since closing one may release another socket.
- Connecting creates the server-side socket immediately (Linux's
  embryo), links it with the client, copies credentials (client gets the
  listener's from listen(), the embryo gets the client's) and queues it
  on the listener; accept pops it. Releasing a listener releases its
  embryos (clients see a hang-up).
- Names: abstract names in a global `BTreeMap` of weak references; path
  binds create an S_IFSOCK inode through the VFS and register the inode
  (by address; the socket keeps the inode alive) in a second table that
  connect/sendto consult after a normal path walk. Autobind picks
  `\0xxxxx`. Release removes both entries; the path stays in the file
  system (connect then gives ECONNREFUSED), as on Linux.
- Release: shuts both directions, gives a connected peer SHUTDOWN_MASK
  (POLLHUP, EOF, EPIPE) and ECONNRESET if data was left unread, and
  closes the files of unreceived SCM_RIGHTS messages. Reference cycles
  through in-flight fds are not collected.
- `/proc/net/unix` is generated from a table of all live sockets.

## Network (`net/inet/`, C3d, D2)

- **Stack**: smoltcp 0.12 (no_std + alloc; TCP, UDP, ICMP, IPv4, IPv6)
  with one `Interface` of `Medium::Ip` holding every link's addresses,
  and one `SocketSet`. All of it, plus the links, routes, policy rules
  and the socket bookkeeping, lives in `inet::NET` (a spin mutex).
  smoltcp's own source address choice (the first IPv4 address for
  everything) is bypassed for TCP/UDP: `Net::source_for` picks 127.0.0.1
  for loopback, the destination for local addresses, else the address of
  the link the destination (or the default gateway) is on; for what
  smoltcp sends on its own (ICMP) the cards' IPv4 addresses come before
  127.0.0.1. The interface MTU is 64 KiB (lo's): loopback carries 64 KiB
  segments and datagrams.
- **Device** (`inet::Dev`, below smoltcp): an outgoing IP packet to a
  local address (127/8, ::1, any link's address) is queued straight back
  as received: that is `lo`. IPv4 to anywhere else goes out on the card
  whose subnet holds the destination, else on the default route's card
  to its gateway, in an Ethernet frame, fragmented to the card's 1500
  bytes when bigger (datagrams; TCP keeps to the peer's MSS); the next
  hop's MAC comes from a per-card ARP cache (requests sent at most once a
  second, packets wait in a small queue until the reply). The device
  answers ARP requests for its addresses and learns from them. When a
  card's transmit ring is full the device refuses smoltcp's transmit
  token (backpressure: the data stays queued in the socket, no drop and
  retransmission timeout).
- **IPv6 off the machine** (`inet/ndisc.rs`): each card gets fe80::/64 +
  EUI-64 when probed (with the fe80::/64 route) and sends a router
  solicitation once configured. Neighbour solicitations for our
  addresses are answered, advertisements fill a per-card neighbour cache
  (packets wait for it as for ARP); IPv6 goes out on the card whose /64
  holds the destination (link-local: the first card), else to the
  default router. A router advertisement adds prefix + EUI-64 for each
  autonomous /64, the prefix route, and the sender as default router
  (`::/0 via fe80::2 proto ra`, mirrored into smoltcp); QEMU's user
  network gives fec0::/64. No DAD, no fragments, no MLD.
- **Links**: `lo` (index 1) and one `ethN` per virtio-net card in PCI
  order; flags, MTU, name and addresses are kernel state changed by
  rtnetlink (`net/rtnetlink.rs`) and the ifreq ioctls (`net/ifreq.rs`),
  then mirrored into smoltcp (`Net::sync`). After probing, the last card
  gets QEMU user networking's static configuration (10.0.2.15/24,
  default route via 10.0.2.2), like Linux's `ip=`. Routes and fib rules
  are stored and dumped; forwarding uses only the main table's default
  route and the links' subnets.
- **Running the stack**: every socket operation that may produce packets
  polls the stack (`with_net`), looping while loopback packets are
  queued. Otherwise a soft interrupt: the BSP's timer tick
  (`inet::tick`) sets a flag when a card's receive ring moved (read
  lock-free from the used index) or smoltcp's next timer is due, and the
  next return to user mode or idle loop on any CPU polls
  (`inet::softirq`, try-lock). After a poll that changed socket state
  every inet socket's wait queue is woken with unknown events (the
  callers may hold a socket's lock, so masks are not computed there).
- **virtio-net** (`drivers/virtio/net.rs`): queues 0 (receive) and 1
  (transmit), 256 posted 2 KiB receive buffers (virtio header + frame),
  128 transmit bounce buffers reclaimed lazily (`can_send` tells the
  device whether one is free), no offloads; modern and legacy
  transports. `Virtqueue` gained non-blocking `add`/`pop_used` next to
  virtio-blk's `submit_and_wait`.
- **Sockets** (`inet/socket.rs`, `InetSocket` behind `InetFile`): TCP
  connect makes one smoltcp socket; listen keeps a pool of eight smoltcp
  sockets listening on the endpoint (smoltcp accepts one connection per
  socket). `Net::poll` lets smoltcp take at most eight received packets
  per `Interface::poll` and tops the pools up in between (up to the
  backlog), so a burst of SYNs never finds the pool empty and gets
  refused; accept takes an established one out of the pool (its state
  counts toward POLLIN). Closed connections stay in the set as orphans
  until their FIN handshake ends (at most 60 s); an orphan that receives
  data is reset, and so is a connection closed with unread data or with
  SO_LINGER {1, 0}, as on Linux. A connection that goes to Closed
  without our close (reset or timeout) reports ECONNRESET once (recv,
  send or SO_ERROR, with POLLERR), then EOF / EPIPE; POLLRDHUP comes with
  the peer's FIN even while data is left. UDP gets its smoltcp socket
  when it gets a port. ICMP "ping" sockets (SOCK_DGRAM + IPPROTO_ICMP /
  IPPROTO_ICMPV6) get a smoltcp ICMP socket bound to their port as echo
  identifier: only echo requests may be sent (the identifier is
  rewritten), only replies are received. AF_INET6 sockets are dual-stack
  (v4-mapped peers). Raw sockets only hold options. Ports are reserved in `Tables`
  (EADDRINUSE unless both sides set SO_REUSEADDR and neither listens;
  ephemeral ports 32768-60999). Lock order: a socket's state, then
  `NET`; nothing sleeps holding either.
- **Netfilter** (`inet/netfilter.rs`): x_tables tables per family and
  name, created on first use with the built-in chains' ACCEPT policies;
  IPT_SO_SET_REPLACE stores the blob (old counters copied back as
  zeros), GET_INFO/GET_ENTRIES return it, every match/target revision
  exists. Nothing is enforced.
- **rtnetlink**: dumps of links (with stats), addresses, routes (plus
  the local table), rules and neighbours (empty); changes of addresses,
  routes, links (name, MTU, flags; renaming an up link is EBUSY) and
  rules, multicast to the RTNLGRP_LINK / *_IFADDR / *_ROUTE groups.
- **ethtool** (`net/ifreq.rs`, SIOCETHTOOL): GDRVINFO (driver
  `virtio_net`, bus_info = the card's PCI address), GLINK, GSET (speed
  and duplex unknown, like Linux's virtio_net), GPERMADDR; `lo` only
  answers GLINK, everything else is EOPNOTSUPP.
- **/proc/net**: tcp, tcp6, udp, udp6, unix, netlink, arp, dev, route,
  ipv6_route, if_inet6, the iptables names, and the statistics files
  netd, ConnectivityService and tools read (snmp, snmp6, netstat,
  sockstat, sockstat6, protocols, raw, raw6, icmp, icmp6, igmp, igmp6,
  dev_mcast, packet; counters mostly zero).

## Real-hardware-class controllers (`drivers/nvme.rs`, `drivers/ahci.rs`, `drivers/usb/`, E1)

Drivers for controllers found on PCs rather than in VMs, developed against
QEMU's emulations (`-device nvme`, `ahci` + `ide-hd`, `qemu-xhci` +
`usb-kbd`/`usb-mouse`). All are polled like virtio (no IOAPIC/MSI-X yet),
use `pci::dma_alloc` (zeroed contiguous frames through the direct map)
for rings and bounce buffers, and register PCI sysfs directories with
`pci::sysfs_dir`.

- **NVMe** (`drivers/nvme.rs`, class 01.08.02): disable, admin queues
  (64 entries) in AQA/ASQ/ACQ, enable (timeout from CAP.TO), IDENTIFY
  controller (MDTS caps the transfer at min(128 KiB, 2^MDTS pages), NN),
  create I/O completion + submission queue 1, then IDENTIFY each active
  namespace (CNS 2 list on NVMe >= 1.1). Namespaces with 512-byte LBAs
  become disks `nvme<C>n<N>` on blkext (259:n, partitions `nvme0n1p1`...
  also 259:n, as on Linux) below `.../0000:00:04.0/nvme/nvme0/block/`.
  One command in flight per controller: the entry goes in the submission
  queue, the tail doorbell (CAP.DSTRD stride) is rung, the completion is
  found by its phase bit, the head doorbell acknowledges it; data through
  a 128 KiB bounce buffer described by PRP1/PRP2 or a PRP list page.
  Read, write, flush.
- **AHCI** (`drivers/ahci.rs`, class 01.06.01): GHC.AE; for every
  implemented port with a device (SSTS.DET = 3) and the ATA signature
  (QEMU's `-cdrom` ATAPI drive on the q35 ICH9 controller is skipped):
  stop, command list / received FIS / one command table in one page,
  start; IDENTIFY DEVICE (LBA48 size, model, serial); READ/WRITE DMA EXT
  (one PRD, 128 KiB bounce buffer) and FLUSH CACHE EXT in slot 0, polled
  on PxCI, errors (PxIS.TFES, TFD.ERR) restart the port and give EIO.
  Disks are sda, sdb, ... (8:16n, partitions +1..15), with libata's
  sysfs path (`.../ata1/host0/target0:0:0/0:0:0:0/block/sda`).
- **xHCI** (`drivers/usb/mod.rs`, class 0c.03.30): BIOS hand-off (legacy
  support capability), halt, reset, DCBAA (+ scratchpad buffers),
  command ring and one event ring segment of 256 TRBs (link TRB with
  toggle cycle), run. Connected root ports are reset (USB2) and
  enumerated: Enable Slot, Address Device (EP0 max packet by speed,
  Evaluate Context when bMaxPacketSize0 differs), device/config/string
  descriptors, SET_CONFIGURATION; each HID interface of the boot subclass
  (protocol 1 keyboard, 2 mouse) gets SET_PROTOCOL(boot), SET_IDLE,
  Configure Endpoint for its interrupt IN endpoint (Interval from
  bInterval and speed) and 16 Normal TRBs kept posted. A stalled control
  request is recovered with Reset Endpoint + Set TR Dequeue Pointer (a
  timed-out one after Stop Endpoint). Since E3: root ports are powered
  when HCCPARAMS1.PPC says software controls power, USB3 links get up to
  1 s to leave Polling, the slot context carries the route string, hub
  flag/port count/TT think time and, for low/full-speed devices behind a
  high-speed hub, the TT hub slot and port; up to 8 controllers. No
  hot-plug, no LED output reports.
- **Hubs** (`drivers/usb/hub.rs`, E3): hub descriptor (0x29, SuperSpeed
  0x2a), Configure Endpoint marking the slot as a hub, SET_HUB_DEPTH on
  SuperSpeed hubs, PORT_POWER on every port, the power-on delay plus
  100 ms, then per connected port: PORT_RESET until C_PORT_RESET (a
  SuperSpeed port only if its link does not enable itself), change bits
  cleared, speed from the port status, and the device enumerated with the
  route string extended by the port (`1-2.3` in sysfs names). Five tiers
  at most.
- **Mass storage** (`drivers/usb/storage.rs`, E3): interfaces of class
  8/6/0x50 (SCSI over Bulk-Only; UAS-only devices are logged and left
  alone). Both bulk endpoints are configured (SuperSpeed max burst from
  the companion descriptor). A command is the 31-byte CBW, the data stage
  through a 120 KiB bounce buffer (240 sectors, Linux's usb-storage
  default; Normal TRBs chained at 64 KiB boundaries, TD Size filled in,
  the event matched by TRB address so a short packet's extra event is
  harmless) and the 13-byte CSW (tag and signature checked). Stalls are
  cleared on both sides (Reset Endpoint/Set TR Dequeue,
  CLEAR_FEATURE(ENDPOINT_HALT)); transport errors, timeouts and phase
  errors do Bulk-Only reset recovery; CHECK CONDITION fetches REQUEST
  SENSE. Probe: INQUIRY, TEST UNIT READY for up to 10 s, READ
  CAPACITY(10) or (16); 512-byte blocks only. I/O is READ/WRITE(10), or
  (16) past 2^32 sectors, three tries; SYNCHRONIZE CACHE on flush. The
  disk shares the sd names and 8:16n numbers with AHCI
  (`block::next_sd`), below
  `.../0000:00:07.0/usb1/1-1/1-1:1.0/host0/target0:0:0/0:0:0:0/block/sda`.
  Disk I/O runs on the caller with the controller locked, dispatching the
  HID events it meets.
- **HID boot protocol** (`drivers/usb/hid.rs`): keyboard reports are
  diffed against the previous one (modifier bits, six usages, rollover
  errors ignored) and mapped with Linux's usbkbd table; mouse reports
  give BTN_LEFT/RIGHT/MIDDLE, REL_X/REL_Y and REL_WHEEL. Devices are
  input devices named like Linux's ("QEMU QEMU USB Keyboard", bus 0x03,
  phys `usb-0000:00:06.0-5/input0`) below
  `.../usb1/1-5/1-5:1.0/0003:0627:0001.0001/input/inputN`. Like
  virtio-input, the BSP's timer tick sees the next event TRB's cycle bit
  flip and the soft interrupt (idle loop / return to user mode) decodes
  the reports and re-posts the TRBs.
- **Boot disk** (`block::resolve_boot_disk`, E3): with
  `androidboot.boot_devices=auto androidboot.partition_map=auto` the
  kernel picks, after the disk drivers, the disk with a GPT partition
  named `super` (preferring the one whose GPT disk GUID is the one Limine
  loaded the kernel from, `boot::boot_disk_guid`), and rewrites the two
  values to its controller's PCI prefix (what ueventd matches,
  `pci0000:00/0000:00:14.0`) and `sda2,super;sda3,frp;sda4,misc`; the
  command line and /proc/bootconfig show the result. All other hardware
  disks become read-only.
- **AOSP over NVMe**: `run-qemu.sh --nvme` attaches super, frp and misc as
  NVMe controllers at 00:04.0-00:06.0 (same `androidboot.boot_devices`)
  with `androidboot.partition_map=nvme0n1,super;nvme1n1,frp;nvme2n1,misc`;
  first-stage init finds `by-name/super` and maps the dynamic partitions
  as before.

## Real PCs (`arch/x86_64/acpi.rs`, `drivers/fbcon.rs`, E3)

What a PC without virtio devices or a serial port needs (see
docs/usb-boot.md for booting one from a USB stick):

- **ACPI tables** (`arch/x86_64/acpi.rs`): RSDP (physical in Limine's
  base revision 3, else through the direct map) -> XSDT/RSDT; MADT
  (enabled/online-capable local APIC and x2APIC ids, I/O APICs), FADT (PM
  timer as I/O port or GAS, 24/32 bits; reset register; PM1a/b control;
  DSDT, whose `\_S5_` package bytes give SLP_TYP for soft-off), HPET.
  Tables are mapped uncached on demand; checksum errors are only logged.
  `power_off` tries ACPI S5 first, `reboot` the FADT reset register.
- **Timer calibration** (`arch/x86_64/apic.rs`): the LAPIC timer and the
  TSC are measured over 20 ms of HPET time (enabled if the firmware left
  it off), else ACPI PM timer, else (as before) 10 ms of PIT channel 2,
  which Intel PCHs may gate off. When CPUID leaf 0x15 (crystal x ratio)
  or 0x16 states the TSC frequency and the measurement is more than 2%
  away, CPUID wins.
- **SMP**: APs still start through Limine's MP response (built from the
  MADT); the kernel compares it with the MADT and logs CPUs Limine did not
  report, and uses at most `MAX_CPUS` (64).
- **Framebuffer console** (`drivers/fbcon.rs`, `drivers/font.rs`): every
  console print (the log, /dev/kmsg lines, panics) also goes to a text
  grid in RAM drawn on the boot framebuffer with the 8x16 VGA font
  (scaled 2x/3x from 2560/3840 pixels wide), colored by log level.
  Scrolling moves the grid half a screen and redraws it, so the
  framebuffer is only written. It draws until simpledrm shows its first
  frame; then Pause/Break on a USB keyboard toggles the log over Android's
  picture (scanout held meanwhile, repainted after). A panic always draws.
- **Write-combining**: PAT is programmed as Linux does (entry 1 WC) on
  every CPU, and the boot framebuffer's direct-map pages use it, since
  real video memory is uncached by the MTRRs.
- **Serial**: the console UART is used only if its scratch register
  works; `serial=off` stops the log there (a 16550 with nothing attached
  paces the log at 115200 baud). `loglevel=N` sets the log level.

## Input (`drivers/input/`, `drivers/virtio/input.rs`, D2)

- **virtio-input** (`drivers/virtio/input.rs`): virtio device type 18
  (QEMU's virtio-keyboard/-mouse/-tablet/-multitouch-pci, Cuttlefish's
  devices). The configuration space gives the name, serial, ids,
  property bits, the codes of every event type and the absolute axis
  ranges, which become an input device. Queue 0 is kept full of 8-byte
  event buffers (posted in order, so buffer i is descriptor i); the BSP's
  timer tick notices the used ring move and the next return to user mode
  or idle loop hands the events to the input core (the same soft
  interrupt pattern as virtio-net). Queue 1 (status) takes LED events
  written to the device's node.
- **Input core** (`drivers/input/mod.rs`): `register(caps, parent,
  sink)` makes `<parent>/input/inputN` in sysfs (class input; uevent
  with PRODUCT, NAME, PHYS, UNIQ, PROP, EV, KEY, ..., MODALIAS; name,
  phys, uniq, properties, modalias, id/*, capabilities/*) and its child
  `eventN` (MAJOR=13, MINOR=64+N, DEVNAME=input/eventN), whose "add"
  uevents make ueventd create /dev/input/eventN; devtmpfs gets the node
  too. Events are filtered against the device state as on Linux (a key
  already down, an unchanged axis or switch, a zero relative motion are
  dropped; keys, LEDs, sounds, switches, axes and MT slots are tracked
  for the EVIOCG* state ioctls), collected into a packet and delivered
  to the clients at SYN_REPORT (an empty packet is dropped).
- **evdev** (13:64+N): every open is a client with a queue of 1024
  events (on overflow the queue becomes SYN_DROPPED), a clock
  (EVIOCSCLOCKID: realtime, monotonic, boottime; events are stamped
  with monotonic time and converted on read), an event mask
  (EVIOCGMASK/EVIOCSMASK, per type and per code) and a grab
  (EVIOCGRAB: only the grabbing client gets events). Reads return whole
  24-byte input_events (EINVAL for a smaller buffer, EAGAIN, blocking,
  ENODEV once the device is gone); poll/epoll through the client's wait
  queue. Ioctls: EVIOCGVERSION/GID/GREP/SREP/GNAME/GPHYS/GUNIQ/GPROP/
  GMTSLOTS/GKEY/GLED/GSND/GSW/GBIT/GABS/SABS/RMFF/GEFFECTS/GRAB/REVOKE/
  GMASK/SMASK/SCLOCKID (EVIOCSFF: ENOSYS). Writes inject events
  (`sendevent`): delivered like the device's, and LED/sound/repeat
  events also go to the driver (virtio status queue, uinput reader).
- **uinput** (`drivers/input/uinput.rs`, 10:223): UI_SET_*BIT,
  UI_DEV_SETUP, UI_ABS_SETUP or the legacy `uinput_user_dev` write, then
  UI_DEV_CREATE registers a device under devices/virtual; events written
  to the file are its events, events written to its node (LEDs) can be
  read back; UI_GET_SYSNAME, UI_GET_VERSION (5), UI_DEV_DESTROY or close
  remove it.
- **run-qemu.sh `--input`** (default for `--ktest`, `--aosp-gfx-test`,
  `--aosp-input-test`): virtio-keyboard-pci 00:10.0, virtio-tablet-pci
  00:11.0, virtio-multitouch-pci 00:12.0, modern only. The QEMU
  monitor's `sendkey` and `mouse_button` go to them once the driver is
  up (HMP's `mouse_move` is relative and goes to the PS/2 mouse).

## Binder (`drivers/binder/`)

ABI-compatible with Linux binder, protocol 8, 64-bit (the user-visible
behaviour is listed in [syscalls.md](syscalls.md#binder-c1)). The upstream
C and Rust drivers were semantic references; the code is new.

- **Files**: `abi.rs` (ioctl/BC/BR numbers, object types, struct offsets;
  structures are parsed as little-endian byte arrays), `state.rs` (the
  object model and its rules), `protocol.rs` (BINDER_WRITE_READ: commands,
  transactions, object translation, the read loop), `mod.rs` (device
  registry, file operations, ioctls, mmap, binderfs, `binder-control`).
- **Devices and binderfs**: a device = a context (by minor) with its own
  context manager node. `DEVICES` names the minors (binder, hwbinder,
  vndbinder, then BINDER_CTL_ADD additions). binderfs is a tmpfs root
  holding char nodes 244:minor, so chmod/unlink work as on any tmpfs;
  `dev::open_chr` routes major 244 to `binder::open`, which creates a
  binder process (or the control file for minor 1023).
- **One table, one lock**: every process, thread, node, ref, death
  notification and transaction lives in `state::State` behind one spin
  lock, referring to each other by id. A queued work item whose object is
  gone is skipped when dequeued. Two rules keep it deadlock-free: no user
  memory is touched under the lock (commands, transaction data and
  scatter-gather buffers are copied in before taking it; the read loop
  builds the BR stream in a kernel buffer copied out afterwards), so the
  address-space lock is never taken inside it; and open files are never
  dropped under it (the last close of a binder file takes it again):
  they go to the guard's "grave" and are dropped, or closed for fd-array
  descriptors, after unlocking.
- **Processes and threads**: `Proc` per open (pid, context, threads by
  tid, nodes by user pointer, refs by descriptor, todo list, wait queue,
  buffer mapping, thread-pool counters). `Thread` per calling task, made on
  first use: looper state, todo list, transaction stack top, extended
  error. Threads sleep on their own key; queuing process work wakes one
  idle looper (which then stops counting as idle, so the next item wakes
  another) and the process's wait queue (poll/epoll).
- **Buffers**: mmap creates a shmem object (at most 4 MiB used) mapped
  shared and never writable, so the PTEs keep pointing at the object's
  frames; transactions are written into it with `Shmem::write`. A best-fit
  allocator (`Mapping`) hands out ranges; each `Buffer` records the
  references it holds (`Held`), the files still to install and the fd-array
  descriptors to close, and is freed by BC_FREE_BUFFER or with the process.
  The VMA keeps the binder file open, like on Linux.
- **Transactions**: `prepare` copies and validates (offsets, object types
  and sizes, PTR parents, FDA bounds) without the lock; `transact` picks
  the target (handle 0 = context manager; a synchronous call made while
  serving one goes to the thread of the target process already waiting in
  the chain, else to the process todo; a reply to the caller's thread),
  allocates, translates objects (taking refs with the counting rules
  below), writes the buffer and queues it. Oneway calls to one node are
  serialised: the next waits in `Node::async_todo` until the buffer of the
  current one is freed. Synchronous calls are pushed on both threads'
  stacks (the receiver's when it reads the call), replies pop them.
  Files are installed in the receiver at read time (it is the current
  process then), as O_CLOEXEC descriptors written into the buffer.
- **Reference counting** follows Linux: a node counts internal strong refs
  (refs of other processes), local strong/weak refs (kernel-held: in-flight
  buffers and unacknowledged BR_ACQUIRE/BR_INCREFS) and the set of
  processes holding a ref; what the owner has been told (`has_strong`,
  `has_weak`) changes through BR_INCREFS/ACQUIRE/RELEASE/DECREFS, queued
  on the sending thread when an object is first sent, on the owner's todo
  otherwise. A node is freed when nothing references it; a ref when both
  its counts are zero.
- **Release** (last close): calls the process's threads were serving get
  BR_DEAD_REPLY (`send_failed_reply` walks to a live caller), threads are
  detached from calls they made, its nodes become dead (death
  notifications are queued to every holder that asked; pending oneway
  calls are dropped), its refs and queued work are dropped (undelivered
  synchronous calls fail with BR_DEAD_REPLY) and all its buffers are freed.
  The context manager slot is cleared.
- **Death notifications**: a `Death` per ref that asked, with the Linux
  state machine (DEAD_BINDER -> BC_DEAD_BINDER_DONE, CLEAR before or after
  delivery -> BR_CLEAR_DEATH_NOTIFICATION_DONE, on the clearing thread if it
  is a looper). They are process work, so only loopers receive them.

## Graphics (`drivers/virtio/gpu.rs`, `drivers/drm/`, `drivers/dma_buf/`, D1)

What AOSP's cuttlefish graphics stack in `guest_swiftshader` mode needs
(minigbm's 2D virtio_gpu backend, the ranchu HWC3 composer on DRM,
SurfaceFlinger with ANGLE over SwiftShader): the user-visible behaviour
is in [syscalls.md](syscalls.md#graphics-d1).

- **virtio-gpu** (`drivers/virtio/gpu.rs`): 2D only (no virgl, blobs or
  cursor queue), F_EDID negotiated. One control queue, polled like
  virtio-blk, one command at a time from a 16-page request buffer (header
  + up to ~4000 backing entries; physically contiguous pages are merged
  into one entry) and a response page. GET_DISPLAY_INFO and GET_EDID at
  probe; RESOURCE_CREATE_2D + ATTACH_BACKING, SET_SCANOUT,
  TRANSFER_TO_HOST_2D + RESOURCE_FLUSH, DETACH_BACKING + UNREF for the
  DRM driver. The first device (PCI 00:02.0 from run-qemu.sh) is used.
- **Buffers** (`dma_buf::Buffer`): a shmem object whose pages are all
  allocated at creation (`Shmem::populate`, so the physical addresses
  can go to the device), the size, exporter name, a dma-buf name, one
  anonymous inode for every dma-buf file of it, a weak pointer to its
  open dma-buf file (re-exporting returns it, as Linux's dma_buf->file)
  and the DRM fake mmap offset. Dumb buffers, PRIME imports and dma-heap
  allocations are all Buffers, so any of them can be a framebuffer.
- **dma-buf files**: mmap maps the shmem object (`FileOps::mmap_shmem`
  now gets the page offset and returns the object's page index, which is
  how DRM resolves its fake offsets), lseek(SEEK_END) gives the size,
  `/proc/<pid>/fd` says `/dmabuf:NAME` (`FileOps::link_name`), fdinfo
  has size/count/exp_name/name. No caches or implicit fences: SYNC is
  argument checking, EXPORT_SYNC_FILE returns a signalled fence.
- **DRM** (`drivers/drm/mod.rs`): minors card0 (226:0) and renderD128
  (226:128), sysfs class `drm` below the virtio device plus devtmpfs
  nodes. A `DrmFile` per open: GEM handle table (handle -> `Arc<Buffer>`,
  one handle per buffer per file), client caps, the event queue (read
  returns whole `drm_event`s, poll POLLIN) and a wait queue. Ioctls take
  the caller's struct size from the command and zero-pad / truncate,
  like Linux's drm_ioctl. The first card0 opener is master
  (SET/DROP_MASTER bookkeeping only; KMS ioctls do not check it). Render
  nodes get EACCES for KMS ioctls except the dumb-buffer ones (minigbm
  allocates through renderD128). The emulated vblank counter runs at 60 Hz
  from the first query.
- **KMS** (`drivers/drm/kms.rs`): fixed objects (plane 31, CRTC 32,
  encoder 33, connector 34), properties 1-20 with Linux's names, flags
  and ranges, framebuffers and blobs with ids from 40 up, all under one
  lock with the committed `State` (plane fb/crtc/rects, CRTC active and
  mode blob, connector CRTC, DPMS). Every path — atomic, SETCRTC,
  SETPLANE, PAGE_FLIP, OBJ_SETPROPERTY, DIRTYFB, RMFB — builds a new
  State, checks it (plane fb <-> crtc, source rectangle inside the fb,
  the primary plane covering the CRTC unscaled, ALLOW_MODESET for
  modesets, formats) and applies it: the shown framebuffer gets a
  virtio-gpu resource on first use (width = pitch / 4, backed by the
  buffer's pages, destroyed with the framebuffer), SET_SCANOUT when the
  resource or rectangle changes, then TRANSFER_TO_HOST_2D + FLUSH of the
  rectangle; no framebuffer -> scanout disabled. Commits are synchronous:
  IN_FENCE_FD fences are waited for before the lock is taken,
  OUT_FENCE_PTR gets an already signalled sync file, flip events are
  queued at once. Closing a file removes its framebuffers and blobs (and
  turns the display off if one was shown).
- **sync_file** (`drivers/dma_buf/sync_file.rs`): `Fence` = (context,
  seqno, driver, timeline name, status, timestamp), signalled once; a
  sync file holds fences and polls POLLIN when all have signalled. Every
  signal wakes one global wait queue (poll/epoll re-check). sw_sync
  timelines (open of the misc node 10:250, placed at
  /sys/kernel/debug/sync/sw_sync) make fences for future values; closing
  a timeline fails its pending fences with -ENOENT.
- **dma-heap**: /dev/dma_heap/system (249:0, sysfs class `dma_heap`):
  DMA_HEAP_IOCTL_ALLOC returns a dma-buf of a new zeroed Buffer.
- **Boot framebuffer (simpledrm, E2)**: the realistic display path on
  real hardware without an open GPU driver (this machine's RTX 4070):
  the linear framebuffer the firmware set up (UEFI GOP, or VBE under
  BIOS), which Limine reports (framebuffer request; `boot::framebuffer`
  gives its physical address, size, pitch and channel positions; the
  memory-map entry is in the direct map). Limine picks the mode from
  `resolution:` in limine.conf (run-qemu.sh writes `--gpu-size` there
  for `--fb`; OVMF offers the EDID's preferred mode of QEMU's VGA and
  bochs-display, so 720x1280 works under UEFI; SeaBIOS's VGA BIOS has it
  only for bochs-display). When no virtio-gpu was probed,
  `drm::register_boot_framebuffer` creates the same DRM device on it
  (`kms::Output::Boot`; sysfs `devices/platform/simple-framebuffer.0`,
  GET_UNIQUE `simple-framebuffer.0`): one CRTC / plane / connector whose
  only mode is the framebuffer's size, dumb buffers, PRIME and the
  whole KMS core of D1. A commit that shows a framebuffer copies the
  plane's source rectangle into the boot framebuffer, converting from
  any of the 8 supported 32 bpp formats to the firmware's channel layout
  (page flip by copy; no double buffering in the hardware, so a flip can
  tear). The stores are non-temporal (`movnti` + `sfence`): the direct
  map is write-back, and streaming stores bypass the cache like a
  write-combining mapping would on real hardware. Driver name
  "simpledrm"; `simpledrm.name=virtio_gpu` (run-qemu.sh adds it for AOSP
  boots with `--fb`) lets AOSP's cuttlefish HALs use it unmodified:
  minigbm picks its backend by DRM driver name and has none for
  simpledrm, the ranchu composer refuses drivers without "virtio" in
  the name; both then only use generic DRM (dumb buffers, PRIME, atomic
  KMS), and the virtgpu ioctls answer as the 2D-only device anyway.

## Android devices of D3a (`drivers/ashmem.rs`, `drivers/hvc.rs`, `net/tun.rs`)

- **ashmem** (misc 10:126, uevent name `ashmem`; ueventd also makes
  /dev/ashmem<boot_id>, the node libcutils opens): one region per open
  file; SET_NAME / SET_SIZE until the first mmap, which creates a shmem
  object of that size (shared by every mapping and fork); SET_PROT_MASK
  only removes rights (a later shared writable mmap is EPERM); PIN/UNPIN
  never purge.
- **hvc1..hvc17** (229:N): Cuttlefish's virtio-console ports to its host
  daemons, without a host: tty ioctls work (raw mode), writes are
  discarded, reads wait forever (EAGAIN non-blocking). Enough for the
  oemlock, bluetooth, NFC, UWB, ... HALs to start and register.
- **/dev/net/tun** (10:200): TUNSETIFF (IFF_TUN, %d names) adds a link
  (ARPHRD_NONE, POINTOPOINT|NOARP) to the rtnetlink link list and sysfs
  for as long as the file is open; no traffic is routed to it, writes
  are dropped. system_server's Thread network service creates
  `thread-wpan` with it.
- **AF_VSOCK** (`syscall/net.rs`): stream sockets on top of AF_UNIX's
  abstract namespace (`\0vsock:PORT`), for the local CID (1, 3, ANY);
  the host CID 2 refuses connections. Cuttlefish's lights HAL listens on
  a vsock port.
- **AF_KEY**: a socket that can be created and closed (libbpf's
  synchronizeKernelRCU); no key management.
- **AF_PACKET** (`net/packet.rs`): frames a card receives are copied to
  the packet sockets bound to it (protocol or ETH_P_ALL, after their
  classic BPF filter: a small interpreter with Linux's SKF_NET_OFF /
  SKF_LL_OFF offsets) before the IP stack sees them; sendto puts a whole
  frame (SOCK_RAW) or a payload behind a built header (SOCK_DGRAM) on the
  card. NetworkStack's DHCP client gets eth0's address from QEMU's DHCP
  server this way after EthernetService took the card over.
- sysfs: class devices also appear as /sys/class/<class>/<name>
  (net interfaces had no /sys/class/net links before D3a).
- **/proc/config.gz**: Android 16's required options for a 6.12 kernel
  (`fs/kconfig.txt`, from kernel/configs/b/android-6.12), gzip with stored
  blocks (framework Debug.isVmapStack and VINTF read it).
- **Device-tree fstab** (`fs/sysfs.rs`): `dtfstab=` on the command line
  becomes /sys/firmware/devicetree/base/firmware/android/fstab/<name>/
  {dev,mnt_point,type,mnt_flags,fsmgr_flags,status}; with
  androidboot.android_dt_dir pointing there, first stage mounts from it
  and fs_mgr's default fstab (boot HAL: /misc; vold; mount_all) still has
  it after switch_root.
- bpffs nodes get the policy's genfscon labels (fs_bpf, fs_bpf_net_shared,
  ...) when nothing else labels them (ClatCoordinator checks them).

## Console tty (`fs/tty.rs`)

A subset of Linux's N_TTY line discipline on COM1. There is no IOAPIC
routing yet, so the BSP's timer tick (100 Hz) drains the UART receive FIFO
(at most 64 bytes per tick; QEMU holds back further input while the FIFO
is full, so nothing is lost) and runs each byte through the line
discipline under an `IrqLock`: ICRNL/IGNCR/INLCR/ISTRIP, canonical line
editing (VERASE, VKILL, VWERASE, VEOF = EOF on an empty line, VEOL/VEOL2),
echo (ECHO, ECHOE, ECHOK, ECHOKE, ECHONL, ECHOCTL) and ISIG. Readers sleep
on the tty's wait queue: canonical reads return at most one line,
non-canonical ones follow VMIN/VTIME; O_NONBLOCK gives EAGAIN. VINTR,
VQUIT and VSUSP send SIGINT/SIGQUIT/SIGTSTP to the foreground process
group (TIOCSPGRP, default 1); since the process table lock is not
interrupt-safe, the tick only records them and `deliver_signals` sends
them from process context, on the next return to user mode or in the idle
loop. Output applies OPOST/ONLCR and goes straight to the UART. Missing:
SIGTTIN/SIGTTOU, sessions and controlling ttys (TIOCSCTTY is a no-op),
IXON flow control, devpts.

## Tests

`#[test_case]` functions anywhere in the crate; `cargo test` boots a test
kernel which runs them after full boot (SMP included) and writes to
`isa-debug-exit` (0x10 -> QEMU status 33 = pass, 0x11 -> 35 = fail); a panic
fails the run; the script times out after 120 s. Current tests (32; E1 added NVMe PRP lists, ATA strings, xHCI endpoint math, HID keyboard and mouse reports): int3
returns, all CPUs online, per-CPU area, frame alloc/free/zero/contiguous, heap
Box/Vec/1 MiB, map/unmap a page, kernel text RX-only, timer ticks advance, TSC
clock advances, guarded kernel stack, user address space (mmap, copy in/out
across pages, fork copy, munmap, read-only EFAULT, no frame leak), frame
refcounts, COW fork (pages shared read-only, copy on write fault and on
`copy_out`, refcount-1 reuse, shared mapping stays shared, grandchild after
the middle generation exits, mprotect keeps shared pages read-only, no frame
leak), madvise DONTNEED and mremap move/shrink/grow/FIXED+DONTUNMAP (no
frame leak), shmem objects (initramfs-backed reads, partial-page truncate,
sparse growth, hole punch, seals, no frame leak), page-cache sharing (two
address spaces and read/write see the same frames, private COW from the
cache, equal futex keys, F_SEAL_WRITE EBUSY, truncate -> SIGBUS, fork,
mremap old_size 0 and growth past the object, DONTNEED keeps shared data,
mincore/msync, no frame leak), mprotect EACCES on a read-only shared
mapping, AF_UNIX address parsing and socket queues (stream coalescing
and peeking, message boundaries, hang-up on release), VFS
(cpio unpack into tmpfs, path walk with `..` and symlinks, mounting a
tmpfs and walking back out of it, rename/unlink keeping dentries in sync,
umount), the binder buffer allocator (best fit, coalescing), LZ4
decoding (literals, overlapping matches, partial output, bad offsets),
GPT parsing and partition I/O through the block cache on a RAM disk.

User-space conformance tests (`tests/`, see `tests/README.md`) run in the
guest with `tools/run-qemu.sh --ktest [FILTER]`. Phase B2 state: all B1
and B2 tests pass with the same case counts as on the Linux host (b1_hello
22, b1_time 24, b1_fork_wait 24, b1_execve 7, b1_signals 36, b1_threads 16,
b1_tls 14, b2_files 92, b2_pipe 27, b2_getdents 13, b2_devproc 26); the
B3/B4 tests fail cleanly (missing syscalls) without taking the kernel down.
B2 was checked with 1, 4 and 8 CPUs, debug and release. Phase B3: b3_mmap
40/40, b3_fork_cow 11/11, b3_memfd 35/35 and b3_shmem 35/35 (same counts as
the Linux host). Phase B4a: b4_epoll 43/43, b4_eventfd 19/19, b4_timerfd
18/18, b4_signalfd 16/16 and b4_poll 32/32 (select/pselect6, ppoll's
mask, FIFOs, epoll EPERM/ELOOP, anon_inode names and fdinfo), same counts
as the Linux host, with 1, 4 and 8 CPUs. Phase B4b: b4_unix 64/64 (socketpair of
all three types, SCM_RIGHTS across processes, MSG_CTRUNC, path and
abstract listeners, SO_PEERCRED, SCM_CREDENTIALS, shutdown/EPIPE), same as
the Linux host, with 1, 4 and 8 CPUs; the whole suite still passes.
Phase B5 (AOSP shell, [aosp-shell.md](aosp-shell.md)):
`tools/run-qemu.sh --aosp-sh-test` boots unmodified AOSP mksh and toybox
(dynamically linked against bionic) and passes 15/15 checks, with 1, 4 and
8 CPUs, debug and release; ktest and `--test` still pass.
Phase C1 (binder): c1_binder 54/54 (binderfs and BINDER_CTL_ADD, mmap
rules, separate contexts, a context manager child reached through handle
0 with BR_TRANSACTION_SEC_CTX, sync and ordered oneway calls, sender pid,
a server's object passed through the context manager to the client, the
client's object called back while it waits (nested call) and returned to
it as a binder, fds both ways, scatter-gather buffers with an fd array,
BR_SPAWN_LOOPER and two calls served by two looper threads, death
notification on the server's exit, BR_DEAD_REPLY, BR_FAILED_REPLY after
releasing a handle, extended errors), with 1, 2, 4 and 8 CPUs and release;
it is skipped on the Linux host. One in-kernel test covers the buffer
allocator (25 tests). The whole suite, `--test` and `--aosp-sh-test` pass.
AOSP's servicemanager starts on the kernel and gets through its binder
setup, then stops at SELinux (see [aosp-shell.md](aosp-shell.md)).
Phase C2 (block devices): `--ktest` attaches the C2 test disk
(`tools/mkc2-images.py`: a GPT disk with ext4 4 KiB, ext3 1 KiB/htree,
erofs plain, erofs lz4hc compact and erofs lz4 legacy images of one source
tree, plus a scratch partition) as vda (modern, writes to an overlay) and
again as vdb (legacy-only, read-only). c2_block 19/19, c2_fs 59/59 (every
image compared file by file with the host tree in /c2/tree; xattrs, mmap,
exec from the image, EROFS, statfs), c2_loop 17/17 and c2_dm 20/20; all
four skip on the Linux host (no disk, loop/dm need root). Two in-kernel
tests were added (27). `--aosp-disk-test` maps system_a and vendor_a out
of AOSP's super.img with dmctl create-from-super (AOSP's own liblp and
libdm on these ioctls), mounts them and checks 9/9, including every file's
md5 (2349 in system, plus vendor) against a host `fsck.erofs --extract`.
Fixed on the way: every exiting process leaked its task (kernel stack,
`Process`, executable path) because `exit_group` callers held an
`Arc<Task>` in frames that never unwind; the exit paths now drop them.

Phase C3a (Android init, [android-init.md](android-init.md)):
`--aosp-init-test` boots unmodified AOSP `/init` from init_boot.img's
ramdisk with super.img as the boot disk: first-stage mount of the dynamic
partitions (uevents over netlink, dm-linear, erofs), switch_root (MS_MOVE
onto `/`), SELinux policy load into the selinuxfs stub, second stage,
property service, ueventd coldboot, apexd-bootstrap (10 APEXes on loop
devices), linkerconfig (a pty), cgroups, `early-init` / `init` /
`late-init`; then `getprop`/`setprop` in init's console shell: 22/22.
New user-space suites: c3_selinux 36, c3_cgroup 42 (both Rust kernel
only), c3_inotify 28, c3_keyctl 26, c3_netlink 25 (9 on the host, which
cannot trigger uevents unprivileged), c3_misc 49 (36 on the host); the
host oracle passes all of them. `--ktest` 32/32, `--test` 27/27,
`--aosp-sh-test` 15/15, `--aosp-disk-test` 9/9 still pass.

Phase C3b (Android's core daemons, [android-init.md](android-init.md)):
mount namespaces with shared-subtree propagation (init's bootstrap and
default namespaces: servicemanager finds the vendor APEX VINTF fragments,
init parses the APEX rc files), per-thread credentials, the kernel log
buffer (/dev/kmsg reads, /proc/kmsg for logd's klogd, syslog),
/dev/hw_random, epoll-able PSI files, RAM disks for a tmpfs /data and
/metadata. The boot reaches `post-fs-data`, the default mount namespace,
apexd's full activation and `load-bpf-programs`, with ueventd,
servicemanager (20+ services: keystore2, keymint, gatekeeper, vold,
apexservice, suspend), hwservicemanager, logd, lmkd, prng_seeder, vold,
keystore2, tombstoned running; netbpfload then fails (no eBPF) and init
reboots (~30 s). `--aosp-init-test` 34/34; new suites c3_kmsg 23,
c3_creds 15, c3_sysfs 14 (9 on the host), c3_mntns 8 (8 on the host,
in a user namespace); `--ktest` 36/36, `--test` 27/27, `--aosp-sh-test`
15/15, `--aosp-disk-test` 9/9 pass.

Phase C3c (eBPF, [android-init.md](android-init.md#c3c-ebpf)): the
bpf(2) syscall with real maps (array, per-CPU, mmapable array, hash, LRU,
LPM trie, ring buffer, map-in-map and the other key/value types),
programs that are stored but never run, BTF blobs, links and recorded
cgroup attachments, ids, OBJ_GET_INFO_BY_FD, a real bpffs at
/sys/fs/bpf (pin, get, rename, chmod), OFD locks, uname 6.12.0 (Android
16's GKI) and /proc/sys/net/core/bpf_jit_*. netbpfload, the platform
bpfloader and uprobestatsbpfload load and pin everything critical,
`bpf.progs_loaded=1`, init starts netd (its bpf setup passes; it then
exits on iptables) and zygote; init stays up. The per-inode erofs
decompression buffers that filled the kernel heap once zygote started
are now one small global cache. `--aosp-init-test` 41/41; new suite
c3_bpf 117 (skipped on the host: bpf needs CAP_BPF there).

Phase C3d (network and adbd, [android-init.md](android-init.md#c3d-network-and-adbd)):
virtio-net, smoltcp's TCP/IP behind AF_INET/AF_INET6 sockets (loopback,
dual stack, ARP over the card), rtnetlink, the interface ioctls, /proc/net
and the net sysctls, stored iptables/ip6tables tables, and cgroup.events
notifications (init's service restarts no longer wait 2.2 s each). netd
finishes its setup; adbd listens on TCP 5555 and the host's `adb connect`
/ `adb shell` / `push` / `pull` work through QEMU's port forward.
`--aosp-adb-test` 17/17 (debug and release); new suite c3_net 130 (97 on the Linux
host, where rtnetlink changes, eth0 and iptables are skipped);
`--ktest` 38/38, `--test` 27/27, `--aosp-sh-test` 15/15,
`--aosp-disk-test` 9/9, `--aosp-init-test` 41/41 (debug and release) pass.

Phase D1 (graphics, [android-init.md](android-init.md#d1-graphics)):
virtio-gpu (2D), a DRM device (card0 + renderD128: dumb buffers, PRIME,
KMS with legacy and atomic modesetting, flip events, fences), dma-buf
files, the system dma-heap, sync files and sw_sync. With a virtio-gpu,
AOSP's minigbm allocator, the ranchu HWC3 composer and SurfaceFlinger
(RenderEngine on ANGLE over SwiftShader) run; the boot animation reaches
the scanout and `adb shell screencap -p` works. New suites d1_dumb 42,
d1_kms 83, d1_prime 42 (9 on the host: the dma-heap part), d1_sync 26
(skipped on the host: sw_sync is in debugfs); `--aosp-gfx-test` 21/21
checks the stack end to end. `--ktest` 42/42, `--test` 27/27,
`--aosp-sh-test` 15/15, `--aosp-disk-test` 9/9, `--aosp-init-test` 41/41,
`--aosp-adb-test` 17/17 pass.

Phase D2 (input and network, [android-init.md](android-init.md#d2-input-and-network)):
`adb exec-out` works (the pty's TCGETS overflowed bionic's termios in
adbd's raw-PTY child); virtio keyboard/tablet/multitouch are evdev nodes
that `getevent` lists and that QEMU's `sendkey` reaches; the TCP stack
resets, reports resets and keeps up with bursts and backpressure; ICMP
ping sockets, IPv4 fragmentation and IPv6 off the machine (NDP, SLAAC)
work. New suites d2_evdev 86 (74 on the Linux host, through uinput),
d2_tcp 39 (39 on the host), d2_udp 43 (24 on the host: no eth0 part);
new `--aosp-input-test` 17/17. `--test` 27/27, `--ktest` 45/45,
`--aosp-sh-test` 15/15, `--aosp-disk-test` 9/9, `--aosp-init-test` 41/41,
`--aosp-adb-test` 21/21 (now with exec-out and `nc` to the host),
`--aosp-gfx-test` 22/22 (now with `exec-out screencap`) pass.

Phase D3a (zygote and system_server, [android-init.md](android-init.md#d3a-zygote-and-system_server)):
userfaultfd for ART's concurrent mark-compact GC, then everything
system_server and the apps needed: PI futexes, pidfds, POSIX timers,
membarrier, per-thread names, ashmem, AF_VSOCK (local), AF_KEY,
AF_PACKET with classic BPF filters, tun links, hvc stand-ins, bpffs
genfscon labels, /proc/config.gz, /proc/<pid>/cmdline from memory, the
frp and misc partitions and a device-tree fstab, EROFS from access(W_OK)
on read-only mounts; and the kernel-time fixes (mount index and
namespace cleanup, heap size classes, erofs directory cache) that took
the boot from ~400 s to ~20-25 s. `--aosp-boot-test` 24/24
(sys.boot_completed=1, Launcher3 home). New suites d3_uffd 83 (80 on the
host), d3_proc 98 (on the host: AF_PACKET, AF_KEY, tun, hvc, ashmem and
the read-only mount skipped), d3_timers 35 (35 on the host).
`--test` 27/27, `--ktest` 48/48, `--aosp-sh-test` 15/15,
`--aosp-disk-test` 9/9, `--aosp-init-test` 41/41 (hwservicemanager may
have stopped itself), `--aosp-adb-test` 21/21, `--aosp-gfx-test` 22/22,
`--aosp-input-test` 17/17 pass. Fixed on the way: a TLB shootdown that
named offline CPUs spun forever, a shmem read of a hole past an empty
initramfs backing panicked, timeouts with huge tv_sec overflowed (futex,
sigtimedwait, clock_nanosleep, utimensat).

Phase D3b (an APK from source, [android-init.md](android-init.md#d3b-adb-install-and-an-app)):
splice(2) for PackageInstaller's streamed `adb install`, and dm devices'
sysfs `slaves/` and `holders/` links (a restarted apexd maps every /apex
mount back to its loop device; without them a restarted system_server
lost all APEX packages and crash-looped on "There must be exactly one
permissions manager"). apps/hello, built by `apps/hello/build.sh`
(aapt2, javac, d8, NDK clang, apksigner; no Gradle) installs (installd
creates the app directories and extracts the JNI library, artd's
dex2oat verifies it) and `am start` runs its Activity, which calls its
x86_64 JNI library and draws through SurfaceFlinger on the scanout.
New `--aosp-apk-test` 22/22; new suite d3_splice 10 (10 on the host);
c2_dm checks the slaves/holders links. `--test` 27/27, `--ktest` 49/49,
`--aosp-sh-test` 15/15, `--aosp-disk-test` 9/9, `--aosp-init-test` 41/41,
`--aosp-adb-test` 21/21, `--aosp-gfx-test` 22/22, `--aosp-input-test`
17/17, `--aosp-boot-test` 24/24 pass.

Phase E1 (real-hardware-class controllers, tested on QEMU's emulations):
NVMe, AHCI SATA and xHCI with HID boot keyboards/mice (see the section
above); `run-qemu.sh --nvme` boots AOSP from NVMe (`--aosp-boot-test
--nvme` 24/24, sys.boot_completed=1). New `--hw-test` (c2_fs 59/59 over
NVMe, new suite e1_hw 27: SATA and NVMe disks, writes, mounts, USB key
and mouse events through evdev); new kernel tests for NVMe PRPs, ATA
strings, xHCI endpoint math and HID report decoding. Disks also get
`mq/0/nr_tags` (apexd reads it). `--test` 32/32, `--ktest` 50/50,
`--aosp-sh-test` 15/15, `--aosp-disk-test` 9/9, `--aosp-init-test` 41/41,
`--aosp-adb-test` 21/21, `--aosp-gfx-test` 22/22, `--aosp-input-test`
17/17, `--aosp-boot-test` 24/24, `--aosp-apk-test` 22/22 pass.

E3 (USB stick on a real PC): USB mass storage (Bulk-Only + SCSI) and
hubs on the xHCI driver, the boot disk found at run time
(`androidboot.*=auto`, the PC's other disks read-only), ACPI tables
(MADT check of the MP list, HPET/PM timer calibration with PIT and CPUID
fallbacks, ACPI reset and S5), the kernel log on the framebuffer
(Pause/Break toggles it over Android), write-combined framebuffer,
`serial=off`, `loglevel=`. New `tools/mkusb.sh`, `tools/mkusbimg.py`,
`tools/aosp-bootargs.sh` and `run-qemu.sh --usb-boot-test` 28/28 (OVMF
boots Limine from the stick image's ESP, sys.boot_completed=1 in ~30 s,
Launcher3 at 1920x1080; also with `--usb-dev bot`). `--hw-test` adds a
usb-storage disk and a hub (e1_hw 28 cases). `--test` 36/36, `--ktest`
50/50, `--aosp-sh-test` 15/15, `--aosp-disk-test` 9/9,
`--aosp-init-test` 41/41, `--aosp-adb-test` 21/21, `--aosp-gfx-test`
22/22, `--aosp-input-test` 17/17, `--aosp-boot-test` 24/24,
`--aosp-apk-test` 22/22, `--aosp-fb-test` 21/21 pass.

Phase E2 (graphics on the firmware framebuffer, the real-hardware path
for a machine whose GPU has no open driver): Limine's framebuffer and
firmware-type requests (the kernel logs `firmware: UEFI|BIOS` and the
framebuffer), a simpledrm-style output for the D1 DRM/KMS core (see
Graphics), `run-qemu.sh --fb[=std|bochs]`. With no virtio-gpu, a plain
VGA under OVMF (UEFI is run-qemu.sh's default; BIOS boots the same ISO)
and `simpledrm.name=virtio_gpu`, AOSP's minigbm, ranchu composer and
SurfaceFlinger run unchanged on the boot framebuffer: new
`--aosp-fb-test` 21/21 (sys.boot_completed=1 in ~125 s, Launcher3 on
screencap, QEMU's screendump of the VGA matching the screencap). New
kernel test for the format-converting copy. `--aosp-gfx-test` no longer
fails when the framework finishes booting before adb connects (the
boot animation had already exited, so only SystemUI's layers such as
NotificationShade were listed): zygote is stopped only while
`service.bootanim.exit` is unset, otherwise the framework's layers are
accepted. `--test` 33/33, `--ktest` 50/50, `--hw-test`,
`--aosp-sh-test` 15/15, `--aosp-disk-test` 9/9, `--aosp-init-test` 41/41,
`--aosp-adb-test` 21/21, `--aosp-gfx-test` 22/22, `--aosp-input-test`
17/17, `--aosp-boot-test` 24/24, `--aosp-apk-test` 22/22 pass.

E3 (USB stick on a real PC): USB mass storage (Bulk-Only + SCSI) and
hubs on the xHCI driver, the boot disk found at run time
(`androidboot.*=auto`, the PC's other disks read-only), ACPI tables
(MADT check of the MP list, HPET/PM timer calibration with PIT and CPUID
fallbacks, ACPI reset and S5), the kernel log on the framebuffer
(Pause/Break toggles it over Android), write-combined framebuffer,
`serial=off`, `loglevel=`. New `tools/mkusb.sh`, `tools/mkusbimg.py`,
`tools/aosp-bootargs.sh` and `run-qemu.sh --usb-boot-test` 28/28 (OVMF
boots Limine from the stick image's ESP, sys.boot_completed=1 in ~30 s,
Launcher3 at 1920x1080; also with `--usb-dev bot`). `--hw-test` adds a
usb-storage disk and a hub (e1_hw 28 cases). `--test` 36/36, `--ktest`
50/50, `--aosp-sh-test` 15/15, `--aosp-disk-test` 9/9,
`--aosp-init-test` 41/41, `--aosp-adb-test` 21/21, `--aosp-gfx-test`
22/22, `--aosp-input-test` 17/17, `--aosp-boot-test` 24/24,
`--aosp-apk-test` 22/22, `--aosp-fb-test` 21/21 pass.

## Known gaps / next steps

- Graphics follow-ups: virgl/3D and blob resources, interrupt-driven
  virtio-gpu with asynchronous commits (fences are signalled at once
  now), a cursor plane, hotplug.
- E1 follow-ups: MSI-X interrupts and several commands in flight (NVMe
  queues per CPU, AHCI NCQ), NVMe namespaces with 4 KiB LBAs, USB
  hot-plug (hubs since E3), UAS and 4 KiB-sector USB disks, HID report descriptors (non-boot devices such as usb-tablet,
  touch screens), keyboard LEDs; IOAPIC interrupts, Intel/AMD GPUs.
- E2 follow-ups: copying only damaged rectangles, mode changes (the
  firmware's mode is the only one), a native "simpledrm" backend in
  userspace (today AOSP needs `simpledrm.name=virtio_gpu`).
- E3 follow-ups: results from the physical PC (docs/usb-boot.md says what
  to report); a persistent /data on the stick's userdata partition; a
  network driver (Intel I225-V) for adb; starting APs without Limine
  (INIT-SIPI from the MADT); AML for \_S5 beyond the byte pattern.
- Input follow-ups: force feedback (EVIOCSFF), key autorepeat (EV_REP is
  only stored), EVIOCREVOKE semantics, /proc/bus/input/devices, uhid,
  interrupt-driven virtio-input. `input keyevent` and InputFlinger run
  in system_server (D3).
- Android (phase D and later): kernel BTF for CO-RE bpf objects, a persistent /data
  (ext4 writes or f2fs, then FBE), a real SELinux policy engine, cgroup
  resource control, a pty line discipline for an interactive `adb shell`
  (commands work), ptrace/process_vm_readv for crash_dump.
- Network follow-ups: IPv6 DAD, MLD, fragments and router lifetimes;
  reassembly of incoming IPv4 fragments; enforcing netfilter and policy
  routing, packet sockets, tun/tap traffic and virtual links, vsock to a host,
  interrupt-driven virtio-net, socket buffer sizes from
  SO_SNDBUF/SO_RCVBUF, ICMP errors to UDP sockets (ECONNREFUSED), TCP
  ETIMEDOUT, real /proc/net/snmp counters.

- VFS follow-ups: permission checks and capabilities (ids are tracked),
  read-only mounts (only access(W_OK) and erofs/ext4 writes honour them),
  slave/unbindable mounts, statfs block accounting, fs-verity.
- D3a/D3b follow-ups: a persistent /data (tmpfs today: `adb install`
  works, but installs and dexopt results vanish at reboot); one boot in
  ~10 took ~110 s instead of ~25 s to sys.boot_completed (cause not
  found); a system_server SIGABRT in a binder thread seen once
  (`abortdump=system_server` to catch it); userfaultfd write-protect, ptrace / process_vm_readv
  (tombstones), a real priority-inheritance scheduler, AF_PACKET rings,
  vsock to a host, the apexd-bootstrap SIGSEGV seen once.
- Memory follow-ups: MAP_GROWSDOWN, swap/reclaim of page-cache pages (none:
  file page caches stay in RAM while the inode lives; only the block caches
  evict), mlock.
- Block/FS follow-ups: ext4 writes and journal replay (/data), f2fs,
  interrupt (MSI-X) driven virtio with several requests in flight, dm targets for
  /data (default-key, bow) and real dm-verity, erofs big pclusters /
  ztailpacking / fragments, lazy umount semantics (Linux returns EBUSY
  while files are open).
- Binder: freezer, oneway spam reports, priority inheritance, SELinux
  contexts, binder_logs; one global lock (fine for now, a per-process
  split later).
- Sockets: netlink protocols other than uevents and NETLINK_ROUTE,
  in-flight fd garbage collection, MSG_OOB, SO_PEEK_OFF.
- ELF: map segments from the file's page cache (MAP_PRIVATE) instead of
  copying them, so text pages are shared between processes.
- Interrupt routing (IOAPIC from the ACPI MADT) for UART RX, and MSI-X
  for virtio, instead of polling.
- Scheduler: per-CPU run queues and load balancing, priorities/nice, CPU
  time beyond /proc's tick counts (CPU-time clocks report uptime; getrusage
  and times report 0), scheduling classes and affinity (stored only), sleep timers
  in a timer wheel instead of one list, real-time signal queueing, job
  control (SIGSTOP/SIGCONT).
- XSAVE (AVX) state, no ACPI parsing yet (power off uses the q35/PIIX PM1a
  port), bootloader-reclaimable memory not reclaimed, heap does not grow,
  panic does not stop other CPUs, getrandom is not a vetted CSPRNG.
