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

- **E1.** NVMe, AHCI, xHCI + bàn phím/chuột USB HID.
- **E2.** simpledrm trên framebuffer của firmware (GOP/VBE).
- **E3.** Boot từ USB stick trên PC thật: USB mass storage (Bulk-Only + SCSI),
  hub USB, bảng ACPI (MADT, FADT/PM timer, HPET), hiệu chuẩn timer không cần
  PIT, log kernel trên framebuffer, `tools/mkusb.sh`,
  `run-qemu.sh --usb-boot-test`; hướng dẫn: `docs/usb-boot.md`. Đã chạy trên
  PC thật (Z790): màn hình, bàn phím/chuột USB (sửa doorbell xHCI, TT cho hub).
- **E4.** Mạng có dây Realtek RTL8125 2.5 GbE (`drivers/rtl8125.rs`), báo
  trạng thái carrier trong lớp netdev cho EthernetTracker/DHCP.
- **E5.** Âm thanh: Intel HD Audio (codec Realtek ALC897, HDMI của NVIDIA) với
  ABI ALSA PCM/control; `run-qemu.sh --aosp-audio-test`.
- **E7.** Bluetooth Intel AX211 qua USB (btusb + nạp firmware Intel), nối HAL
  qua `/dev/hvc5`, `/dev/uhid` cho HID Bluetooth; `run-qemu.sh --aosp-bt-test`.
- **E8.** Webcam USB: isochronous xHCI, UVC thành V4L2 `/dev/video0`, micro
  UAC trên lõi ALSA của E5 (Android ghi âm được); ứng dụng Camera cần sửa
  vendor image.
- **E9.** Nền tảng PC: tắt máy/khởi động lại qua ACPI, nút nguồn, RTC CMOS
  (`/dev/rtc0`), nhiệt độ CPU, HWP/cpufreq, idle MWAIT, nguồn AC.
- **E6.** Wi-Fi: generic netlink + nl80211, lớp kiểu cfg80211/mac80211 cho
  wpa_supplicant (SME phía user), rfkill, radio mô phỏng (`wifisim=`, AP
  WPA2 nối ra virtio-net) với `run-qemu.sh --aosp-wifi-test`; driver
  iwlwifi cho Intel AX211 (firmware 89, MLD API) — xem `docs/wifi.md`.

### Phase F — AOSP 17 from source + Rust HALs

Goal: move the AOSP tree to **android-17.0.0_r1**, built from source, and add
our own **vendor HALs written in Rust** (AIDL, `rust_binary` in Soong) for the
PC hardware that phase E drives. Decisions (2026-11-10): upgrade `aosp/` **in
place** (only ~52 GB free on `/`, so no second tree); the Rust drivers are
**Android HALs in userspace**, not kernel modules.

Every step runs in its own sub-session. Builds stay **sequential** (one at a
time, `tools/limited-build.sh`, see CLAUDE.md), so the HAL steps after F2 can
be coded in parallel but must take turns building.

- **F1. AOSP 17 in place.** Keep the AOSP 16 images the `tools/aosp-*-test.py`
  scripts need in `out/aosp16-images/`, delete `aosp/out/`, `repo init -b
  android-17.0.0_r1` with the same partial-clone flags, `repo sync -j4`,
  re-check the local fixes from `docs/aosp-build.md` (`RELEASE_USE_DEX_V41`,
  `trim.xml`), build `aosp_cf_x86_64_only_phone` image by image, boot it on
  Cuttlefish with the android17 GKI. Done when: `sys.boot_completed=1`, `adb
  shell` works, `docs/aosp-build.md` describes AOSP 17.
  **Done (2026-11-11):** `aosp_cf_x86_64_only_phone-cp2a-userdebug`
  (CP2A.260605.016) with GKI `6.18.16-android17` boots in ~40 s, built by
  `tools/aosp17-build.sh`. The DEX v41 fix is gone; `trim.xml` keeps
  `device/generic/car`; Soong needs a `GOMEMLIMIT` patch and siso takes no
  `-jN`; guest RAM must be 4 GB (ramoops at 0x150000000). AOSP 16 images are
  in `out/aosp16-images/`.
- **F2. Our product + Rust HAL scaffolding.** A device tree kept in this repo
  (`aosp-device/`, linked into `aosp/device/android-pc/pc_x86_64` by a local
  manifest or symlink) for `aosp_pc_x86_64` inheriting the Cuttlefish
  x86_64_only phone; a shared Rust crate (`libpchal`: logging, binder service
  registration, sysfs/devfs helpers); VINTF manifest fragments, init `.rc`,
  SELinux policy for HAL services; one trivial Rust HAL as proof. Done when:
  the product builds and the proof HAL is registered (`lshal`/`service list`).
  **Done (2026-11-11):** `aosp-device/` (copied into `aosp/device/android-pc/pc_x86_64`
  by `tools/aosp-device-link.sh`; symlinks leak into the image) defines
  `aosp_pc_x86_64-cp2a-userdebug` (`PRODUCT_DEVICE pc_x86_64`, own BoardConfig for
  vendor sepolicy), `libpchal`, and the `hals/<name>/` pattern (hal.mk, rc, VINTF
  fragment, sepolicy via `pc_hal_service`). Proof HAL: Rust `android.hardware.light`
  over `/sys/class/leds` + `/sys/class/backlight`, replacing Cuttlefish's lights HAL;
  boots, registered, VINTF clean. `tools/aosp17-build.sh` takes `PRODUCT`; see
  `docs/rust-hals.md`.
