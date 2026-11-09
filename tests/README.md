# tests/ — chương trình test syscall cho kernel Rust

Các chương trình userspace nhỏ, **static, không libc**, viết bằng Rust `no_std`
và gọi kernel trực tiếp bằng lệnh `syscall`. Mỗi chương trình là một ELF
x86_64 `ET_EXEC` (không PIE, không interpreter), nên kernel chỉ cần ELF loader
tối thiểu là chạy được — không phải đợi bionic/musl.

Linux trên host là **oracle**: mọi test phải PASS trên Linux trước khi dùng để
đánh giá kernel Rust. Kernel Rust "đạt mốc" khi chạy cùng binary và cho cùng
kết quả.

## Chạy nhanh

```bash
tests/run-host.sh            # build + chạy tất cả trên host, in tóm tắt
tests/run-host.sh -v b1      # chỉ test B1, in toàn bộ output
tools/mkinitramfs.sh         # -> out/ktest-initramfs.cpio cho kernel Rust
tools/run-initramfs-userns.sh  # thử initramfs đó như PID 1 trên host (không cần root)
```

Yêu cầu: Rust stable (1.85+, edition 2024), `cc` (làm linker), `python3`,
`strip`, `cpio` (chỉ cho `run-initramfs-userns.sh`). Không cần nightly.

## Danh sách test theo mốc kernel

Thứ tự chuẩn nằm ở [`tests.list`](tests.list) (runner và mkinitramfs đều đọc
file này).

