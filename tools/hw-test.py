#!/usr/bin/env python3
"""Run the E1 hardware test VM (run-qemu.sh --hw-test) and feed it input.

Runs QEMU (the command after "--": the ktest initramfs with the C2 test
disk on an NVMe controller, on an AHCI SATA port and as a usb-storage
device, and a qemu-xhci with a usb-hub with usb-kbd and usb-mouse
behind it) with ktest-init running c2_fs and e1_hw. The
serial console is copied to stdout and the log. When e1_hw prints
"E1-HID-READY" (its evdev nodes are open), the QEMU monitor sends
`sendkey a`, then `mouse_move 10 -5`, `mouse_button 1`, `mouse_button 0`,
which only the USB devices can receive (the VM has no virtio-input).
On "E1-HID-STRESS" it floods 3000 `mouse_move 1 0` and a `sendkey a` per
20 moves, then a right click (the USB rings wrap many times). The kernel's
isochronous OUT self-test on the usb-audio speaker (E8,
usb.isoch_selftest=1) must report at least 200 TDs in its 300 ms and no
errors.

Usage: hw-test.py --monitor SOCKET [--log FILE] [--timeout SECS] -- qemu-system-x86_64 ...
Exit status 0 iff ktest-init reports "END ktest-init pass=N fail=0" with
N > 0.
"""
import argparse
import os
import re
import socket
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


def flood(monitor, moves=3000, key_every=20):
    """After "E1-HID-STRESS": `moves` x `mouse_move 1 0` with a `sendkey a`
    every `key_every` moves, then a right click, over one monitor
    connection (each command waits for the prompt)."""
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(20)
    s.connect(monitor)

    def cmd(c):
        buf = b""
        s.sendall(c.encode() + b"\n")
        while not buf.endswith(b"(qemu) "):
            buf += s.recv(4096)

    buf = b""
    while b"(qemu) " not in buf:
        buf += s.recv(4096)
    start = time.time()
    for i in range(moves):
        cmd("mouse_move 1 0")
        if i % key_every == key_every - 1:
            cmd("sendkey a 10")
            time.sleep(0.03)
    time.sleep(1)
    cmd("mouse_button 2")
    time.sleep(0.2)
    cmd("mouse_button 0")
    s.close()
    print(f"hw-test: flooded {moves} moves, {moves // key_every} keys in {time.time() - start:.1f}s", flush=True)


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
    isoch = None
    deadline = time.time() + a.timeout
    timer = threading.Timer(a.timeout, p.kill)
    timer.start()
    with open(a.log, "wb") as log:
        for line in p.stdout:
            log.write(line)
            log.flush()
            sys.stdout.buffer.write(line)
            sys.stdout.flush()
            if b"E1-HID-STRESS" in line:
                threading.Thread(target=flood, args=(a.monitor,), daemon=True).start()
            if b"E1-HID-READY" in line and not injected:
                injected = True
                threading.Thread(target=inject, args=(a.monitor,), daemon=True).start()
            m = re.search(rb"isoch OUT self-test: (\d+) TDs in \d+ ms \(\d+ bytes each\), (\d+) errors", line)
            if m:
                isoch = (int(m.group(1)), int(m.group(2)))
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
    if isoch is None or isoch[0] < 200 or isoch[1] != 0:
        print(f"hw-test: isochronous self-test FAILED: {isoch} (TDs, errors)", file=sys.stderr)
        return 1
    print(f"hw-test: isochronous self-test: {isoch[0]} TDs, no errors")
    return 0 if failed == 0 and passed > 0 else 1


if __name__ == "__main__":
    sys.exit(main())