- **F3. Kernel for AOSP 17 on the real PC** — split in two options
  (decision 2026-11-11: do F3a, then F4–F8 on it; F3b only after all of
  that is finished).
  - **F3a. Stock Linux kernel (GKI android17-6.18).** Build the GKI kernel
    and vendor modules from source for the PC hardware of phase E (NVMe,
    AHCI, xHCI/HID/usb-storage, simpledrm/i915, r8169/RTL8125, HD Audio,
    iwlwifi + btusb AX211, uvcvideo, ACPI/thermal/RTC), pack them into
    `aosp_pc_x86_64` (`vendor_dlkm`/`vendor_boot`), and boot AOSP 17 from a
    USB stick on the real PC. Done when: AOSP 17 reaches the launcher on the
    Z790 PC from USB with display, input, network; Cuttlefish still boots.
    *Done 2026-11-11:* on the Z790 PC from USB: launcher at 2560x1080, USB
    keyboard/mouse, Ethernet, Wi-Fi (AX211, Cuttlefish's Wi-Fi HAL), Bluetooth
    (AOSP's default HAL over an HCI user channel), HD Audio as card 0, webcam
    node, Chromium + F-Droid preinstalled; Cuttlefish still boots. Kernel 6.18.21
    GKI + vendor modules from source (`tools/gki-build.sh`), `pcfb` (UEFI
    framebuffer as DRM `virtio_gpu`), `pchvc` (placeholder Cuttlefish ports),
    stick `tools/mkusb-aosp17.sh`. Open: SELinux enforcing on the PC, real
    sound/camera use (F6/F7). See `docs/usb-boot-aosp17.md`.
  - **F3b. Rust kernel on AOSP 17 userspace (later).** Point
    `tools/aosp-*-test.py` at the AOSP 17 images, fix the syscalls/ioctls
    Android 17 adds. Done when: the existing `--aosp-*-test` suites pass
    again and the F4–F8 HALs work on the Rust kernel too.
- **F4. Health + power + thermal HALs (Rust)** — `android.hardware.health`,
  `power`, `thermal` over the AC supply, thermal zones and cpufreq from E9.
  *Done on Cuttlefish 2026-11-11:* `hals/{health,thermal,power}` (health V5, thermal V3,
  power V7) replace Cuttlefish's HALs; SELinux enforcing with no AVC denials; VTS health
  25/25, thermal 9 + 2 skipped, power 10 + 13 skipped (no hint sessions); `libpchal` gained
  `Callbacks`, `uevent`, `procstat`. Kernel: `ACPI_AC`/`ACPI_BATTERY` modules. A first boot
  on the Z790 PC showed battery-less AC, CPU0/skin 74 °C, EPP switching; the two bugs it
  found (intel_pstate limits not writable, ACPI zones loaded after the HAL) are fixed but
  not yet re-checked there. The real-PC check moves to the combined F4–F8 USB test. See
  `docs/rust-hals.md`.
- **F5. Bluetooth HAL (Rust)** — `android.hardware.bluetooth` H4 over
  `/dev/hvc5` (E7), replacing the C++ default HAL.
  *Done on Cuttlefish/QEMU 2026-11-11:* `hals/bluetooth` replaces the interim C++ HAL
  (`aosp-device-vendor/`, removed): `IBluetoothHci/default` over an HCI user channel to the
  kernel's `hci0` on the PC, or H4 on a UART (`--h4 /dev/hvc5`, Cuttlefish's rootcanal) for
  VTS. Cuttlefish enforcing: `VtsHalBluetoothTargetTest` 16/16, the stack turns ON on it,
  no AVC denials; QEMU USB boot (no controller): the HAL reports `UNABLE_TO_OPEN_INTERFACE`
  after 3 s, the stack gives up after 6 restarts (state OFF). `libpchal` gained `poll`. The
  real-PC check (AX211 scan/pairing) is part of the combined F4–F8 USB test. See
  `docs/rust-hals.md`.
- **F6. Audio HAL (Rust)** — AIDL `android.hardware.audio.core` over the ALSA
  PCM/control ABI (E5, E8 microphone).
- **F7. Camera HAL (Rust)** — AIDL camera provider over V4L2 UVC
  `/dev/video0` (E8), so the stock Camera app works.
- **F8. Wi-Fi HAL (Rust)** — minimal `android.hardware.wifi` for nl80211
  (E6), next to wpa_supplicant.

Order: F1 → F2 → F3a → F4–F8 (tested on the stock kernel, real PC) → F3b
(Rust kernel) last. F4–F8 can be built and smoke-tested on Cuttlefish first.

## Rủi ro chính

- Phạm vi cực lớn (Starnix mất nhiều năm với đội lớn). Ưu tiên chạy được từng mốc.
- Dung lượng đĩa cho AOSP.
- Đồ hoạ là phần khó nhất; dùng render phần mềm trước.
- Phương án dự phòng nếu tiến độ quá chậm: fork Asterinas (kernel Rust tương thích
  Linux ABI, MPL-2.0) rồi bổ sung binder & phần Android.
