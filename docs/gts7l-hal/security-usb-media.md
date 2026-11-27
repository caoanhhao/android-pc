# gts7l HAL check: security, USB, media, camera, CAS

Checked on the tablet (SM-T875, serial `<serial>`) on 2026-11-25, build
`Android/aosp_gts7l/gts7l:17/CP2A.260605.016/eng.caoanh.20261124.1208:userdebug/test-keys`,
security patch 2026-06-05, a minute after boot. All checks were read-only or undone at the
end (test keys deleted, the PIN cleared). SELinux is **Permissive** on this build
(`getenforce`), which matters for the denials listed below.

| HAL | Result |
|---|---|
| KeyMint `rust_nonsecure` (+ RKP component) via Keystore2 | **Works** |
| SecureClock / SharedSecret | **Works** (only because SELinux is permissive, see below) |
| Gatekeeper `nonsecure` + gatekeeperd + LockSettings | **Works** |
| USB gadget: adb | **Works** |
| USB gadget: ACM | **Works** |
| USB gadget: NCM | **Partial**: link and ARP up, no IP traffic under Android (fix on the USB HAL branch, untested, §4) |
| IUsb / IUsbGadget HAL | **Absent** on this build; Rust HAL `hals/usb` added since, untested on the tablet (§4) |
| Codec2 `IComponentStore/software` | **Fixed** on branch `claude/gts7l-fix-codecs-991283` (two vendor properties); proven at runtime, see §5 |
| Hardware video codec (SM8250, qcom-iris) | **Kernel works** (H.264/HEVC/VP9 decode bit-exact); Codec2 HAL built, not yet run on the tablet (§5) |
| Camera | **Absent** (no provider, 0 cameras) |
| CAS | Present: AOSP example service with the ClearKey plugin only |

## 1. KeyMint (`com.android.hardware.keymint.rust_nonsecure`) — Works

The running service is the Rust KeyMint TA (`kmr_ta`) from the `rust_nonsecure` APEX:

```
$ adb shell cat /apex/apex-info-list.xml | grep keymint
<apex-info moduleName="com.android.hardware.keymint"
  modulePath="/vendor/apex/com.android.hardware.keymint.rust_nonsecure.apex" ... isActive="true"
$ adb shell ls -l /proc/389/exe
/apex/com.android.hardware.keymint/bin/hw/android.hardware.security.keymint-service.nonsecure
$ adb shell service list | grep -E 'keymint|secureclock|sharedsecret'
android.hardware.security.keymint.IKeyMintDevice/default
android.hardware.security.keymint.IRemotelyProvisionedComponent/default
android.hardware.security.secureclock.ISecureClock/default
android.hardware.security.sharedsecret.ISharedSecret/default
android.hardware.security.sharedsecret.ISharedSecret/gatekeeper
```

Generate, use and delete a key through Keystore2 (`keystore_cli_v2`, run as root, security
level TEE, which the non-secure KeyMint reports):

```
$ keystore_cli_v2 generate --name=haltest --seclevel=tee
GenerateKey: success
Hardware: ALGORITHM PURPOSE PURPOSE DIGEST x4 PADDING x2 KEY_SIZE RSA_PUBLIC_EXPONENT
          NO_AUTH_REQUIRED ORIGIN BOOT_PATCHLEVEL
Software: CREATION_DATETIME USER_ID
$ keystore_cli_v2 sign-verify --name=haltest
Sign: 256 bytes.
Verify: OK
$ echo "hello gts7l keymint" > pt
$ keystore_cli_v2 encrypt --name=haltest --in=pt --out=ct && keystore_cli_v2 decrypt --name=haltest --in=ct --out=pt2
$ cat pt2; cmp pt pt2 && echo ROUNDTRIP_OK
hello gts7l keymint
ROUNDTRIP_OK
$ keystore_cli_v2 export --name=haltest
ExportKey: Got certificate of length (721)
$ keystore_cli_v2 delete --name=haltest
Successfully deleted key.
```

The built-in suite `keystore_cli_v2 brillo-platform-test`: 17 PASS, 5 ABORT.

```
PASS  RSA-2048/3072/4096 Sign, RSA-2048 Sign (more digests), RSA-2048/3072/4096 Encrypt
PASS  ECDSA-P256 Sign (+more digests), ECDSA-P224/P384/P521 Sign
PASS  AES-128, AES-256, AES-128-GCM, AES-256-GCM
PASS  HMAC-SHA256-64
ABORT HMAC-SHA256-16, HMAC-SHA256-32, HMAC-SHA224-32, HMAC-SHA384-32, HMAC-SHA512-32
```

