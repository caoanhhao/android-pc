# NVIDIA RTX 4070 (Phase G)

Running log for Phase G of `PLAN.md`: driving the PC's RTX 4070 (AD104, PCI `10de:2786`,
HDMI/DP audio function `10de:22bc`) from Android instead of the UEFI GOP framebuffer
(`pcfb`). One section per step.

## G1. nouveau + GSP on the GKI kernel

### What was added

| Piece | Where | What |
|---|---|---|
| Kernel config | `aosp-device/kernel/pc_x86_64.fragment` | `CONFIG_DRM_NOUVEAU=m` (CH7006/SIL164 TV encoders and WMI_BMOF off) |
| Vendor modules | `aosp-device/kernel/BUILD.bazel` (`_PC_MODULES`) | `nouveau.ko` plus what it selects that the GKI image lacks: `drm_display_helper`, `drm_exec`, `drm_gpuvm`, `drm_ttm_helper`, `gpu-sched`, `i2c-algo-bit`, ACPI `video`, `wmi`, `mxm-wmi` |
| Firmware | `tools/gki-build.sh` step `firmware` → `/vendor/firmware/nvidia/ad104/gsp/` | GSP-RM **570.144**: `booter_load`, `booter_unload`, `bootloader`, `gsp` (decompressed; 61 MiB for `gsp-570.144.bin`) |
| Module blocklist | `aosp-device/pc/modules.blocklist` (BoardConfig.mk) | Cuttlefish's list plus `nouveau.ko`: the boot-time load (`dlkm_loader`) skips it |
| Boot entry | `tools/mkusb-aosp17.sh --menu` | "Android 17, NVIDIA nouveau (test, no Android picture)" |
| Init | `aosp-device/pc/init.pc.rc`, `pcgpu.sh` | loads nouveau, gives minigbm nouveau's card node, keeps a mode on the monitor |
| Logs | `aosp-device/pc/pclog.sh` | GPU/HDA PCI functions, nouveau/GSP dmesg, `/sys/class/drm/*` status and modes, `modetest -c`, raw EDID files, full ELD of the `10de:22bc` codec |

The GKI image already has DRM, the KMS helpers, TTM, backlight, I2C, HMM, IOMMU and the
firmware loader built in, so only modules were needed. Kconfig also selects `FW_CACHE` and
`SND_HDA_COMPONENT`: `FW_CACHE` only changes code inside vmlinux (the firmware loader), so
the real kernel keeps it off and nothing in the modules depends on it; `SND_HDA_COMPONENT`
changes `snd-hda-core` and the HDMI codec drivers, which are our vendor modules anyway, and
is what lets `snd-hda-codec-nvhdmi` take the monitor's ELD from nouveau through the DRM
audio component.

Firmware: nouveau (`nvkm/subdev/gsp/ad102.c`) tries GSP-RM 570.144 first, then 535.113.01.
In linux-firmware `nvidia/ad104` is a symlink to `ad102`, and `gsp-570.144.bin` a symlink
to `ga102`'s; `zstd -d` follows both, so the image has plain files at the paths nouveau
asks for. Only the 570.144 set ships (the 535 set would add another 40 MiB). The GKI image
has no `FW_LOADER_COMPRESS`, so the files must be decompressed.

### The `nouveau` boot entry

The Limine menu (`mkusb-aosp17.sh --menu`) gets one more entry; the default entry stays
`pcfb`. Its command line adds:

- `pcfb.enable=0`: pcfb does nothing, so no driver touches the GOP framebuffer that
  nouveau takes over.
- `androidboot.android_pc.gpu=nouveau` (`ro.boot.android_pc.gpu`, the `androidpc.gpu`
  switch of `PLAN.md`): `init.pc.rc` runs `modprobe nouveau.ko` in `early-init` (modprobe
  with an explicit name ignores the blocklist; its dependencies come from `modules.dep`).
- `androidboot.hardware.hwcomposer.mode=noop` and `...display_finder_mode=noop`: the ranchu
  composer only drives a DRM device named `virtio*` (pcfb's disguise), so until G2 brings a
  KMS composer, Android runs on the composer's no-op display (720x1280, nothing on the
  monitor). `display_finder_mode` moved from the shared bootconfig to each entry's command
  line for this (a bootconfig value would win over the command line).
- `androidboot.selinux=permissive`.

So that the framework still boots (the audio test below needs `system_server`), minigbm
needs a device that allocates: nouveau's render node refuses dumb buffers (the ioctl is not
`DRM_RENDER_ALLOW`), so `init.pc.rc` sets `/dev/dri/renderD128` to mode 0000 after coldboot;
minigbm (user `system`) cannot open it and falls back to the card node, where its
`nouveau` backend allocates dumb buffers.

`pcgpu.sh` (service `vendor.pcgpu`, started at `post-fs-data`) runs `modetest -M nouveau -r`
with its stdin held open: the preferred mode on every connected connector, with a test
pattern. That lights the HDMI/DP link, so the NVIDIA HD Audio codec gets an ELD, the
codec's jack switches on, `WiredAccessoryManager` connects `HDMI Out`, and the F9h Rust
audio HAL plays to the monitor. If modetest exits (no monitor yet), it is retried every
10 s; its output goes to `/data/vendor/pcgpu/` and into pclog's state dumps.

