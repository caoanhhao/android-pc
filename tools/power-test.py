#!/usr/bin/env python3
"""Run the E9 power test VM (run-qemu.sh --power-test): restart and power off.

Runs QEMU (the command after "--", without -no-reboot) with ktest-init
running e9_power. On "E9-POWER-READY" the QEMU monitor presses the ACPI
power button (`system_powerdown`); the test then stores a marker in the
CMOS alarm registers and restarts with reboot(2). The second boot finds
the marker ("E9-REBOOTED"), ktest-init ends and powers off.

Usage: power-test.py --monitor SOCKET [--log FILE] [--timeout SECS] -- qemu-system-x86_64 ...
Exit status 0 iff: two kernel boots, the first restarted through the
reset register ("acpi: reset register"), the second printed E9-REBOOTED
and "END ktest-init pass=N fail=0", and QEMU then exited by itself with
status 0 after "acpi: entering S5" (ACPI S5; the isa-debug-exit fallback
gives 33).
"""
import argparse
import importlib.util
import os
import re
import subprocess
import sys
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("aosp_gfx_test", os.path.join(HERE, "aosp-gfx-test.py"))
gfx = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gfx)


def press(monitor):
    time.sleep(0.5)
    gfx.monitor_cmd(monitor, "system_powerdown")
    print("power-test: monitor `system_powerdown`", flush=True)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--monitor", required=True)
    ap.add_argument("--log", default="power-test.log")
    ap.add_argument("--timeout", type=int, default=120)
    ap.add_argument("cmd", nargs=argparse.REMAINDER)
    a = ap.parse_args()
    cmd = a.cmd[1:] if a.cmd and a.cmd[0] == "--" else a.cmd
    p = subprocess.Popen(cmd, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    timer = threading.Timer(a.timeout, p.kill)
    timer.start()
    boots = 0
    seen = set()
    end_line = None
    with open(a.log, "wb") as log:
        for line in p.stdout:
            log.write(line)
            log.flush()
            sys.stdout.buffer.write(line)
            sys.stdout.flush()
            if b"android-pc kernel" in line and b"booting" in line:
                boots += 1
            for marker in (b"E9-POWER-READY", b"E9-REBOOTING", b"E9-REBOOTED", b"acpi: reset register",
                           b"acpi: entering S5", b"power button pressed", b"restart: trying"):
                if marker in line:
                    seen.add(marker)
            if b"E9-POWER-READY" in line:
                threading.Thread(target=press, args=(a.monitor,), daemon=True).start()
            m = re.search(rb"^END ktest-init pass=(\d+) fail=(\d+)", line)
            if m:
                end_line = m
    code = p.wait()
    timer.cancel()
    ok = True

    def need(cond, what):
        nonlocal ok
        print(f"power-test: {'ok  ' if cond else 'FAIL'} {what}", flush=True)
        ok = ok and cond

    need(b"power button pressed" in seen, "the power button press reached the kernel")
    need(b"E9-REBOOTING" in seen, "the test asked for a restart")
    need(b"acpi: reset register" in seen and b"restart: trying" not in seen, "restart through the FADT reset register")
    need(boots == 2, f"two boots (saw {boots})")
    need(b"E9-REBOOTED" in seen, "the second boot found the CMOS marker")
    need(end_line is not None and int(end_line.group(2)) == 0 and int(end_line.group(1)) > 0,
         "ktest-init passed on the second boot")
    need(b"acpi: entering S5" in seen and code == 0, f"power off through ACPI S5 (QEMU exit status {code})")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