The ABORTs are the old test's fault, not the HAL's: it asks for HMAC keys of 16/32 bits,
which KeyMint must reject (minimum 64 bits), and it does:

```
keymint-hal-nonsecure: kmr_ta: failing DeviceGenerateKey request with error ...
  file: "system/keymint/common/src/crypto/hmac.rs", line: 39 },
  kind: Hal(UnsupportedKeySize, "unsupported KEY_SIZE 16 bits for HMAC")
keystore2: ... Error::Km(r#UNSUPPORTED_KEY_SIZE) for AppUid(0)
```

`dumpsys keystore2`: there is no service named `keystore2`; the Keystore2 service is
`android.system.keystore2.IKeystoreService/default`, and
`dumpsys android.system.keystore2.IKeystoreService/default` prints nothing (Keystore2 has no
dump output). Keystore2's own log shows it unlocked the user's CE super key at boot
(`super_key.rs: Unlocking CredentialEncrypted super key for user AndroidUserId(0)`).

Notes:
- Key characteristics carry `BOOT_PATCHLEVEL` but no `OS_VERSION`/`OS_PATCHLEVEL`/
  `VENDOR_PATCHLEVEL`. Not an error for this test; worth a look if attestation is needed.
- The KeyMint service reads `ro.serialno` without a policy allowing it
  (`avc: denied { read open getattr map } ... scontext=u:r:hal_keymint_default:s0
  tcontext=u:object_r:serialno_prop:s0 ... permissive=1`).

## 2. SecureClock / SharedSecret — Works (permissive only)

Both are served by the KeyMint process (`/default`) and the gatekeeper process
(`ISharedSecret/gatekeeper`). Keystore2 negotiates the shared secret with both at boot; the
log shows the call to the gatekeeper HAL went through. Under enforcing SELinux it would not:
the gatekeeper's `ISharedSecret/gatekeeper` instance has no service label.

```
avc: denied { add } for name=android.hardware.security.sharedsecret.ISharedSecret/gatekeeper
  scontext=u:r:hal_gatekeeper_default:s0 tcontext=u:object_r:default_android_service:s0
  tclass=service_manager permissive=1
avc: denied { find } for pid=384 (keystore2) name=...ISharedSecret/gatekeeper
  scontext=u:r:keystore:s0 tcontext=u:object_r:default_android_service:s0 permissive=1
avc: denied { call } comm="keystore2" scontext=u:r:keystore:s0
  tcontext=u:r:hal_gatekeeper_default:s0 tclass=binder permissive=1
```

Fix (not done here): label `android.hardware.security.sharedsecret.ISharedSecret/gatekeeper`
as `hal_gatekeeper_service` in the device's vendor `service_contexts`, as Cuttlefish does
(`device/google/cuttlefish/shared/sepolicy/vendor/service_contexts:20`). The PIN test below proves
the auth path works now; it only keeps working with enforcing SELinux after that fix.

## 3. Gatekeeper (`com.android.hardware.gatekeeper.nonsecure`) — Works

```
$ ps -A | grep gatekeeper
system  388 ... android.hardware.gatekeeper-service.nonsecure
system  757 ... gatekeeperd
```

Starting state: no lock (`cmd lock_settings get-disabled` → `true`, `CredentialType: NONE`,
`SID: 0000000000000000`). Set a PIN, verify it, check a wrong PIN, clear it:

```
$ cmd lock_settings set-pin 7391
Pin set to '7391'
$ dumpsys lock_settings | grep -E 'SID|CredentialType'
    SID: 145619d48b9b2518          <- gatekeeper enrolled the PIN and issued a secure user ID
    CredentialType: PIN
$ cmd lock_settings verify --old 7391
Lock credential verified successfully
$ cmd lock_settings verify --old 0000
Old password '0000' didn't match           (exit 255)
$ cmd lock_settings clear --old 7391
Lock credential cleared
$ dumpsys lock_settings | grep -E 'SID|CredentialType|IsLockScreenDisabled'
    SID: 0000000000000000
    CredentialType: NONE
    IsLockScreenDisabled: true
$ cmd lock_settings get-disabled
true
```

The lock screen is back to "None", as before the test.

## 4. USB gadget (`vendor.gts7l-usb`, `init.gts7l.usb.rc`)

