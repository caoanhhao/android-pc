# Booting AOSP from a USB stick on a real PC (phase E3)

The stick holds Limine, the Rust kernel, the AOSP init ramdisk and AOSP's
`super.img`. It boots through UEFI on an x86_64 PC (tested in QEMU with
`tools/run-qemu.sh --usb-boot-test`; the target is a Gigabyte Z790 AORUS
ELITE AX with an i7-13700K and an RTX 4070). Nothing is installed on the
PC: its own disks are left read-only while Android runs.

What works on real hardware: UEFI boot through Limine, SMP from the ACPI
MADT, timers calibrated against the HPET or ACPI PM timer, the GOP
framebuffer as the display (simpledrm, also showing the kernel log), the
stick itself (USB mass storage, USB 2 or 3, also behind a hub), USB
keyboards and mice (boot protocol), (E4, not yet tried on the PC)
wired Ethernet on the board's Realtek RTL8125 2.5 GbE port: Android's
EthernetService gets an address by DHCP, so DNS, the browser and `adb
connect <address>:5555` should work; the Intel AX211's Bluetooth (on,
scan, pair; see section 7); HD Audio playback (E5); a USB webcam's
microphone as Android's microphone and its camera as a V4L2 device (E8,
section 8: Android's Camera app cannot use the camera with this AOSP
build). What does not: Wi-Fi (Intel AX211), the GPU's acceleration
(Android renders in software, SwiftShader), suspend, hot-plug (plug
keyboard and mouse in before booting). `/data` is in RAM, so nothing
persists between boots.

## 1. The stick

GPT, in this order (partition names matter, the kernel looks for
`super`):

| # | Name | Size | Contents |
|---|---|---|---|
| 1 | `ESP` | 1 GiB | FAT32, label `ANDROIDPC` (EFI system partition type) |
| 2 | `super` | 8 GiB | `super.img`, written with `dd` |
| 3 | `frp` | 1 MiB | zeroed |
| 4 | `misc` | 1 MiB | zeroed |
| 5 | `userdata` | the rest | unused for now |

Making it (as root, `/dev/sdX` being the stick; this erases it):

```bash
sudo sgdisk --zap-all /dev/sdX
sudo sgdisk -n1:1M:+1G -t1:ef00 -c1:ESP -n2:0:+8G -c2:super -n3:0:+1M -c3:frp \
    -n4:0:+1M -c4:misc -n5:0:0 -c5:userdata /dev/sdX
sudo mkfs.fat -F 32 -n ANDROIDPC /dev/sdX1
```

(`parted` or `sfdisk` do the same; the sizes and names are what count.)

## 2. Filling it

With the ESP mounted (udisks mounts it at `/run/media/$USER/ANDROIDPC`
when the stick is plugged in), as your user:

```bash
tools/mkusb.sh --esp /run/media/$USER/ANDROIDPC
```

It builds the kernel and the init ramdisk, copies `EFI/BOOT/BOOTX64.EFI`
(Limine), `boot/limine/limine.conf`, `boot/kernel` and
`boot/initramfs.cpio` onto the ESP, checks the stick's layout, and prints
the root commands for the other partitions, which only need running once
per AOSP build:

```bash
sudo dd if=aosp/out/target/product/vsoc_x86_64_only/super.img of=/dev/disk/by-id/usb-…-part2 bs=16M oflag=direct status=progress
sudo dd if=/dev/zero of=/dev/disk/by-id/usb-…-part3 bs=1M count=1 oflag=direct
sudo dd if=/dev/zero of=/dev/disk/by-id/usb-…-part4 bs=1M count=1 oflag=direct
sync
```

