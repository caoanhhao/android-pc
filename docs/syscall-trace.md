# Syscall/ioctl trace của AOSP tham chiếu (task A2)

Đây là danh sách những gì userspace AOSP thực sự đòi hỏi từ kernel. Kernel Rust phải
cung cấp đủ những thứ này để chạy được cùng image.

- **Image:** `aosp_cf_x86_64_only_phone-trunk_staging-userdebug`, android-16.0.0_r4
  (`BP4A.251205.006`). Xem [aosp-build.md](aosp-build.md).
- **Kernel tham chiếu:** GKI `6.12.38-android16-5`, chạy Cuttlefish trên QEMU/KVM, 4 vCPU, 6 GB RAM.
- **Kết quả boot:** đạt `sys.boot_completed=1` sau khoảng 40 giây. Launcher3 (`QuickstepLauncher`)
  ở trạng thái resumed, `zygote64`, `system_server` và `surfaceflinger` đều chạy.
  Có khoảng 330 process.

## Phương pháp

Có hai nguồn dữ liệu bổ sung cho nhau. Dữ liệu thô và script phân tích nằm trong
`aosp/a2-trace/` (gitignored); chạy lại bằng `python3 aosp/a2-trace/analyze.py`.

| Nguồn | Phủ được gì | Cách lấy |
|---|---|---|
| **ftrace boot** | Từ 0.62 s đến 7.60 s của boot: first-stage init, ueventd, init, apexd, servicemanager, HAL, zygote64, system_server, SystemUI, launcher. Tổng 1 482 750 syscall, 152 loại. | Kernel cmdline `trace_event=raw_syscalls:sys_enter,task:*,sched:sched_process_exec/exit trace_buf_size=128M trace_options=record-tgid,nooverwrite`, cộng bootconfig `androidboot.fastboot.boottrace=enabled`. Nếu thiếu bootconfig này, `atrace.rc` tắt `tracing_on` ở `late-init`. Buffer đầy ở giây 7.6. |
| **strace** | `stop; start`, tức là khởi động lại toàn bộ framework: zygote64, system_server, surfaceflinger, bootanimation, audioserver, netd, apexd, các app. Tổng 70 process, 609 195 syscall, 135 loại. Có tên ioctl đã giải mã và đường dẫn của từng fd. | `strace -f -ff -tt -yy -p 1` trên guest userdebug (`setenforce 0` trong lúc trace). |

Ảnh chụp tĩnh sau khi boot: `/proc/mounts`, `/proc/filesystems`, `/proc/cgroups`,
`/proc/config.gz`, fd đang mở của mọi process, và `ls /dev`.

Giới hạn:
- ftrace raw không có đường dẫn, nên ioctl ở đó chỉ có số lệnh. Ánh xạ sang thiết bị
  lấy từ strace và từ kiểu `_IOC`.
- Không trace kịch bản `adb install` hay chạy app (việc của giai đoạn D3).

## Những điểm chính cho kernel Rust

1. **Chỉ x86_64 thuần.** Không có lệnh `int 0x80` hay syscall compat nào: image là
   `x86_64_only`, không có thư viện 32-bit. Toàn bộ boot chỉ dùng **154 syscall khác nhau**
   (bảng bên dưới).
2. **Không dùng `clone3`, `fork`, `vfork`, `rseq` hay `io_uring`.** Process và thread
   đều tạo bằng `clone`. Chỉ có `unshare(CLONE_NEWNS)`, mỗi app một lần, để tách mount
   namespace và bind-mount `/data/*` dạng tmpfs. Có thêm `setns` (10 lần, trong vold/init).
   Kernel GKI build **không có** `USER_NS`, `PID_NS`, `TIME_NS`, `SYSVIPC`, `POSIX_MQUEUE`,
   `FANOTIFY` hay `CHECKPOINT_RESTORE`. Android không cần các namespace đó.
3. **Binder là IPC xương sống.** `BINDER_WRITE_READ` chiếm khoảng 70% số ioctl, cả lúc boot
   (36 176 lần) lẫn lúc restart (34 156 lần). Binder được mount qua **binderfs** tại
   `/dev/binderfs`, với `binder` và `hwbinder` (`/dev/binder` là symlink).
   `vndbinder` có trong config nhưng không process nào mở. Cmdline có `binder.impl=rust`,
   tức là GKI này đang chạy **Rust Binder**, dùng được làm tham chiếu ngữ nghĩa cho C1.
