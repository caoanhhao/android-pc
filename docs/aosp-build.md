# AOSP tham chiếu cho x86_64 (task A2)

Trạng thái (2026-11-08): **boot được.** Đạt `sys.boot_completed=1` sau khoảng 40 giây,
`adb shell` dùng được, Launcher3 ở trạng thái resumed. Kết quả trace nằm ở
[syscall-trace.md](syscall-trace.md).

## Nguồn

- Manifest `android-16.0.0_r4` (BUILD_ID `BP4A.251205.006`), checkout trong `aosp/` (đã gitignore).
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
- Lỗi gặp phải: lần sync bị 429 làm hỏng index của 4 project. Kiểm tra bằng
  `repo forall -c 'git status --short --untracked-files=no'` và sửa bằng
  `git reset --hard HEAD` trong từng project.
- Dung lượng: `.repo` 28 GB, checkout khoảng 80 GB, `out/` khoảng 100 GB, `cvd-home/` khoảng 10 GB
  (gồm cả RAM file của QEMU, 6 GB).

## Chọn product: `aosp_cf_x86_64_only_phone-trunk_staging-userdebug`

- **Cuttlefish thay vì goldfish:** Cuttlefish là thiết bị ảo tham chiếu của AOSP. Mọi phần cứng
  của nó đều là virtio (blk, console/hvc, gpu, input, vsock), đúng với lộ trình driver của
  kernel Rust. Goldfish thì phụ thuộc goldfish-pipe và emulator riêng.
- **`x86_64_only`:** chỉ có zygote64, không có thư viện 32-bit. Kernel Rust không cần
  syscall compat ia32, và build nhỏ hơn.
- **Không chọn `aosp_cf_x86_64_slim`:** product này không có Launcher/Settings.
- **userdebug:** có `adb root`, `strace` và ftrace.
- Kernel: prebuilt GKI `6.12.38-android16-5` trong `kernel/prebuilts/6.12/x86_64`. Kernel này
  chạy Rust Binder (`binder.impl=rust`).

### Sửa local bắt buộc: tắt `RELEASE_USE_DEX_V41`

Release config `trunk_staging` bật `RELEASE_USE_DEX_V41`, khiến D8 sinh **dex container v41**
(`-JDcom.android.tools.r8.dexContainerExperiment`). Trên nhánh release này, ART đọc
`framework.jar` sai và zygote64 crash liên tục với lỗi
`Unable to find static field UNDEFINED_TIME_NANO`, kèm SEGV trong `ImageSpace::LoadImage`.

Cách sửa: trong `aosp/build/release/flag_values/trunk_staging/RELEASE_USE_DEX_V41.textproto`,
đổi `bool_value: true` thành `false`. Sau đó build lại system, system_ext, product và vendor.
Đây là thay đổi cục bộ trong checkout AOSP, chưa commit ở đâu cả. Cách khác là dùng release
`bp4a` cho khớp tag, nhưng phải build lại gần như toàn bộ.

## Build (theo quy tắc trong CLAUDE.md)

Script `aosp/a2-steps.sh` build từng image một qua `tools/limited-build.sh` và tự retry tối đa
5 lần mỗi bước:
```
cd aosp && STEPS="systemimage systemextimage productimage vendorimage odmimage ..." ./a2-steps.sh
```
- Các bước nặng Java (system, system_ext, product, vendor) chạy `-j3`, còn lại `-j8`, kèm
  `NINJA_HIGHMEM_NUM_JOBS=1`. Lý do: riêng ninja đã chiếm khoảng 6.6 GB RSS và mỗi job R8 cần
  khoảng 4–5 GB, nên `-j10` trở lên bị OOM trong scope 25 GB.
- Build Soong-only không có phony `bootimage`/`superimage`, phải build theo đường dẫn file:
  `out/target/product/vsoc_x86_64_only/{boot,init_boot,vendor_boot,vbmeta*,vendor_dlkm,system_dlkm,odm_dlkm,super}.img`.
  **Nhóm `*_dlkm` + `vbmeta_*` + `super` phải build cùng một lần.** Nếu không, super chứa
  `vendor_dlkm` có root digest mới trong khi `vbmeta_vendor_dlkm` vẫn là bản cũ, và kernel
  reboot liên tục với lỗi `dm-verity device corrupted`.
- ccache: AOSP không còn hỗ trợ ccache, nên không dùng.

## Chạy (không có gói Debian cuttlefish-base, không cần sudo)

Gói host nằm ở `out/host/linux-x86/cvd-host_package.tar.gz`, đã giải nén vào `aosp/cvd-home/`.
Chạy bằng `aosp/cvd-launch.sh`:
- Chạy `unshare --user --map-current-user --mount --keep-caps` rồi overlay `aosp/cvd-shim/lib`
  lên `/usr/lib`, để có `cuttlefish-common/bin/capability_query.py` (một file stub `exit 0`).
  Bind-mount một `/etc/group` có thêm `cvdnetwork` trỏ về gid 1000.
- `--vm_manager=qemu_cli`, dùng QEMU đi kèm gói host. crosvm bị treo khi khởi tạo sandbox
  minijail bên trong user namespace; cách tắt sandbox (`--enable_sandbox=false`) không được
  phép dùng ở đây.
- Các flag: `--cpus=4 --memory_mb=6144 --gpu_mode=guest_swiftshader --start_webrtc=false
  --enable_tap_devices=false --enable_wifi=false --enable_host_bluetooth=false
  --setupwizard_mode=DISABLED --enable_modem_simulator=false`.
  Phải tắt modem simulator: trên host nó segfault liên tục, khiến `libcuttlefish-rild` trong
  guest spam log, đẩy logd/logcat lên 100% CPU và làm adb rớt kết nối.
- adb: `out/host/linux-x86/bin/adb -s 0.0.0.0:6520`. Nếu trước đó đã có `adb connect
  127.0.0.1:6520` thì sẽ có hai transport cho cùng thiết bị, và `adb shell` có thể treo.
  Cách xử lý: `adb kill-server` rồi chỉ dùng serial `0.0.0.0:6520`.
- Dừng: `cd aosp/cvd-home && HOME=$PWD ./bin/stop_cvd`.

## Host không ổn định

Ngày 2026-11-08 có 3 lần reboot không sạch. Kernel host báo `BUG: Bad page state`, có tiến
trình bị kẹt ở trạng thái D không giết được, và trong lúc build có nhiều lỗi ngẫu nhiên:
clang segfault, `free(): invalid pointer` trong lld, JVM SIGSEGV (khoảng 20 lần), modem_simulator
segfault. Tắt hyperthreading không chữa được. Đây là dấu hiệu lỗi phần cứng; i7-13700K thuộc
dòng Raptor Lake có lỗi suy giảm Vmin đã được biết đến.

Build vẫn xong nhờ retry. Boot image ART khi build lại cho ra hash giống hệt, và mọi image đều
qua được `avbtool verify_image`. Nhưng nên kiểm tra lại BIOS/microcode và RAM (memtest86+)
trước khi tin tuyệt đối vào output.
