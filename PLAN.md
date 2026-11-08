# android-pc — Kernel Android viết bằng Rust cho x86_64

## Mục tiêu

Viết một kernel mới bằng **Rust** chạy trên x86_64, đủ tương thích để chạy
**userspace Android (AOSP) biên dịch từ mã nguồn** và cài/chạy được **APK**.

## Chiến lược cốt lõi

Không viết lại Android, chỉ viết lại kernel. Userspace AOSP (bionic, init,
servicemanager, ART/zygote, system_server, SurfaceFlinger…) chỉ nói chuyện với
kernel qua **Linux x86_64 syscall ABI** cộng vài driver đặc thù của Android.
Vậy kernel Rust cần:

1. Cài đặt Linux syscall ABI (giống cách Fuchsia/Starnix và Asterinas làm).
2. Các thành phần riêng của Android: **binder** (`/dev/binder`, `hwbinder`,
   `vndbinder`, binderfs), memfd/ashmem, dma-buf heaps, sync_file, eventfd,
   epoll, inotify, `/proc` & `/sys` đủ cho init, SELinux (bước đầu: permissive /
   stub), cgroups v2 + `/dev/cpuset` tối thiểu.
3. Driver phần cứng: bắt đầu với **QEMU/KVM + virtio** (blk, net, gpu, input,
   console) — rồi mới tới phần cứng thật.

Kernel Linux Android chính chủ (GKI x86_64) được dùng làm **mốc tham chiếu**: cùng
một bản AOSP phải boot được trên cả GKI và kernel Rust, để so sánh hành vi
syscall (strace/log).

APK: build app mẫu từ mã nguồn (Gradle hoặc Soong), cài qua `adb`. Chỉ hỗ trợ APK
x86_64 / Java thuần — APK chỉ có thư viện ARM cần lớp dịch nhị phân (ngoài phạm vi).

## Môi trường host (đã kiểm tra 2026-11-08)

- i7-13700K (24 luồng), 31 GB RAM, KVM có sẵn, GPU NVIDIA RTX 4070.
- Rust 1.99 stable (cần thêm nightly + `rust-src` cho `no_std`/`build-std`).
- **Đĩa trống ~251 GB** — AOSP đầy đủ + build chiếm ~200+ GB → bắt buộc
  `repo init --partial-clone --clone-filter=blob:limit=10M -c --depth=1` và
  build một product tối thiểu. Đây là rủi ro lớn nhất về tài nguyên.
- Chưa có `qemu-system-x86_64` → cần cài.

## Cấu trúc repo

```
kernel/        crate Rust no_std của kernel (workspace cargo)
tests/         chương trình test userspace (static, bionic & musl), conformance syscall
apps/          APK mẫu (mã nguồn)
tools/         script chạy QEMU, tạo image, so sánh strace
aosp/          checkout AOSP (bị .gitignore, rất lớn)
docs/          ghi chú thiết kế, bảng syscall đã/ chưa cài
```

## Các giai đoạn

Mỗi việc dưới đây chạy trong một sub-session riêng. Các việc đánh dấu **[song song]**
không phụ thuộc nhau và có thể chạy cùng lúc.

### Giai đoạn A — Nền móng (song song)

- **A1. Kernel Rust boot được [song song]** — `kernel/`: target
  `x86_64-unknown-none`, bootloader Limine (UEFI + BIOS), serial log, GDT/IDT,
  ngắt, phân trang 4 cấp, physical frame allocator, heap, APIC timer, SMP sơ khai.
  Xong khi: `tools/run-qemu.sh` boot kernel trong QEMU/KVM, in log, chạy test
  `cargo test` kiểu custom test framework trong QEMU.