4. **Kiểu bộ nhớ chia sẻ dùng nhiều thứ hai là ashmem**: ioctl `'w'`, với `ASHMEM_PIN`
   khoảng 9 000 lần. Thiết bị là `/dev/ashmem<boot-id>`, đang mở 411 fd. Tiếp theo là
   **dma-buf heap** (`/dev/dma_heap/system`, `DMA_HEAP_IOCTL_ALLOC`, `DMA_BUF_SET_NAME`)
   và memfd (`memfd_create` 99 lần).
5. **ART dùng `userfaultfd`** cho GC CMC (`UFFDIO_API/REGISTER/COPY/ZEROPAGE`, khoảng
   5 600 lần COPY). Thiếu userfaultfd thì phải ép ART dùng GC khác.
6. **Đồ hoạ:** `/dev/dri/renderD128` và `card0` (virtio-gpu). Dùng `DRM_IOCTL_VERSION`,
   `VIRTGPU_GETPARAM`, các ioctl dumb buffer, PRIME, các ioctl `MODE_*` trong composer, và
   `sync_file` (`SYNC_IOC_FILE_INFO`). Render bằng SwiftShader (CPU), nên không có ioctl 3D.
7. **Block và mount:** device-mapper (`DM_DEV_CREATE/STATUS/SUSPEND`, `DM_TABLE_LOAD/STATUS`)
   cho dm-linear (super) và dm-verity, loop (`LOOP_CTL_GET_FREE`, `LOOP_CONFIGURE`, khoảng
   100 APEX). Có 200 mount **erofs**, `/data` là **f2fs**, dùng fscrypt (`FS_IOC_*`
   `'f'` 19/21/133) và `quotactl` (installd, 1 173 lần).
8. **eBPF bắt buộc với netd/netbpfload:** `bpf()` khoảng 4 100 lần, cộng `bpf-map`,
   `bpf-prog`, `bpf_link`. `perf_event_open` cùng `PERF_EVENT_IOC_SET_BPF` dùng cho
   tracepoint của lmkd và gpuservice. `/sys/fs/bpf` được mount. Giai đoạn đầu có thể stub
   bằng cách trả lỗi cho netbpfload, nhưng khi đó netd/connectivity sẽ báo lỗi.
9. **cgroups: dùng song song v1 và v2.** cgroup2 tại `/sys/fs/cgroup` (`memory`, đường dẫn
   kiểu `uid_<n>/pid_<n>`, `cgroup.procs`, `cgroup.kill`, `cgroup.freeze`, `memory.*`).
   cgroup v1 cho `cpuset` (`/dev/cpuset`), `cpu` (`/dev/cpuctl`) và `blkio` (`/dev/blkio`).
   libprocessgroup ghi vào `tasks` và `cgroup.procs`.
10. **SELinux enforcing:** selinuxfs (`/sys/fs/selinux/context`, `create`, …), xattr
    `security.selinux` (`lgetxattr` khoảng 5 500 lần, `lsetxattr`/`fsetxattr` khoảng 2 300 lần,
    chủ yếu ở ueventd/init), audit netlink. Kế hoạch stub ở giai đoạn đầu: chạy permissive
    (`androidboot.selinux=permissive`), nhưng init vẫn load policy qua selinuxfs.

## Syscall (tất cả, sắp theo tổng số lần gọi)

Cột 2 là số lần trong 7.6 giây đầu của boot (ftrace). Cột 3 là số lần trong lần khởi động
lại framework (strace). Số 0 ở một cột chỉ có nghĩa là nguồn đó không thấy syscall này.

