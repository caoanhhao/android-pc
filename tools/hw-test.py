#!/usr/bin/env python3
"""Run the E1 hardware test VM (run-qemu.sh --hw-test) and feed it input.

Runs QEMU (the command after "--": the ktest initramfs with the C2 test
disk on an NVMe controller and on an AHCI SATA port, and a qemu-xhci
with usb-kbd and usb-mouse) with ktest-init running c2_fs and e1_hw. The
serial console is copied to stdout and the log. When e1_hw prints
"E1-HID-READY" (its evdev nodes are open), the QEMU monitor sends
`sendkey a`, then `mouse_move 10 -5`, `mouse_button 1`, `mouse_button 0`,
which only the USB devices can receive (the VM has no virtio-input).

Usage: hw-test.py --monitor SOCKET [--log FILE] [--timeout SECS] -- qemu-system-x86_64 ...
Exit status 0 iff ktest-init reports "END ktest-init pass=N fail=0" with
N > 0.
"""
import argparse
import os
import re
import subprocess
import sys
import threading
import time
import importlib.util

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("aosp_gfx_test", os.path.join(HERE, "aosp-gfx-test.py"))
gfx = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gfx)


def inject(monitor):
    time.sleep(0.5)
    for cmd in ["sendkey a", "mouse_move 10 -5", "mouse_button 1", "mouse_button 0"]:
        out = gfx.monitor_cmd(monitor, cmd)
        print(f"hw-test: monitor `{cmd}`{': ' + out.strip().splitlines()[-1] if 'rror' in out else ''}", flush=True)
        time.sleep(0.3)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--monitor", required=True)
    ap.add_argument("--log", default="hw-test.log")
    ap.add_argument("--timeout", type=int, default=300)
    ap.add_argument("cmd", nargs=argparse.REMAINDER)
    a = ap.parse_args()
    cmd = a.cmd[1:] if a.cmd and a.cmd[0] == "--" else a.cmd
    p = subprocess.Popen(cmd, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    end_line = None
    injected = False
    deadline = time.time() + a.timeout
    timer = threading.Timer(a.timeout, p.kill)
    timer.start()
    with open(a.log, "wb") as log:
        for line in p.stdout:
            log.write(line)
            log.flush()
            sys.stdout.buffer.write(line)
            sys.stdout.flush()
            if b"E1-HID-READY" in line and not injected:
                injected = True
                threading.Thread(target=inject, args=(a.monitor,), daemon=True).start()
            m = re.search(rb"^END ktest-init pass=(\d+) fail=(\d+)", line)
            if m:
                end_line = m
    p.wait()
    timer.cancel()
    if end_line is None:
        print(f"hw-test: no END line ({'timed out' if time.time() >= deadline else 'QEMU exited'})", file=sys.stderr)
        return 1
    passed, failed = int(end_line.group(1)), int(end_line.group(2))
    if not injected:
        print("hw-test: e1_hw never became ready for input", file=sys.stderr)
        return 1
    print(f"hw-test: ktest-init pass={passed} fail={failed}")
    return 0 if failed == 0 and passed > 0 else 1


if __name__ == "__main__":
    sys.exit(main())
