#!/usr/bin/env python3
"""Check that Android's Bluetooth stack comes up on the Rust kernel (E7).

The controller is rootcanal, AOSP's software Bluetooth controller, run on
the host (aosp/out/host/linux-x86/bin/root-canal) and wired to the guest
through a virtio-console port (QEMU's virtconsole on a socket chardev to
rootcanal's HCI port, see run-qemu.sh --aosp-bt-test). The kernel makes
that port the controller behind /dev/hvc5, where Cuttlefish's Bluetooth
HAL (android.hardware.bluetooth-service.cuttlefish) speaks H4: the same
path a USB controller takes on a real PC, minus the USB transfers.

Checks:
- the kernel bound the virtio-console as hci0;
- AOSP boots (sys.boot_completed=1) and the HAL service runs;
- Bluetooth turns on (`cmd bluetooth_manager enable`, then the state ON
  in `dumpsys bluetooth_manager`), its address is rootcanal's, the
  kernel log shows the HAL's HCI Reset and its completion;
- a scan (Settings' Bluetooth device picker, which starts discovery)
  receives LE advertising reports of rootcanal's two default beacons,
  counted by the kernel's `bluetooth: hci0:` counters line;
- Bluetooth turns off and on again.

Usage: aosp-bt-test.py --rootcanal PATH --hci-port P --test-port T
                       [--boot-timeout SECS] [--log FILE] [--port PORT]
                       -- qemu-system-x86_64 ...
Exit status 0 iff every check passed.
"""
import argparse
import importlib.util
import os
import re
import shutil
import socket
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("aosp_adb_test", os.path.join(HERE, "aosp-adb-test.py"))
adbt = importlib.util.module_from_spec(spec)
spec.loader.exec_module(adbt)

TAG = "aosp-bt-test"


class Run:
    def __init__(self, a):
        self.a, self.passed, self.total, self.failed = a, 0, 0, []

    def check(self, ok, name, detail=""):
        self.total += 1
        if ok:
            self.passed += 1
            print(f"{TAG}: ok   {name}", flush=True)
        else:
            self.failed.append(name)
            print(f"{TAG}: FAIL {name} {detail}", flush=True)
        return ok

    def sh(self, cmd, timeout=60):
        rc, out, err = adbt.adb(self.a, "-s", self.a.serial, "shell", cmd, timeout=timeout)
        return rc, out.replace("\r\n", "\n"), err

    def prop(self, name):
        return self.sh(f"getprop {name}")[1].strip()


def rootcanal_cmd(s, name, *args):
    """One command on rootcanal's test channel (one connection for the
    whole run: rootcanal drops the next one after a client leaves):
    length-prefixed name and arguments; answers are a 4-byte length and
    text."""
    msg = bytes([len(name)]) + name.encode() + bytes([len(args)])
    for x in args:
        msg += bytes([len(x)]) + x.encode()
    s.sendall(msg)
    out = b""
    deadline = time.monotonic() + 3
    while time.monotonic() < deadline:
        try:
            chunk = s.recv(4096)
        except socket.timeout:
            continue
        if not chunk:
            break
        out += chunk
        # Skip "RegisterSendResponse called" and similar notes.
        if b"Devices:" in out or b"called with" in out:
            break
    return out.decode(errors="replace")


def bt_state(r):
    _, out, _ = r.sh("dumpsys bluetooth_manager | head -40", timeout=60)
    m = re.search(r"^\s*state:\s*(\S+)", out, re.M)
    return (m.group(1) if m else ""), out