| Mốc | Test | Kiểm tra |
|-----|------|----------|
| B1 | `b1_hello` | stack khởi đầu (argc/argv/envp/auxv: `AT_PAGESZ`, `AT_PHDR`, `AT_ENTRY`, `AT_RANDOM`), `write`, `getpid/gettid/getuid`, `uname`, `ENOSYS` cho syscall lạ |
| B1 | `b1_time` | `clock_gettime` (MONOTONIC/REALTIME/BOOTTIME), `clock_getres`, `nanosleep`, `clock_nanosleep(TIMER_ABSTIME)`, `gettimeofday`, `getrandom`, `sched_yield` |
| B1 | `b1_fork_wait` | `clone(SIGCHLD)`, `fork`, `vfork`, `wait4` (WNOHANG, -1, ECHILD), mã thoát, `kill`/SIGKILL, `CLONE_PARENT_SETTID` |
| B1 | `b1_execve` | `execve` truyền argv/envp, giữ pid, đóng fd `O_CLOEXEC`, reset signal handler / giữ SIG_IGN và mask, `ENOENT/EACCES/ENOEXEC`, `AT_EXECFN` |
| B1 | `b1_signals` | `rt_sigaction` + `sa_restorer`/`rt_sigreturn`, `SA_SIGINFO`, mask/pending, `SA_RESETHAND`, SIGSEGV/SIGILL/SIGFPE/SIGTRAP từ fault, `sigaltstack`, EINTR, `rt_sigsuspend`, `rt_sigtimedwait`, SIGCHLD |
| B1 | `b1_threads` | thread kiểu bionic (`CLONE_VM|THREAD|SIGHAND|...|CHILD_CLEARTID`), futex WAIT/WAKE/timeout, mutex futex có tranh chấp, `exit_group` giết mọi thread |
| B1 | `b1_tls` | `arch_prctl(ARCH_SET_FS/GS)`, đọc `%fs:0`, giữ qua context switch & fork, `CLONE_SETTLS`, `rdfsbase` (nếu có `HWCAP2_FSGSBASE`) |
| B2 | `b2_files` | `openat` (O_CREAT/EXCL/TRUNC/APPEND/DIRECTORY/NOFOLLOW), read/write/lseek/pread/pwrite/readv/writev, `fstat/newfstatat`, ftruncate, mkdir/rmdir/unlink/rename/link/symlink/readlink, dup/dup3/fcntl, chdir/getcwd, `close_range` |
| B2 | `b2_pipe` | `pipe2`, EOF, EPIPE/SIGPIPE, O_NONBLOCK, dung lượng pipe, reader bị chặn được đánh thức từ process khác, truyền 1 MiB |
| B2 | `b2_getdents` | `getdents64`: đủ entry, `d_type`, buffer nhỏ (đọc tiếp), EINVAL, ENOTDIR |
| B2 | `b2_devproc` | `/dev/null`, `/dev/zero`, `/dev/urandom`, `/proc/self/{exe,fd,stat,cmdline,maps,status}`, `/proc/{meminfo,cmdline,filesystems,mounts}`, `/sys/kernel` |
| B3 | `b3_mmap` | mmap ẩn danh, MAP_FIXED(_NOREPLACE), munmap, mprotect (đọc/ghi/thực thi), mremap, madvise(DONTNEED), 1 GiB lazy, `brk`, stack tự mở rộng |
| B3 | `b3_fork_cow` | COW sau fork theo cả hai chiều, `.data`, MAP_SHARED ẩn danh, trang read-only, stress 256 trang × 2 thế hệ, 50 lần fork |
| B3 | `b3_memfd` | `memfd_create`, hai view MAP_SHARED trùng nhau, seal (SHRINK/GROW/WRITE/SEAL), SIGBUS sau EOF, file mmap PRIVATE (COW) / SHARED (ghi xuống file), msync |
| B3 | `b3_shmem` | tên `/memfd:x (deleted)` trong /proc/self/fd và maps, futex trên bộ nhớ chia sẻ giữa hai process qua hai view khác địa chỉ, mremap file mapping, fallocate (grow/KEEP_SIZE/PUNCH_HOLE), truncate khi đang map (SIGBUS, đuôi trang về 0), F_SEAL_FUTURE_WRITE, MAP_SHARED /dev/zero, mprotect EACCES / msync lỗi |
| B4 | `b4_epoll` | `poll/ppoll`, epoll LT/ET/ONESHOT, MOD/DEL, EPOLLHUP, timeout, đánh thức từ process khác, epoll lồng nhau |
| B4 | `b4_eventfd` | `eventfd2` bộ đếm, EFD_SEMAPHORE, NONBLOCK, tràn, epoll |
| B4 | `b4_timerfd` | `timerfd_create/settime/gettime`, one-shot, periodic, ABSTIME, REALTIME/BOOTTIME |
| B4 | `b4_signalfd` | `signalfd4`, `signalfd_siginfo`, đổi mask, SIGCHLD qua epoll |
| B4 | `b4_poll` | `select/pselect6`, mask tạm của `ppoll` (EINTR, khôi phục mask), FIFO (luật open, O_NONBLOCK/ENXIO, POLLHUP), epoll EPERM/ELOOP, tên `anon_inode:[...]` trong /proc/self/fd và fdinfo |
| B4 | `b4_unix` | AF_UNIX stream/dgram/seqpacket, socketpair, bind/listen/connect/accept4 (đường dẫn & abstract), `SO_PEERCRED`, **SCM_RIGHTS**, `SCM_CREDENTIALS`, MSG_PEEK/TRUNC/CTRUNC, shutdown |
| C1 | `c1_binder` | binder (chỉ chạy trên kernel Rust, trên host thì SKIP): mount binderfs, `BINDER_CTL_ADD`, mmap chỉ đọc, context riêng cho từng device, context manager qua handle 0 (`BR_TRANSACTION_SEC_CTX`), gọi sync/oneway (đúng thứ tự), `sender_pid`, truyền binder object giữa 3 process và trả về chủ, gọi lồng nhau, truyền fd hai chiều, scatter-gather + fd array, `BR_SPAWN_LOOPER` và nhiều looper thread, death notification khi process con thoát, `BR_DEAD_REPLY`/`BR_FAILED_REPLY`, extended error |
| C2 | `c2_block` | đĩa test C2 (vda modern, vdb legacy): node `/dev/block`, phân vùng GPT (sysfs `dev/size/start/partition/uevent` có `PARTNAME`, `/sys/class/block`, `/sys/dev/block`, symlink `by-name`, `/proc/partitions`), ioctl `BLKGETSIZE64/BLKGETSIZE/BLKSSZGET/BLKPBSZGET/BLKBSZGET/BLKROGET/BLKROSET/BLKFLSBUF/BLKDISCARD/BLKZEROOUT`, ghi/đọc qua cache ở offset lẻ, EOF/ENOSPC ở cuối thiết bị |
| C2 | `c2_fs` | mount ro từng image (ext4 4K, ext3 1K + htree + indirect, erofs thường / lz4 compact / lz4 legacy) rồi so cả cây với cây gốc `/c2/tree` (tên, kiểu, mode, kích thước, nội dung, symlink), hard link, xattr `user.*`, `statfs`, mmap, `EROFS`, exec ELF từ image, `st_dev`, umount; lỗi mount `ENOTBLK/EINVAL/ENODEV` |
| C2 | `c2_loop` | `/dev/loop-control` (GET_FREE/ADD/REMOVE), `LOOP_CONFIGURE` (offset, sizelimit, read-only), `LOOP_GET/SET_STATUS64`, sysfs `loop/`, EBUSY, mount erofs từ loop, `LOOP_CLR_FD`, ghi tới file gốc, `LOOP_SET_CAPACITY`, AUTOCLEAR sau umount |
| C2 | `c2_dm` | ioctl device-mapper thô như libdm: VERSION, LIST_VERSIONS, GET_TARGET_VERSION, DEV_CREATE/REMOVE/RENAME/SUSPEND/STATUS, TABLE_LOAD/STATUS/DEPS, LIST_DEVICES; sysfs `slaves/` và `holders/` (D3b); target linear/zero/error, bảng nhiều target, verity (cho qua), mount erofs từ dm, EBUSY khi đang mount |
| C3 | `c3_selinux` | (chỉ kernel Rust) selinuxfs: enforce, load policy theo từng khúc, trang status mmap (sequence, policyload), transaction context/access/create, deny_unknown, null; `/proc/self/attr/*` và `/proc/thread-self/attr/*`, `exec` áp dụng khi execve, `fscreate` gán nhãn file mới; xattr trên tmpfs (set/get/list/remove, XATTR_CREATE/REPLACE, mặc định `unlabeled`); `SO_PEERSEC` |
| C3 | `c3_cgroup` | (chỉ kernel Rust) cgroup2: mount, controllers/subtree_control, mkdir uid_N/pid_M, chuyển process qua `cgroup.procs`, `/proc/<pid>/cgroup`, `cgroup.events` populated, rmdir EBUSY, fork kế thừa nhóm, `cgroup.kill`; cgroup v1: cpuset,noprefix (file không tiền tố, controller trong /proc/mounts, giá trị lấy từ cha), memory, `tasks`, mount lại cùng controller = cùng hierarchy, EBUSY |
| C3 | `c3_inotify` | `inotify_init1`, watch thư mục (CREATE, MODIFY, CLOSE_WRITE, ATTRIB, MOVED_FROM/TO cùng cookie, DELETE, ISDIR), watch file (DELETE_SELF + IGNORED, ONESHOT), MASK_ADD cùng wd, rm_watch, ONLYDIR, EAGAIN/EINVAL, FIONREAD, poll, read bị chặn được process khác đánh thức |
| C3 | `c3_keyctl` | keyring session, add_key user/logon/keyring, cập nhật bằng add_key lần hai, READ, DESCRIBE, SEARCH (cả keyring lồng nhau), LINK/UNLINK, SETPERM, REVOKE, request_key; ENOKEY, EKEYREVOKED, EOPNOTSUPP |
| C3 | `c3_netlink` | NETLINK_KOBJECT_UEVENT như ueventd (group 1, SO_PASSCRED, non-blocking), getsockname, EAGAIN; NETLINK_ROUTE dump kết thúc bằng NLMSG_DONE; trên kernel Rust: uevent khi ghi `change`/`add` vào file `uevent` của sysfs và khi hotplug loop (địa chỉ kernel, SCM_CREDENTIALS uid 0, payload KEY=value), symlink sysfs vào /sys/devices |
| C3 | `c3_misc` | `waitid` (WNOWAIT, WNOHANG, siginfo), `sendfile`, `copy_file_range`, pty (/dev/ptmx, unlockpt, TIOCGPTN, /dev/pts/N, raw mode, hang-up hai chiều), magic link /proc/self/fd (mở lại memfd), /proc/self/ns/mnt, mountinfo, oom_score_adj, prctl (CHILD_SUBREAPER, PDEATHSIG), ioprio; trên kernel Rust: /proc/sys đọc lại giá trị vừa ghi, hostname qua sysctl, /proc/bootconfig, MS_MOVE, unshare/setns |
| C3 | `c3_kmsg` | bộ đệm log kernel như logd/init dùng: ghi `/dev/kmsg` (`<N>`, facility/level), đọc từng bản ghi `prio,seq,usec,-;text`, seq tăng, vị trí riêng mỗi fd (SEEK_SET 0 / SEEK_END), EAGAIN khi hết, EINVAL khi buffer nhỏ, poll POLLIN, reader bị chặn được process khác đánh thức; `syslog` (SIZE_BUFFER, READ_ALL dạng `<N>[time] text`, SIZE_UNREAD); `/proc/kmsg` mở được, epoll, dòng mới (cần root trên host: SKIP) |
| C3 | `c3_creds` | credentials: getres[ug]id, set[res][ug]id/setre[ug]id với luật Linux (root đặt cả ba id, không phải root chỉ id mình có, EPERM), setfsuid, setgroups/getgroups, kế thừa qua fork, /proc/self/status Uid/Gid/Groups, chủ của /proc/<pid>; SO_PEERCRED và SCM_CREDENTIALS (ngầm và tường minh) sau khi hạ uid; chủ file mới (fs id), thư mục set-group-ID; exec file set-user-ID (euid 0, AT_SECURE) (cần root: SKIP trên host) |
| C3 | `c3_sysfs` | cây thiết bị sysfs mà coldboot của ueventd duyệt, kiểm như nhau trên Linux và kernel Rust: /sys/class/{mem,tty,misc,block}/X là symlink vào /sys/devices, `uevent` có MAJOR/MINOR khớp `dev`, DEVNAME, link `subsystem`, /sys/dev/{char,block}/M:m cùng chỗ; /sys/block là disk (DEVTYPE=disk, size, queue/), phân vùng DEVTYPE=partition; số thiết bị của /dev/null... khớp sysfs; trên kernel Rust: hw_random 10:183, trigger PSI trên /proc/pressure/memory với epoll |
| C3 | `c3_mntns` | mount namespace và shared subtree như init dựng (bootstrap/default, /bootstrap-apex bind lên /apex): MS_SHARED/MS_PRIVATE, bind của mount shared là peer (mount/umount bên dưới lan sang nhau), private không nhận gì, mountinfo `shared:N`; unshare(CLONE_NEWNS) và clone(CLONE_NEWNS) cho id `ns/mnt` mới và bản sao bảng mount (mount private ở lại bên trong, shared lan ra ngoài), setns quay lại; chạy trong user namespace nên host không cần root |
| C3 | `c3_bpf` | (chỉ kernel Rust; trên host không có CAP_BPF nên SKIP) eBPF như bpfloader/netd/libbpf dùng: map array, hash (NOEXIST/EXIST, đầy -> E2BIG, duyệt bằng GET_NEXT_KEY), LRU hash loại khoá cũ nhất, per-CPU array, LPM trie (khớp prefix dài nhất), array BPF_F_MMAPABLE dùng chung trang với mmap, mmap ring buffer, MAP_FREEZE, fd chỉ đọc, OBJ_GET_INFO_BY_FD, id (GET_NEXT_ID, GET_FD_BY_ID, giải phóng khi đóng fd cuối); bpffs: pin, OBJ_GET, rename, chmod, unlink, EEXIST, EPERM ngoài bpffs; PROG_LOAD (giữ map qua ld_imm64), prog info, EBADF/EINVAL, attach/query/detach vào cgroup2; BTF_LOAD; khoá OFD trên fd map như mapLockTest của netd |
| C3 | `c3_net` | AF_INET/AF_INET6: TCP loopback (bind cổng tạm, getsockname/getpeername, EADDRINUSE, connect/accept4, dữ liệu hai chiều, FIONREAD, MSG_PEEK, shutdown/EOF, 1 MiB qua process con, ECONNREFUSED, EADDRNOTAVAIL), connect non-blocking (EINPROGRESS, POLLOUT, SO_ERROR), epoll, dual-stack (peer v4-mapped, ::1), UDP (sendto/recvfrom có địa chỉ nguồn, autobind, MSG_PEEK, MSG_TRUNC, connect, v4 -> socket v6); rtnetlink (dump link/addr/route, GETLINK theo index), ioctl giao diện trên lo (SIOCGIFINDEX/NAME/FLAGS/ADDR/NETMASK/MTU/HWADDR/CONF, ENODEV); chỉ kernel Rust: RTM_NEWADDR/DELADDR có multicast, đổi tên link đang up -> EBUSY, eth0 (virtio-net: 10.0.2.15, TCP tới gateway 10.0.2.2 qua ARP -> ECONNREFUSED, /proc/net/arp), sockopt iptables/ip6tables (GET_INFO, GET_ENTRIES, SET_REPLACE trả counters cũ, revision) |
| D1 | `d1_dumb` | (chỉ khi có virtio-gpu) `/dev/dri/card0` + `renderD128` (tên driver `virtio_gpu`, GET_UNIQUE, sysfs class drm, uevent `DEVNAME=dri/card0`), GET_CAP/SET_CLIENT_CAP, CREATE_DUMB (pitch, size, tham số sai), MAP_DUMB + mmap offset giả (hai mapping và process con dùng chung trang), offset sai, handle riêng theo file, GEM_CLOSE/DESTROY_DUMB, render node (ioctl KMS -> EACCES, dumb buffer được), GETPARAM virtgpu của thiết bị 2D |
| D1 | `d1_kms` | (chỉ khi có virtio-gpu) resources/connector/encoder/plane (có và không UNIVERSAL_PLANES), property (tên, enum `type`, ẩn property atomic khi chưa bật ATOMIC), ADDFB/ADDFB2/GETFB/RMFB, SETCRTC/GETCRTC, PAGE_FLIP + event đọc từ fd, DIRTYFB, WAIT_VBLANK, atomic (TEST_ONLY, luật ALLOW_MODESET, kiểm tra plane/fb, OUT_FENCE_PTR -> sync file đã signal, flip event, IN_FENCE_FD chờ fence sw_sync), property blob, đóng fd thì tắt màn hình |
| D1 | `d1_prime` | PRIME HANDLE_TO_FD/FD_TO_HANDLE, fd dma-buf (`/dmabuf:`, lseek SEEK_END, mmap chung trang với DRM, poll, DMA_BUF_IOCTL_SYNC, SET_NAME, fdinfo, EXPORT_SYNC_FILE), import vào render node khác và trong process con, buffer sống sau GEM_CLOSE; `/dev/dma_heap/system` (ALLOC, tham số sai, mmap, import vào DRM làm framebuffer) — phần heap chạy cả trên host |
| D1 | `d1_sync` | (cần sw_sync) SW_SYNC_IOC_CREATE_FENCE/INC, poll trước/sau, SYNC_IOC_FILE_INFO (status, tên driver/timeline, timestamp, num_fences quá nhỏ -> EINVAL), SYNC_IOC_MERGE (cùng timeline giữ điểm sau, hai timeline = hai fence), poll/epoll được process khác đánh thức, `anon_inode:sync_file`, đóng timeline làm fence lỗi |
| D2 | `d2_evdev` | uinput (UI_SET_*BIT, UI_DEV_SETUP, UI_ABS_SETUP, UI_DEV_CREATE, UI_GET_SYSNAME; thiết bị giả làm joystick để desktop trên host bỏ qua) rồi node `eventN`: EVIOCGVERSION/GID/GNAME (cắt ngắn)/GUNIQ/GPROP/GBIT (type, code, type lạ -> EINVAL)/GABS/SABS/GKEY/GLED/GMTSLOTS, EVIOCSCLOCKID (timestamp monotonic), read cả input_event (buffer ngắn -> EINVAL, rỗng -> EAGAIN), lọc sự kiện không đổi, gói tới SYN_REPORT, multitouch slot, epoll và read chặn được process khác đánh thức, hai client + EVIOCGRAB, EVIOCSMASK (theo code và theo type), ghi LED vào node -> client và phía uinput, tràn hàng đợi -> SYN_DROPPED, UI_DEV_DESTROY -> ENODEV/POLLHUP; chỉ kernel Rust: thiết bị virtio-input của VM test (tên, phím, LED, trục tablet, sysfs class/input, uevent) |
| D2 | `d2_tcp` | TCP: 8 MiB mỗi chiều cùng lúc qua 127.0.0.1 và 2 MiB qua ::1 (kiểm từng byte), backpressure non-blocking (EAGAIN, mất POLLOUT, có lại khi bên đọc rút), MSG_WAITALL, shutdown (SHUT_WR: EOF + POLLRDHUP bên kia, EPIPE, half-close; SHUT_RD; EINVAL/ENOTCONN), reset (SO_LINGER 0 và close còn dữ liệu chưa đọc: ECONNRESET một lần rồi EOF/EPIPE, SO_ERROR, POLLERR|POLLHUP; ghi vào peer đã đóng -> RST), 64 connect non-blocking cùng lúc vào một listener, accept hết và echo |
| D2 | `d2_udp` | UDP: 64 datagram xếp hàng đọc lại đúng thứ tự, datagram 60000 byte qua IPv4 và IPv6, MSG_TRUNC, recvmsg có địa chỉ nguồn, socket connect chỉ nhận từ peer, SO_RCVTIMEO, port 0 -> EINVAL; ICMP ping socket (SOCK_DGRAM + IPPROTO_ICMP/ICMPV6) tới 127.0.0.1 và ::1; chỉ kernel Rust: eth0 qua mạng user của QEMU (ping 10.0.2.2, DNS UDP tới 10.0.2.3, datagram 4000 byte bị phân mảnh, IPv6: link-local fe80::, địa chỉ SLAAC fec0::/64, route mặc định trong /proc/net/ipv6_route, ping6 fec0::2, TCP6 tới cổng đóng -> ECONNREFUSED), SIOCETHTOOL (GDRVINFO, GLINK, GPERMADDR, GSET, lo -> EOPNOTSUPP, ENODEV), /proc/net/{snmp,sockstat,dev,protocols} |
| D3 | `d3_uffd` | userfaultfd như GC CMC của ART dùng: cờ của userfaultfd(2) (UFFD_USER_MODE_ONLY), read/poll trước UFFDIO_API (EINVAL/POLLERR), các feature (SIGBUS, MISSING/MINOR_SHMEM, MOVE), MREMAP_DONTUNMAP với bộ nhớ chia sẻ ẩn danh, UFFDIO_REGISTER (ioctls trả về, EINVAL, EBUSY); lỗi trang MISSING đánh thức monitor (thread bị chặn, uffd_msg, POLLIN) rồi được UFFDIO_COPY giải quyết; kết quả COPY/ZEROPAGE (EEXIST, EAGAIN kèm số byte); chế độ SIGBUS (BUS_ADRERR đúng địa chỉ, handler điền trang, kernel truy cập -> EFAULT); mẫu register + mremap(DONTUNMAP) của ART; lỗi MINOR trên shmem với UFFDIO_CONTINUE; UFFDIO_COPY vào shmem; UFFDIO_MOVE (ALLOW_SRC_HOLES); con fork không thừa kế; UNREGISTER, close; /dev/userfaultfd (USERFAULTFD_IOC_NEW, trên host thường bị bỏ qua) |
| D3 | `d3_proc` | pidfd (pidfd_open, POLLIN khi tiến trình thoát, pidfd_send_signal, clone(CLONE_PIDFD), pidfd_getfd), process_madvise, process_mrelease; membarrier; futex PI (LOCK_PI/LOCK_PI2/TRYLOCK_PI/UNLOCK_PI, FUTEX_WAITERS, EDEADLK, EPERM, trao khoá cho thread đang chờ); startstack trong /proc/self/stat nằm trong [stack]; VMA bị mprotect tách ra gộp lại (mremap cả vùng); access(W_OK) trên mount chỉ đọc -> EROFS; AF_KEY; /dev/net/tun; /dev/hvc1; ashmem; AF_VSOCK cục bộ; AF_PACKET trên eth0 (ARP gửi bằng frame thô, nhận qua bộ lọc BPF cổ điển dùng SKF_NET_OFF); /proc/config.gz; /proc/self/cmdline theo argv[0] trong bộ nhớ; thời gian CPU trong /proc/self/stat; wchan. Trên host: các phần cần quyền root/thiết bị bị bỏ qua |
| D3 | `d3_splice` | splice(2) như `FileUtils.copy` của PackageInstaller dùng cho `adb install`: socket -> pipe -> file lặp đến EOF (nhiều lần gọi, dữ liệu đúng, vị trí file dịch chuyển), file -> pipe với offset (vị trí file không đổi), pipe -> file với offset và số byte ngắn khi pipe ít dữ liệu hơn, độ dài 0, EINVAL khi không bên nào là pipe, ESPIPE khi có offset phía pipe, EBADF khi đọc từ đầu ghi, EOF |
| D3 | `d3_timers` | timer POSIX: timer_create (sigevent mặc định = SIGALRM, SIGEV_SIGNAL với SI_TIMER/si_timerid/si_value, SIGEV_THREAD_ID, SIGEV_NONE, EINVAL), timer_settime (tương đối, TIMER_ABSTIME trên CLOCK_REALTIME, tuần hoàn, tắt, giá trị cũ), timer_gettime, timer_getoverrun, timer_delete; setitimer/getitimer(ITIMER_REAL) gây SIGALRM, alarm() trả số giây còn lại |

