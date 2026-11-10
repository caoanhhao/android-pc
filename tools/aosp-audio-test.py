#!/usr/bin/env python3
"""Check sound on unmodified AOSP over the Rust kernel's HD Audio driver (E5).

Runs QEMU (the command after "--", see run-qemu.sh --aosp-audio-test: the
--aosp-boot-test machine plus an intel-hda controller with an hda-duplex
codec whose output QEMU writes to a WAV file, --wav) and waits for
sys.boot_completed=1 the way aosp-boot-test.py does. Then:

- the kernel probed the controller and codec and made card 0, PCM 0
  (serial log), ueventd created /dev/snd/controlC0 and pcmC0D0p
  (system:audio), /proc/asound lists the card and PCM;
- tinypcminfo reads the PCM's hw_params ranges (HW_REFINE), tinymix
  lists the controls and sets/reads the master volume;
- tinyplay plays a 1 kHz tone (writei) and the WAV file gets it: loud,
  at about 1 kHz;
- through the Android API: apps/hello's ToneActivity plays a 440 Hz
  AudioTrack tone; AudioFlinger mixes it, the Cuttlefish primary audio
  HAL (StreamPrimary, tinyalsa on card 0 device 0) writes it to the
  kernel, and the WAV file gets a loud ~440 Hz signal.

Usage: aosp-audio-test.py --wav FILE --apk FILE [--boot-timeout SECS]
                          [--log FILE] [--port PORT] -- qemu-system-x86_64 ...
Exit status 0 iff every check passed.
"""
import argparse
import importlib.util
import math
import os
import re
import shutil
import struct
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("aosp_adb_test", os.path.join(HERE, "aosp-adb-test.py"))
adbt = importlib.util.module_from_spec(spec)
spec.loader.exec_module(adbt)

MILESTONES = [
    ("hda codec probed", rb"hda 0000:00:08\.0: codec 0: "),
    ("card 0 pcm 0", rb"hda 0000:00:08\.0: card 0 pcm 0 "),
    ("second stage", rb"init: init second stage started!"),
    ("adbd started", rb"init: \.\.\. started service 'adbd'"),
]

RATE = 48000


class Run:
    def __init__(self, a):
        self.a, self.passed, self.total, self.failed = a, 0, 0, []

    def check(self, ok, name, detail=""):
        self.total += 1
        if ok:
            self.passed += 1
            print(f"aosp-audio-test: ok   {name}", flush=True)
        else:
            self.failed.append(name)
            print(f"aosp-audio-test: FAIL {name} {detail}", flush=True)
        return ok

    def sh(self, cmd, timeout=60):
        rc, out, err = adbt.adb(self.a, "-s", self.a.serial, "shell", cmd, timeout=timeout)
        return rc, out.replace("\r\n", "\n"), err

    def prop(self, name):
        return self.sh(f"getprop {name}")[1].strip()


def tone_wav(path, freq, seconds):
    """A 48 kHz stereo S16 sine WAV."""
    n = int(RATE * seconds)
    data = bytearray()
    for i in range(n):
        v = int(math.sin(2 * math.pi * freq * i / RATE) * 12000)
        data += struct.pack("<hh", v, v)
    hdr = b"RIFF" + struct.pack("<I", 36 + len(data)) + b"WAVEfmt " + struct.pack("<IHHIIHH", 16, 1, 2, RATE, RATE * 4, 4, 16)
    with open(path, "wb") as f:
        f.write(hdr + b"data" + struct.pack("<I", len(data)) + data)


def wav_size(path):
    try:
        return os.path.getsize(path)
    except OSError:
        return 0