| syscall | boot (ftrace) | restart (strace) |
|---|---:|---:|
| write | 173971 | 6104 |
| read | 122833 | 29921 |
| lseek | 133585 | 3859 |
| mprotect | 82497 | 41906 |
| mmap | 87660 | 31144 |
| sched_yield | 92898 | 15610 |
| futex | 50064 | 50854 |
| close | 69454 | 31011 |
| ioctl | 48637 | 51360 |
| getuid | 43810 | 54215 |
| openat | 69839 | 27733 |
| fstat | 49948 | 34284 |
| newfstatat | 57293 | 20550 |
| readlinkat | 41448 | 33218 |
| writev | 40892 | 25799 |
| munmap | 48410 | 16811 |
| prctl | 36202 | 19431 |
| pread64 | 21227 | 14202 |
| fcntl | 15853 | 10385 |
| madvise | 15365 | 9811 |
| rt_sigprocmask | 11870 | 12542 |
| faccessat | 11942 | 11659 |
| recvfrom | 21525 | 2026 |
| recvmsg | 15884 | 572 |
| epoll_pwait | 8060 | 8245 |
| getdents64 | 12410 | 1460 |
| gettid | 6507 | 2471 |
| fchownat | 7075 | 1476 |
| fchmodat | 6627 | 1504 |
| fstatfs | 5730 | 2185 |
| ppoll | 6425 | 1438 |
| fchmod | 6420 | 1412 |
| mremap | 5929 | 1792 |
| getsockopt | 4180 | 2553 |
| setpriority | 2719 | 3548 |
| lgetxattr | 5549 | 11 |
| getpriority | 2304 | 2962 |
| bpf | 1531 | 2626 |
| rt_sigreturn | 259 | 3596 |
| dup | 1914 | 1574 |
| rt_sigaction | 2607 | 739 |
| sigaltstack | 1774 | 1339 |
| dup3 | 1493 | 1361 |
| statfs | 1847 | 841 |
| mkdirat | 2476 | 176 |
| clone | 1346 | 1043 |
| socket | 1368 | 494 |
| sendto | 1031 | 624 |
| arch_prctl | 875 | 709 |
| lsetxattr | 1532 | 12 |
| timerfd_settime | 642 | 819 |
| connect | 1139 | 318 |
| setresgid | 1360 | 20 |
| clock_gettime | 457 | 732 |
| quotactl | 1173 | 0 |
| getrandom | 830 | 269 |
| accept4 | 836 | 194 |
| getxattr | 934 | 66 |
| set_tid_address | 485 | 340 |
| statx | 412 | 412 |
| epoll_ctl | 437 | 372 |
| ftruncate | 780 | 26 |
| setsockopt | 433 | 333 |
| symlinkat | 753 | 12 |
| eventfd2 | 377 | 386 |
| fsetxattr | 762 | 0 |
| exit | 439 | 320 |
| unlinkat | 653 | 87 |
| nanosleep | 318 | 417 |
| mknodat | 679 | 0 |
| sendmsg | 264 | 350 |
| epoll_create1 | 269 | 276 |
| mount | 346 | 139 |
| pipe2 | 359 | 108 |
| waitid | 305 | 153 |
| membarrier | 174 | 231 |
| renameat2 | 268 | 103 |
| capget | 263 | 82 |
| setpgid | 284 | 46 |
| capset | 178 | 65 |
| sched_getscheduler | 183 | 47 |
| umask | 187 | 35 |
| sched_getaffinity | 180 | 42 |
| execve | 177 | 41 |
| getpid | 177 | 41 |
| uname | 168 | 48 |
| setgroups | 158 | 42 |
| fsync | 79 | 120 |
| wait4 | 111 | 67 |
| flock | 76 | 73 |
| bind | 85 | 43 |
| setuid | 107 | 18 |
| exit_group | 83 | 39 |
| setgid | 105 | 16 |
| getrusage | 84 | 36 |
| memfd_create | 78 | 21 |
| pwrite64 | 24 | 74 |
| pselect6 | 58 | 36 |
| socketpair | 48 | 43 |
| kill | 57 | 26 |
| setxattr | 29 | 50 |
| sched_setscheduler | 43 | 27 |
| fadvise64 | 65 | 0 |
| mlock | 51 | 9 |
| finit_module | 58 | 0 |
| userfaultfd | 25 | 28 |
| timer_settime | 52 | 0 |
| sched_get_priority_min | 28 | 22 |
| getsockname | 33 | 13 |
| geteuid | 26 | 20 |
| sendfile | 7 | 36 |
| inotify_add_watch | 21 | 18 |
| munlock | 39 | 0 |
| listen | 30 | 8 |
| rt_sigtimedwait | 17 | 21 |
| unshare | 17 | 21 |
| timerfd_create | 21 | 17 |
| setresuid | 16 | 20 |
| sysinfo | 19 | 3 |
| epoll_pwait2 (441) | 0 | 22 |
| getpeername | 9 | 9 |
| getrlimit | 12 | 5 |
| getgid | 14 | 3 |
| setrlimit | 13 | 3 |
| pidfd_open | 15 | 0 |
| ioprio_set | 13 | 2 |
| shutdown | 9 | 6 |
| utimensat | 14 | 0 |
| perf_event_open | 11 | 3 |
| getppid | 11 | 1 |
| getegid | 11 | 1 |
| setsid | 7 | 4 |
| sched_getparam | 5 | 5 |
| rt_sigsuspend | 10 | 0 |
| timer_create | 4 | 6 |
| setns | 10 | 0 |
| umount2 | 7 | 3 |
| inotify_init1 | 6 | 3 |
| fdatasync | 0 | 9 |
| getcwd | 5 | 3 |
| sched_get_priority_max | 6 | 2 |
| fchown | 4 | 3 |
| getpgid | 4 | 2 |
| signalfd4 | 5 | 0 |
| syslog | 4 | 0 |
| setregid | 2 | 2 |
| chdir | 4 | 0 |
| setreuid | 2 | 2 |
| sched_setparam | 1 | 1 |
| chroot | 2 | 0 |
| linkat | 1 | 0 |
| settimeofday | 1 | 0 |
| process_mrelease | 1 | 0 |
| mlockall | 1 | 0 |

