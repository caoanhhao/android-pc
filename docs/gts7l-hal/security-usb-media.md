# gts7l HAL check: security, USB, media, camera, CAS

Checked on the tablet (SM-T875, serial `R52R504WFTK`) on 2026-11-25, build
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
| USB gadget: NCM | **Partial**: link and ARP up, no IP traffic under Android |
| IUsb / IUsbGadget HAL | **Absent**; framework USB device manager missing |
| Codec2 `IComponentStore/software` | **Fails**: store registered empty, no codec at all |
| Hardware Venus codecs | **Absent** (driver not loaded) |
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
  iSerial        R52R504WFTK
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
`adb -s R52R504WFTK` works for this whole check (`adb root`, shell, logcat).

### ACM — Works
The host has `/dev/ttyACM0`; on the device a root `sh` (domain `gts7l_usb`) sits on
`/dev/ttyGS0`. A command sent from the host through the tty (raw mode, `sudo`) came back:

```
echo ACM_OK; getprop ro.serialno; id -u; cat /proc/self/attr/current
ACM_OK
R52R504WFTK
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

### IUsb / IUsbGadget HAL — Absent
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

adb, ACM and NCM still work because `init.usb.configfs.rc` drives the gadget from
`sys.usb.config` without a HAL. What does not work: Settings → "USB preferences" (MTP/file
transfer, PTP, MIDI, USB tethering), `svc usb setFunctions`, USB-C port/role info. A gadget
HAL (AOSP's `android.hardware.usb.gadget-service.example`-style, configfs) and an IUsb HAL
(typec class, MAX77705 is a `typec` driver here) would close this.

## 5. Media

### Codec2 `android.hardware.media.c2.IComponentStore/software` — Fails
The service is registered and `media.swcodec` runs, but the store is the empty placeholder:

```
$ adb shell dumpsys android.hardware.media.c2.IComponentStore/software
Beginning of dump -- C2ComponentStore: (unknown)
  Supported components:
    NONE
$ logcat --pid=$(pidof media.swcodec)
E CodecServiceRegistrant: listComponents -- not supported.
$ grep -c codec2_soft /proc/$(pidof media.swcodec)/maps
0                                  (the libcodec2_soft_*.so in the APEX are never loaded)
$ dumpsys media.player
Decoder infos by media types:
Media type 'audio/mp4a-latm':
  Decoder "c2.android.inproc.aac.decoder"
Encoder infos by media types:
                                   (nothing else: no AVC/HEVC/VP8/VP9/AV1, no encoders)
$ screenrecord --verbose --time-limit 5 /data/local/tmp/hal_rec.mp4
Display is 2560x1600 @120.00fps (orientation=ROTATION_90), layerStack=0
Configuring recorder for 2560x1600 video/avc at 20.00Mbps
ERROR: unable to create video/avc codec instance
WARNING: failed at 2560x1600, retrying at 1280x720
ERROR: unable to create video/avc codec instance
-rw-r--r-- 1 root root 0 ... hal_rec.mp4
```

No encode, so no clip to decode either; with an empty codec list every decode fails too
(only the in-process AAC decoder exists). No `stagefright` binary on the image.

Root cause, from `frameworks/av/media/module/codecserviceregistrant/CodecServiceRegistrant.cpp`
and `media/codec2/hal/common/HalSelection.cpp`: the AIDL software store gets the real
platform store only when `IsCodec2AidlHalSelected()` is true, which needs
`media.c2.hal.selection=aidl`. Unset, it defaults to `hidl`, so the registrant registers the
AIDL `/software` instance as `H2C2ComponentStore(nullptr)` (empty), and the HIDL store is
not declared in VINTF, so it is not registered either.

```
$ getprop media.c2.hal.selection
                                   (empty)
$ getprop ro.vendor.api_level      -> 202604;  aflags: com.android.media.codec.flags.aidl_hal enabled
```

Cuttlefish sets it in `device/google/cuttlefish/shared/device.mk:97`:
`PRODUCT_VENDOR_PROPERTIES += media.c2.hal.selection?=aidl`. The same line in
`aosp-device/gts7l/aosp_gts7l.mk` should give the full software codec set (needs a rebuild
and a flash; not done here).

### Hardware Venus codecs — Absent
The DT node exists, the modules and the firmware are on the image, but nothing loads them:

```
/sys/bus/platform/devices/aa00000.video-codec   OF_COMPATIBLE_0=qcom,sm8250-venus, no driver bound
/vendor/lib/modules/venus-core.ko venus-dec.ko venus-enc.ko qcom-iris.ko   (not in modules.load)
/mnt/vendor/firmware/qcom/sm8250/gts7xl/venus.mbn (+ .bNN)
/proc/modules: no venus/iris;  /dev/video*: none
service list: no android.hardware.media.c2.IComponentStore/default (no vendor Codec2 store)
```

So: no hardware video decode/encode. That needs the V4L2 driver loaded with the signed
firmware, plus a V4L2 Codec2 HAL (none in this tree).

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
   `screen /dev/ttyACM0`), gõ `getprop ro.serialno` rồi Enter → phải in ra `R52R504WFTK`.
   Thoát picocom bằng Ctrl-A Ctrl-X.
4. **Cài đặt USB (cần IUsb HAL).** Kéo thanh thông báo xuống khi cắm cáp vào PC: có thông báo
   "USB" / "Charging this device via USB" không? Vào Settings → Connected devices → USB: có mở
   được không, có lựa chọn File transfer không? (Dự đoán: không có, vì thiếu USB HAL.)
5. **Phát video / nhạc (sau khi sửa `media.c2.hal.selection=aidl` và flash lại).** Chép một
   file MP4 (H.264 + AAC) vào máy (`adb push clip.mp4 /sdcard/Movies/`), mở bằng app Files
   hoặc Photos, xem có hình và tiếng không. Với bản hiện tại dự đoán: không phát được video.
6. **Camera.** Mở app Camera: dự đoán báo lỗi "Can't connect to camera" hoặc đóng ngay (không
   có camera HAL). Chỉ cần xác nhận.
