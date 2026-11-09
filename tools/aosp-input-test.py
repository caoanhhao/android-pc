#!/usr/bin/env python3
"""Check input devices and `adb exec-out` in unmodified AOSP on the Rust kernel.

Runs QEMU (the command after "--", see run-qemu.sh --aosp-input-test: the
--aosp-gfx-test boot, which has a virtio-gpu, virtio-keyboard/-tablet/
-multitouch and a QEMU monitor on a unix socket), waits for adbd and
connects the host adb the way aosp-adb-test.py does (own server,
throw-away HOME). Then:

- ueventd made /dev/input/event0..2 (13:64+, group input) from our
  uevents, and /dev/uinput;
- `getevent -lp` lists the three virtio devices with their keys and axes;
- keys typed with the monitor's `sendkey` reach `getevent` on the
  keyboard's node (down and up, in order), and the monitor's
  `mouse_button` reaches a virtio pointer (tablet or multitouch);
- `sendevent` into the keyboard node is seen by another reader;
- `adb exec-out` returns everything a command writes (text, 2 MB, and a
  `screencap -p` PNG of the display's size).

zygote is stopped first (it needs userfaultfd: phase D3); so are
`input keyevent` and InputFlinger, which run in system_server.

Usage: aosp-input-test.py [--boot-timeout SECS] [--log FILE] [--port PORT]
                          [--monitor SOCKET] -- qemu-system-x86_64 ...
Exit status 0 iff every check passed.
"""
import argparse
import hashlib
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

MILESTONES = [
    ("second stage", rb"init: init second stage started!"),
    ("virtio-input devices", rb"input: input2 \(event2\): \"QEMU Virtio MultiTouch\""),
    ("boot", rb"init: processing action \(boot\)"),
    ("adbd started", rb"init: \.\.\. started service 'adbd'"),
]

DEVICES = {
    "QEMU Virtio Keyboard": [r"KEY_A\b", r"KEY_POWER\b", r"LED \(0011\): LED_NUML"],
    "QEMU Virtio Tablet": [r"ABS_X\s+: value \d+, min 0, max 32767", r"BTN_LEFT|BTN_MOUSE"],
    "QEMU Virtio MultiTouch": [r"ABS_MT_SLOT", r"ABS_MT_POSITION_X", r"ABS_MT_TRACKING_ID"],
}


def nodes_by_name(getevent_lp):
    """{device name: /dev/input/eventN} from `getevent -lp`."""
    out, cur = {}, None
    for line in getevent_lp.splitlines():
        m = re.match(r"add device \d+: (\S+)", line)
        if m:
            cur = m.group(1)
            continue
        m = re.match(r'\s+name:\s+"(.*)"', line)
        if m and cur:
            out[m.group(1)] = cur
    return out