Ghi chú:
- `lseek` và `read` rất cao là do `netbpfload` đọc object BPF (130 000 lần `lseek`).
- `sched_yield` chủ yếu đến từ bootanimation (77 000 lần), do vòng spin của SwiftShader/EGL.
- `getuid` và `readlinkat` cao là do system_server (Binder `getCallingUid`, phân giải
  `/proc/self/fd`).
- `clock_gettime` thấp vì phần lớn đi qua **vDSO** (`clock_gettime`, `gettimeofday`,
  `getcpu`). Kernel Rust cần cung cấp vDSO, nếu không bionic vẫn chạy nhưng chậm.

## Syscall theo process chính (ftrace boot, top 10)

| Process | Số lần gọi | Số loại | Gọi nhiều nhất |
|---|---:|---:|---|
| system_server | 271 875 | 102 | write 51k, getuid 29k, readlinkat 24k, read 21k, fstat 20k, ioctl 19k (binder 11k, ashmem 7k), newfstatat, close, openat, writev |
| netbpfload | 212 831 | – | lseek 129k, read 76k, bpf 656, mmap |
| bootanimation | 105 866 | – | sched_yield 77k, mprotect, write, mmap |
| init (pid 1) | 95 762 | 104 | newfstatat 12.7k, close, openat, fcntl, fchmod/fchmodat/fchownat (khoảng 5.5k mỗi loại), recvfrom (uevent/property), ppoll, epoll_pwait, getsockopt (SO_PEERCRED), fsetxattr; ioctl: DM_* và FS_IOC (fscrypt), BLKROSET, EVIOCG*, TIOC* |
| zygote64 | 79 288 | 91 | write 44k (preload), mmap, mprotect, munmap, prctl, fstat, openat, pread64, madvise; ioctl: UFFDIO_* |
| ueventd | 63 571 | 60 | openat 12k, newfstatat 10k, close, write (`uevent` coldboot), lgetxattr 4.6k, getdents64 4.3k (duyệt `/sys`), mkdirat, lsetxattr, setresgid, mknodat 647, symlinkat 672, fchownat; ioctl: DM_DEV_STATUS |
| logd | 50 829 | 60 | futex, writev, recvmsg (socket logdw) |
| apexd | 29 765 (+7 979 ở bootstrap) | 54 | pread64, openat, mount 194, ioctl: LOOP_CTL_*, LOOP_CONFIGURE, DM_* |
| surfaceflinger | 33 955 | 68 | write, mprotect, prctl, mmap, ioctl: binder 1.3k, SYNC_IOC_FILE_INFO 100, DRM PRIME/VIRTGPU/dumb |
| servicemanager | 10 202 | 60 | ioctl 2.7k (BINDER_WRITE_READ, BINDER_GET_NODE_INFO_FOR_REF `0xc018620c`, BINDER_ENABLE_ONEWAY_SPAM_DETECTION), epoll_pwait, newfstatat, read (VINTF manifest) |

Các process khác thấy trong boot: tất cả HAL AIDL của Cuttlefish
(`android.hardware.*-service.*`), vold, keystore2, netd, statsd, installd, gpuservice,
audioserver, cameraserver, mediaserver/extractor/swcodec, traced/traced_probes,
odrefresh/odsign, tombstoned, adbd, prng_seeder, lmkd, linkerconfig, modprobe,
dlkm_loader (`finit_module` 58 lần), aconfigd, update_engine, wificond, mdnsd,
SystemUI, Launcher3, Settings, Bluetooth, Phone, NFC.

## ioctl theo thiết bị

Tên lấy từ strace (`-yy`) hoặc giải mã từ số `_IOC` trong ftrace. Số đếm cộng cả hai nguồn
khi cả hai đều thấy.

