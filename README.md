# android-pc

AOSP 17 (`android-17.0.0_r1`) built from source for a real x86_64 desktop PC, booted from a
USB stick. The product is `aosp_pc_x86_64` (`aosp-device/`): the stock **GKI android17-6.18**
kernel plus vendor modules built from source, and the PC's hardware served by **vendor HALs
written in Rust** (health, power, thermal, Bluetooth, audio, camera, Wi-Fi, the NVIDIA GPU's
Vulkan driver glue). It also ships ARM64 app support (libndk_translation), microG with the
Play Store, a desktop-mode UI and NVIDIA's own driver for the RTX 4070.

Target machine: Gigabyte Z790, i7-13700K, RTX 4070, 2560x1080 monitor
([docs/usb-boot-aosp17.md](docs/usb-boot-aosp17.md)). Cuttlefish runs the same product for
development and VTS. The plan and the status of every phase are in [PLAN.md](PLAN.md); the
earlier Rust kernel (AOSP 16) lives on branch `aosp-16`, other GPU approaches (nouveau/NVK,
Nova, Venus) on branch `gpu-alternatives`.

## Repository layout

| Path | Contents |
|---|---|
| `aosp-device/` | Device tree for `aosp_pc_x86_64`: product makefiles, `pc/` (real-PC boot: `init.pc.rc`, `pclog.sh`), `libpchal` + `hals/<name>` (Rust HALs), `kernel/` (vendor module build, GKI config fragment), `desktop/`, `microg/`, `webview/`, `nbshim/`, `sepolicy/` |
| `aosp-device-vendor/` | Modules that must sit under `vendor/` in the AOSP tree |
| `tools/` | Build, image, update and VM scripts (below); `patches/` for AOSP and nvidia-open |
| `docs/` | Design notes and test logs (see [Documentation](#documentation)) |
| `aosp/` | AOSP checkout (gitignored, ~250 GB with `out/`) |
| `gki/` | GKI kernel checkout (gitignored, ~40 GB with `out/`) |
| `out/` | Stick images, downloads, logs (gitignored) |

## Host requirements

- Arch Linux x86_64 with `repo`, `git`, `python3`, `qemu-desktop`, `edk2-ovmf`, `limine`,
  `mtools`, `sfdisk`; about **400 GB** free disk for the AOSP and GKI checkouts and outputs.
- Every heavy build runs through `tools/limited-build.sh` (8 E-cores, 20 GB RAM, stops under
  10 GB free disk), **one build at a time** and **one VM at a time**. The host's i7-13700K is
  degraded: load on the P-cores froze it. The rules are in [CLAUDE.md](CLAUDE.md) and
  [docs/aosp-build.md](docs/aosp-build.md) ("Flaky host").

## Building the ROM image

### 1. Get the sources

AOSP 17 ([docs/aosp-build.md](docs/aosp-build.md#source)):

```bash
mkdir aosp && cd aosp
repo init -u https://android.googlesource.com/platform/manifest -b android-17.0.0_r1 \
  --partial-clone --clone-filter=blob:limit=10M -c --depth=1 --no-tags -g default,-darwin,-mips
repo sync -c -j4 --no-tags --no-clone-bundle --optimized-fetch --retry-fetches=5
```

GKI kernel ([docs/usb-boot-aosp17.md §2](docs/usb-boot-aosp17.md#2-kernel)):

```bash
mkdir gki && cd gki
repo init -u https://android.googlesource.com/kernel/manifest -b common-android17-6.18-2026-06 \
  --depth=1 --no-tags --partial-clone --clone-filter=blob:limit=10M
mkdir -p .repo/local_manifests && cp ../aosp-device/kernel/gki-trim.xml .repo/local_manifests/trim.xml
repo sync -c -j4 --no-tags --no-clone-bundle --optimized-fetch --retry-fetches=5
```

NVIDIA's open kernel modules (same version as the userspace driver):

```bash
git clone --depth 1 --branch 620.70.06 https://github.com/NVIDIA/open-gpu-kernel-modules gki/nvidia-open
```

### 2. Apply the AOSP patches

Apply each patch in `tools/patches/` to its AOSP project, again after any `repo sync` that
resets that project. The `nvidia-open-*` patches are applied by `tools/nvidia-build.sh`.

| Patch | AOSP project | Why |
|---|---|---|
| `aosp17-soong-gomemlimit.patch` | `build/soong` | passes `SOONG_GOMEMLIMIT` to soong_build (otherwise analysis is OOM-killed) |
| `aosp17-bionic-static-tls-surplus.patch` | `bionic` | static TLS room for NVIDIA's glibc libraries (nvglibc) |
| `aosp17-microg-signature-spoofing.patch` | `frameworks/base` | lets microG stand in for Google Play services |
| `aosp17-credman-empty-ui.patch` | `frameworks/base` | Credential Manager answers when all entries are dropped (TikTok sign-in) |
| `aosp17-input-mouse-as-touch.patch` | `frameworks/native` | mouse clicks reach touch-only games |
| `aosp17-launcher3-recents-only.patch`, `aosp17-launcher3-taskbar-npe.patch` | `packages/apps/Launcher3` | Launcher3 kept for recents only, Smart Dock is the home app |
| `aosp17-minigbm-nvidia.patch`, `aosp17-minigbm-drop-master.patch`, `aosp17-minigbm-rw-lock.patch` | `external/minigbm` | gralloc on nvidia-drm |
| `aosp17-angle-astcenc.patch` | `external/angle` | ASTC support |
| `aosp17-vold-pc-disk-guard.patch` | `system/vold` | vold never touches the PC's own disks |

```bash
git -C aosp/build/soong apply "$PWD/tools/patches/aosp17-soong-gomemlimit.patch"
```

(and likewise for each row; `git -C <project> apply --check` first to see if it is already in).

### 3. Fetch the third-party pieces

These are downloaded on the build machine, checked against pinned hashes and certificates,
and never committed; images built with them are for personal use only.

```bash
tools/fetch-apps.sh                 # Chromium, F-Droid, microG, Play Store, Termux, WebView, Smart Dock
tools/fetch-ndk-translation.sh      # Google's ARM64 -> x86_64 translator
tools/nvidia-userspace-build.sh     # NVIDIA's userspace Vulkan driver (from the host or the .run)
```

### 4. Build the kernel and vendor modules

```bash
tools/gki-build.sh
```

Steps `gki virt pc nvidia install firmware` (pick with `STEPS=...`): the GKI image and its
system_dlkm modules, Cuttlefish's modules, the PC's modules (`aosp-device/kernel`), NVIDIA's
open modules (`tools/nvidia-build.sh`), then installs everything under
`aosp/device/android-pc/kernel/6.18/` and the firmware under `aosp/device/android-pc/firmware/`,
where `aosp-device/BoardConfig.mk` picks them up.

### 5. Build Android

```bash
tools/aosp17-build.sh
```

Builds `aosp_pc_x86_64-cp2a-userdebug` one image at a time through `tools/limited-build.sh`,
retrying flaky steps. It mirrors `aosp-device/` into the AOSP tree first, sets a fresh
`BUILD_NUMBER` (needed for in-place stick updates) and lowers Soong's memory
(`SOONG_GOMEMLIMIT=14GiB`, `SOONG_INCREMENTAL_ANALYSIS=false`). A cold build takes about
6 hours. Useful variables:

```bash
STEPS="system.img vendor.img" tools/aosp17-build.sh            # only some images
AOSP_DEVICE_SRC=/path/to/clean/copy tools/aosp17-build.sh      # release build from a clean tree
PRODUCT=aosp_cf_x86_64_only_phone tools/aosp17-build.sh        # stock Cuttlefish phone
```

Output: `aosp/out/target/product/pc_x86_64/`.

### 6. Make the USB stick image

First install on a new stick (the only time the whole stick is written):

```bash
lsblk -o NAME,SIZE,MODEL,TRAN
tools/mkusb-aosp17.sh --menu --disk /dev/sdX out/aosp17-usb.img
```

`--disk` sizes `userdata` to fill that stick (no root needed; it refuses disks that are not on
USB). The script prints the commands that write the image to the stick. Other options:
`--resolution WxH` (default 2560x1080), `--density N` (default 160), `--enforcing`,
`--append "WORDS"`. Every image boots the RTX 4070 on NVIDIA's driver; `--menu` adds
fallback Limine entries without a GPU driver (UEFI framebuffer: SELinux permissive or
enforcing, the kernel log on screen, no display driver). Partition layout: [docs/usb-boot-aosp17.md §5](docs/usb-boot-aosp17.md#5-the-stick-image).

### 7. Update a stick without losing data

Every later build goes onto the stick **in place**: only the OS partitions (ESP, `vbmeta*`,
`boot`, `init_boot`, `vendor_boot`, `super`) are rewritten; apps, accounts and files in
`userdata` stay. Make the image with the same options as the stick, without `--disk`:

```bash
tools/mkusb-aosp17.sh --menu --userdata 64 out/aosp17-usb-new.img
tools/update-usb-aosp17.sh --dry-run out/aosp17-usb-new.img /dev/sdX
tools/update-usb-aosp17.sh out/aosp17-usb-new.img /dev/sdX
```

Run it as yourself, not with `sudo`: it asks for the password and uses root only for the
commands on the stick. It refuses a build that would not open the old data (different AVB
key, older security patch level or rollback index, same build number) and reads the
partitions back to compare (`Read back and compared: OK`). Never write a whole image over a
stick in use: that wipes `userdata`.

## Tools

### Build

| Tool | What it does |
|---|---|
| `tools/limited-build.sh CMD...` | Runs a build under the host limits: pinned to E-cores 8-15 with `taskset`, 20 GB RAM, nice 10, low I/O weight, aborts below 10 GB free disk. `BUILD_CPUS`, `BUILD_MEM` override. Example: `tools/limited-build.sh m -j8 systemimage` |
| `tools/aosp17-build.sh` | Builds the Android images step by step (see above) |
| `tools/aosp-device-link.sh` | Mirrors `aosp-device/` to `aosp/device/android-pc/pc_x86_64` and `aosp-device-vendor/` to `aosp/vendor/android-pc` (real copies; edit the repo, never the mirror). Run by `aosp17-build.sh` |
| `tools/gki-build.sh` | Builds the GKI kernel, vendor modules and firmware and installs them for the AOSP build |
| `tools/nvidia-build.sh` | Builds NVIDIA's open kernel modules (`nvidia`, `nvidia-modeset`, `nvidia-drm`, `nvidia-uvm`) against the GKI build; output `gki/out/nvidia/dist` |
| `tools/nvidia-userspace-build.sh [VER]` | Collects NVIDIA's userspace driver libraries into `aosp/device/android-pc/nvidia-userspace/` |
| `tools/fetch-apps.sh [NAMES]` | Downloads and verifies the preinstalled apps: `chromium fdroid microg playstore termux webview smartdock` |
| `tools/fetch-ndk-translation.sh` | Downloads libndk_translation from the official emulator image |

### USB stick

| Tool | What it does |
|---|---|
| `tools/mkusb-aosp17.sh [opts] [OUT.img]` | Makes a bootable GPT stick image (Limine, kernel, initrd, AVB images, `super`, `userdata`). Writes a file only |
| `tools/update-usb-aosp17.sh [--dry-run] [--yes] [--force] [--no-verify] NEW.img TARGET` | Updates the OS on a stick (or image file) in place, keeping user data |
| `tools/pclog-read.sh IMAGE-or-/dev/sdX [DEST]` | Copies the boot logs `pclog.sh` wrote to the stick's `PCLOG` partition and joins them into `boot-N/dmesg.txt` and `boot-N/logcat.txt` (default `out/pclog`) |

### Virtual machines (one at a time)

| Tool | What it does |
|---|---|
| `tools/run-qemu-aosp17-usb.sh [IMAGE]` | Boots a stick image in QEMU like the real PC: UEFI, USB stick on xHCI, VGA, e1000e, HD Audio. adb on `127.0.0.1:5556`. Snapshot mode by default (`WRITABLE=1` keeps changes); `NO_WINDOW`, `MEM`, `CPUS`, `AUDIO`, `POINTER=usb-mouse`, `QEMU_EXTRA` |
| `tools/run-qemu-aosp17-vfio.sh [IMAGE]` | Same, with the host's RTX 4070 passed through (VFIO) to test the NVIDIA entry; `--release` gives the card back. Needs IOMMU and the host desktop on the iGPU ([docs/nvidia-native.md](docs/nvidia-native.md)) |
| `aosp/cvd-launch.sh` | Starts Cuttlefish without the Debian packages (gitignored, inside the AOSP tree; [docs/aosp-build.md](docs/aosp-build.md#running-no-debian-cuttlefish-base-no-sudo)). Stop with `cd aosp/cvd-home && HOME=$PWD ./bin/stop_cvd` |
| `tools/vm-wait.sh` | Sourced by every VM launcher: takes a lock and waits until no other QEMU/Cuttlefish runs |
| `tools/mk-test-disks.sh [DIR]` | Creates virtual NVMe/SATA/USB disks for the external-storage test and prints the `QEMU_EXTRA` line |

### Tests and checks

| Tool | What it does |
|---|---|
| `tools/audio-check.sh play\|record ...` | End-to-end audio test through the framework (`ADB="adb -s 127.0.0.1:5556"`) |
| `tools/nvglibc-hosttest.sh` | Runs the bionic-built nvglibc Vulkan test against the host's RTX 4070 in a sandbox |
| `tools/nvgpu-hosttest.sh` | Builds and runs `vulkan.nvidia` and minigbm's nvidia backend on the host GPU, checking every pixel |
| `tools/credman-test/`, `tools/webview-intercept-test/`, `tools/guest-hook-test/` | Small test apps (each with its own `build.sh`) for Credential Manager, WebView and the native bridge |
| `tools/uninstall.sh [--all] [-n]` | Removes what the project installed on the host (pacman packages, toolchains, build data) |

## Documentation

| Document | Topic |
|---|---|
| [PLAN.md](PLAN.md) | Phases, status and commits |
| [docs/aosp-build.md](docs/aosp-build.md) | AOSP sync, build rules, Cuttlefish, the flaky host |
| [docs/usb-boot-aosp17.md](docs/usb-boot-aosp17.md) | Kernel, firmware, boot on the real PC, stick image, storage, in-place updates |
| [docs/rust-hals.md](docs/rust-hals.md) | The Rust HALs and `libpchal` |
| [docs/arm-translation.md](docs/arm-translation.md) | ARM64 apps, microG, Play Store, games |
| [docs/desktop-mode.md](docs/desktop-mode.md), [docs/mouse-input.md](docs/mouse-input.md) | Desktop UI, mouse as touch |
| [docs/nvidia-proprietary.md](docs/nvidia-proprietary.md), [docs/nvidia-native.md](docs/nvidia-native.md) | NVIDIA kernel modules; NVIDIA's userspace driver on Android (nvglibc, `vulkan.nvidia`) |

## Licensing note

The project's own code is in this repository. Google's Play Store and libndk_translation and
NVIDIA's userspace driver are proprietary: the scripts fetch them on the build machine, they
are never committed, and images that contain them are for personal use only.