def sections(getevent_lp):
    """{node: its block of `getevent -lp` text}."""
    out, cur = {}, None
    for line in getevent_lp.splitlines():
        m = re.match(r"add device \d+: (\S+)", line)
        if m:
            cur = m.group(1)
            out[cur] = ""
        if cur:
            out[cur] += line + "\n"
    return out


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--boot-timeout", type=float, default=420)
    ap.add_argument("--log", default=None)
    ap.add_argument("--port", type=int, default=15555)
    ap.add_argument("--server-port", type=int, default=15037)
    ap.add_argument("--adb", default=None)
    ap.add_argument("--monitor", required=True, help="QEMU monitor unix socket (sendkey, mouse_button)")
    ap.add_argument("--settle", type=float, default=150, help="seconds to wait for surfaceflinger")
    ap.add_argument("qemu", nargs=argparse.REMAINDER)
    a = ap.parse_args()
    cmd = a.qemu[1:] if a.qemu[:1] == ["--"] else a.qemu
    if not cmd:
        ap.error("missing QEMU command")
    if a.adb is None:
        top = subprocess.check_output(["git", "worktree", "list", "--porcelain"], cwd=os.path.dirname(HERE), text=True)
        built = os.path.join(top.splitlines()[0].removeprefix("worktree "), "aosp/out/host/linux-x86/bin/adb")
        a.adb = built if os.access(built, os.X_OK) else shutil.which("adb")
    home = tempfile.mkdtemp(prefix="aosp-input-test-")
    a.env = dict(os.environ, HOME=home, ADB_MDNS="0", ADB_MDNS_AUTO_CONNECT="", ANDROID_SDK_HOME=home)
    a.env.pop("ANDROID_SERIAL", None)
    a.serial = f"127.0.0.1:{a.port}"
    log = a.log or os.path.join(home, "serial.log")
    r = gfx.Run(a)
    r.check = lambda ok, name, detail="": check(r, ok, name, detail)
    base = [a.adb, "-P", str(a.server_port), "-s", a.serial]
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
        r.sh("stop zygote; stop zygote_secondary")

        # Nodes made by ueventd.
        rc, out, _ = r.sh("ls -l /dev/input /dev/uinput")
        nodes = re.findall(r"crw-rw---- \d+ root\s+input\s+13,\s+(\d+) .* (event\d+)", out)
        r.check(len(nodes) >= 3 and all(int(m) == 64 + int(n[5:]) for m, n in nodes), "ueventd made /dev/input/event*", out.strip())
        r.check(re.search(r"10,\s+223 .*/dev/uinput", out) is not None, "ueventd made /dev/uinput", out.strip())

        # getevent -lp: names, keys, axes.
        rc, lp, _ = r.sh("getevent -lp")
        by_name = nodes_by_name(lp)
        secs = sections(lp)
        for dev, rxs in DEVICES.items():
            node = by_name.get(dev)
            text = secs.get(node, "")
            missing = [x for x in rxs if not re.search(x, text)]
            r.check(node is not None and not missing, f"getevent -lp: {dev}", f"node {node}, missing {missing}")
        kbd, tablet = by_name.get("QEMU Virtio Keyboard"), by_name.get("QEMU Virtio Tablet")

        # sendkey from the QEMU monitor -> getevent on the keyboard node.
        if kbd:
            p = subprocess.Popen(base + ["shell", f"getevent -lt -c 12 {kbd}"], stdout=subprocess.PIPE,
                                 stderr=subprocess.STDOUT, env=a.env, text=True, errors="replace")
            time.sleep(3)
            for k in ("a", "shift-b"):
                gfx.monitor_cmd(a.monitor, f"sendkey {k}", timeout=10)
            try:
                out, _ = p.communicate(timeout=30)
            except subprocess.TimeoutExpired:
                p.kill()
                out, _ = p.communicate()
            evs = re.findall(r"EV_KEY\s+(KEY_\w+)\s+(DOWN|UP)", out)
            want = [("KEY_A", "DOWN"), ("KEY_A", "UP"), ("KEY_LEFTSHIFT", "DOWN"), ("KEY_B", "DOWN"), ("KEY_B", "UP"), ("KEY_LEFTSHIFT", "UP")]
            r.check(all(w in evs for w in want) and evs.index(("KEY_A", "DOWN")) < evs.index(("KEY_A", "UP")),
                    "QEMU sendkey seen by getevent", out.strip()[:600])
            syns = len(re.findall(r"EV_SYN\s+SYN_REPORT", out))
            r.check(syns >= 2, "key packets end in SYN_REPORT", f"{syns} SYN_REPORTs")

            # sendevent into the node reaches another reader.
            p = subprocess.Popen(base + ["shell", f"getevent -l -c 2 {kbd}"], stdout=subprocess.PIPE,
                                 stderr=subprocess.STDOUT, env=a.env, text=True, errors="replace")
            time.sleep(3)
            r.sh(f"sendevent {kbd} 17 1 1; sendevent {kbd} 0 0 0; sendevent {kbd} 17 1 0; sendevent {kbd} 0 0 0")
            try:
                out, _ = p.communicate(timeout=20)
            except subprocess.TimeoutExpired:
                p.kill()
                out, _ = p.communicate()
            r.check(re.search(r"EV_LED\s+LED_CAPSL\s+00000001", out) is not None, "sendevent seen by getevent", out.strip()[:300])

        # Pointer buttons from the monitor reach a virtio pointer (QEMU
        # sends them to the most recently activated one; HMP's mouse_move
        # is relative motion, which goes to the PS/2 mouse).
        if tablet:
            p = subprocess.Popen(base + ["shell", "getevent -l -c 4"], stdout=subprocess.PIPE,
                                 stderr=subprocess.STDOUT, env=a.env, text=True, errors="replace")
            time.sleep(3)
            gfx.monitor_cmd(a.monitor, "mouse_button 1", timeout=10)
            gfx.monitor_cmd(a.monitor, "mouse_button 0", timeout=10)
            try:
                out, _ = p.communicate(timeout=20)
            except subprocess.TimeoutExpired:
                p.kill()
                out, _ = p.communicate()
            pointers = {n for d, n in by_name.items() if "Tablet" in d or "MultiTouch" in d}
            hits = re.findall(r"(/dev/input/event\d+): EV_KEY\s+BTN_(?:MOUSE|LEFT)\s+(DOWN|UP)", out)
            r.check({x[1] for x in hits} == {"DOWN", "UP"} and all(x[0] in pointers for x in hits),
                    "QEMU mouse_button seen by getevent", out.strip()[:300])

        # adb exec-out: adbd's raw service (a PTY in raw mode).
        p = subprocess.run(base + ["exec-out", "echo exec-out-works; echo second line"], capture_output=True, timeout=60, env=a.env)
        r.check(p.stdout == b"exec-out-works\nsecond line\n" and p.returncode == 0, "adb exec-out echo", repr(p.stdout[:100]))
        p = subprocess.run(base + ["exec-out", "toybox seq 1 300000"], capture_output=True, timeout=120, env=a.env)
        want = "".join(f"{i}\n" for i in range(1, 300001)).encode()
        r.check(p.stdout == want, "adb exec-out 2 MB (seq 1 300000)", f"{len(p.stdout)} bytes, md5 {hashlib.md5(p.stdout).hexdigest()}")

        # screencap through exec-out: wait for surfaceflinger first.
        deadline = time.monotonic() + a.settle
        while time.monotonic() < deadline:
            _, out, _ = r.sh("getprop init.svc.surfaceflinger; service check SurfaceFlinger")
            if "running" in out and "found" in out:
                break
            time.sleep(3)
        detail, png_ok = "", False
        for _ in range(10):
            p = subprocess.run(base + ["exec-out", "screencap -p"], capture_output=True, timeout=120, env=a.env)
            try:
                w, h, rows = gfx.png_pixels(p.stdout)
                lit, colours = gfx.picture_stats(rows)
                detail = f"{len(p.stdout)} bytes, {w}x{h}, {lit} lit pixels"
                png_ok = (w, h) == (720, 1280)
            except Exception as e:  # noqa: BLE001 - report any decode failure
                detail = f"{type(e).__name__}: {e} ({len(p.stdout)} bytes, rc {p.returncode})"
            if png_ok:
                break
            time.sleep(5)
        r.check(png_ok, f"adb exec-out screencap -p is a 720x1280 PNG ({detail})", detail)
        adbt.adb(a, "disconnect", a.serial)
    finally:
        adbt.adb(a, "kill-server", timeout=20)
        qemu.kill()
        qemu.wait()
        shutil.rmtree(home, ignore_errors=True)
    return report(r, log)


def check(r, ok, name, detail=""):
    r.total += 1
    if ok:
        r.passed += 1
        print(f"aosp-input-test: ok   {name}")
    else:
        r.failed.append(name)
        print(f"aosp-input-test: FAIL {name} {detail}")
    return ok


def report(r, log):
    if not r.failed:
        print(f"aosp-input-test: PASS {r.passed}/{r.total}")
        return 0
    print(f"aosp-input-test: {r.passed}/{r.total} passed, failed: {', '.join(r.failed)} (serial log: {log})")
    return 1


if __name__ == "__main__":
    sys.exit(main())