- **A2. AOSP tham chiếu trên x86_64 [song song]** — `aosp/`: sync AOSP (nhánh
  android-16 release) dạng partial clone, build `aosp_cf_x86_64_phone` hoặc
  product tối thiểu, boot trong QEMU với kernel GKI x86_64. Ghi lại danh sách
  syscall/ioctl/thiết bị mà init → zygote → system_server dùng (strace / `ltrace`)
  vào `docs/syscall-trace.md`.
- **A3. Bộ test & APK mẫu [song song]** — `tests/` + `apps/`: test syscall nhỏ
  (static, x86_64) chạy được trên Linux host để làm "oracle"; APK mẫu x86_64 build
  bằng Gradle/command-line SDK từ mã nguồn; script `tools/`.

### Giai đoạn B — Lõi tương thích Linux (tuần tự, sau A1)

- **B1. Tiến trình & ELF**: user mode (ring 3), `syscall`/`sysret`, ELF loader
  (static rồi dynamic với linker của bionic), `execve`, `fork/clone`, `wait4`,
  `exit_group`, TLS (`arch_prctl`), signal cơ bản, futex.
- **B2. VFS**: tmpfs, ramfs/initramfs (cpio), devfs, procfs, sysfs tối thiểu,
  pipe, `openat/read/write/mmap/stat/getdents64`, mount namespace đơn giản.
- **B3. Bộ nhớ ảo đầy đủ**: `mmap/munmap/mprotect/brk`, COW cho fork, file-backed
  mmap, `memfd_create`, shared memory.
- **B4. I/O & đồng bộ**: epoll, eventfd, timerfd, signalfd, socket AF_UNIX
  (stream/dgram/seqpacket, SCM_RIGHTS, SO_PEERCRED), poll/select.
- Mốc: chạy được các test trong `tests/` và một shell (`toybox sh` của AOSP).

### Giai đoạn C — Đặc thù Android (sau B)

- **C1. Binder** (theo ABI binder của Linux, có thể tham khảo Rust Binder upstream
  để hiểu ngữ nghĩa): transaction, ref counting, death notification, binderfs.
- **C2. Thiết bị khối & FS thật**: virtio-blk, ext4 (đọc trước, ghi sau) hoặc
  erofs cho system/vendor, dm-verity tắt, partition dạng super (dm-linear).
- **C3. init của Android**: chạy `/init` first/second stage, property service,
  ueventd, SELinux stub, cgroups, `servicemanager`, `logd`, `adbd` qua virtio-console
  hoặc virtio-net (TCP).
- Mốc: `adb shell` vào kernel Rust.

### Giai đoạn D — Đồ hoạ, ART, APK (sau C)

- **D1. virtio-gpu + DRM/KMS** tối thiểu, dma-buf, sync_file → SurfaceFlinger
  với `drm_hwcomposer` + Mesa (virgl hoặc software `swiftshader`/llvmpipe trước).
- **D2. virtio-input, virtio-net** đầy đủ.
- **D3. zygote → system_server → Launcher**; cài APK mẫu bằng `adb install`, chạy
  được Activity.
- Mốc cuối: APK build từ mã nguồn chạy trên kernel Rust trong QEMU.

### Giai đoạn E — Phần cứng thật (tuỳ chọn)

NVMe/AHCI, xHCI USB (bàn phím/chuột), Intel i915/AMD qua DRM. Lưu ý: NVIDIA
RTX 4070 của máy này không có driver mở khả dụng cho kernel tự viết → thực tế
chạy phần cứng thật cần GPU Intel/AMD, hoặc chỉ dùng framebuffer UEFI GOP +
render phần mềm.

## Rủi ro chính

- Phạm vi cực lớn (Starnix mất nhiều năm với đội lớn). Ưu tiên chạy được từng mốc.
- Dung lượng đĩa cho AOSP.
- Đồ hoạ là phần khó nhất; dùng render phần mềm trước.
- Phương án dự phòng nếu tiến độ quá chậm: fork Asterinas (kernel Rust tương thích
  Linux ABI, MPL-2.0) rồi bổ sung binder & phần Android.