`ktest-init` không phải test mà là runner (xem dưới).

## Định dạng output (giao thức)

Mỗi chương trình in ra **stdout (fd 1)**, mỗi dòng là một lần `write(2)` duy
nhất (không bị xen giữa các process):

```
BEGIN <test>
PASS <test> <case>
FAIL <test> <case> <chi tiết> [<file>:<dòng>]
SKIP <test> <case> <lý do>
# <ghi chú tự do>
END <test> pass=<số PASS> fail=<số FAIL>
```

- `BEGIN` luôn là dòng đầu, `END` là dòng cuối của test.
- `<test>` và `<case>` không có dấu cách; `<chi tiết>` thì có thể có.
- Dòng bắt đầu bằng `#` là ghi chú, bỏ qua khi chấm.
- Panic: in `FAIL <test> panic <thông điệp> [file:dòng]`, rồi `END`, thoát mã 101.
- Process con (sau `fork`) không in `PASS`; nó báo kết quả qua mã thoát, process
  chính kiểm tra rồi in `PASS/FAIL`. Nếu process con panic, nó in một dòng
  `FAIL ... panic` (không có `END`) và thoát 101.
- **Mã thoát**: `0` nếu không có case nào FAIL, `1` nếu có, `101` nếu panic.
- Test chỉ có `SKIP` (không có `PASS`) được coi là đạt khi mã thoát 0 (ví dụ
  `c1_binder` trên host, vì binder chỉ có trên kernel Rust; các test `c2_*`
  trên host vì cần đĩa test C2 — `tools/mkc2-images.py`, được
  `tools/run-qemu.sh --ktest` gắn vào — và quyền root cho loop/dm).

