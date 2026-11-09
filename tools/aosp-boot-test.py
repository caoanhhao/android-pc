#!/usr/bin/env python3
"""Check that AOSP boots to sys.boot_completed=1 on the Rust kernel (D3a).

Runs QEMU (the command after "--", see run-qemu.sh --aosp-boot-test: the
--aosp-gfx-test machine with more memory and CPUs), waits for adbd and
connects the host adb the way aosp-adb-test.py does (own server,
throw-away HOME). Then:

- zygote runs ART with the concurrent mark-compact GC (userfaultfd) and
  forks system_server;
- sys.boot_completed becomes 1 and the boot animation exits;
- system_server registered activity, package, window, input and power
  with servicemanager, `dumpsys activity` and `dumpsys window` answer,
  and Launcher3 is the home activity;
- system_server and zygote keep their pids for a while after boot
  (no crash / watchdog restarts) and `dumpsys package` knows the
  framework.

Usage: aosp-boot-test.py [--boot-timeout SECS] [--log FILE] [--port PORT]
                         -- qemu-system-x86_64 ...
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
spec = importlib.util.spec_from_file_location("aosp_adb_test", os.path.join(HERE, "aosp-adb-test.py"))
adbt = importlib.util.module_from_spec(spec)
spec.loader.exec_module(adbt)

MILESTONES = [
    ("second stage", rb"init: init second stage started!"),
    ("zygote started", rb"init: \.\.\. started service 'zygote'"),
    ("adbd started", rb"init: \.\.\. started service 'adbd'"),
]

SERVICES = ["activity", "package", "window", "input", "power", "user", "display", "SurfaceFlinger"]


class Run:
    def __init__(self, a):
        self.a, self.passed, self.total, self.failed = a, 0, 0, []

    def check(self, ok, name, detail=""):
        self.total += 1
        if ok:
            self.passed += 1
            print(f"aosp-boot-test: ok   {name}", flush=True)
        else:
            self.failed.append(name)
            print(f"aosp-boot-test: FAIL {name} {detail}", flush=True)
        return ok

    def sh(self, cmd, timeout=60):
        rc, out, err = adbt.adb(self.a, "-s", self.a.serial, "shell", cmd, timeout=timeout)
        return rc, out.replace("\r\n", "\n"), err

    def prop(self, name):
        return self.sh(f"getprop {name}")[1].strip()


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--boot-timeout", type=float, default=900, help="seconds until sys.boot_completed")
    ap.add_argument("--log", default=None)
    ap.add_argument("--port", type=int, default=15555)
    ap.add_argument("--server-port", type=int, default=15037)
    ap.add_argument("--adb", default=None)
    ap.add_argument("qemu", nargs=argparse.REMAINDER)
    a = ap.parse_args()
    cmd = a.qemu[1:] if a.qemu[:1] == ["--"] else a.qemu
    if not cmd:
        ap.error("missing QEMU command")
    if a.adb is None:
        top = subprocess.check_output(["git", "worktree", "list", "--porcelain"], cwd=os.path.dirname(HERE), text=True)
        built = os.path.join(top.splitlines()[0].removeprefix("worktree "), "aosp/out/host/linux-x86/bin/adb")
        a.adb = built if os.access(built, os.X_OK) else shutil.which("adb")
    home = tempfile.mkdtemp(prefix="aosp-boot-test-")
    a.env = dict(os.environ, HOME=home, ADB_MDNS="0", ADB_MDNS_AUTO_CONNECT="", ANDROID_SDK_HOME=home)
    a.env.pop("ANDROID_SERIAL", None)
    a.serial = f"127.0.0.1:{a.port}"
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

        # system_server comes from zygote's fork.
        deadline = start + a.boot_timeout
        ss = ""
        while time.monotonic() < deadline and qemu.poll() is None:
            ss = r.sh("pidof system_server")[1].strip()
            if ss:
                break
            time.sleep(5)
        r.check(ss != "", f"system_server forked by zygote ({time.monotonic() - start:.0f}s)")

        done = ""
        while time.monotonic() < deadline and qemu.poll() is None:
            done = r.prop("sys.boot_completed")
            if done == "1":
                break
            time.sleep(5)
        if not r.check(done == "1", f"sys.boot_completed=1 ({time.monotonic() - start:.0f}s)", f"value '{done}'"):
            _, out, _ = r.sh("logcat -d -b main -b system | grep -E 'SystemServerTiming|AndroidRuntime' | tail -15")
            print(out)
            return report(r, log)
        r.check(r.prop("dev.bootcomplete") == "1", "dev.bootcomplete=1")
        # ART's concurrent mark-compact GC runs on userfaultfd: its log
        # lines, or system_server's open [userfaultfd] file.
        _, out, _ = r.sh("logcat -d -b main -b system | grep -cE 'CollectorTypeCMC|concurrent mark compact GC'; "
                         "ls -l /proc/$(pidof system_server)/fd | grep -c userfaultfd")
        counts = [int(x) for x in out.split() if x.isdigit()]
        r.check(sum(counts) > 0, "ART uses the CMC GC (userfaultfd)", out.strip()[:200])

        # The boot animation ends once the launcher draws or boot completes.
        anim = ""
        anim_deadline = time.monotonic() + 120
        while time.monotonic() < anim_deadline:
            anim = r.prop("service.bootanim.exit")
            if anim == "1":
                break
            time.sleep(3)
        r.check(anim == "1", "boot animation exited", f"service.bootanim.exit '{anim}', init.svc.bootanim '{r.prop('init.svc.bootanim')}'")

        _, out, _ = r.sh("service list")
        listed = set(re.findall(r"^\d+\s+(\S+):", out, re.M))
        for s in SERVICES:
            r.check(s in listed, f"service list has {s}")
        rc, out, _ = r.sh("dumpsys activity activities", timeout=120)
        r.check(rc == 0 and "ACTIVITY MANAGER ACTIVITIES" in out, "dumpsys activity activities", out.strip()[:300])
        # The home activity (Launcher3) was started by ActivityManager.
        home_task = re.search(r"type=home .*I=(\S+)", out)
        r.check(home_task is not None and "launcher" in home_task.group(1).lower(), "launcher is the home activity",
                home_task.group(1) if home_task else out.strip()[:300])
        rc, out, _ = r.sh("dumpsys window displays", timeout=120)
        r.check(rc == 0 and "WINDOW MANAGER DISPLAY CONTENTS" in out, "dumpsys window displays", out.strip()[:300])
        rc, out, _ = r.sh("pm path android", timeout=120)
        r.check(rc == 0 and "framework-res.apk" in out, "pm path android", out.strip()[:300])

        ss1, zy1 = r.sh("pidof system_server")[1].strip(), r.sh("pidof zygote64")[1].strip()
        time.sleep(30)
        ss2, zy2 = r.sh("pidof system_server")[1].strip(), r.sh("pidof zygote64")[1].strip()
        r.check(ss1 != "" and ss1 == ss2, "system_server keeps running", f"{ss1} -> {ss2}")
        r.check(zy1 != "" and zy1 == zy2, "zygote keeps running", f"{zy1} -> {zy2}")
        r.check(r.prop("sys.boot_completed") == "1", "still booted (no runtime restart)")
        adbt.adb(a, "disconnect", a.serial)
    finally:
        adbt.adb(a, "kill-server", timeout=20)
        qemu.kill()
        qemu.wait()
        shutil.rmtree(home, ignore_errors=True)
    return report(r, log)


def report(r, log):
    if not r.failed:
        print(f"aosp-boot-test: PASS {r.passed}/{r.total}")
        return 0
    print(f"aosp-boot-test: {r.passed}/{r.total} passed, failed: {', '.join(r.failed)} (serial log: {log})")
    return 1


if __name__ == "__main__":
    sys.exit(main())
