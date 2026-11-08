# AOSP tham chiếu cho x86_64 (task A2)

Trạng thái: **đang dừng giữa chừng**. Đã có `system`, `system_ext` và `product` image.
Build `vendorimage` bị treo do host không ổn định (xem "Sự cố host" bên dưới).
Chưa boot, chưa có trace.

## Nguồn

- Manifest `android-16.0.0_r4` (BUILD_ID `BP4A.251205.006`), trong `aosp/` (đã gitignore).
- Cách tiết kiệm đĩa:
  ```
  repo init -u https://android.googlesource.com/platform/manifest -b android-16.0.0_r4 \
    --partial-clone --clone-filter=blob:limit=10M -c --depth=1 --no-tags -g default,-darwin,-mips
  repo sync -c -j4 --no-tags --no-clone-bundle --optimized-fetch --retry-fetches=5
  ```
  Sync với `-j16` bị googlesource trả HTTP 429. Dùng `-j4` kèm vòng retry thì ổn.
- Local manifest `aosp/.repo/local_manifests/trim.xml` chỉ bỏ các device tree không dùng
  (linaro, amlogic, car, atv, trout). Lần đầu tôi bỏ cả kernel prebuilt arm64,
  `prebuilts/android-emulator` và `qemu-kernel`, nhưng Soong vẫn tham chiếu đến chúng
  (trusty, microdroid) nên phải đưa lại.
- Lỗi gặp phải: lần sync bị 429 làm hỏng index của 4 project (`external/conscrypt`,
  `device/google/cuttlefish_prebuilts`, `cuttlefish_vmm`, `external/chromium-webview`).
  Kiểm tra bằng `repo forall -c 'git status --short --untracked-files=no'` và sửa bằng
  `git reset --hard HEAD` trong từng project.
- Dung lượng: `.repo` 28 GB, checkout khoảng 80 GB, `out/` khoảng 95 GB (sau khi có product image).

## Chọn product: `aosp_cf_x86_64_only_phone-trunk_staging-userdebug`

- **Cuttlefish thay vì goldfish:** Cuttlefish là thiết bị ảo tham chiếu của AOSP. Mọi phần cứng
  của nó đều là virtio (blk, net, gpu, input, console, vsock), đúng với lộ trình driver
  của kernel Rust (PLAN.md, giai đoạn C/D). Goldfish thì phụ thuộc goldfish-pipe/qemu_pipe
  và emulator riêng.
- **`x86_64_only`:** chỉ có zygote64, không có thư viện 32-bit. Vì vậy kernel Rust không cần
  syscall compat ia32 (`int 0x80`, `ioctl` compat), và build cũng nhỏ hơn.
- **Không chọn `aosp_cf_x86_64_slim`:** product này không có `aosp_product`, tức là không có
  Launcher/Settings. Yêu cầu là phải chạy đến launcher.
- **userdebug:** có `adb root`, `strace` và ftrace để trace.
- Kernel: prebuilt GKI x86_64 trong `kernel/prebuilts/6.12/x86_64` (android16-6.12), kèm module
  `kernel/prebuilts/common-modules/virtual-device/6.12/x86-64`.

## Build (theo quy tắc trong CLAUDE.md)

Mỗi lần chỉ build một image, qua `tools/limited-build.sh`:
```
source build/envsetup.sh && lunch aosp_cf_x86_64_only_phone-trunk_staging-userdebug
NINJA_HIGHMEM_NUM_JOBS=1 NINJA_ARGS=-j3 tools/limited-build.sh m -j3 systemextimage   # tương tự cho productimage
NINJA_HIGHMEM_NUM_JOBS=1 NINJA_ARGS=-j8 tools/limited-build.sh m -j8 vendorimage
# tiếp theo: odmimage, vendor_dlkmimage odm_dlkmimage system_dlkmimage,
#            bootimage initbootimage vendorbootimage, superimage, userdataimage, droid
```
- Riêng ninja đã chiếm khoảng 6.6 GB RSS (đồ thị build rất lớn), và mỗi job R8 cần
  khoảng 4–5 GB. Với `-j15` hoặc `-j10`, scope 25 GB bị OOM. Các bước nặng Java
  (system_ext, product) phải chạy `-j3`.
- ccache: AOSP không còn hỗ trợ ccache (biến `USE_CCACHE` đã bị bỏ), nên không dùng.
- Đã xong: `cvd-host_package`, `systemimage`, `systemextimage`, `productimage`.
  Host tool Cuttlefish nằm ở `out/host/linux-x86/bin` (`assemble_cvd`, `run_cvd`, `crosvm`, `adb`).

## Sự cố host (2026-11-08) — cần người dùng xử lý

Trong lúc build `vendorimage`, một tiến trình `clang++` bị kẹt ở trạng thái D trong
`__do_sys_brk` từ 13:07 và SIGKILL không giết được. Mọi `ps`/`pgrep` đọc tới nó cũng bị
treo theo. Các dấu hiệu khác cùng ngày:
- Boot trước có `BUG: Bad page state` (kernel bị taint `B`). Kernel hiện tại vẫn taint `S`
  (CPU_OUT_OF_SPEC).
- Hôm nay có 3 lần reboot không sạch (journal báo "uncleanly shut down").
- Một lỗi `Bad CRC-32` khi giải nén zip, trong khi kiểm tra lại thì zip vẫn hợp lệ.
- Một segfault trong libpython3.14 ở một tiến trình không liên quan.

Các dấu hiệu này chỉ ra lỗi phần cứng (RAM hoặc CPU). i7-13700K thuộc dòng Raptor Lake
có lỗi suy giảm Vmin đã được biết đến. Cũng có thể là bug của kernel 7.2.14. Đề nghị:
1. Reboot để xoá tiến trình D.
2. Cập nhật BIOS/microcode.
3. Tắt XMP hoặc chạy RAM ở tốc độ mặc định.
4. Chạy memtest86+.

Sau đó mới build tiếp. Build trên máy không ổn định có thể sinh output hỏng mà không báo lỗi.
