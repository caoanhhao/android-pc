#!/usr/bin/env python3
"""Check AOSP's graphics stack on the Rust kernel's virtio-gpu.

Runs QEMU (the command after "--", see run-qemu.sh --aosp-gfx-test: the
--aosp-adb-test boot plus a virtio-gpu at 00:02.0 and a QEMU monitor on a
unix socket), waits for adbd and connects the host adb the way
aosp-adb-test.py does (own server, throw-away HOME). Then:

- the DRM and dma-heap nodes exist (ueventd made them from our uevents);
- the minigbm allocator, the ranchu composer (HWC3 on DRM) and
  surfaceflinger are running and registered with servicemanager,
  SurfaceFlinger runs RenderEngine on ANGLE over SwiftShader and starts
  the boot animation, and surfaceflinger keeps its pid (no restarts);
- `adb shell screencap -p` and `adb exec-out screencap -p` give a PNG
  of the display's size that is not blank;
- QEMU's `screendump` of the virtio-gpu scanout (what the composer put on
  the screen through DRM) is not blank either.

zygote is stopped first: the graphics checks do not need the framework
(--aosp-boot-test boots it), and it would only cost CPU here. On a fast
boot the framework may already have finished (the boot animation exited,
the launcher and SystemUI are on screen) by the time adb connects; then
zygote is left running and the layer check accepts the framework's
layers instead of the boot animation's.

Usage: aosp-gfx-test.py [--boot-timeout SECS] [--log FILE] [--port PORT]
                        [--monitor SOCKET] [--out DIR] -- qemu-system-x86_64 ...
Exit status 0 iff every check passed.
"""
import argparse
import importlib.util
import os
import re
import socket
import struct
import sys
import time
import zlib

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("aosp_adb_test", os.path.join(HERE, "aosp-adb-test.py"))
adbt = importlib.util.module_from_spec(spec)
spec.loader.exec_module(adbt)

MILESTONES = [
    ("second stage", rb"init: init second stage started!"),
    ("virtio-gpu probed", rb"virtio-gpu: 1 scanout\(s\), scanout 0 \d+x\d+"),
    ("boot", rb"init: processing action \(boot\)"),
    ("adbd started", rb"init: \.\.\. started service 'adbd'"),
]

SERVICES = ["vendor.graphics.allocator", "vendor.hwcomposer-3", "surfaceflinger"]


