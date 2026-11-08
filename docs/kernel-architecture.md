# Kernel architecture (phase C3b state: AOSP init and its core daemons run)

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
tools/run-qemu.sh --disk IMG[,rw|,snapshot][,legacy]   # attach raw images as virtio-blk disks
                                  # (PCI slots 00:04.0, 00:05.0, ... in order)
tools/run-qemu.sh --initrd out/b1_hello.cpio --append "strace"   # other initramfs, kernel cmdline
tools/run-qemu.sh --bios --no-kvm --smp 2 --mem 512M   # other machine shapes
cd kernel && cargo run / cargo test                    # same, script is the cargo runner
```

Wrap every build or QEMU run in `tools/limited-build.sh` (see `CLAUDE.md`).

Kernel command line (`--append`): `init=PATH` (default `/init`), `strace`
(log every syscall), and everything after `--` becomes init's argv[1..]
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
      mm/              frame (bitmap allocator + refcounts), paging, heap, kstack (guarded
                       kernel stacks), user (address spaces, VMAs, user copies)
      sched/           run queue, context switch, sleep/wake, idle loop;
                       timer (kernel timer callbacks from the tick)
      process/         Task/Process/process table, lifecycle (clone, exec, exit,
                       wait4), elf (loader + initial stack), signal, futex
      block/           block devices: registry, /dev/block nodes, sysfs, block ioctls
                       (mod), per-device page cache (cache), GPT partitions (gpt),
                       loop devices (loop_dev), device mapper (dm), RAM disks (brd)
      syscall/         dispatch table (Linux numbers), fs, time, poll (poll/select,
                       epoll, eventfd, timerfd, signalfd syscalls), net (socket
                       syscalls, msghdr/cmsg handling, socket options), sys
                       (rlimits, rusage, sched_*, priorities, ids, caps,
                       personality, sigqueueinfo, unshare/setns, syslog,
                       hostname, ioprio), keys (add_key/keyctl)
      net/             sockets: constants, credentials; unix (AF_UNIX sockets);
                       netlink (AF_NETLINK: kernel uevents, stub protocols)
      fs/              vfs (inodes, dentries, mounts, path walk), file (open
                       files, fd tables), tmpfs (also rootfs/devtmpfs/sysfs),
                       dev (character devices), tty (console line discipline),
                       procfs, pipe (+ FIFOs), poll
                       (event bits, wait queues, poll loop), epoll, eventfd,
                       timerfd, signalfd; erofs (+ lz4), ext4 (read-only disk
                       file systems); sysfs (device tree, uevent files), sysctl
                       (/proc/sys table), selinuxfs, cgroupfs (cgroup v1/v2),
                       inotify, pty (ptmx + devpts), kmsg (printk ring: /dev/kmsg,
                       /proc/kmsg, syslog); mod.rs: cpio unpacking,
                       open, mountable file system types
      drivers/         serial (COM1); binder/ (binder IPC + binderfs: abi, state,
                       protocol, mod); pci (config space, enumeration); virtio/
                       (transports + virtqueues, blk); later net, gpu, dma-buf, ...
```

Rule: generic code calls `crate::arch::*` re-exports, not `arch::x86_64::*`.

## Boot sequence (BSP)

1. `serial::init`, `logger::init`.
2. `boot::init`: check Limine base revision, log bootloader/cmdline.
3. `arch::init_bsp_early`: static BSP `PerCpu` (GDT + TSS with a double-fault
   IST stack, GS base -> `PerCpu`), load the IDT, enable SSE/FXSR (+FSGSBASE)
   for user code and program STAR/LSTAR/SFMASK.
4. `mm::init`: log memory map -> frame allocator -> new kernel page tables
   (all 256 kernel-half PDPTs pre-allocated) -> heap.
5. `arch::init_bsp_late`: remap+mask the 8259 PIC, enable LAPIC, calibrate the
   LAPIC timer and the TSC against PIT channel 2 (10 ms), start the periodic
   timer, `sti`.
