#!/usr/bin/env python3
"""Check that unmodified AOSP /init boots on the Rust kernel.

Runs QEMU (the command after "--") with the AOSP init initramfs and
super.img (see run-qemu.sh --aosp-init-test) and follows init's log on the
serial console (init writes it to /dev/kmsg, which the kernel prints as
"kmsg: init: ..."). The boot milestones must appear in order: first stage,
first-stage mount (dm-linear system/vendor), switch_root, SELinux policy
load, second stage, property service, and the early-init, init and
late-init triggers, post-fs-data with the switch to the default mount
namespace and apexd's activation. Then init's `console` service (a shell
on the serial console, started by "on init && property:ro.debuggable=1")
must run getprop and setprop, and show the core daemons at work: coldboot
done and ueventd's device permissions, servicemanager's services
(`service list`), logd (`logcat -d`, the kernel log through klogd,
`dmesg`), daemons under their own uids, keystore2 running, the tmpfs
/data, and (C3c) eBPF: the bpf loaders finished (bpf.progs_loaded=1),
their objects are pinned in bpffs, netd's bpf setup worked and init did
not reboot. Commands are typed into it and checked like aosp-sh-test.py
does.

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
    ("post-fs-data", rb"init: processing action \(post-fs-data\)"),
    ("default mount namespace", rb"init: Switched to default mount namespace"),
    ("apexd activated", rb"init: Wait for property 'apexd.status=activated' took"),
    # C3c: eBPF. netbpfload (tethering APEX, libbpf) loads the network
    # programs, execs the platform bpfloader, which execs it back to set
    # bpf.progs_loaded; init then starts netd.
    ("load-bpf-programs", rb"init: processing action \(load-bpf-programs\)"),
    ("netbpfload done", rb"NetBpfLoad: success\."),
    ("bpf-progs-loaded", rb"init: processing action \(bpf-progs-loaded\)"),
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
    # ueventd: coldboot finished (Android 16 sets ro.cold_boot_done instead
    # of creating /dev/.coldboot_done) and nodes made with ueventd.rc's modes.
    ("getprop ro.cold_boot_done; getprop init.svc.ueventd; ls -l /dev/hw_random",
     [r"^true\r?$", r"^running\r?$", r"cr-------- +1 prng_seeder +prng_seeder +10, +183 .*/dev/hw_random"]),
    # servicemanager: native services registered over binder. keystore2
    # starts before post-fs-data makes /data/misc/keystore, exits once and
    # is restarted 5 s later (like on a first boot): wait for it.
    ("i=0; while [ $i -lt 40 ] && ! service check android.system.keystore2.IKeystoreService/default"
     " | toybox grep -q ': found'; do sleep 0.5; i=$((i+1)); done; echo waited=$i",
     [r"waited=([0-9]|[1-3][0-9])\r?$"]),
    ("service list | toybox head -n 60",
     [r"Found \d+ services", r"\tmanager: \[android\.os\.IServiceManager\]",
      r"android\.system\.keystore2\.IKeystoreService/default",
      r"android\.hardware\.security\.keymint\.IKeyMintDevice/default", r"\tvold: \[android\.os\.IVold\]"]),
    ("for s in servicemanager hwservicemanager keystore2 logd; do echo $s=$(getprop init.svc.$s); done",
     [r"servicemanager=running", r"hwservicemanager=running", r"keystore2=running", r"logd=running"]),
    # logd: logs from services (logcat), init's kmsg lines through klogd
    # (the kernel buffer), dmesg (syslog(2)).
    ("logcat -d | toybox grep -cE ' (servicemanager|keystore2|vold|lowmemorykiller) *:'", [r"^\d{2,}\r?$"]),
    ("logcat -d -b kernel | toybox grep -c ' init *: '", [r"^\d{2,}\r?$"]),
    ("dmesg | toybox grep -c 'init: '", [r"^\d{2,}\r?$"]),
    # Credentials: daemons run under their own uids.
    ("ps -A -o USER,NAME | toybox grep -E ' (logd|servicemanager|keystore2)$'",
     [r"^logd +logd\r?$", r"^system +servicemanager\r?$", r"^keystore +keystore2\r?$"]),
    # /metadata and /data: tmpfs on the RAM disks, filled by post-fs-data.
    ("toybox grep -E ' /data | /metadata ' /proc/mounts; ls -d /data/misc/keystore",
     [r"/dev/block/ram0 /metadata tmpfs", r"/dev/block/ram1 /data tmpfs", r"^/data/misc/keystore\r?$"]),
    # eBPF (C3c): the loaders finished, their maps and programs are pinned
    # in bpffs, netd's bpf setup (libnetd_updatable) worked on them, and
    # init did not reboot (no "reboot,bpfloader-failed"): still up 10 s on.
    ("getprop bpf.progs_loaded; getprop init.svc.bpfloader; ls -d /sys/fs/bpf/netd_shared/mainline_done",
     [r"^1\r?$", r"^stopped\r?$", r"^/sys/fs/bpf/netd_shared/mainline_done\r?$"]),
    ("toybox grep -E ' /sys/fs/bpf ' /proc/mounts; ls /sys/fs/bpf/netd_shared | toybox grep -c '^map_netd_';"
     " ls /sys/fs/bpf/netd_shared | toybox grep -c '^prog_netd_'; ls -l /sys/fs/bpf/netd_shared/map_netd_cookie_tag_map",
     [r"^bpf /sys/fs/bpf bpf ", r"^[1-9]\d\r?$", r"^[1-9]\d?\r?$", r"^-rw-rw---- +1 root +net_bw_acct +0 .*map_netd_cookie_tag_map"]),
    ("i=0; while [ $i -lt 40 ] && ! logcat -d | toybox grep -q 'libnetd_updatable_init success'; do sleep 0.5; i=$((i+1)); done;"
     " logcat -d | toybox grep -c 'libnetd_updatable_init success'", [r"^[1-9]\d*\r?$"]),
    ("sleep 10; echo powerctl=$(getprop sys.powerctl); echo still-up",
     [r"^powerctl=\r?$", r"^still-up\r?$"]),
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
