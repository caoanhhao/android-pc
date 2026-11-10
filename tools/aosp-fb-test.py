#!/usr/bin/env python3
"""Boot AOSP on the firmware framebuffer (simpledrm, phase E).

Runs QEMU (the command after "--", see run-qemu.sh --aosp-fb-test: the
--aosp-boot-test machine with no virtio-gpu, a plain VGA / bochs-display
whose linear framebuffer OVMF's GOP (or the VGA BIOS's VBE) sets up and
Limine hands over, and a QEMU monitor), then:

- the kernel booted through the expected firmware and drove the boot
  framebuffer as the DRM device (simpledrm), with no virtio-gpu;
- /dev/dri/card0 is the simple-framebuffer platform device's;
- the graphics HALs and surfaceflinger run on it through the same
  software path as with virtio-gpu (minigbm dumb buffers, the ranchu
  composer on KMS, RenderEngine on ANGLE over SwiftShader);
- sys.boot_completed becomes 1 and the boot animation exits;
- Launcher3 is resumed with the keyguard dismissed and `screencap -p`
  shows it (not blank);
- QEMU's screendump of the VGA framebuffer (what the kernel copied there
  on each page flip) is not blank and matches the screencap, so the
  copy and the pixel format conversion are right;
- surfaceflinger and system_server keep running.

With --usb (run-qemu.sh --usb-boot-test: OVMF boots Limine from the ESP of
a USB stick image on a qemu-xhci, no virtio disks), also:
- the kernel found the stick as an sd disk and resolved
  androidboot.boot_devices/partition_map=auto to its controller and its
  super/frp/misc partitions (the disk Limine loaded it from);
- /dev/block/by-name/super is that disk's partition 2, below the xHCI
  function in sysfs; no virtio-blk disk exists;
- the USB keyboard and mouse are evdev devices (no virtio-input).

Usage: aosp-fb-test.py [--boot-timeout SECS] [--log FILE] [--port PORT]
                       [--monitor SOCKET] [--out DIR] [--firmware UEFI|BIOS]
                       [--size WxH] [--usb] -- qemu-system-x86_64 ...
Exit status 0 iff every check passed.
"""
import argparse
import importlib.util
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))


def load(name, file):
    spec = importlib.util.spec_from_file_location(name, os.path.join(HERE, file))
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    return m


adbt = load("aosp_adb_test", "aosp-adb-test.py")
gfx = load("aosp_gfx_test", "aosp-gfx-test.py")
apk = load("aosp_apk_test", "aosp-apk-test.py")

SERVICES = ["vendor.graphics.allocator", "vendor.hwcomposer-3", "surfaceflinger"]


class Run(apk.Run):
    def check(self, ok, name, detail=""):
        self.total += 1
        if ok:
            self.passed += 1
            print(f"aosp-fb-test: ok   {name}", flush=True)
        else:
            self.failed.append(name)
            print(f"aosp-fb-test: FAIL {name} {detail}", flush=True)
        return ok


