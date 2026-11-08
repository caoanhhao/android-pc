# Kernel architecture (phase B4a state)

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
      logger.rs        `log` backend -> serial, "[uptime] LEVEL cpuN msg"
      time.rs          ticks (HZ = 100), TSC clock (monotonic/realtime ns)
      sync.rs          IrqLock (spinlock with local interrupts off)
      errno.rs         Errno / KResult
      random.rs        RDRAND + xorshift (getrandom, AT_RANDOM, /dev/urandom)
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
      syscall/         dispatch table (Linux numbers), fs, time, poll (poll/select,
                       epoll, eventfd, timerfd, signalfd syscalls)
      fs/              vfs (inodes, dentries, mounts, path walk), file (open
                       files, fd tables), tmpfs (also rootfs/devtmpfs/sysfs),
                       dev (character devices), procfs, pipe (+ FIFOs), poll
                       (event bits, wait queues, poll loop), epoll, eventfd,
                       timerfd, signalfd; mod.rs: cpio unpacking, open,
                       mountable file system types
      drivers/         serial (COM1); later virtio, binder, dma-buf, ...
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
   into a tmpfs that becomes the root mount, start `/init` as pid 1 (cwd
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
    is first touched. No size cap (sparse, up to 2^63-1); `write` and
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
    parent shares the address space.
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
  pending, altstack), clear_child_tid, robust list.
- `Process` = thread group: pid (= leader tid), signal actions and
  process-wide pending signals, fd table (`Arc`, shared by CLONE_FILES),
  `FsContext` (cwd as a `PathRef`, umask; copied by fork), the executable
  path and argv (for /proc), exit signal, name.
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
- ELF loader (`elf.rs`): ET_EXEC and static-PIE ET_DYN, PT_LOAD segments
  (pages shared by two segments get both protections), bss zeroed, brk after
  the last segment. PT_INTERP is refused with ENOEXEC and a log line until
  bionic's linker64 can be loaded. Initial stack: argc, argv, envp, auxv
  (AT_PHDR, AT_PHENT, AT_PHNUM, AT_PAGESZ, AT_BASE, AT_FLAGS, AT_ENTRY,
  AT_UID/EUID/GID/EGID, AT_HWCAP, AT_HWCAP2, AT_CLKTCK, AT_SECURE,
  AT_MINSIGSTKSZ, AT_RANDOM, AT_EXECFN, AT_PLATFORM), strings at the top,
  RSP 16-byte aligned at argc.
- Signals and futexes: see [syscalls.md](syscalls.md).

## Files (`fs/`)

One global mount namespace. The pieces:

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
  above `/`. `path_of` builds absolute paths the same way.
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
    kmsg, tty, console, ttyS0 nodes plus `pts/` and `shm/`. Device nodes
    on any file system open the driver in `dev.rs` by major:minor. The
    console is a minimal tty (termios and window size ioctls, so isatty()
    works); it has no input yet (reads return EOF). /dev/kmsg writes go to
    the kernel log.
  - procfs: `self`, `thread-self`, `<pid>/{stat,status,cmdline,comm,maps,
    mounts,fd/,fdinfo/,exe,cwd,root}`, `meminfo`, `cpuinfo`, `uptime`, `mounts`,
    `filesystems`, `cmdline`, `version`, `loadavg`, `stat`. Contents are
    generated at open. `maps` shows `s`/`p`, file offset, device and inode,
    and names file mappings by path (`/dev/zero (deleted)` for anonymous
    shared memory). `maps`/`VmSize` are only filled in for the calling
    process (reading another task's address space pointer would race with
    its execve). `fd/N` are symlinks whose target is the open file's path
    (`pipe:[ino]` for pipes, `anon_inode:[eventfd]`/`[eventpoll]`/
    `[timerfd]`/`[signalfd]` for event files, which share one 0600
    anonymous inode); they are not "magic" links yet. `fdinfo/N`: pos,
    flags (with O_CLOEXEC), mnt_id (device), ino, then per-type lines.
  - sysfs: one shared tmpfs with the empty top-level directories (block,
    bus, class, dev, devices, firmware, fs, kernel, module, power).
  - pipes: 64 KiB, blocking and O_NONBLOCK, atomic writes up to PIPE_BUF,
    EOF when writers are gone, EPIPE + SIGPIPE when readers are gone.
    FIFO nodes (mknod S_IFIFO on any file system) get a pipe on first open,
    looked up by inode in a global table of weak references (dropped with
    the last open end). Opening blocks for the other side unless O_NONBLOCK
    (a nonblocking writer with no reader gets ENXIO); O_RDWR never blocks.
- Everything runs as root: no permission checks except the execute bit
  (execve, access X_OK). chown/chmod are stored; MS_RDONLY is recorded only.

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

## Tests

`#[test_case]` functions anywhere in the crate; `cargo test` boots a test
kernel which runs them after full boot (SMP included) and writes to
`isa-debug-exit` (0x10 -> QEMU status 33 = pass, 0x11 -> 35 = fail); a panic
fails the run; the script times out after 120 s. Current tests (22): int3
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
mapping, VFS
(cpio unpack into tmpfs, path walk with `..` and symlinks, mounting a
tmpfs and walking back out of it, rename/unlink keeping dentries in sync,
umount).

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
as the Linux host, with 1, 4 and 8 CPUs; b4_unix waits for B4b.

## Known gaps / next steps

- VFS follow-ups: permission checks and credentials, read-only mounts,
  devpts, console input (and its poll), /proc/<pid>/fd magic
  links (so a memfd cannot be re-opened through /proc/self/fd/N yet),
  /proc/sys, a real sysfs, statfs block accounting.
- Memory follow-ups: MAP_GROWSDOWN, swap/reclaim of page-cache pages (none:
  everything stays in RAM), mlock, a page cache for disk file systems (C2)
  behind the same `Inode::shmem` hook.
- Dynamic ELF (PT_INTERP: load bionic's linker64 and pass AT_BASE).
- Scheduler: per-CPU run queues and load balancing, priorities/nice, CPU
  time accounting (CPU-time clocks currently report uptime), sleep timers
  in a timer wheel instead of one list, real-time signal queueing, job
  control (SIGSTOP/SIGCONT), credentials.
- XSAVE (AVX) state, no ACPI parsing yet (power off uses the q35/PIIX PM1a
  port), bootloader-reclaimable memory not reclaimed, heap does not grow,
  panic does not stop other CPUs, getrandom is not a vetted CSPRNG.
