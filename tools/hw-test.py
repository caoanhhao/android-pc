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
20 moves, then a right click (the USB rings wrap many times).
On "E9-PWRBTN-READY" (e9_platform) it presses the ACPI power button with
`system_powerdown`. At the end ktest-init powers off: QEMU must exit by
itself with status 0 (ACPI S5; the isa-debug-exit fallback gives 33).

The VM has no RTL8125 (QEMU has no model of it): the kernel must log
"rtl8125: no RTL8125 found" and nothing else from that driver (E4: the
driver probes cleanly without its device).

Usage: hw-test.py --monitor SOCKET [--log FILE] [--timeout SECS] -- qemu-system-x86_64 ...
Exit status 0 iff ktest-init reports "END ktest-init pass=N fail=0" with
N > 0, the RTL8125 check passed and QEMU then exits with status 0.
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
    rtl_absent = False
    rtl_other = []
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
            if b"E9-PWRBTN-READY" in line:
                threading.Thread(target=lambda: (time.sleep(0.5), print(f"hw-test: monitor `system_powerdown`", flush=True),
                                                 gfx.monitor_cmd(a.monitor, "system_powerdown")), daemon=True).start()
            if b"E1-HID-READY" in line and not injected:
                injected = True
                threading.Thread(target=inject, args=(a.monitor,), daemon=True).start()
            if b"rtl8125: no RTL8125 found" in line:
                rtl_absent = True
            elif b"rtl8125:" in line:
                rtl_other.append(line.decode(errors="replace").strip())
            m = re.search(rb"^END ktest-init pass=(\d+) fail=(\d+)", line)
            if m:
                end_line = m
    code = p.wait()
    timer.cancel()
    if end_line is None:
        print(f"hw-test: no END line ({'timed out' if time.time() >= deadline else 'QEMU exited'})", file=sys.stderr)
        return 1
    passed, failed = int(end_line.group(1)), int(end_line.group(2))
    if not injected:
        print("hw-test: e1_hw never became ready for input", file=sys.stderr)
        return 1
    print(f"hw-test: ktest-init pass={passed} fail={failed}, QEMU exit status {code}")
    if not rtl_absent or rtl_other:
        print(f"hw-test: rtl8125 probe without the device: FAIL (absent line {'seen' if rtl_absent else 'missing'}, other: {rtl_other})", file=sys.stderr)
        return 1
    print("hw-test: rtl8125 probes cleanly without the device")
    if code != 0:
        print("hw-test: the power off did not end QEMU through ACPI S5", file=sys.stderr)
        return 1
    return 0 if failed == 0 and passed > 0 else 1


if __name__ == "__main__":
    sys.exit(main())