def match_share(a_rows, b_rows):
    """Share of sampled pixels (every 4th row and pixel) whose channels
    all differ by at most 24 between two same-sized pictures."""
    same = total = 0
    for ra, rb in zip(a_rows[::4], b_rows[::4]):
        for i in range(0, min(len(ra), len(rb)), 16):
            total += 1
            same += all(abs(x - y) <= 24 for x, y in zip(ra[i:i + 3], rb[i:i + 3]))
    return same / max(total, 1)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--boot-timeout", type=float, default=900, help="seconds until sys.boot_completed")
    ap.add_argument("--log", default=None)
    ap.add_argument("--port", type=int, default=15555)
    ap.add_argument("--server-port", type=int, default=15037)
    ap.add_argument("--adb", default=None)
    ap.add_argument("--monitor", default=None, help="QEMU monitor unix socket (for screendump)")
    ap.add_argument("--out", default=None, help="directory for the screenshots")
    ap.add_argument("--firmware", default="UEFI", help="what the kernel must report having booted from")
    ap.add_argument("--size", default="720x1280", help="the framebuffer mode")
    ap.add_argument("--usb", action="store_true", help="AOSP boots from a USB stick (run-qemu.sh --usb-boot-test)")
    ap.add_argument("qemu", nargs=argparse.REMAINDER)
    a = ap.parse_args()
    cmd = a.qemu[1:] if a.qemu[:1] == ["--"] else a.qemu
    if not cmd:
        ap.error("missing QEMU command")
    width, height = (int(x) for x in a.size.split("x"))
    if a.adb is None:
        top = subprocess.check_output(["git", "worktree", "list", "--porcelain"], cwd=os.path.dirname(HERE), text=True)
        built = os.path.join(top.splitlines()[0].removeprefix("worktree "), "aosp/out/host/linux-x86/bin/adb")
        a.adb = built if os.access(built, os.X_OK) else shutil.which("adb")
    home = tempfile.mkdtemp(prefix="aosp-fb-test-")
    a.env = dict(os.environ, HOME=home, ADB_MDNS="0", ADB_MDNS_AUTO_CONNECT="", ANDROID_SDK_HOME=home)
    a.env.pop("ANDROID_SERIAL", None)
    a.serial = f"127.0.0.1:{a.port}"
    out_dir = a.out or home
    os.makedirs(out_dir, exist_ok=True)
    log = a.log or os.path.join(home, "serial.log")
    milestones = [
        (f"booted from {a.firmware}", re.escape(f"firmware: {a.firmware}").encode()),
        (f"boot framebuffer {a.size}", re.escape(f"framebuffer: {a.size} 32 bpp").encode()),
        ("simpledrm drives it", re.escape(f"simpledrm: {a.size} boot framebuffer").encode() + rb" at 0x[0-9a-f]+ as card0"),
    ]
    if a.usb:
        milestones += [
            ("USB stick is an sd disk", rb"usb-storage: sd[a-z]: "),
            ("boot disk resolved (the one Limine loaded the kernel from)",
             rb"boot disk: sd[a-z] \(\d+ MiB, the one the kernel was loaded from\), boot device pci0000:00/0000:00:[0-9a-f.]+, "
             rb"partitions sd[a-z]2,super;sd[a-z]3,frp;sd[a-z]4,misc"),
        ]
    milestones += [
        ("second stage", rb"init: init second stage started!"),
        ("zygote started", rb"init: \.\.\. started service 'zygote'"),
        ("adbd started", rb"init: \.\.\. started service 'adbd'"),
    ]
    r = Run(a)
    qemu = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=open(log, "wb"), stderr=subprocess.STDOUT)
    try:
        start = time.monotonic()
        for name, rx in milestones:
            ok = adbt.wait_log(log, rx, start + a.boot_timeout, qemu)
            if not r.check(ok, f"{name} ({time.monotonic() - start:.1f}s)", f"({rx.decode()}) not seen"):
                return report(r, log)
        r.check(b"virtio-gpu:" not in open(log, "rb").read(), "no virtio-gpu")

        adbt.adb(a, "start-server", timeout=60)
        state, deadline = "", time.monotonic() + 120
        while time.monotonic() < deadline:
            rc, out, err = adbt.adb(a, "connect", a.serial, timeout=20)
            if "connected to" in out:
                rc, out, err = adbt.adb(a, "-s", a.serial, "wait-for-device", "get-state", timeout=20)
                state = out.strip()
                if state == "device":
                    break
            time.sleep(2)
        if not r.check(state == "device", f"adb connect {a.serial}", f"state '{state}'"):
            return report(r, log)

        _, out, _ = r.sh("ls -l /dev/dri/card0; readlink /sys/class/drm/card0")
        r.check(re.search(r"226,\s+0 .*card0", out) and "platform/simple-framebuffer.0/drm/card0" in out,
                "card0 is the simple-framebuffer device", out.strip())
        if a.usb:
            _, out, _ = r.sh("readlink /dev/block/by-name/super; readlink /sys/block/sda; ls /sys/block")
            lines = out.split()
            r.check(lines[:1] == ["/dev/block/sda2"], "by-name/super is sda2", out.strip())
            r.check(len(lines) > 1 and re.search(r"/usb\d+/\d+-\d+/\d+-\d+:1\.0/host\d+/target\d+:0:0/\d+:0:0:0/block/sda$", lines[1]),
                    "sda is below the xHCI controller", out.strip())
            r.check(not any(l.startswith("vd") for l in lines[2:]), "no virtio disk", out.strip())
            _, out, _ = r.sh("cat /proc/cmdline")
            r.check("androidboot.partition_map=sda2,super;sda3,frp;sda4,misc" in out
                    and re.search(r"androidboot\.boot_devices=pci0000:00/0000:00:[0-9a-f.]+ ", out), "partition map filled in",
                    out.strip()[:300])
            _, out, _ = r.sh("getevent -p 2>/dev/null | grep name:")
            r.check("QEMU QEMU USB Keyboard" in out and "QEMU QEMU USB Mouse" in out and "virtio" not in out.lower(),
                    "USB keyboard and mouse, no virtio input", out.strip())

        # The graphics services, on the same software path as virtio-gpu.
        deadline = start + a.boot_timeout
        states = {}
        while time.monotonic() < deadline and qemu.poll() is None:
            _, out, _ = r.sh("; ".join(f"getprop init.svc.{s}" for s in SERVICES))
            states = dict(zip(SERVICES, out.split()))
            if all(states.get(s) == "running" for s in SERVICES):
                break
            time.sleep(3)
        for s in SERVICES:
            r.check(states.get(s) == "running", f"service {s} running", f"state {states.get(s)}")
        found = {}
        want = {
            "renderengine on swiftshader": r"SurfaceFlinger: renderer\s*: ANGLE .*SwiftShader",
            f"composer on DRM ({a.size})": rf"RanchuHwc: init display:0 with config: id: 0 w:{width} h:{height}",
        }
        while time.monotonic() < deadline and len(found) < len(want):
            _, out, _ = r.sh("logcat -d -b main -b system")
            found.update({k: True for k, rx in want.items() if re.search(rx, out)})
            if len(found) < len(want):
                time.sleep(3)
        for k in want:
            r.check(k in found, k)

        done = ""
        while time.monotonic() < deadline and qemu.poll() is None:
            done = r.prop("sys.boot_completed")
            if done == "1":
                break
            time.sleep(5)
        if not r.check(done == "1", f"sys.boot_completed=1 ({time.monotonic() - start:.0f}s)", f"value '{done}'"):
            return report(r, log)
        ss, sf = r.sh("pidof system_server")[1].strip(), r.sh("pidof surfaceflinger")[1].strip()
        anim, anim_deadline = "", time.monotonic() + 120
        while time.monotonic() < anim_deadline:
            anim = r.prop("service.bootanim.exit")
            if anim == "1":
                break
            time.sleep(3)
        r.check(anim == "1", "boot animation exited", f"service.bootanim.exit '{anim}'")

        # The launcher, on screen (SurfaceFlinger) and in the framebuffer.
        resumed, deadline = "", time.monotonic() + 120
        while time.monotonic() < deadline:
            r.sh("wm dismiss-keyguard")
            resumed = r.resumed()
            if "launcher3" in resumed.lower() and r.sh("dumpsys window | grep -c 'mCurrentFocus=.*[Ll]auncher'")[1].strip() not in ("", "0"):
                break
            time.sleep(3)
        r.check("launcher3" in resumed.lower(), "Launcher3 resumed, keyguard dismissed", resumed.strip()[:300])
        ok, detail, cap = False, "", None
        for _ in range(10):
            cap, lit, colours, detail = apk.screencap(r, os.path.join(out_dir, "launcher.png"))
            ok = cap is not None and len(cap) == height and len(cap[0]) == width * 4 and lit > 500 and colours > 16
            if ok:
                break
            time.sleep(3)
        r.check(ok, f"launcher on screen (screencap {detail})", detail)
        ok, detail = False, ""
        for _ in range(10):
            dump, detail = apk.screendump(a, os.path.join(out_dir, "launcher-framebuffer.ppm"))
            if dump is not None and cap is not None and len(dump) == height:
                lit, colours = gfx.picture_stats(dump)
                share = match_share(cap, dump)
                detail += f", {lit} lit pixels, {colours} colours, {share:.0%} like the screencap"
                ok = lit > 500 and colours > 16 and share > 0.9
            if ok:
                break
            # The launcher may still be settling: take a fresh screencap.
            time.sleep(3)
            cap = apk.screencap(r, os.path.join(out_dir, "launcher.png"))[0] or cap
        r.check(ok, f"launcher in the boot framebuffer (screendump {detail})", detail)

        time.sleep(10)
        r.check(sf != "" and r.sh("pidof surfaceflinger")[1].strip() == sf, "surfaceflinger keeps running", sf)
        r.check(ss != "" and r.sh("pidof system_server")[1].strip() == ss, "system_server keeps running (no restart)", ss)
        with open(os.path.join(out_dir, "logcat.txt"), "w") as f:
            f.write(r.sh("logcat -d -b all", timeout=120)[1])
        adbt.adb(a, "disconnect", a.serial)
    finally:
        adbt.adb(a, "kill-server", timeout=20)
        qemu.kill()
        qemu.wait()
        shutil.rmtree(home, ignore_errors=True) if out_dir != home else None
    return report(r, log)


def report(r, log):
    if not r.failed:
        print(f"aosp-fb-test: PASS {r.passed}/{r.total}")
        return 0
    print(f"aosp-fb-test: {r.passed}/{r.total} passed, failed: {', '.join(r.failed)} (serial log: {log})")
    return 1


if __name__ == "__main__":
    sys.exit(main())