| Thiết bị / fd | ioctl (số lần, xấp xỉ) | Process dùng |
|---|---|---|
| `/dev/binderfs/binder` | `BINDER_WRITE_READ` `0xc0306201` (70k), `BINDER_SET_MAX_THREADS` `0x40046205`, `BINDER_VERSION` `0xc0046209`, `BINDER_ENABLE_ONEWAY_SPAM_DETECTION` `0x40046210`, `BINDER_THREAD_EXIT` `0x40046208`, `BINDER_GET_NODE_INFO_FOR_REF` `0xc018620c`, `BINDER_SET_CONTEXT_MGR_EXT` `0x4018620d` (servicemanager) | gần như mọi process |
| `/dev/binderfs/hwbinder` | `BINDER_VERSION`, `BINDER_SET_MAX_THREADS`, `0x40046210` | hwservicemanager, một vài process HIDL còn sót (allocator@1.0). Không có traffic `WRITE_READ` đáng kể |
| `/dev/ashmem<boot-id>` | `ASHMEM_PIN` `0x40087707` (khoảng 17k), `ASHMEM_GET_SIZE` `0x7704`, `ASHMEM_SET_NAME` `0x41007701`, `ASHMEM_SET_SIZE` `0x40087703`, `ASHMEM_SET_PROT_MASK` `0x40087705` | system_server, app, audioserver, mediaswcodec |
| `anon_inode:[userfaultfd]` | `UFFDIO_API` `0xc018aa3f`, `UFFDIO_REGISTER` `0xc020aa00`, `UFFDIO_UNREGISTER` `0x8010aa01`, `UFFDIO_COPY` `0xc028aa03` (5.8k), `UFFDIO_ZEROPAGE` `0xc020aa04` | zygote64, system_server, app (ART CMC GC) |
| `/dev/dma_heap/system` | `DMA_HEAP_IOCTL_ALLOC` `0xc0184800` | gralloc (minigbm), mediaswcodec, system_server, app |
| fd dma-buf (`/dmabuf:`) | `DMA_BUF_SET_NAME` `0x40086201` | như trên |
| `anon_inode:sync_file` | `SYNC_IOC_FILE_INFO` `0xc0383e04` | surfaceflinger, app (HWUI) |
| `/dev/dri/renderD128`, `/dev/dri/card0` (virtio-gpu) | `DRM_IOCTL_VERSION` `0xc0406400`, `VIRTGPU_GETPARAM` `0xc0106443`, `PRIME_FD_TO_HANDLE` `0xc00c642e`, `PRIME_HANDLE_TO_FD` `0xc00c642d`, `MODE_CREATE_DUMB` `0xc02064b2`, `MODE_MAP_DUMB` `0xc01064b3`, `MODE_DESTROY_DUMB` `0xc00464b4`, `MODE_GETPLANERESOURCES`/`ATOMIC`/`ADDFB2`… (`0xc03864bc`, `0xc04064aa`, `0xc01064bd`, `0xc02064b9`) | surfaceflinger, composer3 (ranchu), gralloc, bootanimation, app |
| `/dev/device-mapper` | `DM_DEV_CREATE` `…fd03`, `DM_DEV_SUSPEND` `…fd06`, `DM_DEV_STATUS` `…fd07`, `DM_TABLE_LOAD` `…fd09`, `DM_TABLE_STATUS` `…fd0c` | init (first stage: super và verity), ueventd, apexd, vold, update_engine |
| `/dev/loop-control`, `/dev/block/loopN` | `LOOP_CTL_ADD` `0x4c80`, `LOOP_CTL_GET_FREE` `0x4c82`, `LOOP_CONFIGURE` `0x4c0a`, `LOOP_GET_STATUS64` `0x4c05` | apexd (khoảng 100 APEX), vold |
| block (`/dev/block/*`) | `BLKROSET` `0x125d`, `BLKGETSIZE64` `0x80081272` | init, vold |
| file trên f2fs | fscrypt `FS_IOC_GET_ENCRYPTION_POLICY_EX` `0x800c6613`/`FS_IOC_ADD_ENCRYPTION_KEY` `0x400c6615`, `FS_IOC_ENABLE_VERITY` `0x40806685`, `F2FS_IOC_GET_FEATURES` `0x8004f50c` | init, vold, system_server, keystore2 |
| `/dev/input/event0..5` | `EVIOCGVERSION/ID/NAME/PHYS/UNIQ/BIT/PROP/KEY`, `EVIOCSCLOCKID`, `EVIOCSREP` | system_server (InputReader), init (keychord) |
| socket UDP/netlink | `SIOCGIFINDEX` `0x8933`, `SIOCGIFFLAGS`/`SIOCSIFFLAGS` `0x8913/0x8914`, `SIOCGIFNAME` `0x8910`, `SIOCGIFCONF` `0x8912` | netd, mdnsd, system_server, init |
| tty/pty, pipe | `TCGETS` `0x5401`, `TIOCSCTTY`, `TIOCGPTN`, `TIOCSPTLCK`, `FIONREAD` `0x541b` (trên file thường) | sh, logcat, adbd, app |
| `anon_inode:[perf_event]` | `PERF_EVENT_IOC_SET_BPF` `0x40042408`, `PERF_EVENT_IOC_ENABLE` `0x2400` | lmkd, gpuservice, system_server |

