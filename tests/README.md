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
| C2 | `c2_dm` | ioctl device-mapper thô như libdm: VERSION, LIST_VERSIONS, GET_TARGET_VERSION, DEV_CREATE/REMOVE/RENAME/SUSPEND/STATUS, TABLE_LOAD/STATUS/DEPS, LIST_DEVICES; target linear/zero/error, bảng nhiều target, verity (cho qua), mount erofs từ dm, EBUSY khi đang mount |

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
