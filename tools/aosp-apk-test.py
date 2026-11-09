#!/usr/bin/env python3
"""Install and run an APK built from source on the Rust kernel (D3b).

Runs QEMU (the command after "--", see run-qemu.sh --aosp-apk-test: the
--aosp-boot-test machine plus a QEMU monitor), waits for
sys.boot_completed=1 the way aosp-boot-test.py does, then:

- dismisses the keyguard and checks that Launcher3 is the resumed
  activity and draws on the screen (`screencap -p` not blank);
- `adb install` of apps/hello/build/hello.apk (built by run-qemu.sh with
  apps/hello/build.sh): PackageInstaller streams it in (splice),
  installd creates the app directories and extracts the JNI library,
  artd runs dex2oat (filter verify), `pm list packages` and
  `dumpsys package` know it;
- `am start -W -n com.androidpc.hello/.MainActivity`: a cold start, the
  app logs HELLO_OK after its x86_64 JNI call, `dumpsys activity` shows
  the activity resumed and `dumpsys window` its window focused, visible
  and with a surface; the screen (`screencap -p` and QEMU's screendump
  of the virtio-gpu scanout) shows the app's light background;
- the app and system_server keep running.

Usage: aosp-apk-test.py [--boot-timeout SECS] [--log FILE] [--port PORT]
                        [--monitor SOCKET] [--apk FILE] [--out DIR]
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


def load(name, file):
    spec = importlib.util.spec_from_file_location(name, os.path.join(HERE, file))
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    return m


adbt = load("aosp_adb_test", "aosp-adb-test.py")
gfx = load("aosp_gfx_test", "aosp-gfx-test.py")

PKG = "com.androidpc.hello"
ACTIVITY = f"{PKG}/.MainActivity"

MILESTONES = [
    ("second stage", rb"init: init second stage started!"),
    ("zygote started", rb"init: \.\.\. started service 'zygote'"),
    ("adbd started", rb"init: \.\.\. started service 'adbd'"),
]


class Run:
    def __init__(self, a):
        self.a, self.passed, self.total, self.failed = a, 0, 0, []

    def check(self, ok, name, detail=""):
        self.total += 1
        if ok:
            self.passed += 1
            print(f"aosp-apk-test: ok   {name}", flush=True)
        else:
            self.failed.append(name)
            print(f"aosp-apk-test: FAIL {name} {detail}", flush=True)
        return ok

    def sh(self, cmd, timeout=60):
        rc, out, err = adbt.adb(self.a, "-s", self.a.serial, "shell", cmd, timeout=timeout)
        return rc, out.replace("\r\n", "\n"), err

    def prop(self, name):
        return self.sh(f"getprop {name}")[1].strip()

    def resumed(self):
        _, out, _ = self.sh("dumpsys activity activities | grep -E 'ResumedActivity'", timeout=120)
        return out


def light_share(rows, top, bottom):
    """Share of the pixels in rows [top, bottom) (every 2nd row and pixel)
    that are light (every channel above 200): the hello app's background."""
    light = total = 0
    for r in rows[top:bottom:2]:
        for i in range(0, len(r), 8):
            total += 1
            light += min(r[i:i + 3]) > 200
    return light / max(total, 1)


def screencap(r, path):
    """(rows, lit pixels, colours, detail) of `adb exec-out screencap -p`;
    rows is None on an error."""
    a = r.a
    try:
        p = subprocess.run([a.adb, "-P", str(a.server_port), "-s", a.serial, "exec-out", "screencap", "-p"],
                           capture_output=True, timeout=120, env=a.env)
        with open(path, "wb") as f:
            f.write(p.stdout)
        w, h, rows = gfx.png_pixels(p.stdout)
        lit, colours = gfx.picture_stats(rows)
        return rows, lit, colours, f"{w}x{h}, {lit} lit pixels, {colours} colours"
    except Exception as e:  # noqa: BLE001 - report any decode failure
        return None, 0, 0, f"{type(e).__name__}: {e}"