Một test **đạt** khi: mã thoát 0, có đúng một `BEGIN`/`END`, không có dòng
`FAIL`, số `PASS` > 0 (hoặc chỉ có `SKIP`) và khớp với `pass=` trong `END`. `run-host.sh` kiểm tra
đúng các điều kiện này (và coi mọi dòng lạ là lỗi).

Ví dụ:

```
BEGIN b2_pipe
PASS b2_pipe pipe2
PASS b2_pipe pipe_fds_distinct
...
END b2_pipe pass=27 fail=0
```

## Runner trong guest: `ktest-init`

`ktest-init [DIR] [FILTER...]` chạy lần lượt các test trong `tests.list` từ
thư mục `DIR` (mặc định `/tests`; tham số bắt đầu bằng `/` hoặc `.` là DIR).
`FILTER` là tiền tố tên (`b1`, `b3_mmap`). Mỗi test là một case của chính
`ktest-init`, nên output lồng nhau nhưng vẫn theo cùng giao thức:

```
BEGIN ktest-init
BEGIN b1_hello
...
END b1_hello pass=22 fail=0
PASS ktest-init b1_hello
...
END ktest-init pass=19 fail=0
```

Timeout 120 s mỗi test (SIGKILL rồi `FAIL ktest-init <test> timeout ...`).