## Device node

Đây là các fd đang mở trên toàn hệ thống sau khi boot, cộng các node được open trong lúc
trace. ueventd tạo node theo uevent coldboot, với `mknodat` 679 lần và `symlinkat` 672 lần.

| Node | Ghi chú |
|---|---|
| `/dev/null` (713 fd), `/dev/zero`, `/dev/urandom` (48), `/dev/random`, `/dev/hw_random` | chuẩn |
| `/dev/ashmem<boot-id>` (411 fd) | ashmem, misc device |
| `/dev/binderfs/{binder,hwbinder,vndbinder,binder-control}`, symlink `/dev/binder`, `/dev/hwbinder`, `/dev/vndbinder` | binderfs |
| `/dev/pmsg0` (124 fd), `/dev/kmsg` (16), `/dev/kmsg_debug` | log của liblog vào pstore và kmsg |
| `/dev/hvc0..N` (virtio-console) | console và các kênh HAL Cuttlefish (keymint, gatekeeper, bt, gnss…) |
| `/dev/dri/card0`, `/dev/dri/renderD128` | virtio-gpu |
| `/dev/dma_heap/system` | dma-buf heap |
| `/dev/input/event0..5` | virtio-input: touch, keyboard, power, rotary… |
| `/dev/device-mapper`, `/dev/block/dm-*` (khoảng 100), `/dev/block/loop*`, `/dev/block/vd*` (virtio-blk), `/dev/block/zram0`, `/dev/block/by-name/*` | storage |
| `/dev/fuse` (2) | FUSE cho `/storage/emulated` (MediaProvider) |
| `/dev/tun` | VPN/netd |
| `/dev/uhid`, `/dev/uinput` | có node, không thấy dùng trong trace |
| `/dev/pts/*`, `/dev/ptmx` | adb shell |
| `/dev/cpuset/*`, `/dev/cpuctl/*`, `/dev/blkio/*` | mount cgroup v1 |
| `/dev/socket/*` | socket AF_UNIX do init tạo (property_service, logd, zygote, lmkd, …) |
| `/dev/__properties__/*` | property area (mmap từ file tmpfs) |

## Filesystem được mount

Lấy từ `/proc/mounts` sau khi boot, tổng 250 mount.

| fs | Số mount | Ở đâu |
|---|---:|---|
| erofs | 200 | `/system`, `/system_ext`, `/product`, `/vendor`, `/odm`, `/vendor_dlkm`, `/system_dlkm`, các `/apex/*` (từ loop) |
| tmpfs | 12 | `/dev`, `/mnt`, `/apex`, `/linkerconfig`, `/storage`, `/tmp`, `/bootstrap-apex`, `/mnt/vm`, … |
| f2fs | 11 | `/data` và các bind mount (`/data_mirror`, …), `/metadata` |
| ext4 | 3 | |
| fuse | 8 | `/storage/emulated`, `/mnt/user/0/…`, `/storage/<sdcard>` |
| vfat | 2 | sdcard ảo |
| cgroup (v1) | 3 | `/dev/cpuset`, `/dev/cpuctl`, `/dev/blkio` |
| cgroup2 | 1 | `/sys/fs/cgroup` (memory, freezer) |
| proc, sysfs, devpts, selinuxfs, tracefs, pstore, bpf, configfs, fusectl, binder | 1 mỗi loại | đường dẫn chuẩn (`/config` là configfs) |

`/proc/filesystems` có 34 loại; danh sách đầy đủ ở `aosp/a2-trace/filesystems.txt`.
Ngoài `mount` thường, init/vold/zygote còn dùng `MS_BIND`, `MS_REC|MS_SLAVE`/`MS_PRIVATE`
và `MS_MOVE`. Mỗi app làm `unshare(CLONE_NEWNS)` rồi mount 5 tmpfs (`/data/data`,
`/data/user`, `/data/user_de`, `/data/misc/profiles/{cur,ref}`) để cô lập dữ liệu.
Thao tác `umount2` rất ít.

## /proc và /sys được truy cập

**/proc** (từ strace, đã chuẩn hoá `<pid>`):
- `/proc/self/fd/*` (33k, phân giải fd bằng readlink), `/proc/self/{exe,cmdline,stat,status,maps,smaps_rollup,environ,cgroup,mountinfo,oom_score_adj,ns/*,task/*,time_in_state}`,
  `/proc/thread-self/*`.
