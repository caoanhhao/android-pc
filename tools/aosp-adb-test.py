#!/usr/bin/env python3
"""Check `adb shell` into unmodified AOSP running on the Rust kernel.

Runs QEMU (the command after "--", see run-qemu.sh --aosp-adb-test: the
AOSP init boot with QEMU's user network forwarding host 127.0.0.1:PORT to
the guest's TCP port 5555) and follows the serial log until init has
started adbd (persist.adb.tcp.port=5555 makes it listen on TCP; there is
no USB FunctionFS). Then the host adb, with a server of its own on a
private port (so other adb servers and devices, such as Cuttlefish's, are
left alone) and a throw-away HOME, connects to 127.0.0.1:PORT and runs
commands through `adb shell` (the shell protocol: stdout, exit status),
pushes and pulls a file (the sync service), reads output through
`adb exec-out` (the raw service: text, the pushed file, 3 MB) and checks
it, and runs `toybox nc` from the guest to a host server at 10.0.2.2. netd
must have finished its setup (its iptables chains are in the stored
netfilter tables).

Usage: aosp-adb-test.py [--boot-timeout SECS] [--timeout SECS] [--log FILE]
                        [--port PORT] [--adb PATH] [--run CMD]...
                        -- qemu-system-x86_64 ARGS...
Exit status 0 iff every check passed.
"""
import argparse
import hashlib
import os
import re
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)

# (description, regex on the serial log), in boot order.
MILESTONES = [
    ("second stage", rb"init: init second stage started!"),
    ("virtio-net eth0", rb"virtio-net: eth0 "),
    ("bpf-progs-loaded", rb"init: processing action \(bpf-progs-loaded\)"),
    ("boot", rb"init: processing action \(boot\)"),
    ("adbd started", rb"init: \.\.\. started service 'adbd'"),
]

# (adb shell command, regexes on its output, expected exit status).
CHECKS = [
    ("getprop ro.build.version.sdk", [r"^\d{2}$"], 0),
    ("ls /system", [r"^bin$", r"^etc$", r"^framework$", r"^lib64$"], 0),
    ("echo hello-$((6 * 7)); id", [r"^hello-42$", r"^uid=\d+\((shell|root)\)"], 0),
    ("getprop init.svc.adbd; getprop service.adb.tcp.port; getprop persist.adb.tcp.port", [r"^running$", r"^5555$"], 0),
    # The network as the shell sees it: eth0 with QEMU's address (ip uses
    # rtnetlink), adbd's listening socket and our connection in /proc.
    ("ip -4 addr show eth0; ip route show", [r"inet 10\.0\.2\.15/24 .*eth0", r"default via 10\.0\.2\.2 dev eth0"], 0),
    ("cat /proc/net/tcp /proc/net/tcp6 | toybox grep -c ':15B3 '", [r"^[1-9]\d*$"], 0),
    # netd finishes its setup now that iptables-restore works (it is still
    # restarted whenever zygote aborts: ART needs userfaultfd, phase D).
    ("logcat -d | toybox grep -c 'Netd started in'; logcat -d | toybox grep -c 'Failed to initialize BandwidthController'; true",
     [r"^[1-9]\d*$", r"^0$"], 0),
    ("iptables -w -S INPUT; ip6tables -w -S OUTPUT; ip6tables -w -L INPUT -n | toybox head -n 1",
     [r"^-P INPUT ACCEPT$", r"^-A INPUT -j bw_INPUT$", r"^-A OUTPUT -j bw_OUTPUT$", r"^Chain INPUT \(policy ACCEPT\)$"], 0),
    ("exit 3", [], 3),
]


def adb(a, *args, timeout=30, check=False):
    cmd = [a.adb, "-P", str(a.server_port), *args]
    try:
        r = subprocess.run(cmd, capture_output=True, timeout=timeout, env=a.env, text=True, errors="replace")
    except subprocess.TimeoutExpired:
        return 124, "", f"timeout after {timeout}s"
    return r.returncode, r.stdout, r.stderr