Khi là **PID 1**, trước khi chạy nó `mount` proc → `/proc`, sysfs → `/sys`,
devtmpfs → `/dev`, tmpfs → `/tmp` (lỗi mount chỉ in dòng `#`), reap các
process mồ côi, và cuối cùng gọi `reboot(LINUX_REBOOT_CMD_POWER_OFF)` (PID 1
không được exit). Tham số kernel cmdline sau `--` được kernel chuyển thành argv
của init, nên `... -- b1` chỉ chạy B1.

Kernel Rust nên ghi `/dev/console` ra serial; harness QEMU chỉ cần tìm dòng
`END ktest-init pass=N fail=0`.

## initramfs: `tools/mkinitramfs.sh`

```bash
tools/mkinitramfs.sh                    # out/ktest-initramfs.cpio, /init = ktest-init
tools/mkinitramfs.sh --gzip             # .cpio.gz
tools/mkinitramfs.sh --init b1_hello -o out/b1_hello.cpio   # 1 test làm /init (trước khi có fork/execve)
```

Cấu trúc archive (định dạng `newc`, do `tools/mkcpio.py` sinh ra, không cần root
để tạo device node, tái lập byte-cho-byte):

```
/init                     ktest-init (hoặc test được chọn)
/tests/ktest-init, /tests/<test>...
/dev/console (c 5,1)  /dev/tty (c 5,0)  /dev/null (c 1,3)  /dev/zero (c 1,5)
/dev/random (c 1,8)   /dev/urandom (c 1,9)
/proc  /sys  /tmp (1777)          điểm mount rỗng
```