def wait_state(r, want, secs):
    deadline = time.monotonic() + secs
    state, out = "", ""
    while time.monotonic() < deadline:
        state, out = bt_state(r)
        if state == want:
            break
        time.sleep(3)
    return state, out


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--boot-timeout", type=float, default=900)
    ap.add_argument("--log", default=None)
    ap.add_argument("--port", type=int, default=15555)
    ap.add_argument("--server-port", type=int, default=15047)
    ap.add_argument("--adb", default=None)
    ap.add_argument("--rootcanal", required=True)
    ap.add_argument("--hci-port", type=int, required=True)
    ap.add_argument("--test-port", type=int, required=True)
    ap.add_argument("qemu", nargs=argparse.REMAINDER)
    a = ap.parse_args()
    cmd = a.qemu[1:] if a.qemu[:1] == ["--"] else a.qemu
    if not cmd:
        ap.error("missing QEMU command")
    top = subprocess.check_output(["git", "worktree", "list", "--porcelain"], cwd=os.path.dirname(HERE), text=True)
    if a.adb is None:
        built = os.path.join(top.splitlines()[0].removeprefix("worktree "), "aosp/out/host/linux-x86/bin/adb")
        a.adb = built if os.access(built, os.X_OK) else shutil.which("adb")
    home = tempfile.mkdtemp(prefix=f"{TAG}-")
    a.env = dict(os.environ, HOME=home, ADB_MDNS="0", ADB_MDNS_AUTO_CONNECT="", ANDROID_SDK_HOME=home)
    a.env.pop("ANDROID_SERIAL", None)
    a.serial = f"127.0.0.1:{a.port}"
    log = a.log or os.path.join(home, "serial.log")
    rc_log = os.path.splitext(log)[0] + "-rootcanal.log"
    r = Run(a)
    base = a.hci_port
    rc = subprocess.Popen([a.rootcanal, f"-hci_port={a.hci_port}", f"-test_port={a.test_port}",
                           f"-link_port={base + 1}", f"-link_ble_port={base + 2}"],
                          stdout=open(rc_log, "wb"), stderr=subprocess.STDOUT)
    qemu = test = None
    try:
        deadline = time.monotonic() + 20
        test = None
        while time.monotonic() < deadline and test is None:
            try:
                test = socket.create_connection(("127.0.0.1", a.test_port), timeout=1)
            except OSError:
                time.sleep(0.3)
        if not r.check(test is not None, "rootcanal listening", f"(log {rc_log})"):
            return report(r, log)
        test.settimeout(0.5)
        # rootcanal's default beacons (be:ac:01:55:00:01/02) on the LE phy.
        out = rootcanal_cmd(test, "list")
        print(f"{TAG}: rootcanal: {' '.join(out.split())[-200:]}")

        qemu = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=open(log, "wb"), stderr=subprocess.STDOUT)
        start = time.monotonic()
        ok = adbt.wait_log(log, rb"bluetooth: virtio-console \S+ \(H4\) is hci0", start + 120, qemu)
        r.check(ok, "kernel: virtio-console bound as hci0 on /dev/hvc5")
        ok = adbt.wait_log(log, rb"init: \.\.\. started service 'bt_hci'", start + a.boot_timeout, qemu)
        r.check(ok, f"Bluetooth HAL service started ({time.monotonic() - start:.0f}s)")
        ok = adbt.wait_log(log, rb"init: \.\.\. started service 'adbd'", start + a.boot_timeout, qemu)
        if not r.check(ok, "adbd started"):
            return report(r, log)

        adbt.adb(a, "start-server", timeout=60)
        state, deadline = "", time.monotonic() + 120
        while time.monotonic() < deadline:
            _, out, _ = adbt.adb(a, "connect", a.serial, timeout=20)
            if "connected to" in out:
                _, out, _ = adbt.adb(a, "-s", a.serial, "wait-for-device", "get-state", timeout=20)
                state = out.strip()
                if state == "device":
                    break
            time.sleep(2)
        if not r.check(state == "device", f"adb connect {a.serial}", f"state '{state}'"):
            return report(r, log)

        deadline = start + a.boot_timeout
        done = ""
        while time.monotonic() < deadline and qemu.poll() is None:
            done = r.prop("sys.boot_completed")
            if done == "1":
                break
            time.sleep(5)
        if not r.check(done == "1", f"sys.boot_completed=1 ({time.monotonic() - start:.0f}s)", f"value '{done}'"):
            return report(r, log)
        pid = r.sh("pidof android.hardware.bluetooth-service.cuttlefish")[1].strip()
        r.check(pid != "", "Bluetooth HAL running")

        # On by default after boot on Cuttlefish; enable explicitly too.
        r.sh("cmd bluetooth_manager enable", timeout=60)
        st, out = wait_state(r, "ON", 180)
        if not r.check(st == "ON", "Bluetooth state ON", f"state '{st}':\n{out[:1500]}"):
            _, lc, _ = r.sh("logcat -d -b main -b system | grep -iE 'bluetooth|bt_' | tail -40")
            print(lc)
            return report(r, log)
        addr = r.sh("settings get secure bluetooth_address")[1].strip()
        r.check(re.fullmatch(r"[0-9A-F]{2}(:[0-9A-F]{2}){5}", addr, re.I) is not None and addr != "00:00:00:00:00:00",
                f"adapter address {addr}")
        with open(log, "rb") as f:
            serial = f.read()
        r.check(b"bluetooth: command 0x03/0x003" in serial, "kernel saw the HAL's HCI Reset")
        r.check(re.search(rb"bluetooth: event \[0e, 04, 01, 03, 0c, 00\]", serial) is not None,
                "kernel delivered the Reset complete")

        # A scan: Settings' device picker starts discovery (inquiry and LE
        # scan); rootcanal's two beacons advertise on its LE phy. The
        # kernel's counters line (every 10 s at most) counts the reports.
        r.sh("input keyevent KEYCODE_WAKEUP; wm dismiss-keyguard", timeout=30)
        r.sh("am start -a android.bluetooth.devicepicker.action.LAUNCH", timeout=60)
        adv, deadline = 0, time.monotonic() + 90
        while time.monotonic() < deadline and adv == 0:
            time.sleep(5)
            with open(log, "rb") as f:
                counts = re.findall(rb"bluetooth: hci0: .*?(\d+) LE advertising reports, (\d+) inquiry results", f.read())
            if counts:
                adv = int(counts[-1][0])
        r.check(adv > 0, f"LE scan sees rootcanal's beacons ({adv} advertising reports)")
        _, out, _ = r.sh("dumpsys bluetooth_manager | grep -iE 'BE:AC:01:55' | head -5", timeout=60)
        print(f"{TAG}: dumpsys on the beacons: {out.strip()[:300] or '(nothing)'}")
        r.sh("input keyevent KEYCODE_HOME", timeout=30)

        r.sh("cmd bluetooth_manager disable", timeout=60)
        st, _ = wait_state(r, "OFF", 120)
        r.check(st == "OFF", "Bluetooth turns off", f"state '{st}'")
        r.sh("cmd bluetooth_manager enable", timeout=60)
        st, _ = wait_state(r, "ON", 120)
        r.check(st == "ON", "Bluetooth turns on again", f"state '{st}'")
        r.check(r.prop("sys.boot_completed") == "1", "still booted (no runtime restart)")
        adbt.adb(a, "disconnect", a.serial)
    finally:
        adbt.adb(a, "kill-server", timeout=20)
        if qemu:
            qemu.kill()
            qemu.wait()
        if test:
            test.close()
        rc.kill()
        rc.wait()
        shutil.rmtree(home, ignore_errors=True)
    return report(r, log)


def report(r, log):
    if not r.failed:
        print(f"{TAG}: PASS {r.passed}/{r.total}")
        return 0
    print(f"{TAG}: {r.passed}/{r.total} passed, failed: {', '.join(r.failed)} (serial log: {log})")
    return 1


if __name__ == "__main__":
    sys.exit(main())