Then eject it (`udisksctl power-off -b /dev/sdX`). After a kernel change
only `tools/mkusb.sh` needs running again. Options: `--resolution WxH`
(default 1920x1080, falls back to the firmware's mode), `--density N`
(default 240), `--serial` (also log to COM1), `--append "WORDS"`.

## 3. Firmware settings (Gigabyte Z790; other boards are similar)

Enter the setup with **Del** at power-on, then:

- **Boot → Secure Boot: Disabled** (Limine and the kernel are not signed).
- **Boot → CSM Support: Disabled** (UEFI only).
- **Boot → Fast Boot: Disabled** (otherwise USB devices may not be
  initialized before the OS).
- **Settings → IO Ports → USB Configuration → XHCI Hand-off: Enabled**
  (the kernel asks the firmware to release the USB controllers).
- Leave Above 4G decoding / Resizable BAR as they are.

Save with F10. To boot the stick once, press **F12** at power-on and pick
the `UEFI: SanDisk …` entry (not the one without `UEFI:`). Plug the stick
into a rear USB port of the board (not a front-panel or monitor hub, at
least the first time); USB 3 or USB 2 both work. Plug the keyboard and
mouse in before powering on.

## 4. What you should see

1. The Limine menu (black, "android-pc" entry, a 3 s countdown, then a few
   lines of Limine messages).
2. The kernel log in small text on a black screen (Pause/Break toggles it
   later): `acpi:` (tables, CPUs, timers), `LAPIC timer … calibrated
   against the HPET`, `SMP: all N CPUs online` (24 with hyper-threading on a 13700K), `pci:`, `nvme`/`ahci`
   lines for the PC's own disks, `xhci:` controllers and devices,
   `usb-storage: sdX: SanDisk …`, then the important one:
   `boot disk: sdX (… MiB, the one the kernel was loaded from), boot device
   pci0000:00/…, partitions sdX2,super;sdX3,frp;sdX4,misc`. Then Android's
   init lines (`init: init first stage started!`, `init: init second
   stage started!`, services starting). Warnings are yellow, errors red.
3. After roughly half a minute to a few minutes the Android boot animation
   replaces the log (the display now belongs to Android). Booting
   continues for a few more minutes (everything is rendered on the CPU).
4. The launcher (home screen) at 1920x1080, or first the lock screen
   (no PIN: drag upward from the bottom with the mouse). The mouse moves a
   pointer, the keyboard types. **Pause/Break** shows the kernel log over Android and
   hides it again.

### Wired network (E4)

Plug a cable from the board's 2.5 GbE port (RTL8125) into a router with
DHCP before or after booting. In the kernel log (Pause/Break once
Android is up):

- early, after `pci:`: `rtl8125: probing 04:00.0 10ec:8125 rev 05`,
  then `rtl8125: eth0: RTL8125B XID 641, MAC xx:xx:…:xx (MAC0_BKP),
  BAR2 …, polled, no firmware` and `rtl8125: eth0: up, waiting for link`;
- a few seconds later: `rtl8125: eth0: link up, 1000 Mb/s full-duplex`
  (or 2500 / 100, as the router negotiates; with the PHYstatus and PHYSR
  values) and `net: eth0 carrier on`;
- once Android's framework runs (a minute or more into the boot):
  `net: eth0 address 192.168.x.y/24` and `net: default route via
  192.168.x.1 dev eth0 table …`: DHCP worked;
- every few seconds while traffic flows: `rtl8125: eth0: rx N pkts …
  B, E err D drop; tx N pkts … B, B busy`. Rising rx and tx counts with
  0 err mean the card works.

Then the status bar shows the Ethernet icon (`<··>`), Chrome/the browser
loads pages, and from another machine on the LAN `adb connect
192.168.x.y:5555` followed by `adb shell` works (accept nothing: adbd
runs without authentication on this build). Pulling the cable logs
`link down` and `net: eth0 carrier off`; plugging it back in gets a new
DHCP lease.

### Sound (phase E5)

Android plays through the motherboard's HD Audio (Realtek ALC897): the
rear green Line Out and the front-panel headphone jack play the same
sound at the same time (no automatic muting). The kernel log (Pause/Break)
shows, shortly after the `xhci:` lines:

```
hda 0000:00:1f.3: 8086:7a50 (class 04.03.00)
hda 0000:00:1f.3: HDA 1.0, N input / N output streams, 64-bit true
hda 0000:00:1f.3: codec mask 0x0001          (0x0005 if the iGPU's HDMI codec is on)
hda 0000:00:1f.3: codec 0: Realtek ALC897 (10ec:0897 ...), AFG 0x01, NN widgets
hda 0000:00:1f.3:   pin 0x14 01014010 Line Out Rear Green jack ...
hda 0000:00:1f.3:   pin 0x1b 02214020 HP Out Front Green jack ...   (numbers vary by board)
hda 0000:00:1f.3:   LineOut: 0x14 -> 0x0c -> 0x02
hda 0000:00:1f.3:   Headphone: 0x1b -> 0x0c -> 0x02
hda 0000:00:1f.3: Front Headphone Jack (pin 0x1b): plugged|empty
hda 0000:00:1f.3: card 0 pcm 0 (Realtek Analog): stream N tag 1, 1 DAC(s), volume 0..87 (0 dB at 87)
```

and, for the graphics card's HDMI/DP audio (card 1, device 3; Android does
not use it unless the command line has `snd.primary=hdmi`):
`hda: NVIDIA ... audio function ... enabled`, `hda 0000:01:00.1: codec
0: Nvidia HDMI/DP ...`, `HDMI: 0x05 -> 0x04` lines, `card 1 pcm 3`.

