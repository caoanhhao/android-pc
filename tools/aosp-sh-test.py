#!/usr/bin/env python3
"""Scripted check of the AOSP shell on the serial console.

Runs QEMU (the command after "--"), waits for the initramfs /init to say
"aosp-init: ready", then types each command into the interactive mksh on
the serial console, one at a time. Each command is followed by
`echo "@@END$((N))@@ rc=$?"`, so the terminal's echo of the typed line
never matches the marker; the output up to the marker must match the
expected regexes and the exit status must be 0. Finally `exit` makes init
exit and the kernel power off.

Usage: aosp-sh-test.py [--timeout SECS] [--log FILE] -- qemu-system-x86_64 ARGS...
Exit status 0 iff every check passed.
"""
import argparse
import os
import re
import select
import subprocess
import sys
import time

SHA = "3f16b3fdf0666f842c87110e2fe86fe3a6ab8e391c2e931b6a9b4f432ac15117"  # "android-pc\n"

# (command, regexes that must all match the output, as MULTILINE).
CHECKS = [
    ("echo hello-$((6 * 7))", [r"^hello-42\r?$"]),
    ("ls -1 /", [r"^system\r?$", r"^apex\r?$", r"^proc\r?$", r"^linkerconfig\r?$"]),
    ("cat /proc/self/maps", [
        r"^5555\S+ r-xp .* /system/bin/toybox\r?$",
        r"r-xp .* /apex/com\.android\.runtime/bin/linker64\r?$",
        r"r-xp .* /apex/com\.android\.runtime/lib64/bionic/libc\.so\r?$",
        r"rw-p .*\[stack\]\r?$",
    ]),
    ("ps -A", [r"^root +1 +0 .* sh\s*$", r"^root +\d+ +1 .* ps\s*$"]),
    ("uname -a", [r"^Linux android-pc \S+ .*x86_64"]),
    ("id", [r"^uid=0\(root\) gid=0\(root\)"]),
    ("toybox seq 100 | toybox wc -l", [r"^100\r?$"]),
    ("toybox seq 1000 | toybox grep 7 | toybox sort -n | toybox tail -n 1", [r"^997\r?$"]),
    ("sleep 1 & p=$!; echo started; wait $p; echo waited=$?", [r"^started\r?$", r"^waited=0\r?$"]),
    ("(exit 3) & wait $!; echo status=$?", [r"^status=3\r?$"]),
    ("echo android-pc > /data/f && toybox sha256sum /data/f", [r"^" + SHA + r"  /data/f\r?$"]),
    ("readlink /proc/self/exe; cat /proc/self/cmdline | toybox tr '\\0' ' '; echo",
     [r"^/system/bin/toybox\r?$", r"^cat /proc/self/cmdline \r?$"]),
    ("mount | toybox grep -c -e proc -e sysfs -e devtmpfs -e tmpfs", [r"^[4-9]\r?$"]),
    # Line editing: the typed 'X' is erased with DEL (mksh's editor).
    ("echo ab-X\x7fcd", [r"^ab-cd\r?$"]),
]

# Interactive: ^C (the tty sends SIGINT to the foreground job) ends a
# `sleep 600` long before it would finish; the shell survives.
INTERRUPT = "sleep 600; echo not-interrupted"


class Console:
    def __init__(self, cmd, log):
        self.p = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        self.buf = b""
        self.log = log

    def send(self, s):
        self.p.stdin.write(s.encode())
        self.p.stdin.flush()

    def expect(self, pattern, timeout):
        """Read until `pattern` (bytes regex) matches; return the text before
        the match and consume through it. None on timeout or EOF."""
        rx = re.compile(pattern)
        end = time.monotonic() + timeout
        while True:
            m = rx.search(self.buf)
            if m:
                before = self.buf[: m.start()]
                self.buf = self.buf[m.end():]
                return before.decode(errors="replace"), m
            left = end - time.monotonic()
            if left <= 0:
                return None
            r, _, _ = select.select([self.p.stdout], [], [], left)
            if not r:
                continue
            data = os.read(self.p.stdout.fileno(), 65536)
            if not data:
                return None
            self.log.write(data)
            self.log.flush()
            self.buf += data

    def close(self, timeout=20):
        try:
            self.p.stdin.close()
        except OSError:
            pass
        try:
            return self.p.wait(timeout)
        except subprocess.TimeoutExpired:
            self.p.kill()
            return None


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--timeout", type=float, default=30, help="seconds per command")
    ap.add_argument("--boot-timeout", type=float, default=90)
    ap.add_argument("--log", default="/dev/null")
    ap.add_argument("qemu", nargs=argparse.REMAINDER)
    a = ap.parse_args()
    cmd = a.qemu[1:] if a.qemu[:1] == ["--"] else a.qemu
    if not cmd:
        ap.error("missing QEMU command")

    with open(a.log, "wb") as log:
        con = Console(cmd, log)
        if con.expect(rb"aosp-init: ready", a.boot_timeout) is None:
            print("aosp-sh-test: FAIL: no 'aosp-init: ready' (boot failed)")
            con.close(1)
            return 1
        # Prompt of the interactive shell.
        con.expect(rb"# ", a.timeout)
        failed = 0
        for n, (command, regexes) in enumerate(CHECKS, 1):
            con.send(f'{command}; echo "@@END$(({n}))@@ rc=$?"\n')
            r = con.expect(rb"@@END%d@@ rc=(\d+)" % n, a.timeout)
            if r is None:
                print(f"aosp-sh-test: FAIL [{n}] {command}: no output marker within {a.timeout:.0f}s")
                failed += 1
                break
            out, m = r
            missing = [x for x in regexes if not re.search(x, out, re.M)]
            rc = int(m.group(1))
            if missing or rc != 0:
                failed += 1
                print(f"aosp-sh-test: FAIL [{n}] {command}: rc={rc}, unmatched {missing}")
                print("    | " + "\n    | ".join(out.splitlines()))
            else:
                print(f"aosp-sh-test: ok   [{n}] {command}")
        if not failed:
            n = len(CHECKS) + 1
            con.send(INTERRUPT + "\n")
            time.sleep(2)
            con.send("\x03")
            time.sleep(0.5)
            con.send(f'echo "@@END$(({n}))@@ rc=$?"\n')
            r = con.expect(rb"@@END%d@@ rc=(\d+)" % n, a.timeout)
            if r is None or "not-interrupted" in r[0].replace(INTERRUPT, "") or r[1].group(1) == b"0":
                failed += 1
                print(f"aosp-sh-test: FAIL [{n}] ^C during `{INTERRUPT}`")
                if r:
                    print("    | " + "\n    | ".join(r[0].splitlines()))
            else:
                print(f"aosp-sh-test: ok   [{n}] ^C during `{INTERRUPT}` (rc={int(r[1].group(1))})")
        if not failed:
            con.send("exit\n")
        code = con.close()
        if not failed and code is None:
            print("aosp-sh-test: FAIL: QEMU did not exit after the shell's exit")
            failed += 1
        total = len(CHECKS) + 1
        print(f"aosp-sh-test: {total - failed}/{total} passed" if failed else f"aosp-sh-test: PASS {total}/{total}")
        return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