What to expect on the real PC: the monitor shows the GOP picture, then a modetest colour
pattern once nouveau and `pcgpu.sh` are up (no Android UI on this entry). pclog's HDMI check
beeps on each HDMI PCM at 150 s, and its audio check plays 440 Hz through the audio HAL at
240 s and 480 s.

`modetest` comes from libdrm and lands in `/system/bin` (`pc.mk`): the generated vendor
partition takes a `PRODUCT_PACKAGES` name in all its variants, so the system variant is
installed and allowed past `generic_system.mk`'s artifact path requirement.

### QEMU check (2026-11-14)

QEMU has no NVIDIA GPU; the check is that both entries still boot and nouveau is harmless.
Image `out/aosp17-usb-g1-qemu.img` (`--menu --append console=ttyS0,115200`), booted with
`NO_WINDOW=1 tools/run-qemu-aosp17-usb.sh`:

- Default entry: `sys.boot_completed=1`, lock screen drawn through pcfb, nouveau not
  loaded (blocklist works); its helper modules load and sit unused.
- nouveau entry (the test copy's `limine.conf` set to `default_entry: 5`): command line has
  `pcfb.enable=0 androidboot.android_pc.gpu=nouveau ...mode=noop ...display_finder_mode=noop`
  after the base `pcfb.enable=1` (the later one wins); `nouveau.ko` loaded by `init.pc.rc`
  and idle (no device to bind), pcfb loaded but inert, `ro.vendor.hwcomposer.mode=noop`,
  SurfaceFlinger on the composer's `EMU_display_0`, `sys.boot_completed=1`,
  `vendor.pcgpu` running (retrying modetest, no card). In QEMU minigbm uses vkms's card
  node; on the PC it gets nouveau's.
- Rerun after the host reboots (builds `eng.caoanh.20261114.1255` and `.1305`): same result, with
  `/system/bin/modetest` present. It showed that `pcgpu.sh`'s first version (`sleep | modetest`)
  never retried, because the pipeline waits for the `sleep`. The script now holds modetest's
  stdin through a FIFO and kills the holder when modetest exits. In QEMU it retries every
  10 s (`modetest exited (255)`, no nouveau card). On `.1305` the default entry
  still uses pcfb and does not load nouveau. Release image `out/aosp17-usb-g1.img` is
  build `.1305`.

### Status

**Code done, real-PC test pending** (G1–G5 are tested on the PC together).

### Real-PC test

Release image for the stick (same `mkusb` options as the stick, tiny `userdata`, which
the update never copies):

```bash
tools/mkusb-aosp17.sh --menu --userdata 64 out/aosp17-usb-g1.img
```

Find the stick (`lsblk -o NAME,SIZE,MODEL,TRAN`, a ~57 GB SanDisk on `usb`), check, then
update it in place (keeps apps and data; docs/usb-boot-aosp17.md §10):

```bash
sudo tools/update-usb-aosp17.sh --dry-run out/aosp17-usb-g1.img /dev/sdX
```

```bash
sudo tools/update-usb-aosp17.sh out/aosp17-usb-g1.img /dev/sdX
```

On the PC (monitor on the RTX 4070's HDMI or DP port, speakers on in the monitor):

1. Boot the default entry once: Android as before (pcfb), nouveau not in `lsmod`.
2. Reboot, pick **"Android 17, NVIDIA nouveau (test, no Android picture)"** in the Limine
   menu. Expected: the GOP picture, then a black screen while nouveau boots GSP, then a
   modetest colour pattern at the monitor's native mode. At about 150 s a beep on each
   HDMI PCM (pclog `hdmi-check.txt`), at 240 s and 480 s a 440 Hz tone through the Rust
   audio HAL (`audio-check.txt`). Note what was seen and heard.
3. Leave it running about 10 minutes (the 600 s pclog dump), then reboot into the default
   entry and plug the stick into the build host.
4. Read the logs: `tools/pclog-read.sh /dev/sdX`, then in the nouveau boot's directory:
   - `dmesg.txt`: `nouveau ... gsp: RM version 570.144`-style lines, no `gsp` errors or
     timeouts, `pcgpu:` lines;
   - `state-*.txt` sections `== pci display`, `== nouveau dmesg`, `== drm connectors`
     (the monitor's connector `connected`, its native mode first), `== modetest -c`,
     `== eld card1 (10de:22bc)` (`monitor_present 1`, `eld_valid 1`, the monitor's name),
     `== audio devices` (HDMI connected), `== pcgpu`;
   - `edid-card0-*.bin` (`edid-decode` on the host);
   - `hdmi-check.txt` and `audio-check.txt` (`play` must report device type 9, HDMI).

Done when GSP boots, the connector is connected at its native mode, and the 440 Hz tone
from the audio check plays on the monitor.

Fallbacks if it fails: firmware not found → check the path in dmesg against
`/vendor/firmware/nvidia/ad104/gsp/`; GSP boot timeout → try the 535.113.01 set (add it to
`tools/gki-build.sh`); framework does not boot on this entry → the test pattern and the
ALSA beep in `hdmi-check.txt` still show whether the link and ELD work.