- `/proc/<pid>/{stat,cmdline,maps,smaps,smaps_rollup,status,schedstat,timerslack_ns,cgroup,fd,task,dmabuf_pss,dmabuf_rss}`.
  Lưu ý `dmabuf_*` là bản vá của Android.
- `/proc/sys/net/ipv4/*`, `/proc/sys/net/ipv6/*` (netd), `/proc/sys/kernel/random/{boot_id,uuid}`,
  `/proc/sys/vm/watermark_scale_factor`, `/proc/sys/net/core/*`.
- `/proc/{meminfo,stat,loadavg,zoneinfo,pagetypeinfo,pressure/memory,mounts,filesystems,config.gz,uid_procstat,uid_cputime,net/*}`.
  `uid_*` là bản vá Android và có thể stub.
- init ghi nhiều vào `/proc/sys/*`: `kernel.*`, `vm.*`, `net.*`, `kptr_restrict`,
  `perf_event_paranoid`, `sched_*`.

**/sys**:
- `/sys/fs/cgroup/{system,apps}/uid_<n>/pid_<n>/{cgroup.procs,cgroup.kill,cgroup.freeze,cgroup.events,memory.*,cpu.*}`.
  Số truy cập vào system cộng apps khoảng 1 600.
- `/sys/devices/system/cpu/{possible,present,online,cpuN/cache/index*/*}`,
  `cpufreq/policyN/{affected_cpus,related_cpus,scaling_*_freq,stats/time_in_state}`,
  `cpufreq/schedutil/*`.
- `/sys/kernel/dmabuf/buffers/<inode>/{exporter_name,size}`, `/sys/kernel/tracing/trace_marker`
  (atrace của mọi process: chỉ cần open/write được), `/sys/kernel/mm/{transparent_hugepage,ksm,pgsize_migration}/*`.
- `/sys/fs/selinux/{context,create,access,…}`, `/sys/fs/bpf/*` (pin map/prog),
  `/sys/block/{dm-*,zram0,vd*}/*`, `/sys/dev/char/*`, `/sys/class/*`, `/sys/devices/pci0000:00/*`.
- ueventd duyệt toàn bộ `/sys` (`getdents64` 4.3k), ghi `uevent` để coldboot và đọc
  `modalias` (dlkm_loader/modprobe nạp khoảng 58 module bằng `finit_module`).

## Socket

| Họ socket | Dùng cho |
|---|---|
| `AF_UNIX` `SOCK_STREAM` / `SOCK_SEQPACKET` / `SOCK_DGRAM` | property_service, logd (logdw dgram, logdr seqpacket), zygote, lmkd, tombstoned, adbd, statsd, netd (fwmark), … Cần `SO_PEERCRED` (getsockopt, khoảng 2 200 lần ở init), `SCM_RIGHTS`, `SCM_CREDENTIALS`, socket abstract và socket trên filesystem |
| `AF_NETLINK` `NETLINK_KOBJECT_UEVENT` | ueventd, vold, init, system_server |
| `AF_NETLINK` `NETLINK_ROUTE`/`NETFILTER`/`AUDIT`/`INET_DIAG`/`SOCK_DIAG` | netd, mdnsd, logd/auditd |
| `AF_INET`/`AF_INET6` `SOCK_RAW` (98) | netd, ping, connectivity (ND/RA, ICMPv6) |
| `AF_INET`/`AF_INET6` `SOCK_DGRAM`/`SOCK_STREAM` | DNS, mdnsd, ioctl `SIOC*` |
| `AF_KEY` (7) | netd (IPsec) |
| `AF_VSOCK` | adbd (adb qua vsock), các HAL Cuttlefish (`socket_vsock_proxy`) |

## anon inode và fd đặc biệt (sau khi boot)

Số đếm là số fd đang mở trên toàn hệ thống:

| Loại | Số fd |
|---|---:|
| eventfd | 439 |
| eventpoll (epoll) | 343 |
| bpf-map | 50 |
| userfaultfd | 42 |
| pidfd | 42 |
| sync_file | 25 |
| timerfd | 21 |
| bpf-prog | 21 |
| perf_event | 10 |
| inotify | 7 |
| signalfd | 2 |
| bpf_link | 1 |

Thêm memfd (`memfd:` với seal `F_ADD_SEALS`) và dma-buf.

## Kernel config dùng trong boot (GKI, trích `/proc/config.gz`)