def analyze(path, start):
    """The loudest stretch QEMU wrote after byte `start` of its WAV (48 kHz
    stereo S16, header of 44 bytes): (rms, frequency, seconds) of the left
    channel over the 50 ms blocks louder than a threshold."""
    with open(path, "rb") as f:
        f.seek(max(start, 44))
        raw = f.read()
    raw = raw[: len(raw) // 4 * 4]
    left = [s[0] for s in struct.iter_unpack("<hh", raw)]
    block = RATE // 20
    loud = []
    for i in range(0, len(left) - block + 1, block):
        b = left[i:i + block]
        rms = math.sqrt(sum(x * x for x in b) / block)
        if rms > 1000:
            loud.append(b)
    if not loud:
        peak = max((abs(x) for x in left), default=0)
        return 0.0, 0.0, 0.0, peak
    samples = [x for b in loud for x in b]
    rms = math.sqrt(sum(x * x for x in samples) / len(samples))
    crossings = sum(1 for b in loud for i in range(1, len(b)) if (b[i - 1] < 0) != (b[i] < 0))
    secs = len(samples) / RATE
    return rms, crossings / 2 / secs, secs, max(abs(x) for x in samples)


def wait_for_audio(path, start, want_secs, timeout):
    """Wait until QEMU wrote `want_secs` of loud audio after `start`."""
    deadline = time.monotonic() + timeout
    res = (0.0, 0.0, 0.0, 0)
    while time.monotonic() < deadline:
        time.sleep(1)
        res = analyze(path, start)
        if res[2] >= want_secs:
            break
    return res


def log_since(log, off):
    with open(log, "rb") as f:
        f.seek(off)
        return f.read().decode(errors="replace")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--boot-timeout", type=float, default=900, help="seconds until sys.boot_completed")
    ap.add_argument("--log", default=None)
    ap.add_argument("--wav", required=True, help="QEMU's -audiodev wav output")
    ap.add_argument("--apk", required=True, help="apps/hello's APK (ToneActivity)")
    ap.add_argument("--port", type=int, default=15555)
    ap.add_argument("--server-port", type=int, default=15037)
    ap.add_argument("--adb", default=None)
    ap.add_argument("qemu", nargs=argparse.REMAINDER)
    a = ap.parse_args()
    cmd = a.qemu[1:] if a.qemu[:1] == ["--"] else a.qemu
    if not cmd:
        ap.error("missing QEMU command")
    if a.adb is None:
        top = subprocess.check_output(["git", "worktree", "list", "--porcelain"], cwd=os.path.dirname(HERE), text=True)
        built = os.path.join(top.splitlines()[0].removeprefix("worktree "), "aosp/out/host/linux-x86/bin/adb")
        a.adb = built if os.access(built, os.X_OK) else shutil.which("adb")
    home = tempfile.mkdtemp(prefix="aosp-audio-test-")
    a.env = dict(os.environ, HOME=home, ADB_MDNS="0", ADB_MDNS_AUTO_CONNECT="", ANDROID_SDK_HOME=home)
    a.env.pop("ANDROID_SERIAL", None)
    a.serial = f"127.0.0.1:{a.port}"
    log = a.log or os.path.join(home, "serial.log")
    r = Run(a)
    if os.path.exists(a.wav):
        os.remove(a.wav)
    qemu = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=open(log, "wb"), stderr=subprocess.STDOUT)
    try:
        start = time.monotonic()
        for name, rx in MILESTONES:
            ok = adbt.wait_log(log, rx, start + a.boot_timeout, qemu)
            if not r.check(ok, f"{name} ({time.monotonic() - start:.1f}s)", f"({rx.decode()}) not seen"):
                return report(r, log)

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

        # The device nodes, as ueventd made them, and /proc/asound.
        _, out, _ = r.sh("ls -l /dev/snd")
        r.check(re.search(r"^crw-rw---- 1 system audio 116,\s+0 .* controlC0$", out, re.M) is not None
                and re.search(r"^crw-rw---- 1 system audio 116,\s+16 .* pcmC0D0p$", out, re.M) is not None,
                "/dev/snd/controlC0, pcmC0D0p (system:audio, 116:0/16)", out.strip())
        _, out, _ = r.sh("cat /proc/asound/cards /proc/asound/pcm")
        r.check(re.search(r"^ 0 \[PCH\s*\]: HDA-Intel - HDA Intel PCH$", out, re.M) is not None
                and re.search(r"^00-00: .* : playback 1$", out, re.M) is not None, "/proc/asound/cards and pcm", out.strip())
        rc, out, _ = r.sh("tinypcminfo -D 0 -d 0")
        r.check(rc == 0 and re.search(r"Rate:\s+min=\d+Hz\s+max=\d+Hz", out) is not None and "S16_LE" in out,
                "tinypcminfo -D 0 -d 0 (HW_REFINE)", out.strip()[-600:])
        rc, out, _ = r.sh("tinymix -D 0")
        r.check(rc == 0 and "Master Playback Volume" in out and "Master Playback Switch" in out, "tinymix lists the controls", out.strip()[:600])
        vol = re.search(r"Master Playback Volume\s*:?\s*(\d+)", out)
        rc, out, _ = r.sh("tinymix -D 0 'Master Playback Volume' 30 && tinymix -D 0 'Master Playback Volume'")
        r.check(rc == 0 and re.search(r"\b30\b", out) is not None, "tinymix sets the master volume", out.strip()[:300])
        if vol:
            r.sh(f"tinymix -D 0 'Master Playback Volume' {vol.group(1)}")

        # system_server and the audio HAL.
        deadline = start + a.boot_timeout
        done = ""
        while time.monotonic() < deadline and qemu.poll() is None:
            done = r.prop("sys.boot_completed")
            if done == "1":
                break
            time.sleep(5)
        if not r.check(done == "1", f"sys.boot_completed=1 ({time.monotonic() - start:.0f}s)", f"value '{done}'"):
            return report(r, log)

        # tinyplay: a 1 kHz tone through writei.
        tone = os.path.join(home, "tone.wav")
        tone_wav(tone, 1000, 2.0)
        rc, out, err = adbt.adb(a, "-s", a.serial, "push", tone, "/data/local/tmp/tone.wav", timeout=60)
        r.check(rc == 0, "adb push tone.wav", (out + err).strip())
        played = False
        for _ in range(6):
            mark = wav_size(a.wav)
            rc, out, err = r.sh("tinyplay /data/local/tmp/tone.wav -D 0 -d 0", timeout=60)
            if rc == 0 and "Playing sample" in out:
                played = True
                break
            # The HAL may hold the PCM for a moment (EBUSY).
            time.sleep(3)
        r.check(played, "tinyplay plays tone.wav on card 0 device 0", (out + err).strip()[-400:])
        rms, freq, secs, peak = wait_for_audio(a.wav, mark, 1.5, 15)
        r.check(secs >= 1.5 and 900 <= freq <= 1100, f"QEMU captured the 1 kHz tone ({secs:.2f}s, {freq:.0f} Hz, rms {rms:.0f})",
                f"(peak {peak}, wav {wav_size(a.wav)} bytes)")

        # The Android API: AudioTrack -> AudioFlinger -> audio HAL -> ALSA.
        rc, out, err = adbt.adb(a, "-s", a.serial, "install", "-r", a.apk, timeout=300)
        r.check(rc == 0 and "Success" in out, "adb install hello.apk", (out + err).strip()[-300:])
        r.sh("logcat -c")
        logoff = os.path.getsize(log)
        mark = wav_size(a.wav)
        rc, out, _ = r.sh("am start -W -n com.androidpc.hello/.ToneActivity", timeout=120)
        r.check(rc == 0 and "Status: ok" in out, "am start ToneActivity", out.strip()[-300:])
        rms, freq, secs, peak = wait_for_audio(a.wav, mark, 1.5, 30)
        _, lc, _ = r.sh("logcat -d -s AndroidPcHello:* AHAL_StreamAlsa:* AHAL_StreamPrimary:* AHAL_AlsaUtils:* alsa_device_proxy:*")
        r.check("TONE_OK" in lc, "ToneActivity played (TONE_OK)", lc.strip()[-600:])
        r.check(secs >= 1.5 and 400 <= freq <= 480, f"QEMU captured the AudioTrack 440 Hz tone via the HAL ({secs:.2f}s, {freq:.0f} Hz, rms {rms:.0f})",
                f"(peak {peak}; logcat: {lc.strip()[-400:]})")
        klog = log_since(log, logoff)
        r.check("snd: pcmC0D0p hw_params" in klog, "the audio HAL opened pcmC0D0p",
                "\n".join(l for l in klog.splitlines() if "snd:" in l or "hda" in l)[-600:])
        xr = len(re.findall(r"snd: pcmC0D0p underrun", klog))
        # One when the tone ends (the HAL stops feeding before standby).
        r.check(xr <= 2, f"no repeated underruns during HAL playback ({xr})")
        adbt.adb(a, "disconnect", a.serial)
    finally:
        adbt.adb(a, "kill-server", timeout=20)
        qemu.kill()
        qemu.wait()
        shutil.rmtree(home, ignore_errors=True)
    return report(r, log)


def report(r, log):
    if not r.failed:
        print(f"aosp-audio-test: PASS {r.passed}/{r.total}")
        return 0
    print(f"aosp-audio-test: {r.passed}/{r.total} passed, failed: {', '.join(r.failed)} (serial log: {log})")
    return 1


if __name__ == "__main__":
    sys.exit(main())