6. `smp::start_aps`: per AP allocate `PerCpu`, a 64 KiB stack and an IST
   stack, `MpInfo::bootstrap`. The AP loads the kernel CR3, switches stack,
   installs GDT/TSS/GS/IDT, user-mode support, its LAPIC and timer, then
   enters the scheduler's idle loop. The BSP waits up to 1 s for all of them.
7. Tests (`cfg(test)`), or: unpack the initramfs (first Limine module)
   into a tmpfs that becomes the root mount, fill in sysfs (mount
   points, memory/tty/misc character devices), create loop0-7 and the
   RAM disks asked for with `brd.rd_nr=` (`block::init`), enumerate PCI and bring up virtio-blk disks with
   their partitions (`drivers::init`), start `/init` as pid 1 (cwd
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
- **Heap**: `linked_list_allocator` on one contiguous block (1/8 of free RAM,
  clamped to 4..64 MiB) reached via the direct map. Interrupts are off while
  its lock is held, so interrupt handlers may allocate. Does not grow yet.
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
- Signals and futexes: see [syscalls.md](syscalls.md).

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
  mount on a dentry; `parent_of` handles `..` at a mount root and never goes
  above `/`. `path_of` builds absolute paths the same way. MS_MOVE
  changes a mount's mount point. The root directory is whatever is
  mounted (moved) on top of the namespace's root mount (the initial
  rootfs or its copy), so after Android's switch_root every path walk
  starts in the system image; mounts not reachable from it drop out of
  /proc/mounts. A generation counter and wait queue let /proc/mounts
  readers poll for changes.
- **Namespaces and propagation** (`vfs::MntNs`, `copy_ns`,
  `mount_in_group`): unshare/clone(CLONE_NEWNS) copy every mount of the
  namespace (same root dentries, so the copies show the same trees);
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
  protocol (header + data area, BUFFER_FULL). Details of both in
  [syscalls.md](syscalls.md#block-devices-c2).
- **erofs** (`fs/erofs.rs`): superblock, compact/extended inodes (nid =
  inode number, st_dev = the block device), FLAT_PLAIN and tail-packed
  FLAT_INLINE data, directories (sorted dirent blocks), symlinks, device
  nodes, inline and shared xattrs, and LZ4 files with full or compact
  (4B/2B) lcluster indexes following Linux's zmap.c: an offset maps to its
  extent (lookback through NONHEAD lclusters), whose single-block pcluster
  is decompressed (`fs/lz4.rs`, partial decoding; 0-padded or not) or, for
  plain pclusters, copied (interlaced or shifted). The last decompressed
  pcluster is kept per inode for sequential reads. That covers AOSP's
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
fails the run; the script times out after 120 s. Current tests (27): int3
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

## Known gaps / next steps

- Android init (C3c and later): eBPF for netbpfload/netd (the next boot
  blocker), a persistent /data (ext4 writes or f2fs, then FBE), a real
  SELinux policy engine, cgroup resource control, a pty line discipline
  for `adb shell`, adbd over virtio-net or virtio-console,
  ptrace/process_vm_readv for crash_dump.

- VFS follow-ups: permission checks and capabilities (ids are tracked),
  read-only mounts, slave/unbindable mounts, statfs block accounting.
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
- Sockets: AF_INET/INET6, netlink protocols other than uevents, in-flight fd garbage
  collection, MSG_OOB, SO_PEEK_OFF.
- ELF: map segments from the file's page cache (MAP_PRIVATE) instead of
  copying them, so text pages are shared between processes.
- Interrupt routing (IOAPIC from the ACPI MADT) for UART RX, and MSI-X
  for virtio, instead of polling.
- Scheduler: per-CPU run queues and load balancing, priorities/nice, CPU
  time accounting (CPU-time clocks currently report uptime; getrusage and
  times report 0), scheduling classes and affinity (stored only), sleep timers
  in a timer wheel instead of one list, real-time signal queueing, job
  control (SIGSTOP/SIGCONT).
- XSAVE (AVX) state, no ACPI parsing yet (power off uses the q35/PIIX PM1a
  port), bootloader-reclaimable memory not reclaimed, heap does not grow,
  panic does not stop other CPUs, getrandom is not a vetted CSPRNG.
