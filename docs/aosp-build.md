# AOSP reference build for x86_64 (A2 → F1: AOSP 17)

Status (2026-11-11): **AOSP 17 boots.** `aosp_cf_x86_64_only_phone-cp2a-userdebug`
built from source reaches `sys.boot_completed=1` in about 40 s on Cuttlefish with the
android17-6.18 GKI, `adb shell` works and Launcher3 is resumed.

AOSP 16 and the Rust kernel that ran on it live on the `aosp-16` branch.

## Source

- Manifest `android-17.0.0_r1` (BUILD_ID `CP2A.260605.016`, `PLATFORM_VERSION=17`),
  checkout in `aosp/` (gitignored). It was upgraded in place from android-16.0.0_r4.
- Disk-saving init/sync:
  ```
  repo init -u https://android.googlesource.com/platform/manifest -b android-17.0.0_r1 \
    --partial-clone --clone-filter=blob:limit=10M -c --depth=1 --no-tags -g default,-darwin,-mips
  repo sync -c -j4 --no-tags --no-clone-bundle --optimized-fetch --retry-fetches=5
  ```
  googlesource answers HTTP 429 at `-j16`; `-j4` plus a retry loop works
  (`aosp/f1-sync.sh`). The 16 → 17 sync took about 20 minutes.
- A local change in a project blocks its checkout (`repo sync` reports
  `platform/build/release checkout ... failed`). Revert it, then `repo sync -l`.
- Local manifest `aosp/.repo/local_manifests/trim.xml` drops unused device trees
  (amlogic yukawa, linaro hikey/dragonboard, google trout, google_car, atv).
  **`device/generic/car` must stay on AOSP 17:**
  `system/software_defined_vehicle/samples` (group `swcar`) depends on
  `device.generic.car.emulator-aidl-latest-ndk-defaults`. Excluding the whole `swcar`
  group instead breaks `platform_testing/libraries/sdv`.
- Sizes (AOSP 17): `.repo` 46 GB, checkout ~105 GB, `out/` 143 GB,
  `cvd-home/` ~23 GB with a running instance.

## Product and release: `aosp_cf_x86_64_only_phone-cp2a-userdebug`

- **Cuttlefish, not goldfish:** all of its hardware is virtio (blk, console/hvc, gpu,
  input, vsock), which matches the Rust kernel's driver roadmap.
- **`x86_64_only`:** zygote64 only, no 32-bit libraries, so no ia32 compat syscalls.
- **userdebug:** `adb root`, `strace`, ftrace.
- **Release `cp2a`:** the release config of the tag (BUILD_ID `CP2A...`). On AOSP 16,
  `trunk_staging` enabled `RELEASE_USE_DEX_V41` (dex container v41) and zygote64
  crash-looped, so `build/release` needed a local fix. On AOSP 17 no release sets
  that flag any more, so the fix is gone. `cp2a` boots as is.
- **Kernel:** Cuttlefish phones default to `TARGET_KERNEL_USE=6.12` (android16-6.12).
  The build sets `TARGET_KERNEL_USE=6.18` to use the android17 GKI
  `6.18.16-android17-1-gb61cd7ae4209-ab15097451` from `kernel/prebuilts/6.18/x86_64`
  plus the matching `common-modules/virtual-device/6.18/x86-64` modules. It still runs
  Rust Binder (`binder.impl=rust`).

## Build (rules from CLAUDE.md)

`tools/aosp17-build.sh` builds one step at a time through `tools/limited-build.sh`
(12 cores, 25 GB, disk floor) and retries each step up to 5 times (the host is flaky,
see below). It stops retrying on a Soong analysis error, which is deterministic.
```
tools/aosp17-build.sh                         # all steps
STEPS="system.img vendor.img" tools/aosp17-build.sh
```
`PRODUCT` selects the lunch product: `aosp_pc_x86_64` (default since F2, our device tree,
see [rust-hals.md](rust-hals.md)) or `aosp_cf_x86_64_only_phone` (stock).
Default steps: `system.img system_ext.img product.img vendor.img odm.img`,
`boot+init_boot+vendor_boot`, the `*_dlkm + vbmeta* + super` group, `userdata.img`,
`hosttools` (cvd host package, adb, root-canal, mkfs/fsck.erofs, lz4, unpack_bootimg),
then `droid`.