When Android plays something (a key click in the launcher, a video, an
alarm preview in Settings > Sound) the log gets `snd: pcmC0D0p hw_params
48000 Hz, 2 ch, S16_LE, 4 x 1024 frames` and `hda 0000:00:1f.3: stream 1
start`; Android stops the stream ~3 s after the sound ends (`stream 1
stop`). `snd: pcmC0D0p underrun` lines now and then are harmless; many
in a row (with crackling) or `DMA position stalled` mean the stream does
not run: report them with the `hda` lines above. If nothing is heard but
the stream starts, report which jack you used and the `pin` lines.

Power off and restart from Android (hold the power key or use the power
menu's **Power off** / **Restart**); see section 5. Holding the case's
power button for 4 s still cuts the power whatever the software does;
nothing on the stick needs a clean shutdown.

## 5. Power, clock, temperatures (phase E9)

The kernel log shows one line per platform device at boot:

- `acpi: … reset 0xcf9=0x6, S5 (…)`: what power off and restart use.
  **Power off** (Android's power menu, `adb shell reboot -p`, `svc power
  shutdown`) ends in `reboot(2): power off`, `power off`, `acpi: entering
  S5 (SLP_TYPa … SLP_TYPb …)` and the PC turns off (ACPI S5 from the
  DSDT's `\_S5` package, no AML interpreter, so no `_PTS`). **Restart**
  ends in `restarting`, `acpi: reset register 0xcf9 (space 1) <- 0x6`; if
  that does nothing, `restart: trying port 0xcf9`, `… the keyboard
  controller`, `… triple fault` follow 100 ms apart. Both bring the log
  back over Android's picture first, so a PC that does not turn off or
  restart shows why (photograph the last lines).
- `acpi: switched to ACPI mode` (or `already in ACPI mode`) and `power
  button: ACPI fixed feature, PM1a_EVT 0x1800, SCI IRQ 9 masked, inputN`:
  the case's power button is the input device "Power Button" (KEY_POWER).
  A short press puts Android to sleep (screen off) and wakes it again;
  each press logs `power button pressed`. A long press of a fixed-feature
  button only sends one press (the hardware reports no release), so the
  power menu comes from the keyboard or `adb shell input keyevent
  --longpress POWER`.
- `rtc: CMOS RTC 2026-11-10 13:05:59 UTC (BCD, 24-hour, century reg
  0x32)`: the wall clock at boot, and /dev/rtc0 for Android (alarms,
  `hwclock`). The RTC is read as **UTC**, as Linux keeps it; a PC whose
  Windows keeps local time in the RTC needs `--append "rtc_offset=SECONDS"`
  for `tools/mkusb.sh` (e.g. `rtc_offset=25200` for UTC+7).
- `cpu: digital thermal sensors + package (TjMax 100 C), HWP on (EPP
  balance_performance), APERF/MPERF yes, hybrid P/E cores, base 3400 MHz`
  and `cpufreq: 24 policies (intel_pstate, HWP), 16 P + 8 E CPUs online`
  (or similar): HWP lets the P-cores turbo; the scheduler wakes idle
  P-cores first. `thermal: x86_pkg_temp thermal_zone0, coretemp 16 cores,
  package 35 C`: the temperatures in /sys/class/thermal and
  /sys/class/hwmon/hwmon0 (`adb shell cat
  /sys/class/thermal/thermal_zone0/temp`). `nohwp` on the command line
  leaves the firmware's P-states.
- `idle: mwait, hint 0x20 (MWAIT C3)`: idle CPUs use deep C-states (C6)
  instead of `hlt`; `idle=hlt` on the command line goes back to `hlt`
  (try it if the PC freezes when idle), `idle=c1` stays in C1.
- `power supply: AC (Mains) online, no battery`: Android's battery icon
  shows the PC as plugged in.

## 6. When something fails: what to report

There is no network and no serial port, so the screen is the log. Please
send:

1. **Photos of the screen**, as sharp as possible (whole screen, then a
   close-up of the last 20 lines). If Android's picture is up (boot
   animation or a frozen screen), press **Pause/Break** first to bring the
   kernel log back, wait a second, then photograph it. If the log scrolls,
   wait until it stops (or 2 minutes) before the photo.
2. **Where it stopped**: firmware menu without a `UEFI:` entry for the
   stick, Limine menu, black screen without text, the kernel log
   (which last line), boot animation (for how long), launcher.
3. The `xhci:` lines: every connected root port (`usb1 port N (USB 2):
   connected`), each device's speed, slot, route and TT, its VID:PID and
   interfaces (`if0 alt 0 class 03/01/01 …`), what got bound (`if2 bound
   as Mouse`) and the first reports a keyboard or mouse sends (`report
   [..]`, press a key and move the mouse once Android is up, then
   Pause/Break to show the log). If input stops working, the last
   `xhci: usb1 status:` line (printed once a second while counters change)
   shows the controller status, the event ring position and, per keyboard
   or mouse, reports, errors, the last completion code, queued transfers
   and the endpoint state (1 running, 2 halted).
4. For Wi-Fi: every `iwlwifi:` and `wifi:` line (firmware version,
   `ALIVE status`, `PNVM`, `NVM`, `regulatory`, `ready`, scans, joining,
   auth/assoc replies with their status codes, `FIRMWARE ERROR` and the
   two error-table lines). Turn Wi-Fi on and try to connect first, then
   Pause/Break and photograph the log.
5. Any line with `KERNEL PANIC`, `ERROR` (red) or `WARN` (yellow),
   especially `xhci:`, `usb-storage:`, `boot disk:`, `SMP:`, `acpi:`,
   `simpledrm:`, `rtc:`, `power button:`, `cpu:`, `idle:`.
6. Which USB port the stick was in (rear/front, USB 2 black or USB 3
   blue/red/teal), what keyboard and mouse were plugged in and where, and
   the monitor's resolution.
7. For the network: every `rtl8125:` and `net:` line (probe, XID, MAC,
   `link up`/`link down` with PHYstatus/PHYSR, the counter lines) and
   whether the port's LEDs light up.

Common cases:

- *No `UEFI: SanDisk` entry in the F12 menu*: Secure Boot or CSM is on,
  or the ESP lacks `EFI/BOOT/BOOTX64.EFI` (re-run `tools/mkusb.sh`).
- *Limine says the kernel or config is missing*: re-run `tools/mkusb.sh`
  and eject the stick properly.
- *`boot disk: no disk has a GPT partition named "super"`*: the stick was
  not detected (look for `xhci:`/`usb-storage:` warnings above it; try a
  rear USB 2 port) or partition 2 is not named `super`.
- *Android stops at `init: … Failed to mount /system`* or similar:
  partition 2 does not hold `super.img` (re-run the `dd`).
- *No `rtl8125:` line at all*: the card is disabled in the firmware setup
  (Settings → IO Ports → Onboard LAN Controller) or has another PCI id
  (`rtl8125: no RTL8125 found`).
- *`rtl8125: … link up` never comes*: check the cable and the router's
  port LEDs; the driver loads no rtl_nic firmware patch, which Linux
  applies when available. If the link stays down or flaps (`link up` /
  `link down` repeating), that patch may be needed: report the PHYstatus
  values.
- *Link up but no `net: eth0 address`*: DHCP got no answer; the counter
  line shows whether frames arrive (rx) and leave (tx); `err` counts
  receive errors.
- *Power off leaves the PC on* (the log shows `acpi: entering S5` and then
  `power off: ACPI S5 did not take`): photograph the `acpi:` lines from
  the start of the log (the `S5` values and the tables) and hold the
  power button.
- *The PC freezes once Android is idle*: add `idle=hlt` (deep C-states).

## 7. Bluetooth (Intel AX211, phase E7)

The board's AX211 Bluetooth sits on USB (`8087:0033`, full speed, root
port 14 of the xHCI at 00:14.0). At boot the kernel downloads its
firmware (`intel/ibt-0040-0041.sfi` and `.ddc` from linux-firmware,
built into the kernel; the controller asks for `ibt-1040-0041`, which
linux-firmware links to the same file) and hands the controller to
Android's Bluetooth HAL as an H4 link on `/dev/hvc5`. In Android:
Settings → Connected devices → Pair new device (Bluetooth turns on by
itself after boot; the quick settings tile toggles it). Headphones
(A2DP), phones, LE devices and Bluetooth keyboards/mice should pair
(the stack creates those through `/dev/uhid`; the kernel log then shows
`uhid: "<name>" 0005:vvvv:pppp: … input N`).
Not supported: voice over Bluetooth (SCO/HFP audio, which needs the
isochronous USB endpoints), anything after suspend, and pairings do not
survive a reboot (`/data` is in RAM). The AX211's antennas must be
screwed on the board's back panel, as for Linux.

On screen (the kernel log; Pause/Break shows it over Android) the
`bluetooth:` lines tell where it stands:

1. `xhci: usb1-14: 8087:0033 class 0xe0 …`, then `bluetooth: usb1-14
   8087:0033: … full speed` and `endpoints: events 81 (64 B), ACL in 82
   out 02 (64 B)`.
2. `bluetooth: intel: hardware variant 0x…, CNVi … CNVr …, bootloader
   (build 38 19.40 …), …, OTP address XX:XX:…` (after a warm reboot it
   may say `operational firmware` and skip the download).
3. `downloading intel/ibt-1040-0041.sfi (721052 bytes, version 24-29.26,
   boot address 0x100800) in 2909 commands`, then `download 25%` … `100%`,
   `firmware loaded in N ms` (about 1–3 s), `firmware booted in N ms`.
4. `DDC parameters: 2 of 2 records applied`, `running operational
   firmware build …`, and `Intel controller ready in N ms (…), address
   XX:XX:…`.
5. `bluetooth: usb1-14 8087:0033 is hci0, on /dev/hvc5 (H4) for the
   Bluetooth HAL`.
6. Once Android turns Bluetooth on: `bluetooth: command 0x03/0x003 []`
   (the HAL's HCI Reset), `bluetooth: event [0e, 04, 01, 03, 0c, 00]`,
   then the stack's first commands and events, and every 10 s at most a
   counters line `bluetooth: hci0: N commands, N ACL out, N events, N ACL
   in, N failed commands, N dropped (HAL attached)`. Failed commands are
   logged (`command 0x…/0x… failed, status 0x…`, the first 20).

If it fails, photograph the `bluetooth:` lines: a red `Intel setup
failed: …` line names the step (`no answer` to a command, `secure send
k/2909: …`, `no firmware download result`, `firmware did not boot`,
`firmware … is not built into the kernel` if the build host lacked
linux-firmware), `transfer error` warnings give the xHCI completion code.

Testing without the PC: `tools/run-qemu.sh --aosp-bt-test` runs AOSP's
rootcanal (a software controller) on the host behind a virtio-console
port, which the kernel treats like the USB controller; the firmware
download is covered by the in-kernel tests (`--test`,
`bluetooth_intel_download`, against a simulated Intel bootloader).

## 8. Wi-Fi (Intel AX211, phase E6)

`tools/mkusb.sh` also copies `boot/firmware/iwlwifi-so-a0-gf-a0-89.ucode`
and `…gf-a0.pnvm` (decompressed from the host's
`/lib/firmware/intel/iwlwifi`); Limine loads them as modules. In Android,
Settings → Network & internet → Internet lists the networks once Wi-Fi is
on; pick one and enter its password. The driver has not run on this PC
yet: report the `iwlwifi:` and `wifi:` lines (section 6, item 4). Details
and the expected log lines: `docs/wifi.md`.

## 9. The webcam and its microphone (E8)

Tested with a UGREEN Camera 4K (`eba4:6579`, high speed, behind a
Genesys hub): UVC 1.00 with MJPEG (3840x2160 to 480x320) and YUYV
(640x480 to 2592x1944) on isochronous alternate settings 1 to 11 (up to
3 x 1024 bytes per microframe), and a UAC 1.0 microphone (48 kHz, 2
channels, 16-bit, 192 bytes every millisecond). Plug it in before
powering on (no hot-plug).

What the kernel provides:

- **Camera** → `/dev/video0` (V4L2, MMAP streaming; `uvcvideo`-like
  `bus_info` `usb-0000:00:14.0-11.2`). The formats, frame sizes and
  intervals are the camera's own.
- **Microphone** → its own ALSA card (`/dev/snd/pcmC1D0c` behind the
  board's HD Audio, card 0) **and card 0's capture device**
  `/dev/snd/pcmC0D0c`. AOSP's audio HAL (the AIDL example HAL in
  `com.android.hardware.audio`) plays to and records its "Built-In Mic"
  from card 0 device 0 only; HD Audio has no capture PCM, so the kernel
  gives card 0 a `pcmC0D0c` that records from the first microphone card
  (the log says `sound: card 0 (…) has no capture: pcmC0D0c records from
  card N "…" (Android's microphone)`). Android thus plays through HD
  Audio and records from the webcam, at whatever rate and channel count
  the app asks for (8 to 48 kHz, mono or stereo: the kernel converts).
  **Recording works in any app** (Sound Recorder, a voice search, `adb
  shell tinycap … -D 0 -d 0`). `--aosp-audio-test` checks this with the
  kernel's test microphone in place of the webcam (tinycap and an
  AudioRecord app). Without HD Audio the webcam's card is card 0 itself.

**Why the Camera app shows no camera.** The Cuttlefish vendor image of
this AOSP build ships only the *emulated* camera HAL
(`com.google.emulated.camera.provider.hal`, from
`hardware/google/camera/devices/EmulatedCamera`), which draws synthetic
scenes and never opens `/dev/video*`. AOSP's V4L2 camera HAL exists in
the source tree but is not part of the product:
`android.hardware.camera.provider-V1-external-service`
(`hardware/interfaces/camera/provider/default` and `device/default`). It
watches `/dev` for `video*` nodes and uses exactly the ioctls the kernel
implements (QUERYCAP, ENUM_FMT — it only accepts MJPEG (and Z16 depth),
which the UGREEN offers up to 4K —, ENUM_FRAMESIZES,
ENUM_FRAMEINTERVALS, G/S_PARM, S_FMT, REQBUFS MMAP, QUERYBUF + mmap,
QBUF/DQBUF, STREAMON/OFF). Enabling it means changing the vendor image,
which this project does not do (no edits to `aosp/`):

1. `PRODUCT_PACKAGES += android.hardware.camera.provider-V1-external-service`
   (e.g. in `device/google/cuttlefish/shared/camera/device_vendor.mk`);
2. a VINTF manifest entry for the AIDL HAL
   `android.hardware.camera.provider.ICameraProvider/external/0` (the
   module has no `vintf_fragments`; add one, or `DEVICE_MANIFEST_FILE`),
   otherwise servicemanager refuses to register it;
3. `PRODUCT_COPY_FILES += frameworks/native/data/etc/android.hardware.camera.external.xml:$(TARGET_COPY_OUT_VENDOR)/etc/permissions/android.hardware.camera.external.xml`
   so apps see external cameras;
4. optionally `/vendor/etc/external_camera_config.xml` (without it the
   HAL uses its defaults and treats every `/dev/video*` as external);
5. SELinux: `hal_camera_default` needs `video_device` access (moot while
   the kernel does not enforce).

Then `m vendorimage superimage`, `dd` the new `super.img` to the stick
(section 2), and the Camera app lists the webcam as an external camera
next to the emulated back/front ones. The kernel side needs no change.

What to look for in the kernel log (Pause/Break; lines are printed once,
then the streaming ones at most once a second):

1. Enumeration: `xhci: usb1-11.2: high speed, slot N, …`, `eba4:6579
   class 0xef "Image+" "UGREEN Camera 4K"`, the interfaces (`if1 alt 11
   class 0e/02/00 endpoints [82/1024B/1]`, `if3 alt 1 class 01/02/00 …`).
2. Camera: `uvc: usb1-11.2 video: UVC 1.00, control if0, streaming if1,
   2 formats, isochronous alts [1:1x192 … 11:3x1024]`, then
   `v4l2: video0 "Image+ UGREEN Camera 4K": Motion-JPEG (1): 3840x2160@30
   1280x720@60 …` and `YUYV 4:2:2 (2): …`, then `… is /dev/video0`.
3. Microphone: `uac: usb1-11.2 audio: ADC 1.00, control if2, input
   terminals [0x0201], 1 capture settings`, `if3 alt 1: 2 ch, 16-bit in 2
   bytes, rates [48000], endpoint 83 192 bytes every 8 intervals`,
   `sound: card 1 "Image+ UGREEN Camera 4K" (USB-Audio): capture
   pcmC1D0c, device 48000 Hz, 2 channels, 16-bit` (the card number
   follows the HD Audio cards), then `sound: card 0 (…) has no capture:
   pcmC0D0c records from card 1 …`.
4. When recording: `sound: card 1: hw_params 48000 Hz, 1 ch, …`, `uac:
   usb1-11.2 audio: streaming: if3 alt 1, endpoint 83 DCI 7, 1 x 192 bytes
   per interval (1000 per s), 64 TDs queued`, then `sound: card 1:
   capturing 48000 Hz 1 ch: 48000 frames/s, 0 overruns` (after a second,
   then every ten). Once a second also `uac: …: N TDs (+1000), …
   0 errors (0 missed, 0 ring overruns, …)`.
5. When a V4L2 program streams (with the HAL above, or a test binary):
   `uvc: …: committed format 1 frame 2 interval 333333 -> 333333 (max
   frame … bytes, max payload 3072 bytes): alt 11 (3072 bytes/interval)`,
   `v4l2: video0: stream on, Motion-JPEG 1280x720 …`, `v4l2: video0: first
   frame, … bytes`, then every second `v4l2: video0: 1280x720 Motion-JPEG:
   30.0 fps, 95 KiB/frame, 0 dropped (no buffer), 0 damaged; total …`
   and the endpoint's `uvc: …: N TDs (+8000), … errors …` line.

Trouble signs: `not enough USB bandwidth for alt N` (another device on
the same controller holds periodic bandwidth: move the camera to another
rear port, away from the hub), `missed` or `ring overruns` growing every
second (the kernel is not keeping up: fewer frames, more `damaged`),
`frame … damaged (…)` repeated (lost intervals; the camera at a smaller
size or a USB 2 port directly on the board), `probe SET_CUR failed` or
`SET_INTERFACE(…) failed` (the camera refuses the format: report the
lines above it), `USB Audio Class 2, not supported` (another microphone).
