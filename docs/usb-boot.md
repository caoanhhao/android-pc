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
keyboards and mice (boot protocol). What does not: networking (no driver
for the Intel I225-V / Wi-Fi, so no adb), sound, the GPU's acceleration
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

Wi-Fi (Intel AX211, see `docs/wifi.md`): `tools/mkusb.sh` also copies
`boot/firmware/iwlwifi-so-a0-gf-a0-89.ucode` and `…gf-a0.pnvm`
(decompressed from the host's `/lib/firmware/intel/iwlwifi`). In Android,
Settings → Network & internet → Internet lists the networks once Wi-Fi is
on; pick one and enter its password.

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

Power off with the power button (or hold it); nothing on the stick needs
a clean shutdown.

## 5. When something fails: what to report

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
   `simpledrm:`.
6. Which USB port the stick was in (rear/front, USB 2 black or USB 3
   blue/red/teal), what keyboard and mouse were plugged in and where, and
   the monitor's resolution.

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