- **Images are file targets.** The build is Soong-only and has no `bootimage`/`superimage`
  phony, so steps name files in `out/target/product/vsoc_x86_64_only/`.
- **The `*_dlkm` + `vbmeta_*` + `super` group must be built in one step.** Otherwise
  super carries a `vendor_dlkm` with a new root digest while `vbmeta_vendor_dlkm` is
  stale, and the kernel reboots with `dm-verity device corrupted`.
- **Siso replaces ninja on AOSP 17.** Siso rejects `-jN` in `NINJA_ARGS`
  (`flag provided but not defined: -j3`); it takes `--local_jobs` from `m -jN`. The
  build therefore passes `NINJA_ARGS=` (empty), and `tools/limited-build.sh` only
  defaults `NINJA_ARGS` when it is unset, not when it is empty.
- **Soong analysis needs a heap cap.** On AOSP 17, `soong_build` goes past the 25 GB
  scope and is OOM-killed. `soong_build` runs under `env -i`, so `GOMEMLIMIT`
  cannot be passed from outside. A local patch to `build/soong/ui/build/soong.go`
  ([tools/patches/aosp17-soong-gomemlimit.patch](../tools/patches/aosp17-soong-gomemlimit.patch))
  forwards `SOONG_GOMEMLIMIT` as `GOMEMLIMIT`, and the build script sets `18GiB`.
  With the cap, analysis takes about 4 minutes. Re-apply the patch after a `repo sync`
  that resets `build/soong` (`git -C aosp/build/soong apply ../../tools/patches/...`).
  Since the 20 GB cap (commit 5114656) and G3's Mesa NVK modules, analysis still went
  past the cap (soong_build at 20 GB of anonymous memory with `GOMEMLIMIT=14GiB`) while
  incremental analysis was on. `SOONG_INCREMENTAL_ANALYSIS=false` with
  `SOONG_GOMEMLIMIT=14GiB` fits (docs/nvidia.md, G3).
- **Parallelism is limited by RAM, not CPU.** Siso alone holds about 10 GB RSS and
  each R8/D8 job needs 4–5 GB. At `-j3` the scope peaked at 22.5 of 25 GB with 4 GB
  of swap in use. So the Java-heavy steps (system, system_ext, product, vendor,
  droid) run at `-j3` with `NINJA_HIGHMEM_NUM_JOBS=1`, and the rest at `-j8`.
- Build time: the final run from a partially built `out/` took 1 h 25 min (system.img
  finishing → droid). The whole cold build took about 6 hours of wall time across
  restarts (one host hang and one stuck process, see below).
- ccache is not supported by AOSP any more and is not used.

## Running (no Debian cuttlefish-base, no sudo)

The host package `out/host/linux-x86/cvd-host_package.tar.gz` is extracted into
`aosp/cvd-home/`. Launch with `aosp/cvd-launch.sh` (gitignored, like everything under
`aosp/`), stop with `cd aosp/cvd-home && HOME=$PWD ./bin/stop_cvd`.

- `unshare --user --map-current-user --mount --keep-caps`, then make
  `/usr/lib/cuttlefish-common/bin/capability_query.py` (an `exit 0` stub from
  `aosp/cvd-shim/lib`) appear under `/usr/lib`, and bind-mount an `/etc/group` that adds
  `cvdnetwork` with gid 1000. The script overlays `cvd-shim/lib` onto `/usr/lib`; if
  overlayfs cannot be mounted (for example the running kernel's modules were removed
  by a package upgrade), it falls back to a tmpfs of symlinks to the real `/usr/lib`
  plus `cuttlefish-common`.
- launch_cvd needs host vsock (`vhost_vsock`). After a kernel package upgrade without
  a reboot it fails with `Can't tombstone server socket: 'Address family not supported
  by protocol'`. Reboot into the installed kernel.
