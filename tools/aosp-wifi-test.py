#!/usr/bin/env python3
"""Check that AOSP's Wi-Fi stack works on the Rust kernel (E6).

Runs QEMU (the command after "--", see run-qemu.sh --aosp-wifi-test: the
--aosp-boot-test machine plus the kernel's simulated radio, `wifisim=`,
whose access points bridge to a second virtio-net card on QEMU's
user-mode network 10.0.3.0/24). After sys.boot_completed=1:

- the kernel side: wlan0 (DEVTYPE=wlan, phy80211), /sys/class/ieee80211,
  /dev/rfkill and the rfkill class;
- `cmd wifi set-wifi-enabled enabled`: the Wi-Fi HAL, wificond and
  wpa_supplicant come up over nl80211 and Wi-Fi reports enabled;
- `cmd wifi start-scan`: both simulated access points show up in
  `cmd wifi list-scan-results`;
- `cmd wifi connect-network <ssid> wpa2 <passphrase>`: WPA2-Personal
  association and the 4-way handshake (wpa_supplicant over AF_PACKET),
  then DHCP gives wlan0 an address on 10.0.3.0/24 and the gateway
  answers pings through wlan0 (the internet path of QEMU's NAT);
- a wrong passphrase is refused (the simulated AP deauthenticates).

Usage: aosp-wifi-test.py [--boot-timeout SECS] [--log FILE] [--port PORT]
                         [--ssid S --psk P --open-ssid S]
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
spec = importlib.util.spec_from_file_location("aosp_boot_test", os.path.join(HERE, "aosp-boot-test.py"))
boot = importlib.util.module_from_spec(spec)
spec.loader.exec_module(boot)
adbt = boot.adbt


def wait(fn, secs, step=3):
    deadline = time.monotonic() + secs
    v = fn()
    while not v and time.monotonic() < deadline:
        time.sleep(step)
        v = fn()
    return v


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--boot-timeout", type=float, default=900)
    ap.add_argument("--log", default=None)
    ap.add_argument("--port", type=int, default=15555)
    ap.add_argument("--server-port", type=int, default=15037)
    ap.add_argument("--adb", default=None)
    ap.add_argument("--ssid", default="AndroidPC-WPA2")
    ap.add_argument("--psk", default="androidpc123")
    ap.add_argument("--open-ssid", default="AndroidPC-Open")
    ap.add_argument("qemu", nargs=argparse.REMAINDER)
    a = ap.parse_args()
    cmd = a.qemu[1:] if a.qemu[:1] == ["--"] else a.qemu
    if not cmd:
        ap.error("missing QEMU command")
    if a.adb is None:
        top = subprocess.check_output(["git", "worktree", "list", "--porcelain"], cwd=os.path.dirname(HERE), text=True)
        built = os.path.join(top.splitlines()[0].removeprefix("worktree "), "aosp/out/host/linux-x86/bin/adb")
        a.adb = built if os.access(built, os.X_OK) else shutil.which("adb")
    home = tempfile.mkdtemp(prefix="aosp-wifi-test-")
    a.env = dict(os.environ, HOME=home, ADB_MDNS="0", ADB_MDNS_AUTO_CONNECT="", ANDROID_SDK_HOME=home)
    a.env.pop("ANDROID_SERIAL", None)
    a.serial = f"127.0.0.1:{a.port}"
    log = a.log or os.path.join(home, "serial.log")
    r = boot.Run(a)
    qemu = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=open(log, "wb"), stderr=subprocess.STDOUT)
    try:
        start = time.monotonic()
        for name, rx in boot.MILESTONES:
            ok = adbt.wait_log(log, rx, start + a.boot_timeout, qemu)
            if not r.check(ok, f"{name} ({time.monotonic() - start:.1f}s)", f"({rx.decode()}) not seen"):
                return report(r, log)
        ok = adbt.wait_log(log, rb"wifi: phy0 \(hwsim\) is wlan0", start + 60, qemu)
        r.check(ok, "kernel registered the simulated radio as wlan0")

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
        deadline = start + a.boot_timeout
        done = ""
        while time.monotonic() < deadline and qemu.poll() is None:
            done = r.prop("sys.boot_completed")
            if done == "1":
                break
            time.sleep(5)
        if not r.check(done == "1", f"sys.boot_completed=1 ({time.monotonic() - start:.0f}s)", f"value '{done}'"):
            return report(r, log)

        # The kernel side.
        _, out, _ = r.sh("cat /sys/class/net/wlan0/uevent; ls /sys/class/net/wlan0/phy80211/ /sys/class/ieee80211/; "
                         "ls -l /dev/rfkill; cat /sys/class/rfkill/rfkill0/type /sys/class/rfkill/rfkill0/state")
        r.check("DEVTYPE=wlan" in out, "wlan0 is a wlan device", out[:300])
        r.check("phy0" in out, "/sys/class/ieee80211/phy0", out[:300])
        r.check("/dev/rfkill" in out and "wlan" in out, "rfkill device for the radio", out[:300])

        # Enable Wi-Fi: HAL, wificond, wpa_supplicant over nl80211.
        r.sh("cmd wifi set-wifi-enabled enabled")
        status = lambda: (lambda s: s if "Wifi is enabled" in s else "")(r.sh("cmd wifi status")[1])
        st = wait(status, 120)
        if not r.check(bool(st), "Wi-Fi enabled", r.sh("cmd wifi status")[1][:300]):
            dump(r, "WifiService|WifiNative|WifiVendorHal|wificond|wpa_supplicant|android.hardware.wifi|WifiHal|HalDevMgr|SupplicantStaIfaceHal|nl80211|WifiController|ActiveModeWarden|ClientModeImpl|avc:")
        for svc in ["wificond", "wpa_supplicant"]:
            pid = wait(lambda: r.sh(f"pidof {svc}")[1].strip(), 30)
            r.check(pid != "", f"{svc} runs")
        hal = r.sh("ps -A | grep -c android.hardware.wifi")[1].strip()
        r.check(hal not in ("", "0"), "Wi-Fi HAL runs", hal)

        # Scan.
        def scan_has_both():
            r.sh("cmd wifi start-scan")
            time.sleep(4)
            out = r.sh("cmd wifi list-scan-results")[1]
            return out if a.ssid in out and a.open_ssid in out else ""
        res = wait(scan_has_both, 90, step=1)
        if not r.check(bool(res), f"scan finds {a.ssid} and {a.open_ssid}", r.sh("cmd wifi list-scan-results")[1][:500]):
            dump(r, "WifiScanning|WificondScan|wificond|scan|nl80211")

        # A wrong passphrase must not connect.
        r.sh(f"cmd wifi connect-network {a.ssid} wpa2 wrong-passphrase")
        time.sleep(15)
        st = r.sh("cmd wifi status")[1]
        r.check(f'connected to "{a.ssid}"' not in st, "wrong passphrase is refused", st[:300])
        r.sh(f"cmd wifi forget-network $(cmd wifi list-networks | grep {a.ssid} | cut -d' ' -f1)")

        # Connect with WPA2-Personal.
        r.sh(f"cmd wifi connect-network {a.ssid} wpa2 {a.psk}")
        conn = lambda: (lambda s: s if f'connected to "{a.ssid}"' in s.replace("\\", "") else "")(r.sh("cmd wifi status")[1])
        st = wait(conn, 120)
        if not r.check(bool(st), f"connected to {a.ssid}", r.sh("cmd wifi status")[1][:600]):
            dump(r, "wpa_supplicant|ClientModeImpl|SupplicantStaIfaceHal|WifiNative|nl80211|EAPOL|WPA:")
        addr = wait(lambda: re.search(r"inet (10\.0\.3\.\d+)/", r.sh("ip -4 addr show wlan0")[1]), 90)
        r.check(addr is not None, "DHCP address on wlan0", r.sh("ip addr show wlan0")[1][:300])
        rc, out, _ = r.sh("ping -c 3 -W 5 10.0.3.2", timeout=60)
        r.check(rc == 0 and " 0% packet loss" in out, "gateway 10.0.3.2 answers through wlan0", out[-300:])
        _, out, _ = r.sh("dumpsys connectivity | grep -E 'NetworkAgentInfo.*WIFI' | head -3", timeout=60)
        print(f"aosp-wifi-test: info {out.strip()[:400]}")
        _, out, _ = r.sh("logcat -d | grep -iE 'wpa_supplicant.*(CTRL-EVENT-CONNECTED|WPA: Key negotiation completed)' | tail -2")
        r.check("CTRL-EVENT-CONNECTED" in out or "Key negotiation completed" in out, "wpa_supplicant completed the handshake",
                out.strip()[:300])
        adbt.adb(a, "disconnect", a.serial)
    finally:
        adbt.adb(a, "kill-server", timeout=20)
        qemu.kill()
        qemu.wait()
        shutil.rmtree(home, ignore_errors=True)
    return report(r, log)


def dump(r, pattern):
    """Print the matching logcat lines (after a failed check)."""
    noise = "getWifiEnabledState|isScanAlwaysAvailable|getConnectionInfo|handleShellCommand"
    _, out, _ = r.sh(f"logcat -d -b all | grep -vE '{noise}' | grep -E -A6 '{pattern}|Exception' | head -250", timeout=120)
    print(out)


def report(r, log):
    if not r.failed:
        print(f"aosp-wifi-test: PASS {r.passed}/{r.total}")
        return 0
    print(f"aosp-wifi-test: {r.passed}/{r.total} passed, failed: {', '.join(r.failed)} (serial log: {log})")
    return 1


if __name__ == "__main__":
    sys.exit(main())
