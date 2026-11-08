#!/usr/bin/env python3
"""Check that unmodified AOSP /init boots on the Rust kernel.

Runs QEMU (the command after "--") with the AOSP init initramfs and
super.img (see run-qemu.sh --aosp-init-test) and follows init's log on the
serial console (init writes it to /dev/kmsg, which the kernel prints as
"kmsg: init: ..."). The boot milestones must appear in order: first stage,
first-stage mount (dm-linear system/vendor), switch_root, SELinux policy
load, second stage, property service, and the early-init, init and
late-init triggers. Then init's `console` service (a shell on the serial
console, started by "on init && property:ro.debuggable=1") must run
getprop and setprop: commands are typed into it and checked like
aosp-sh-test.py does.

Usage: aosp-init-test.py [--boot-timeout SECS] [--timeout SECS] [--log FILE]
                         [--run CMD]... -- qemu-system-x86_64 ARGS...
Exit status 0 iff every check passed.
"""
import argparse
import importlib.util
import os
import re
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("aosp_sh_test", os.path.join(HERE, "aosp-sh-test.py"))
sh = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sh)

# (description, regex on the serial log), in boot order.
MILESTONES = [
    ("first stage", rb"init: init first stage started!"),
    ("dm-linear system", rb"Created logical partition system_a on device /dev/block/dm-\d+"),
    ("dm-linear vendor", rb"Created logical partition vendor_a on device /dev/block/dm-\d+"),
    ("system mounted", rb"__mount\(source=/dev/block/dm-\d+,target=/system,type=erofs\)=0"),
    ("switch_root", rb"init: Switching root to '/system'"),
    ("vendor mounted", rb"__mount\(source=/dev/block/dm-\d+,target=/vendor,type=erofs\)=0"),
    ("selinux_setup", rb"init: Opening SELinux policy"),
    ("policy loaded", rb"SELinux: policy loaded"),
    ("second stage", rb"init: init second stage started!"),
    ("property service", rb"init: Created socket '/dev/socket/property_service'"),
    ("ueventd coldboot", rb"ueventd: Coldboot took"),
    ("early-init", rb"init: processing action \(early-init\)"),
    ("init", rb"init: processing action \(init\)"),
    ("console service", rb"init: \.\.\. started service 'console'"),
    ("late-init", rb"init: processing action \(late-init\)"),
]

# Commands for init's console shell: (command, regexes).
CHECKS = [
    ("getprop ro.build.version.sdk", [r"^\d{2}\r?$"]),
    ("getprop ro.hardware; getprop ro.boot.serialno", [r"^cutf_cvm\r?$", r"^RUSTPC000001\r?$"]),
    ("getprop ro.debuggable; getprop sys.init.updatable_crashing; getprop | toybox grep -c '^\\['",
     [r"^1\r?$", r"^\d{3,}\r?$"]),
    ("setprop debug.rustpc.test hello-$((6 * 7)); getprop debug.rustpc.test", [r"^hello-42\r?$"]),
    ("getprop ro.product.device; id -Z 2>/dev/null; cat /proc/self/attr/current; echo",
     [r"^vsoc_x86_64_only\r?$", r"u:r:shell:s0"]),
    ("toybox grep -E ' / | /vendor ' /proc/mounts", [r"/dev/block/dm-\d+ / erofs ro", r"/dev/block/dm-\d+ /vendor erofs ro"]),
]


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--timeout", type=float, default=30, help="seconds per shell command")
    ap.add_argument("--boot-timeout", type=float, default=300)
    ap.add_argument("--log", default="/dev/null")
    ap.add_argument("--run", action="append", default=[])
    ap.add_argument("qemu", nargs=argparse.REMAINDER)
    a = ap.parse_args()
    cmd = a.qemu[1:] if a.qemu[:1] == ["--"] else a.qemu
    if not cmd:
        ap.error("missing QEMU command")

    failed = 0
    with open(a.log, "wb") as log:
        con = sh.Console(cmd, log)
        start = time.monotonic()
        for name, rx in MILESTONES:
            left = a.boot_timeout - (time.monotonic() - start)
            r = con.expect(rx, max(left, 1))
            if r is None:
                print(f"aosp-init-test: FAIL milestone '{name}' ({rx.decode()}) not seen")
                con.close(1)
                print(f"aosp-init-test: {MILESTONES.index((name, rx))}/{len(MILESTONES) + len(CHECKS)} passed")
                return 1
            print(f"aosp-init-test: ok   {name} ({time.monotonic() - start:.1f}s)")
        # The console service: wait for its prompt, then type commands.
        con.send("\n")
        if con.expect(rb"console:/ [$#] ", a.timeout * 2) is None:
            print("aosp-init-test: FAIL no console shell prompt")
            con.close(1)
            return 1
        print(f"aosp-init-test: ok   console shell ({time.monotonic() - start:.1f}s)")
        checks = [(c, []) for c in a.run] if a.run else CHECKS
        for n, (command, regexes) in enumerate(checks, 1):
            con.send(f'{command}; echo "@@END$(({n}))@@ rc=$?"\n')
            r = con.expect(rb"@@END%d@@ rc=(\d+)" % n, a.timeout)
            if r is None:
                print(f"aosp-init-test: FAIL [{n}] {command}: no output marker within {a.timeout:.0f}s")
                failed += 1
                break
            out, m = r
            missing = [x for x in regexes if not re.search(x, out, re.M)]
            if a.run:
                print(f"aosp-init-test: [{n}] {command} (rc={int(m.group(1))})")
                print("    | " + "\n    | ".join(out.splitlines()))
            elif missing:
                failed += 1
                print(f"aosp-init-test: FAIL [{n}] {command}: unmatched {missing}")
                print("    | " + "\n    | ".join(out.splitlines()))
            else:
                print(f"aosp-init-test: ok   [{n}] {command}")
        con.p.kill()
        con.close(5)
    total = len(MILESTONES) + 1 + len(checks)
    print(f"aosp-init-test: {total - failed}/{total} passed" if failed else f"aosp-init-test: PASS {total}/{total}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