Device side: configfs gadget `g1` on `a600000.usb` with `f1` (ffs.adb), `f8` (acm.gs8),
`f9` (ncm.gs9); `sys.usb.config=adb`, `sys.usb.state=adb`; `init.svc.vendor.gts7l-usb` and
`init.svc.vendor.gts7l-console` running.

Host side (`lsusb -v -d 18d1:4ee7`, trimmed):

```
Bus 001 Device 014: ID 18d1:4ee7 Google Inc. Nexus/Pixel Device (charging + debug)
Negotiated speed: High Speed (480Mbps)
  iManufacturer  android-pc
  iProduct       android-pc Galaxy Tab S7 LTE
  iSerial        <serial>
  bNumInterfaces 5      MaxPower 500mA
  If 0  class 2/2/1   CDC Abstract Control Model (ACM)
  If 1  class 10      CDC ACM Data
  If 2  class 2/13    CDC Network Control Model (NCM)
  If 3  class 10/0/1  CDC Network Data (alt 0, alt 1)
  If 4  class 255/66/1 ADB Interface
$ lsusb -t
  Port 001: Dev 014, If 0 cdc_acm | If 1 cdc_acm | If 2 cdc_ncm | If 3 cdc_ncm | If 4 usbfs
```

### adb — Works
`adb -s $SERIAL` works for this whole check (`adb root`, shell, logcat).

### ACM — Works
The host has `/dev/ttyACM0`; on the device a root `sh` (domain `gts7l_usb`) sits on
`/dev/ttyGS0`. A command sent from the host through the tty (raw mode, `sudo`) came back:

```
echo ACM_OK; getprop ro.serialno; id -u; cat /proc/self/attr/current
ACM_OK
<serial>
0
u:r:gts7l_usb:s0
:/ #
```

Side note: on open the console prints `No controlling tty` warnings, and a host tty left in
its default (echo on) mode echoes them back into the shell; use a raw terminal
(`picocom`, `screen`, or `stty raw -echo`).

### NCM — Partial
Link is up on both sides, ARP resolves, but no IP traffic gets through:

```
host$ ip -br a show enp0s20f0u1i2
enp0s20f0u1i2  UP  172.16.42.2/24
host$ ip neigh show dev enp0s20f0u1i2
172.16.42.1 lladdr 3a:e3:d5:38:4e:04 REACHABLE
host$ ping -c 3 172.16.42.1
3 packets transmitted, 0 received, 100% packet loss
host$ echo > /dev/tcp/172.16.42.1/5555     -> fails (adbd listens: tcp6 [::]:5555 LISTEN)
dev$ ip -br a | grep usb0
usb0  UP  172.16.42.1/24
dev$ ip rule
0:      from all lookup local
11000:  from all iif lo oif dummy0 uidrange 0-0 lookup dummy0
16000:  from all fwmark 0x10063/0x1ffff iif lo lookup local_network
17000:  from all iif lo oif dummy0 lookup dummy0
20000:  from all fwmark 0x0/0x10000 lookup local_network
32000:  from all unreachable
dev$ ip route get 172.16.42.2
RTNETLINK answers: Network is unreachable
```

Cause: once netd is up, Android's policy routing has no rule that reaches the `main` table,
where the `usb0` route lives, so replies to the host are unroutable. (Works in the bring-up
shell, which has no netd.) Fix idea, not done: have `gts7l-usb.sh` put `usb0` into netd's
local network (`ndc network interface add local usb0` and
`ndc network route add local usb0 172.16.42.0/24`), or add an `ip rule` for `iif usb0` /
`to 172.16.42.0/24` that looks up `main`.