def screendump(a, path):
    """(rows, detail) of QEMU's screendump of the scanout, or (None, error)."""
    if not a.monitor:
        return None, "no monitor socket"
    try:
        if os.path.exists(path):
            os.unlink(path)
        gfx.monitor_cmd(a.monitor, f"screendump {path}")
        w, h, rows = gfx.ppm_pixels(open(path, "rb").read())
        return rows, f"{w}x{h}"
    except Exception as e:  # noqa: BLE001
        return None, f"{type(e).__name__}: {e}"


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--boot-timeout", type=float, default=900, help="seconds until sys.boot_completed")
    ap.add_argument("--log", default=None)
    ap.add_argument("--port", type=int, default=15555)
    ap.add_argument("--server-port", type=int, default=15037)
    ap.add_argument("--adb", default=None)
    ap.add_argument("--monitor", default=None, help="QEMU monitor unix socket (for screendump)")
    ap.add_argument("--apk", default=os.path.join(os.path.dirname(HERE), "apps/hello/build/hello.apk"))
    ap.add_argument("--out", default=None, help="directory for the screenshots")
    ap.add_argument("qemu", nargs=argparse.REMAINDER)
    a = ap.parse_args()
    cmd = a.qemu[1:] if a.qemu[:1] == ["--"] else a.qemu
    if not cmd:
        ap.error("missing QEMU command")
    if not os.path.isfile(a.apk):
        ap.error(f"no APK at {a.apk} (apps/hello/build.sh)")
    if a.adb is None:
        top = subprocess.check_output(["git", "worktree", "list", "--porcelain"], cwd=os.path.dirname(HERE), text=True)
        built = os.path.join(top.splitlines()[0].removeprefix("worktree "), "aosp/out/host/linux-x86/bin/adb")
        a.adb = built if os.access(built, os.X_OK) else shutil.which("adb")
    home = tempfile.mkdtemp(prefix="aosp-apk-test-")
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

        done, deadline = "", start + a.boot_timeout
        while time.monotonic() < deadline and qemu.poll() is None:
            done = r.prop("sys.boot_completed")
            if done == "1":
                break
            time.sleep(5)
        if not r.check(done == "1", f"sys.boot_completed=1 ({time.monotonic() - start:.0f}s)", f"value '{done}'"):
            return report(r, log)
        ss = r.sh("pidof system_server")[1].strip()

        # 1. The launcher on the screen once the keyguard is gone.
        resumed, deadline = "", time.monotonic() + 120
        while time.monotonic() < deadline:
            r.sh("wm dismiss-keyguard")
            resumed = r.resumed()
            if "launcher3" in resumed.lower() and r.sh("dumpsys window | grep -c 'mCurrentFocus=.*[Ll]auncher'")[1].strip() not in ("", "0"):
                break
            time.sleep(3)
        r.check("launcher3" in resumed.lower(), "Launcher3 resumed, keyguard dismissed", resumed.strip()[:300])
        ok, detail = False, ""
        for _ in range(10):
            rows, lit, colours, detail = screencap(r, os.path.join(out_dir, "launcher.png"))
            ok = rows is not None and len(rows) == 1280 and lit > 500 and colours > 16
            if ok:
                break
            time.sleep(3)
        r.check(ok, f"launcher on screen (screencap {detail})", detail)

        # 2. adb install: streamed install (splice), installd, dex2oat.
        adbt.adb(a, "-s", a.serial, "logcat", "-c", timeout=30)
        rc, out, err = adbt.adb(a, "-s", a.serial, "install", "-r", "-t", a.apk, timeout=300)
        r.check(rc == 0 and "Success" in out, "adb install", (out + err).strip()[-300:])
        _, out, _ = r.sh(f"pm list packages {PKG}")
        r.check(f"package:{PKG}" in out.split(), f"pm list packages shows {PKG}", out.strip()[:200])
        _, out, _ = r.sh(f"pm path {PKG}")
        m = re.search(r"package:(/data/app/\S+)/base\.apk", out)
        r.check(m is not None, "installed under /data/app", out.strip()[:200])
        app_dir = m.group(1) if m else "/nonexistent"
        _, out, _ = r.sh(f"ls {app_dir}/lib/x86_64/libhello.so {app_dir}/oat/x86_64/base.vdex; "
                         f"stat -c '%U %n' /data/user/0/{PKG}")
        r.check("libhello.so" in out and "base.vdex" in out and "u0_a" in out,
                "installd: JNI library extracted, dex2oat output, app data dir", out.strip()[:300])
        _, out, _ = r.sh(f"logcat -d | grep -E 'Dexopt result: \\[packageName = {PKG}\\]|dex2oat64 .*{PKG}' | tail -3")
        r.check("status=PERFORMED" in out or "dex2oat" in out, "dex2oat ran (artd)", out.strip()[:300])
        _, out, _ = r.sh(f"dumpsys package {PKG} | grep -E 'versionName|primaryCpuAbi'")
        r.check("versionName=1.0" in out and "primaryCpuAbi=x86_64" in out, "dumpsys package: version, x86_64 ABI",
                out.strip()[:200])

        # 3. Start the activity.
        rc, out, _ = r.sh(f"am start -W -n {ACTIVITY}", timeout=180)
        r.check("Status: ok" in out and f"Activity: {ACTIVITY}" in out, "am start -W", out.strip()[:400])
        hello = ""
        for _ in range(20):
            hello = r.sh("logcat -d -s AndroidPcHello:I | grep HELLO_OK")[1].strip()
            if hello:
                break
            time.sleep(1)
        r.check("sum(1..100) = 5050" in hello and "machine=x86_64" in hello, "app ran its x86_64 JNI code (HELLO_OK)",
                hello[:200])
        resumed = r.resumed()
        r.check(ACTIVITY in resumed and "topResumedActivity" in resumed, "dumpsys activity: MainActivity resumed",
                resumed.strip()[:300])
        win, deadline = "", time.monotonic() + 60
        while time.monotonic() < deadline:
            _, win, _ = r.sh("dumpsys window windows", timeout=120)
            m = re.search(r"Window #\d+ Window\{\S+ u0 com\.androidpc\.hello/[^}]*\}:(.*?)(?=\n  Window #|\Z)", win, re.S)
            win = m.group(1) if m else ""
            if "isVisible=true" in win and "mHasSurface=true" in win:
                break
            time.sleep(2)
        r.check("isVisible=true" in win and "mHasSurface=true" in win, "dumpsys window: app window visible with a surface",
                " ".join(re.findall(r"(?:isVisible|mHasSurface|isOnScreen)=\w+", win)) or "no app window")
        _, out, _ = r.sh("dumpsys window | grep mCurrentFocus")
        r.check(PKG in out, "app window has the focus", out.strip()[:200])

        # The app's light background on the screen and on the scanout.
        share = 0.0
        for _ in range(10):
            rows, _, _, detail = screencap(r, os.path.join(out_dir, "hello.png"))
            share = light_share(rows, 200, 1100) if rows and len(rows) >= 1100 else 0.0
            if share > 0.6:
                break
            time.sleep(2)
        r.check(share > 0.6, f"app drawn (screencap: {share:.0%} light pixels)", detail)
        share, detail = 0.0, ""
        for _ in range(10):
            rows, detail = screendump(a, os.path.join(out_dir, "hello-scanout.ppm"))
            share = light_share(rows, 200, 1100) if rows and len(rows) >= 1100 else 0.0
            if share > 0.6:
                break
            time.sleep(2)
        r.check(share > 0.6, f"app on the virtio-gpu scanout (screendump: {share:.0%} light pixels)", detail)

        app = r.sh(f"pidof {PKG}")[1].strip()
        time.sleep(10)
        r.check(app != "" and r.sh(f"pidof {PKG}")[1].strip() == app, "app process keeps running", app)
        r.check(ss != "" and r.sh("pidof system_server")[1].strip() == ss, "system_server keeps running (no restart)", ss)
        # For a post-mortem (next to the screenshots).
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
        print(f"aosp-apk-test: PASS {r.passed}/{r.total}")
        return 0
    print(f"aosp-apk-test: {r.passed}/{r.total} passed, failed: {', '.join(r.failed)} (serial log: {log})")
    return 1


if __name__ == "__main__":
    sys.exit(main())