def png_pixels(data):
    """(width, height, rows of RGBA bytes) of an 8-bit RGB/RGBA PNG."""
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise ValueError("not a PNG")
    pos, idat, hdr = 8, b"", None
    while pos < len(data):
        n, ty = struct.unpack(">I4s", data[pos:pos + 8])
        body = data[pos + 8:pos + 8 + n]
        pos += 12 + n
        if ty == b"IHDR":
            hdr = struct.unpack(">IIBBBBB", body)
        elif ty == b"IDAT":
            idat += body
        elif ty == b"IEND":
            break
    w, h, depth, ctype, _, _, interlace = hdr
    if depth != 8 or ctype not in (2, 6) or interlace:
        raise ValueError(f"unsupported PNG depth={depth} type={ctype} interlace={interlace}")
    bpp = 4 if ctype == 6 else 3
    raw = zlib.decompress(idat)
    stride = w * bpp
    rows, prev = [], bytearray(stride)
    for y in range(h):
        f = raw[y * (stride + 1)]
        line = bytearray(raw[y * (stride + 1) + 1:(y + 1) * (stride + 1)])
        for i in range(stride):
            a = line[i - bpp] if i >= bpp else 0
            b = prev[i]
            c = prev[i - bpp] if i >= bpp else 0
            if f == 1:
                line[i] = (line[i] + a) & 255
            elif f == 2:
                line[i] = (line[i] + b) & 255
            elif f == 3:
                line[i] = (line[i] + (a + b) // 2) & 255
            elif f == 4:
                p = a + b - c
                pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                line[i] = (line[i] + (a if pa <= pb and pa <= pc else b if pb <= pc else c)) & 255
        rows.append(bytes(line) if bpp == 4 else bytes(x for i in range(w) for x in (*line[i * 3:i * 3 + 3], 255)))
        prev = line
    return w, h, rows


def ppm_pixels(data):
    """(width, height, rows of RGBA bytes) of a binary PPM (P6)."""
    m = re.match(rb"P6\s+(\d+)\s+(\d+)\s+(\d+)\s", data)
    if not m:
        raise ValueError("not a P6 PPM")
    w, h = int(m.group(1)), int(m.group(2))
    px = data[m.end():m.end() + w * h * 3]
    rows = [bytes(x for i in range(w) for x in (*px[(y * w + i) * 3:(y * w + i) * 3 + 3], 255)) for y in range(h)]
    return w, h, rows


def picture_stats(rows):
    """(non-black pixel count, distinct colours) over every 2nd pixel of
    every 2nd row."""
    lit, colours = 0, set()
    for r in rows[::2]:
        for i in range(0, len(r), 8):
            px = r[i:i + 3]
            colours.add(px)
            if max(px) > 16:
                lit += 4
    return lit, len(colours)


def monitor_cmd(path, cmd, timeout=20):
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(timeout)
    s.connect(path)
    out = b""
    try:
        out += s.recv(4096)
        s.sendall(cmd.encode() + b"\n")
        time.sleep(1.5)
        s.settimeout(1)
        while True:
            chunk = s.recv(4096)
            if not chunk:
                break
            out += chunk
    except OSError:
        pass
    finally:
        s.close()
    return out.decode(errors="replace")


class Run:
    def __init__(self, a):
        self.a, self.passed, self.total, self.failed = a, 0, 0, []

    def check(self, ok, name, detail=""):
        self.total += 1
        if ok:
            self.passed += 1
            print(f"aosp-gfx-test: ok   {name}")
        else:
            self.failed.append(name)
            print(f"aosp-gfx-test: FAIL {name} {detail}")
        return ok

    def sh(self, cmd, timeout=60):
        rc, out, err = adbt.adb(self.a, "-s", self.a.serial, "shell", cmd, timeout=timeout)
        return rc, out.replace("\r\n", "\n"), err


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--boot-timeout", type=float, default=420)
    ap.add_argument("--log", default=None)
    ap.add_argument("--port", type=int, default=15555)
    ap.add_argument("--server-port", type=int, default=15037)
    ap.add_argument("--adb", default=None)
    ap.add_argument("--monitor", default=None, help="QEMU monitor unix socket (for screendump)")
    ap.add_argument("--out", default=None, help="directory for screencap.png / screendump.ppm")
    ap.add_argument("--settle", type=float, default=150, help="seconds to wait for the first frame")
    ap.add_argument("qemu", nargs=argparse.REMAINDER)
    a = ap.parse_args()
    cmd = a.qemu[1:] if a.qemu[:1] == ["--"] else a.qemu
    if not cmd:
        ap.error("missing QEMU command")
    if a.adb is None:
        import shutil
        import subprocess
        top = subprocess.check_output(["git", "worktree", "list", "--porcelain"], cwd=os.path.dirname(HERE), text=True)
        built = os.path.join(top.splitlines()[0].removeprefix("worktree "), "aosp/out/host/linux-x86/bin/adb")
        a.adb = built if os.access(built, os.X_OK) else shutil.which("adb")
    import shutil
    import subprocess
    import tempfile
    home = tempfile.mkdtemp(prefix="aosp-gfx-test-")
    a.env = dict(os.environ, HOME=home, ADB_MDNS="0", ADB_MDNS_AUTO_CONNECT="", ANDROID_SDK_HOME=home)
    a.env.pop("ANDROID_SERIAL", None)
    a.serial = f"127.0.0.1:{a.port}"
    out_dir = a.out or home
    os.makedirs(out_dir, exist_ok=True)
    log = a.log or os.path.join(home, "serial.log")
    r = Run(a)
    qemu = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=open(log, "wb"), stderr=subprocess.STDOUT)
    try:
        start = time.monotonic()
        for name, rx in MILESTONES:
            ok = adbt.wait_log(log, rx, start + a.boot_timeout, qemu)
            if not r.check(ok, f"{name} ({time.monotonic() - start:.1f}s)", f"({rx.decode()}) not seen"):
                return report(r, log)

        adbt.adb(a, "start-server", timeout=60)
        state, deadline = "", time.monotonic() + 90
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
        # Stop the framework while the boot animation still runs (it then
        # never exits). Once WindowManager has set service.bootanim.exit
        # the animation is on its way out (it plays its last frames
        # first), so the framework is kept.
        framework_up = r.sh("getprop service.bootanim.exit")[1].strip() == "1"
        if not framework_up:
            r.sh("stop zygote; stop zygote_secondary")
            framework_up = r.sh("getprop service.bootanim.exit")[1].strip() == "1"
        if framework_up:
            print("aosp-gfx-test: note: the framework finished booting first (service.bootanim.exit=1)")

        rc, out, _ = r.sh("ls -l /dev/dri/card0 /dev/dri/renderD128 /dev/dma_heap/system")
        r.check(rc == 0 and re.search(r"226,\s+0 .*card0", out) and re.search(r"226,\s+128 .*renderD128", out)
                and "dma_heap/system" in out, "device nodes from ueventd", out.strip())

        # The three graphics services up and registered.
        deadline = time.monotonic() + a.settle
        states = {}
        while time.monotonic() < deadline:
            _, out, _ = r.sh("; ".join(f"getprop init.svc.{s}" for s in SERVICES))
            states = dict(zip(SERVICES, out.split()))
            if all(states.get(s) == "running" for s in SERVICES):
                break
            time.sleep(3)
        for s in SERVICES:
            r.check(states.get(s) == "running", f"service {s} running", f"state {states.get(s)}")
        for name in ["android.hardware.graphics.allocator.IAllocator/default",
                     "android.hardware.graphics.composer3.IComposer/default", "SurfaceFlinger", "SurfaceFlingerAIDL"]:
            # A service registers a moment after init reports it running.
            while True:
                _, out, _ = r.sh(f"service check {name}")
                if f"Service {name}: found" in out or time.monotonic() >= deadline:
                    break
                time.sleep(2)
            r.check(f"Service {name}: found" in out, f"{name} registered", out.strip())

        # RenderEngine on ANGLE/SwiftShader, display connected, boot animation.
        found = {}
        want = {
            "renderengine on swiftshader": r"SurfaceFlinger: renderer\s*: ANGLE .*SwiftShader",
            "display connected": r"SurfaceFlinger: Connecting display",
            "composer on DRM (720x1280)": r"RanchuHwc: init display:0 with config: id: 0 w:720 h:1280",
            "boot animation": r"SurfaceFlinger: Enter boot animation",
        }
        while time.monotonic() < deadline and len(found) < len(want):
            _, out, _ = r.sh("logcat -d -b main -b system")
            for k, rx in want.items():
                if re.search(rx, out):
                    found[k] = True
            if len(found) < len(want):
                time.sleep(3)
        for k in want:
            r.check(k in found, k)
        _, pid1, _ = r.sh("pidof surfaceflinger")
        time.sleep(10)
        _, pid2, _ = r.sh("pidof surfaceflinger")
        r.check(pid1.strip() != "" and pid1.strip() == pid2.strip(), "surfaceflinger keeps running", f"{pid1.strip()} -> {pid2.strip()}")
        rc, out, _ = r.sh("dumpsys SurfaceFlinger --list", timeout=60)
        if framework_up:
            r.check(rc == 0 and ("BootAnimation" in out or re.search(r"NotificationShade|Launcher|StatusBar", out)),
                    "boot animation or framework layers (boot animation already done)", out.strip()[:300])
        else:
            r.check(rc == 0 and "BootAnimation" in out, "boot animation layer", out.strip()[:300])

        # screencap through SurfaceFlinger: a PNG that is not blank, read
        # from `adb shell screencap -p`'s stdout (shell protocol). The boot
        # animation fades its logo in, so try for a while.
        base = [a.adb, "-P", str(a.server_port), "-s", a.serial]
        for how, argv in [("adb shell screencap -p", base + ["shell", "screencap", "-p"]),
                          ("adb exec-out screencap -p", base + ["exec-out", "screencap", "-p"])]:
            png_ok, detail = False, ""
            cap_deadline = time.monotonic() + 90
            while time.monotonic() < cap_deadline and not png_ok:
                try:
                    p = subprocess.run(argv, capture_output=True, timeout=120, env=a.env)
                    data = p.stdout
                    with open(os.path.join(out_dir, "screencap.png"), "wb") as f:
                        f.write(data)
                    w, h, rows = png_pixels(data)
                    lit, colours = picture_stats(rows)
                    detail = f"{w}x{h}, {lit} lit pixels, {colours} colours"
                    png_ok = (w, h) == (720, 1280) and lit > 500 and colours > 4
                except Exception as e:  # noqa: BLE001 - report any decode failure
                    detail = f"{type(e).__name__}: {e} ({len(p.stdout) if 'p' in dir() else 0} bytes, rc {p.returncode}, {p.stderr[:200]!r})"
                if not png_ok:
                    time.sleep(5)
            r.check(png_ok, f"{how} not blank ({detail})", detail)

        # What reached the scanout: QEMU's screendump of the virtio-gpu.
        dump = os.path.join(out_dir, "screendump.ppm")
        dump_ok, detail = False, "no monitor socket"
        if a.monitor:
            for _ in range(10):
                if os.path.exists(dump):
                    os.unlink(dump)
                monitor_cmd(a.monitor, f"screendump {dump}")
                try:
                    w, h, rows = ppm_pixels(open(dump, "rb").read())
                    lit, colours = picture_stats(rows)
                    detail = f"{w}x{h}, {lit} lit pixels, {colours} colours"
                    dump_ok = (w, h) == (720, 1280) and lit > 500 and colours > 4
                except Exception as e:  # noqa: BLE001
                    detail = f"{type(e).__name__}: {e}"
                if dump_ok:
                    break
                time.sleep(5)
        r.check(dump_ok, f"QEMU screendump not blank ({detail})", detail)
        adbt.adb(a, "disconnect", a.serial)
    finally:
        adbt.adb(a, "kill-server", timeout=20)
        qemu.kill()
        qemu.wait()
        shutil.rmtree(home, ignore_errors=True) if out_dir != home else None
    return report(r, log)


def report(r, log):
    if not r.failed:
        print(f"aosp-gfx-test: PASS {r.passed}/{r.total}")
        return 0
    print(f"aosp-gfx-test: {r.passed}/{r.total} passed, failed: {', '.join(r.failed)} (serial log: {log})")
    return 1


if __name__ == "__main__":
    sys.exit(main())
