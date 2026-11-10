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
keyboards and mice (boot protocol), and (E4, not yet tried on the PC)
wired Ethernet on the board's Realtek RTL8125 2.5 GbE port: Android's
EthernetService gets an address by DHCP, so DNS, the browser and `adb
connect <address>:5555` should work. What does not: Wi-Fi and Bluetooth
(Intel AX211), sound, the GPU's acceleration
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
4. Any line with `KERNEL PANIC`, `ERROR` (red) or `WARN` (yellow),
   especially `xhci:`, `usb-storage:`, `boot disk:`, `SMP:`, `acpi:`,
   `simpledrm:`, `rtc:`, `power button:`, `cpu:`, `idle:`.
5. Which USB port the stick was in (rear/front, USB 2 black or USB 3
   blue/red/teal), what keyboard and mouse were plugged in and where, and
   the monitor's resolution.
6. For the network: every `rtl8125:` and `net:` line (probe, XID, MAC,
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