def md5_server():
    """A host TCP server on 127.0.0.1 (the guest's 10.0.2.2): one
    connection, read until EOF, answer the md5 of what came, close.
    Returns (port, thread)."""
    srv = socket.socket()
    srv.bind(("127.0.0.1", 0))
    srv.listen(1)
    srv.settimeout(60)

    def serve():
        try:
            c, _ = srv.accept()
            c.settimeout(60)
            h = hashlib.md5()
            while chunk := c.recv(65536):
                h.update(chunk)
            c.sendall(h.hexdigest().encode() + b"\n")
            c.close()
        except OSError:
            pass
        finally:
            srv.close()

    t = threading.Thread(target=serve, daemon=True)
    t.start()
    return srv.getsockname()[1], t


def wait_log(path, rx, deadline, qemu):
    pat = re.compile(rx)
    while time.monotonic() < deadline:
        with open(path, "rb") as f:
            if pat.search(f.read()):
                return True
        if qemu.poll() is not None:
            return False
        time.sleep(0.5)
    return False


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--timeout", type=float, default=60, help="seconds per adb command")
    ap.add_argument("--boot-timeout", type=float, default=300)
    ap.add_argument("--log", default=None)
    ap.add_argument("--port", type=int, default=15555, help="host port forwarded to the guest's 5555")
    ap.add_argument("--server-port", type=int, default=15037, help="our own adb server's port")
    ap.add_argument("--adb", default=None)
    ap.add_argument("--run", action="append", default=[])
    ap.add_argument("qemu", nargs=argparse.REMAINDER)
    a = ap.parse_args()
    cmd = a.qemu[1:] if a.qemu[:1] == ["--"] else a.qemu
    if not cmd:
        ap.error("missing QEMU command")
    if a.adb is None:
        top = subprocess.check_output(["git", "worktree", "list", "--porcelain"], cwd=ROOT, text=True)
        built = os.path.join(top.splitlines()[0].removeprefix("worktree "), "aosp/out/host/linux-x86/bin/adb")
        a.adb = built if os.access(built, os.X_OK) else shutil.which("adb")
    if not a.adb:
        print("aosp-adb-test: no adb binary")
        return 1
    home = tempfile.mkdtemp(prefix="aosp-adb-test-")
    a.env = dict(os.environ, HOME=home, ADB_MDNS="0", ADB_MDNS_AUTO_CONNECT="", ANDROID_SDK_HOME=home)
    a.env.pop("ANDROID_SERIAL", None)
    log = a.log or os.path.join(home, "serial.log")
    serial = f"127.0.0.1:{a.port}"

    total = len(MILESTONES) + 2 + (len(a.run) if a.run else len(CHECKS) + 5)
    passed = 0
    qemu = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=open(log, "wb"), stderr=subprocess.STDOUT)
    try:
        start = time.monotonic()
        for name, rx in MILESTONES:
            if not wait_log(log, rx, start + a.boot_timeout, qemu):
                print(f"aosp-adb-test: FAIL milestone '{name}' ({rx.decode()}) not seen (log: {log})")
                print(f"aosp-adb-test: {passed}/{total} passed")
                return 1
            passed += 1
            print(f"aosp-adb-test: ok   {name} ({time.monotonic() - start:.1f}s)")

        # Connect: adbd may need a moment to listen after it started.
        adb(a, "start-server", timeout=60)
        state = ""
        deadline = time.monotonic() + 90
        while time.monotonic() < deadline:
            rc, out, err = adb(a, "connect", serial, timeout=20)
            if "connected to" in out:
                rc, out, err = adb(a, "-s", serial, "wait-for-device", "get-state", timeout=20)
                state = out.strip()
                if state == "device":
                    break
            time.sleep(2)
        if state != "device":
            print(f"aosp-adb-test: FAIL adb connect {serial}: state '{state}' ({out.strip()} {err.strip()})")
            print(f"aosp-adb-test: {passed}/{total} passed")
            return 1
        passed += 1
        print(f"aosp-adb-test: ok   adb connect {serial} ({time.monotonic() - start:.1f}s)")
        rc, out, err = adb(a, "devices", "-l")
        if re.search(rf"^{re.escape(serial)}\s+device\b.*product:aosp_cf_x86_64_only_phone", out, re.M):
            passed += 1
            print("aosp-adb-test: ok   adb devices lists the AOSP product")
        else:
            print(f"aosp-adb-test: FAIL adb devices -l: {out.strip()}")

        checks = [(c, [], None) for c in a.run] if a.run else CHECKS
        for n, (command, regexes, status) in enumerate(checks, 1):
            rc, out, err = adb(a, "-s", serial, "shell", command, timeout=a.timeout)
            lines = out.replace("\r\n", "\n")
            missing = [x for x in regexes if not re.search(x, lines, re.M)]
            if a.run:
                print(f"aosp-adb-test: [{n}] {command} (rc={rc})")
                print("    | " + "\n    | ".join((out + err).splitlines()))
                passed += 1
            elif missing or rc != status:
                print(f"aosp-adb-test: FAIL [{n}] adb shell {command!r}: rc={rc} (want {status}), unmatched {missing}")
                print("    | " + "\n    | ".join((out + err).splitlines()))
            else:
                passed += 1
                print(f"aosp-adb-test: ok   [{n}] adb shell {command!r}")
        if not a.run:
            # The sync service: push a file, read it back, pull it.
            src = os.path.join(home, "payload.bin")
            data = bytes((i * 7) % 256 for i in range(300_000))
            with open(src, "wb") as f:
                f.write(data)
            dst = os.path.join(home, "pulled.bin")
            r1 = adb(a, "-s", serial, "push", src, "/data/local/tmp/payload.bin", timeout=a.timeout)
            r2 = adb(a, "-s", serial, "shell", "md5sum /data/local/tmp/payload.bin", timeout=a.timeout)
            r3 = adb(a, "-s", serial, "pull", "/data/local/tmp/payload.bin", dst, timeout=a.timeout)
            md5 = hashlib.md5(data).hexdigest()
            ok = r1[0] == 0 and md5 in r2[1] and r3[0] == 0 and os.path.exists(dst) and open(dst, "rb").read() == data
            if ok:
                passed += 1
                print("aosp-adb-test: ok   adb push / md5sum / pull of 300 kB")
            else:
                print(f"aosp-adb-test: FAIL adb push/pull: push={r1} md5={r2} pull={r3}")
            # exec-out: adbd's raw service (a PTY in raw mode): no \r\n
            # translation, every byte, then the stream ends.
            base = [a.adb, "-P", str(a.server_port), "-s", serial]
            for desc, argv, want in [
                ("adb exec-out echo", ["exec-out", "echo exec-out-works; echo two"], b"exec-out-works\ntwo\n"),
                ("adb exec-out cat of the pushed file", ["exec-out", "cat /data/local/tmp/payload.bin"], data),
                ("adb exec-out 3 MB", ["exec-out", "head -c 3000000 /dev/zero"], bytes(3000000)),
            ]:
                try:
                    p = subprocess.run(base + argv, capture_output=True, timeout=a.timeout, env=a.env)
                    got, rc = p.stdout, p.returncode
                except subprocess.TimeoutExpired:
                    got, rc = b"", 124
                if got == want and rc == 0:
                    passed += 1
                    print(f"aosp-adb-test: ok   {desc}")
                else:
                    print(f"aosp-adb-test: FAIL {desc}: rc={rc}, {len(got)} bytes (want {len(want)}) {got[:60]!r}")
            # Out of the guest through QEMU's user network: toybox nc to
            # a host server at 10.0.2.2 sends 400 kB, half-closes, and
            # reads the server's md5 of it until the server closes.
            port, t = md5_server()
            want = hashlib.md5("".join(f"{i}\n" for i in range(1, 70001)).encode()).hexdigest()
            rc, out, err = adb(a, "-s", serial, "shell", f"toybox seq 1 70000 | toybox nc 10.0.2.2 {port}", timeout=a.timeout)
            t.join(5)
            if out.strip() == want:
                passed += 1
                print("aosp-adb-test: ok   toybox nc to a host server via 10.0.2.2 (400 kB up, md5 back)")
            else:
                print(f"aosp-adb-test: FAIL toybox nc to 10.0.2.2:{port}: rc={rc} {out.strip()[:80]!r} {err.strip()[:200]!r} (want {want})")
        adb(a, "disconnect", serial)
    finally:
        adb(a, "kill-server", timeout=20)
        qemu.kill()
        qemu.wait()
        shutil.rmtree(home, ignore_errors=True)
    if passed == total:
        print(f"aosp-adb-test: PASS {passed}/{total}")
        return 0
    print(f"aosp-adb-test: {passed}/{total} passed (serial log: {log})")
    return 1


if __name__ == "__main__":
    sys.exit(main())