- **Guest RAM is 4 GB (`--memory_mb=4096`, override with `MEM_MB`). Do not use 6 GB with
  QEMU.** For QEMU, Cuttlefish 17 hardcodes `ramoops.mem_address=0x150000000`
  (`device/google/cuttlefish/host/libs/config/kernel_args.cpp`). With 6144 MB, guest
  System RAM runs to `0x1bfffffff`, so ramoops writes over ordinary RAM. The guest
  kernel then fails at random: oopses in `persistent_ram_write`, `Bad pagetable`,
  `Bad page map`, `kernel BUG at mm/usercopy.c:102`, and zero-length aconfig boot
  files in `/metadata/aconfig/boot`. system_server crash-loops with
  `Invalid feature flag: android.security.aapm_feature_disable_insecure_wifi_autojoin`.
  With 4096 MB, RAM ends at `0x13fffffff` and ramoops lies above it.
- `--vm_manager=qemu_cli`. crosvm hangs setting up its minijail sandbox inside the
  user namespace.
- Flags: `--cpus=4 --gpu_mode=guest_swiftshader --start_webrtc=false
  --enable_tap_devices=false --enable_wifi=false --enable_host_bluetooth=false
  --setupwizard_mode=DISABLED --enable_modem_simulator=false`. The modem simulator
  segfaults on this host, and the guest's `libcuttlefish-rild` then floods the log.
- **Desktop window (default):** `--qemu_binary_dir=aosp/cvd-qemu-window` wraps Arch's
  QEMU (`/usr/bin/qemu-system-x86_64`, 11.1) and adds a `usb-tablet` on `xhci` for an
  absolute pointer. The screen is served over VNC at `127.0.0.1:6444`, and
  `aosp/cvd-window.py` (a small Python GTK3/cairo VNC viewer) shows it as the
  "Android (cvd-1)" window. Set `NO_WINDOW=1` to skip the window. Do not use QEMU's
  `-display gtk`: it pushes the window size into virtio-gpu and system_server
  crash-loops on the display change.
- adb: `out/host/linux-x86/bin/adb -s 0.0.0.0:6520`. If you also ran `adb connect
  127.0.0.1:6520`, there are two transports for one device and `adb shell` can hang.
  Run `adb kill-server` and use only the `0.0.0.0:6520` serial.

## Flaky host

The i7-13700K (Raptor Lake, known Vmin degradation) keeps failing under build load:

- unclean reboots (2026-11-08 ×3, 2026-11-10 ×1, the journal just stops);
- `BUG: Bad page state`;
- kernel oops under siso/soong: 2026-11-14 01:09, `general protection fault, kernel NULL
  pointer dereference` in `nd_jump_root` (Comm: soong_build) right after an RCU warning in
  `rcu_sched_clock_irq` (soong_ui), both on CPU#4. After it every new `soong_build` hung
  in D state in `lock_mm_and_find_vma` (unkillable), and `pgrep -f`/`pgrep -a`/`ps` that
  read its cmdline hung too (load ~19 with no CPU use). Not a siso bug: check
  `journalctl -k | grep -E 'Oops|WARNING'` first; only a host reboot clears it. Watchdogs
  should use `pgrep -x` or `/proc/*/stat`, never cmdline reads. It came back 3 h after a
  reboot (12:09): the same `rcu_sched_clock_irq` WARNING (tree_plugin.h:823) in
  `soong_build`, then `rcu_exp_handler` and an expedited RCU stall on CPU 11, siso in D.
  The same warning twice in the Go-based `soong_build` points at the host kernel
  (7.2.15-arch1, PREEMPT full) as much as at the CPU; try the `linux-lts` kernel (with
  `nvidia-open-lts`) and Intel Default Settings in the BIOS;
- processes stuck in D state. On 2026-11-11 a `clang++` was stuck in `exit_mmap`,
  `blocked on an rw-semaphore, but the owner is not found`, and siso waited on it
  forever ("ninja may be stuck"). Kill the build and restart it; ninja/siso resume;
- random clang/lld/JVM/Go segfaults.

The build completes thanks to retries, and `avbtool verify_image --follow_chain_partitions`
passes on the AOSP 17 `vbmeta.img` chain.
Still, check BIOS/microcode and RAM (memtest86+) before fully trusting the output.