Binary trong initramfs đã `strip --strip-debug` (vẫn giữ symbol table). Để
symbolize một fault trong guest, dùng bản chưa strip ở
`tests/target/x86_64-unknown-linux-gnu/release/<test>` (`addr2line -e ...`).

Với `--init <test>`: khi test thoát, init kết thúc — kernel Rust nên coi đó là
hết test và in mã thoát thay vì panic.

`tools/run-initramfs-userns.sh [CPIO] [-- FILTER...]` giải nén archive và chạy
`/init` như PID 1 trong user/pid/mount/net namespace mới trên kernel host
(bind-mount `/dev` của host vì không tạo được device node khi không có root).
Dùng để kiểm tra chính archive và đường chạy PID 1 của `ktest-init`.

## Cấu trúc mã

```
Cargo.toml, .cargo/config.toml   workspace riêng; link static, -nostartfiles, non-PIE
src/lib.rs     runtime "rt": _start, đọc argv/envp/auxv, macro check!/check_eq!/
               check_ok!/check_err!, println!, run_child (fork+wait), spawn/join
               thread, mem*/strlen, panic handler
src/sys.rs     macro syscall!, hằng số ABI (errno, O_*, MAP_*, SIG*, CLONE_*,
               EPOLL*...), struct ABI (Stat, Sigaction, EpollEvent packed, Msghdr...)
src/nr.rs      số syscall x86_64
src/bin/*.rs   một file một test
tests.list     thứ tự chạy
run-host.sh    runner trên host (oracle)
```

### Viết test mới

```rust
#![no_std]
#![no_main]
use rt::*;
rt::test_main!("b2_example", main);

fn main() {
    let fd = check_ok!(open(c"/tmp".as_ptr() as *const u8, O_RDONLY | O_DIRECTORY, 0), "open_tmp");
    check_err!(read(fd as i32, &mut [0u8; 4]), EISDIR, "read_dir_eisdir");
}
```

Thêm tên vào `tests.list` (đúng mốc), chạy `tests/run-host.sh b2_example` cho
tới khi PASS trên Linux. Quy tắc:

- Test phải tự dọn dẹp (dùng `scratch_dir()` → `/tmp/ktest-<test>-<pid>`), không
  phụ thuộc cwd, stdin hay quyền root, và chạy được song song với chính nó.
- Không dựa vào thời gian chính xác: chỉ kiểm tra cận dưới (không ngủ ít hơn) và
  cận trên rất rộng.
- Tên case là định danh ổn định (snake_case) để so sánh kết quả giữa Linux và
  kernel Rust theo thời gian.
- Test re-exec chính nó dùng `is_child_mode()` (argv[1] == `--rt-child`).