### IUsb / IUsbGadget HAL — Absent on this build
No `android.hardware.usb.IUsb/*` or `IUsbGadget/*` service (the only USB entry in
`service list` is the framework's `usb: [android.hardware.usb.IUsbManager]`), no
`/sys/class/android_usb`. As a result `UsbService` creates no device manager and no port
manager:

```
$ dumpsys usb          (66 lines)
USB MANAGER STATE (dumpsys usb):
  alsa_manager={ cards_parser=-1 }
  settings_manager={ ... gallery3d IngestActivity, com.android.mtp ReceiverActivity ... }
  permissions_manager={ ... }
  (no device_manager, no port_manager, no host_manager)
$ svc usb getFunctions
Killed   (rc 137)
AndroidRuntime: FATAL EXCEPTION: main ... java.lang.IllegalStateException
  at com.android.internal.util.Preconditions.checkState(Preconditions.java:246)
  at com.android.server.usb.UsbService.getCurrentFunctions(UsbService.java:970)
```

Root cause, in the framework (`frameworks/base/services/usb/.../UsbService.java:241`):
`UsbDeviceManager` is created only when `/sys/class/android_usb` exists. The Android common
kernel creates that class (`CONFIG_USB_CONFIGFS_UEVENT`); mainline has nothing like it. The
missing gadget HAL alone would only have selected the legacy `sys.usb.config` handler.

### USB HAL (branch `claude/gts7l-usb-hal-5d9a0d`) — ready to merge, untested on the tablet
Written 2026-11-25. The user's rule since then: no partial build is flashed; the main session
merges every branch, builds once, flashes once and checks each fix. So nothing below ran on
the tablet yet. What was checked: the host unit tests (13 pass: function plans and product
IDs, the configfs layout on a fake gadget, the typec parser on the tablet's own sysfs
values), patch 0270 compiled against the gts7l kernel config (no warning), an out-of-tree
module with the same class built and modversioned against the gts7l kernel, and the HAL
type-checked by rustc in the AOSP build (the first build stopped only on `dead_code` lints,
fixed since; the build after was cancelled under the new rule).

The pieces:

| Piece | What it does |
|---|---|
| `aosp-device/hals/usb` (`android.hardware.usb-service.pc`, Rust on libpchal) | One process, two services. **IUsbGadget/default** (V2): maps the framework's `GadgetFunction` bits to the configfs gadget `g1` (ADB `ffs.adb`, MTP `ffs.mtp`, PTP `ffs.ptp`, MIDI `midi.gs5`, RNDIS `rndis.gs4`, NCM `ncm.gs9`; charging only = nothing bound), Google's 18d1 product IDs (MTP 4ee1, MTP+adb 4ee2, RNDIS 4ee3/4ee4, PTP 4ee5/4ee6, adb 4ee7, MIDI 4ee8/4ee9, NCM 4eeb/4eec), EF/02/01 device class when a function needs an IAD. A thread watches `/dev/usb-ffs/*` with inotify and binds the gadget again when adbd or the MTP server comes back (the kernel unbinds the whole gadget when a FunctionFS daemon closes, e.g. on `adb root`). ACCESSORY, AUDIO_SOURCE and UVC answer `CONFIGURATION_NOT_SUPPORTED` (no f_accessory/f_audio_source in mainline, no UVC daemon). **IUsb/default** (V4): `port0` from `/sys/class/typec` (current data/power role, mode UFP/DFP/accessory, DRP support, plugged or not), pushed to the framework on every typec uevent. The MAX77705 driver's role files are read-only, so role swaps report `NOT_SUPPORTED`; disabling USB data, contaminant detection, power limiting and port reset are `NOT_SUPPORTED` too. |
| kernel patch `0270-usb-gadget-configfs-register-an-empty-android_usb-cl.patch` | Registers an empty `android_usb` class, so `UsbService` creates its device and port managers. |
| `gts7l/rro` (`gts7l_overlay_frameworks_base_core`) | `config_enableUdcSysfsUsbStateUpdate=true`: the framework reads the gadget state from `/sys/class/udc/a600000.usb/state` instead of `USB_STATE` uevents of an `android0` device (the flag `enable_udc_sysfs_usb_state_update` is already on, genfs labels version 202604). |
| `init.gts7l.usb.rc` | Mounts the `mtp` and `ptp` FunctionFS ports (uid/gid mtp, `no_disconnect`) before system_server, which writes their descriptors at start; hands the gadget files the HAL writes to `system`; `on property:vendor.usb.gadget.hal=1` sets `sys.usb.configfs=2` (only init and system_server may set it). |
| `gts7l-usb.sh` | Puts the bring-up NCM link into netd's local network (the NCM fix below) and steps aside once `sys.usb.configfs=2`. |
| `aosp_gts7l.mk` | The HAL (`hals.list`), the RRO, `persist.vendor.usb.gadget.acm=1`. |

How the gadget changes hands: until the framework's first request (at boot completion) the
gadget is the bring-up one from `init.usb.configfs.rc` (f1 adb, f8 ACM, f9 NCM), so adb, the
ACM shell and NCM still work if the framework never comes up. The first
`setCurrentUsbFunctions` sets `vendor.usb.gadget.hal=1`; init answers with
`sys.usb.configfs=2`, which disables the stock `sys.usb.config` triggers; the HAL then
unbinds, relinks the requested functions as f1..fN and binds.

The bring-up and debug paths afterwards:
- **adb**: the framework keeps ADB in every configuration while USB debugging is on (it is,
  `persist.sys.usb.config=adb`).
- **ACM root shell**: kept in every configuration, also "charging only", while
  `persist.vendor.usb.gadget.acm=1` (the product default). `setprop
  persist.vendor.usb.gadget.acm 0` and a function change drop it.
- **NCM 172.16.42.1**: only before the HAL takes over (boot, or when the framework is dead).
  Afterwards NCM is the tethering function and Android's tethering gives it its own address.
  To get the old link back without the framework: `setprop ctl.stop vendor.usb-pc`, then as
  root `echo > /config/usb_gadget/g1/UDC`, link `functions/ncm.gs9` (mkdir it first if needed)
  and write `a600000.usb` to UDC; or boot the recovery bring-up image.

### NCM IP traffic — fixed in the same path (untested)
`gts7l-usb.sh` now adds `usb0` to netd's local network with the 172.16.42.0/24 route
whenever it puts 172.16.42.1 on it, so replies to the host find a table. For tethering
(RNDIS by default, NCM with `settings put global tether_force_usb_functions 1`) the
Tethering module does the same itself, once the HAL exists.

### Checks for the merged build (main session)
```
adb shell service list | grep -i 'hardware.usb'      # IUsb/default and IUsbGadget/default
adb shell ls -d /sys/class/android_usb               # patch 0270
adb shell dumpsys usb | grep -A8 -E 'device_manager|port_manager'
adb shell getprop sys.usb.configfs                   # 2 after boot
adb shell svc usb getFunctions                       # adb
adb shell svc usb setFunctions mtp; sleep 3; lsusb -d 18d1:   # 18d1:4ee2 (MTP+adb)
mtp-detect | head -30; gio mount -li | grep -i -A3 mtp
adb shell svc usb setFunctions ""; sleep 3; lsusb -d 18d1:    # back to 18d1:4ee7 (adb)
adb root; sleep 5; adb shell id                      # gadget rebound after adbd restart
logcat -s android.hardware.usb-service.pc            # the HAL's log
```

## 5. Media

Status after the media fix (branch `claude/gts7l-fix-codecs-991283`, 2026-11-25). The checks
below ran on the old build (`eng.caoanh.20261124.1208`) with the fix applied at runtime
(`setprop`, `modprobe`); nothing was flashed. The image with the fix built
(`eng.caoanh.20261125.0926`, every step passed) but is not on the tablet: the main session
flashes once after merging every branch.

### Software codecs (Codec2 `IComponentStore/software`) — Fixed

Before: the store was registered but empty (`dumpsys ... IComponentStore/software` →
`Supported components: NONE`, `CodecServiceRegistrant: listComponents -- not supported`), only
the in-process AAC decoder existed, and `screenrecord` failed with `unable to create video/avc
codec instance`.

Cause, from `frameworks/av/media/module/codecserviceregistrant/CodecServiceRegistrant.cpp` and
`media/codec2/hal/common/HalSelection.cpp`: the AIDL software store gets the real platform store
only when `media.c2.hal.selection=aidl`. Unset it defaults to HIDL, the AIDL `/software`
instance is an empty `H2C2ComponentStore(nullptr)`, and the HIDL store is not in VINTF.

With the selection set, `screenrecord` got a codec but crashed with a null dereference in
`CCodec::createInputSurface()` (`libsfplugin_ccodec.so`): there is no `IOmx` service to make an
input surface, and Codec2's own input surface is off. `debug.stagefright.c2inputsurface=-1` makes
the client use its in-process `AidlGraphicBufferSource`.

Fix, in `aosp-device/gts7l/aosp_gts7l.mk` (both as on Cuttlefish,
`device/google/cuttlefish/shared/device.mk`):

```
PRODUCT_VENDOR_PROPERTIES += media.c2.hal.selection?=aidl debug.stagefright.c2inputsurface=-1
```

Proof (runtime `setprop` of both, then `stop/start media.swcodec`, `setprop ctl.restart media`):

```
$ dumpsys android.hardware.media.c2.IComponentStore/software | grep name:
c2.android.aac.decoder  c2.android.aac.encoder  c2.android.amrnb.decoder  c2.android.amrnb.encoder
c2.android.amrwb.decoder  c2.android.amrwb.encoder  c2.android.apv.decoder  c2.android.apv.encoder
c2.android.av1-dav1d.decoder  c2.android.av1.decoder  c2.android.av1.encoder  c2.android.avc.decoder
c2.android.avc.encoder  c2.android.flac.decoder  c2.android.flac.encoder  c2.android.g711.alaw.decoder
c2.android.g711.mlaw.decoder  c2.android.gsm.decoder  c2.android.h263.decoder  c2.android.h263.encoder
c2.android.hevc.decoder  c2.android.hevc.encoder  c2.android.iamf.decoder  c2.android.mp3.decoder
c2.android.mpeg2.decoder  c2.android.mpeg4.decoder  c2.android.mpeg4.encoder  c2.android.opus.decoder
c2.android.opus.encoder  c2.android.raw.decoder  c2.android.vorbis.decoder  c2.android.vp8.decoder
c2.android.vp8.encoder  c2.android.vp9.decoder  c2.android.vp9.encoder  c2.android.xheaac.encoder

$ screenrecord --verbose --time-limit 5 /data/local/tmp/rec.mp4
Configuring recorder for 2560x1600 video/avc at 20.00Mbps
Content area is 2560x1600 at offset x=0 y=0
Encoder stopping; recorded 5 frames in 5 seconds        (static screen: frames only on change)
-rw------- 1 root root 355908 ... rec.mp4
host$ ffprobe rec.mp4  ->  h264 Constrained Baseline 2560x1600, 5 frames
```

MediaCodec through the framework (`MediaCodecList`, `MediaExtractor`, the media.extractor
service), with `docs/gts7l-hal/codec-check/` (`CodecCheck.java`, run by `build.sh` with
`app_process`; clips from `make-clips.sh` on the host, 1280x720 30 fps 3 s, 48 kHz audio):

```
$ build.sh list            -> 60 codecs: every c2.android.* above plus their OMX.google.* aliases
decode avc_aac.mp4   video/avc   c2.android.avc.decoder       in=90  out=90  1280x720 931ms EOS
decode avc_aac.mp4   audio/aac   c2.android.aac.decoder       in=142 out=141 48000Hz/1ch 332ms EOS
decode hevc.mp4      video/hevc  c2.android.hevc.decoder      in=90  out=90  1280x720 690ms EOS
decode vp8_opus.webm video/vp8   c2.android.vp8.decoder       in=90  out=90  1280x720 926ms EOS
decode vp8_opus.webm audio/opus  c2.android.opus.decoder      in=151 out=151 48000Hz/1ch 424ms EOS
decode vp9.webm      video/vp9   c2.android.vp9.decoder       in=90  out=90  1280x720 593ms EOS
decode av1.mp4       video/av01  c2.android.av1-dav1d.decoder in=90  out=90  1280x720 795ms EOS
decode mp3.mp3       audio/mpeg  c2.android.mp3.decoder       in=126 out=126 48000Hz/1ch 73ms EOS
decode flac.flac     audio/raw   c2.android.raw.decoder       in=141 out=141 48000Hz/1ch 110ms EOS
                     (the FLAC extractor hands out PCM, so the raw decoder runs)
decode opus.ogg      audio/opus  c2.android.opus.decoder      in=151 out=151 48000Hz/1ch 115ms EOS
encode video/avc  1280x720 c2.android.avc.encoder  frames=60 packets=61 bytes=246530  857ms EOS
encode video/hevc 1280x720 c2.android.hevc.encoder frames=60 packets=61 bytes=227635 5879ms EOS
encode video/vp8  1280x720 c2.android.vp8.encoder  frames=60 packets=60 bytes=496326 1124ms EOS
encode video/vp9  1280x720 c2.android.vp9.encoder  frames=60 packets=61 bytes=299888 1023ms EOS
```

`dumpsys media.player` lists the same set once system_server and mediaserver have restarted
with the property (after a boot of the fixed image). Still no `stagefright` binary on the image;
`CodecCheck` replaces it.

### Hardware video codec (SM8250 Venus/Iris2) — Kernel works, Codec2 HAL built

**Kernel.** On 7.3 both `venus` and `qcom-iris` know `qcom,sm8250-venus`, but `venus` drops
that compatible when `CONFIG_VIDEO_QCOM_IRIS` is set (`venus/core.c`, `#if
!IS_ENABLED(CONFIG_VIDEO_QCOM_IRIS)`), so `qcom-iris` is the driver. Our config already builds
it (`=m`), with the DT's `firmware-name = "qcom/sm8250/gts7xl/venus.mbn"` from `gts7l_fw`. It was
only missing from the modules that init loads; `qcom-iris.ko` is now in
`aosp-device/gts7l/kernel/modules.bp` (`src_filenames_to_load`, modprobe pulls videodev,
v4l2-mem2mem and videobuf2). No kernel patch needed.

```
$ modprobe -d /vendor/lib/modules qcom-iris     (mc, videobuf2-*, videodev, v4l2-mem2mem, qcom-iris)
/sys/class/video4linux/video14  qcom-iris-decoder
/sys/class/video4linux/video15  qcom-iris-encoder     (video0-13 are the camera's camss nodes)
decoder  OUTPUT: H264 HEVC VP90 (dyn-resolution)   CAPTURE: NV12 Q08C P010 Q10C
```

Hardware decode with `docs/gts7l-hal/codec-check/v4l2dec.c` (the kernel's stateful decoder
flow, NDK `aarch64-linux-android30-clang -static`) on streams split one access unit per buffer by
`v4l2-hdr.py`. `v4l2-ctl --stream-from-hdr` cannot drive it: it counts the compressed Q08C/Q10C
capture formats and decides the device is no codec, so it never waits for the source change.

```
$ v4l2dec /dev/video14 H264 avc.hdr avc.y
capture NV12 1280x736 stride 1280 size 1413120, min buffers 5, 9 buffers
avc.hdr: 90 units in, 90 frames out in 207 ms (434 fps)
$ v4l2dec /dev/video14 HEVC hevc.hdr hevc.y
capture NV12 1280x736 stride 1280 size 1413120, min buffers 7, 11 buffers
hevc.hdr: 90 units in, 90 frames out in 227 ms (396 fps)
$ v4l2dec /dev/video14 VP90 vp9.hdr vp9.y
capture NV12 1280x736 stride 1280 size 1413120, min buffers 4, 8 buffers
vp9.hdr: 90 units in, 90 frames out in 224 ms (402 fps)
host: luma of every frame vs ffmpeg's software decode of the same stream (first 720 of 736 rows)
avc: bit-exact 90/90   hevc: bit-exact 90/90   vp9: bit-exact 90/90
```

No dmesg errors from iris. The encoder node exists but was not tried.

**Codec2 HAL** (`aosp-device/gts7l/media/`): `android.hardware.media.c2-service.gts7l` serves
AOSP's `external/v4l2_codec2` components (`c2.v4l2.avc/hevc/vp9.decoder`) as the AIDL
`IComponentStore/default`. v4l2_codec2's own service is HIDL only, which the AIDL HAL selection
above never asks, so `service.cpp` wraps `V4L2ComponentStore::Create()` in libcodec2_aidl's
`utils::ComponentStore`. C++, not Rust: it is a thin wrapper around a C++ library and the Codec2
C++ API. With it: `media_codecs_c2.xml` (the three decoders, up to 4096x2304), the extra
seccomp syscalls v4l2_codec2 needs on arm64 (`codec2.vendor.ext-arm64.policy`), the VINTF
fragment, an init service (user `mediacodec`, group `camera`), `/dev/video*` and `/dev/media*`
as `system:camera 0660` in `ueventd.gts7l.rc`, and the binary labelled `mediacodec_exec` (the
core policy already lets `mediacodec` use `video_device`). The module builds and the image
build passed; **it has not run on the tablet**. Encoders are not exposed (v4l2_codec2's encoders
are AVC/VP8/VP9 and none was tested on iris).

Untested, for the first flash of the merged build:
- `service list | grep IComponentStore/default`, `dumpsys android.hardware.media.c2.IComponentStore/default`
  should list the three `c2.v4l2.*` decoders, `logcat -s android.hardware.media.c2-service.gts7l`.
- `build.sh decode /data/local/tmp/clips/avc_aac.mp4` should pick `c2.v4l2.avc.decoder`
  (hardware codecs rank first); `build.sh decode FILE c2.android.avc.decoder` forces software.
- The decoders' graphic output buffers come from minigbm's msm backend and are imported into
  iris as dmabufs; the NV12 layout iris wants is 128-byte stride, 32-line aligned height (see
  the 1280x736 above). If playback through the HAL fails or shows garbage, that layout is the
  first suspect. Fallback: drop `android.hardware.media.c2-service.gts7l` from
  `aosp_gts7l.mk`; the software codecs keep working without it.

## 6. Camera — Absent

```
$ service list | grep -c camera.provider
0
$ dumpsys media.camera
Number of camera devices: 0
Number of normal camera devices: 0
$ pm list features | grep camera       -> (none)
$ ls /dev/video*                       -> none
```

`cameraserver` runs and the AOSP Camera2 app is installed, but there is no camera provider
HAL, so no camera. Expected for now (CAMSS/sensor drivers are not part of this kernel).

## 7. CAS — present (example only)

```
$ service list | grep cas
android.hardware.cas.IMediaCasService/default
$ ls -l /proc/<pid>/exe
/apex/com.android.hardware.cas/bin/hw/android.hardware.cas-service.example
$ ls /vendor/lib64/mediacas
libclearkeycasplugin.so
```

AOSP's example CAS service with only the ClearKey test plugin. No real conditional-access
system; nothing on the tablet needs one. State only, not tested.

## Side effects of this check

- `logcat -c` was run once (05:11) to get a clean log for the PIN test; older log lines of
  that boot are gone from the ring buffers.
- `keystore_cli_v2 delete-all` was run as root after the brillo test, which deletes the keys
  of uid 0's Keystore namespace (only test keys were there).
- A few stray strings reached the ACM root shell during the first try (`/systemecho: not
  found`); nothing ran.

## Cần bạn tự kiểm tra

1. **Màn hình khoá với PIN thật (Gatekeeper qua UI).** Mở Settings → Security → Screen lock →
   PIN, đặt PIN (ví dụ 1379). Tắt màn hình bằng nút nguồn, bật lại, nhập sai PIN một lần
   (phải báo sai), rồi nhập đúng (phải mở được). Sau đó vào lại Settings → Screen lock →
   None để bỏ khoá. Báo lại: có mở khoá được không, có treo hay crash không.
2. **Mở khoá sau khi khởi động lại có PIN (Keystore CE key + Gatekeeper sau reboot).** Chỉ làm
   khi không có session nào đang dùng máy: đặt PIN như bước 1, khởi động lại máy, nhập PIN ở màn
   hình khoá đầu tiên. Báo lại: máy có vào được màn hình chính không. Xong thì bỏ PIN (None).
3. **Cổng serial ACM bằng tay.** Trên PC: `picocom -b 115200 /dev/ttyACM0` (hoặc
   `screen /dev/ttyACM0`), gõ `getprop ro.serialno` rồi Enter → phải in ra `<serial>`.
   Thoát picocom bằng Ctrl-A Ctrl-X.
4. **Cài đặt USB (sau khi bản gộp có USB HAL được flash).** Cắm cáp vào PC, kéo thanh thông báo
   xuống: phải có thông báo "Charging this device via USB". Chạm vào → Settings → Connected
   devices → USB. Lần lượt chọn: **File transfer** (trên PC phải hiện ổ "android-pc Galaxy Tab S7
   LTE" trong trình quản lý file, mở được thư mục Download), **PTP** (PC thấy máy như máy ảnh),
   **MIDI**, **No data transfer** (chỉ sạc), và trong Settings → Network → Hotspot & tethering bật
   **USB tethering** (PC phải nhận được mạng mới và vào được web qua máy). Sau mỗi lần đổi, adb
   phải vẫn chạy (`adb devices`). Báo lại: mục nào không chọn được, báo lỗi hay làm mất adb.
5. **Phát video có tiếng (sau khi bản gộp có sửa media được flash).** Chép file thử 1080p
   H.264 + AAC (20 s, hình test có đồng hồ, tiếng bíp) vào máy:
   `adb -s $SERIAL push gts7l-test-1080p.mp4 /sdcard/Movies/` (tạo bằng
   `docs/gts7l-hal/codec-check/make-clips.sh`, hoặc bất kỳ MP4 nào của bạn). Mở bằng app Files hoặc Photos/Gallery. Báo lại: có hình
   không, hình có mượt không, có méo/sọc xanh lá không (nếu có là lỗi bộ giải mã phần cứng),
   có tiếng không (tiếng phụ thuộc vào việc sửa âm thanh của phiên audio). Thử thêm một video
   YouTube/MP4 thật nếu có.
6. **Camera.** Mở app Camera: dự đoán báo lỗi "Can't connect to camera" hoặc đóng ngay (không
   có camera HAL). Chỉ cần xác nhận.