**Bắt buộc, có bằng chứng sử dụng trong trace:**
- Android: `ANDROID_BINDER_IPC`, `ANDROID_BINDERFS` (`ANDROID_BINDER_DEVICES="binder,hwbinder,vndbinder"`),
  `ANDROID_BINDER_IPC_RUST=m` (đang dùng, do `binder.impl=rust`), `ASHMEM`, `DMABUF_HEAPS`
  (+ heap system), `SYNC_FILE`, `USERFAULTFD`, `MEMFD_CREATE`, `ANON_VMA_NAME`
  (prctl `PR_SET_VMA_ANON_NAME`), `PSTORE_PMSG`.
- Process và cô lập: `CGROUPS`, `MEMCG` (v2) và `MEMCG_V1`, `CPUSETS` (+`CPUSETS_V1`),
  `CGROUP_SCHED`, `UCLAMP_TASK(_GROUP)`, `CGROUP_FREEZER`, `BLK_CGROUP`, `CGROUP_BPF`,
  `PSI` (lmkd đọc `/proc/pressure/memory`), `NAMESPACES` với `UTS_NS` và `NET_NS`
  (mount ns luôn có), `SECCOMP_FILTER` (app có filter seccomp do zygote cài),
  `SECURITY_SELINUX`, `AUDIT`, `KEYS` (fscrypt, keystore).
- I/O: `EPOLL`, `EVENTFD`, `TIMERFD`, `SIGNALFD`, `INOTIFY_USER`, `AIO`, `IO_URING` (có,
  nhưng trace không thấy dùng), `PERF_EVENTS`, `BPF_SYSCALL` + `BPF_JIT`, `NETFILTER` +
  `IP_NF_IPTABLES` (iptables-restore của netd), `TUN`, `VSOCKETS`.
- Storage: `BLK_DEV_LOOP`, `DM_VERITY` (+`FEC`), dm-linear, `DM_DEFAULT_KEY`, `DM_BOW`,
  `DM_USER`, `EROFS_FS`, `F2FS_FS`, `EXT4_FS`, `FS_ENCRYPTION`, `FS_VERITY`, `FUSE_FS`,
  `ZRAM` (+swap), `VIRTIO_FS`.
- Thiết bị (Cuttlefish): `VIRTIO_PCI`, `VIRTIO_BLK`, `VIRTIO_CONSOLE` (hvc),
  `DRM_VIRTIO_GPU`, virtio-input (`INPUT_EVDEV`), `VIRTIO_VSOCKETS`, `VIRTIO_BALLOON`,
  `HW_RANDOM_VIRTIO`, `RTC_DRV_CMOS`, `SERIAL_8250` (earlycon).
- Boot: `BOOT_CONFIG` (Android đọc `androidboot.*` từ `/proc/bootconfig`), `MODULES` +
  `MODULE_SIG` (`finit_module`), `IKCONFIG_PROC`, `STATIC_USERMODEHELPER` (rỗng, tức là
  không có usermode helper), `FTRACE`.

**GKI tắt, nên kernel Rust không cần:** `USER_NS`, `PID_NS`, `TIME_NS`, `SYSVIPC`,
`POSIX_MQUEUE`, `FANOTIFY`, `CHECKPOINT_RESTORE`, và toàn bộ compat 32-bit trên image này.

## Gợi ý thứ tự triển khai (gắn với PLAN.md)

1. **B1–B4**: 154 syscall ở bảng trên. Khoảng 40 syscall đầu chiếm hơn 95% số lần gọi.
   Có thể stub (trả `-ENOSYS`/`-EPERM`) cho `quotactl`, `perf_event_open`,
   `process_mrelease`, `settimeofday` và `syslog` mà boot vẫn sống.
2. **C1 binder** qua binderfs: `WRITE_READ`, `SET_MAX_THREADS`, `VERSION`, `THREAD_EXIT`,
   `ENABLE_ONEWAY_SPAM_DETECTION`, `GET_NODE_INFO_FOR_REF`, `SET_CONTEXT_MGR_EXT`.
   Chỉ cần thiết bị `binder` và `hwbinder`.
3. **ashmem, memfd, dma-heap, sync_file, userfaultfd** trước khi chạy ART/zygote.
   Có thể ép ART tắt CMC qua `dalvik.vm.gctype` để hoãn userfaultfd.
4. **C2/C3**: device-mapper (linear + verity, hoặc tắt verity), loop + `LOOP_CONFIGURE`
   cho APEX, erofs (chỉ đọc), f2fs hoặc ext4 cho `/data`, cgroup v1 + v2 tối thiểu,
   selinuxfs ở chế độ permissive, netlink uevent, binderfs, pstore/pmsg (có thể thay bằng
   `/dev/null`), bpf (có thể để netbpfload báo lỗi; khi đó netd sẽ dừng).
5. **D1**: virtio-gpu DRM (dumb buffer + PRIME đủ cho SwiftShader và composer ranchu).
