# Fingerprint unlock in lockdown

Stock Android: "Lockdown" in the power menu locks the screen, hides notifications on the lock
screen, turns off Smart Lock (trust agents) and **requires the PIN/password** for the next
unlock: fingerprint and face stop working until the PIN is entered.

On `aosp_pc_x86_64` (the real PC and Waydroid) a **strong (Class 3) fingerprint still unlocks**
in lockdown, and **right after Android starts**, without the PIN first ("Fingerprint right
after boot" below). Everything else lockdown does stays. The fingerprint here is AOSP's virtual
fingerprint HAL (`com.android.hardware.biometrics.fingerprint.virtual`, an under-display sensor
configured by the `persist.vendor.fingerprint.virtual.*` properties in Waydroid's
`waydroid.cfg`); it reports itself as `STRONG`. `aosp_gts7l` (the Galaxy Tab S7) had the same
setup until 2026-11-27 ("gts7l" below); it now uses its real power-key sensor through our Rust
HAL (`hals/fingerprint`, docs/gts7l.md §25), which the PC product leaves out.

## What changes

| | stock lockdown | with `config_biometricUnlockInLockdown` |
|---|---|---|
| screen locks at once | yes | yes |
| notifications hidden on the lock screen | yes | yes, until the next unlock |
| trust agents (Smart Lock) | off | off |
| fingerprint (Class 3) unlock | no, PIN required | yes; it ends the lockdown |
| face / weak or convenience biometrics | no | no |
| PIN/password | unlocks, ends lockdown | same |

## Implementation

`tools/patches/aosp17-lockdown-biometric.patch` (apply in `aosp/frameworks/base`), gated by the
framework resource `config_biometricUnlockInLockdown` (default `false`, so the patch alone
changes nothing). The RRO `pc_overlay_frameworks_base_core` (`aosp-device/pc/rro`, on the
vendor partition) sets it to `true` for this product. A static `DEVICE_PACKAGE_OVERLAYS` does
not work here: the system image is `generic_system`, whose framework-res ignores device
overlays.

- `core/res`: the new `bool config_biometricUnlockInLockdown`.
- `LockPatternUtils.StrongAuthTracker`: with the setting on,
  `STRONG_AUTH_REQUIRED_AFTER_USER_LOCKDOWN` counts as a flag that allows biometrics, for strong
  biometrics only (`isBiometricAllowedForUser(true, user)`); weak biometrics still see it as
  blocking. Every strong-auth tracker (SystemUI's `KeyguardUpdateMonitor` and
  `BiometricSettingsRepository`, `TrustManagerService`) goes through this.
- `KeyguardUpdateMonitor.shouldListenForFingerprint`: keyguard keeps the fingerprint sensor
  (and the UDFPS touch area) listening while the user is in lockdown.
- `biometrics/Utils.isUserEncryptedOrLockdown`: `FingerprintService.authenticate` no longer
  refuses Keyguard's request in user lockdown (DPM lock-now and encrypted-after-boot still
  refuse).
- `LockSettingsStrongAuth.handleStrongBiometricUnlock`: a strong biometric unlock clears
  `STRONG_AUTH_REQUIRED_AFTER_USER_LOCKDOWN` for that user, as a PIN would. Without this the
  user would stay "in lockdown" while unlocked, and `NotificationManagerService` would keep
  every notification hidden until the PIN is entered.

Keystore needs no change here: `TrustManagerService` passes the user's biometric SIDs to keystore
when the device locks regardless of strong-auth flags, and the fingerprint's auth token unlocks
`UNLOCKED_DEVICE_REQUIRED` keys exactly as in a normal fingerprint unlock. Lockdown never evicts
the CE storage key, so apps' data and keys stay available.

## Waydroid: the touch must match

The virtual HAL decides a touch on the sensor by `vendor.fingerprint.virtual.enrollment_hit`:
the touch matches when it names an enrolled finger, and fails when it is 0. The property is not
persistent, so after every boot it was 0 and touching the sensor never unlocked, lockdown or
not. The user's data has one finger (`Finger 1`, id 1, in
`/data/system/users/0/settings_fingerprint_5.xml`, matching
`persist.vendor.fingerprint.virtual.enrollments=1`), so Waydroid's `[properties]` now set
`vendor.fingerprint.virtual.enrollment_hit = 1` (`/var/lib/waydroid/waydroid.cfg` and
`properties.aosp17`; `tools/waydroid/aosp17-switch.sh` adds it on a new install). `[properties]`
reach the container through `waydroid_base.prop`, which only `waydroid upgrade` rewrites, so run
`tools/waydroid/aosp17-switch.sh use aosp17` after editing them. With `ro.debuggable=0` in Waydroid,
`cmd fingerprint sync`/`fingerdown` do nothing; the sensor is touched on screen (around x=180,
y=560, radius 36).

The virtual HAL sends an empty hardware auth token (`hat = {}`), so keystore logs "Auth token
has zero GK SID" and a fingerprint unlock cannot unlock `UNLOCKED_DEVICE_REQUIRED` keys (keys
created with `setUnlockedDeviceRequired(true)`). They stay locked until the password is
entered. This is the virtual sensor's limit, the same with or without lockdown. Ordinary
keystore keys work.

## Security tradeoff

Lockdown exists for situations where someone could force a finger onto the sensor (an
arrest, a robbery, a border check): it makes the PIN, which cannot be taken by force, the only
way in. With this setting lockdown no longer protects against that: it only hides the lock
screen notifications and turns off trust agents until the next unlock. The fingerprint itself
is unchanged: five failed attempts still lock it out (temporary lockout, then the PIN after
the permanent lockout). A reboot no longer requires the PIN either, but that is a separate
setting ("Fingerprint right after boot" below). In Waydroid the "fingerprint" is a virtual sensor, so anyone who can run
commands in the container (`waydroid shell`, adb) can trigger a match; lockdown with the stock
behaviour would have stopped that.

To get stock lockdown back, set `config_biometricUnlockInLockdown` to `false` in
`aosp-device/pc/rro/res/values/config.xml` and rebuild `vendor.img`.

## Test

2026-11-19, Waydroid (`images-aosp17`, system `eng.builder.20261118.2254`), on a test data
directory (`XDG_DATA_HOME=~/.local/share/waydroid-test`) with a test password and the user's
`settings_fingerprint_5.xml` copied in. The RRO was built by hand with `aapt2` and installed in
Waydroid's overlay (`/var/lib/waydroid/overlay/vendor/overlay/`), because `vendor.img` could not be
rebuilt then:

- Unlocked with the password, then `cmd lock_settings require-strong-auth --old <pw>
  STRONG_AUTH_REQUIRED_AFTER_USER_LOCKDOWN` (what the power menu's Lockdown does):
  `strongAuthRequired=0x20`, keyguard shown, `KeyguardUpdateMonitor` fingerprint
  `listening: actual=1`.
- A long touch on the sensor: `FingerprintAuthenticationClient` succeeded, `deviceLocked=0`,
  strong-auth flags back to 0 (lockdown ended), twice.
- After that, `keystore_cli_v2 generate` / `sign-verify` / `delete`: `Verify: OK`.
- Keystore logs for a lockdown fingerprint unlock and an ordinary one were the same
  (`on_device_unlocked(..., password.is_some()=false)`).
- Then the user's own data started with the new image: `android.pc.overlay` enabled,
  `possible=true` (finger enrolled), `enrollment_hit=1`.

## Fingerprint right after boot

Stock Android wants the PIN once after every start before a fingerprint may unlock
(`STRONG_AUTH_REQUIRED_AFTER_BOOT`). On `aosp_pc_x86_64` an enrolled fingerprint unlocks
straight after boot, the convenience waydroid-nvidia offered as an option ("Fingerprint via
adb", `fingerprint/boot-unlock/`).

### Why the framework setting alone is not enough

The RRO `pc_overlay_frameworks_base_core` sets `config_strongAuthRequiredOnBoot` to `false`, so
the strong-auth flags start at 0 and keyguard listens for the fingerprint at once. That alone
(waydroid-nvidia's overlay) leaves the user worse off on Android 17: the fingerprint dismisses the
keyguard, but the user stays `RUNNING_LOCKED`, because only the synthetic password, which the
PIN unlocks, unlocks the credential-encrypted (CE) storage. The screen shows
`com.android.settings/.FallbackHome` ("starting...") with no keyguard and no way to type the
PIN until the screen is locked again. This is the same in Waydroid, whose `/data` is not
encrypted: `StorageManagerService` counts CE storage as unlocked only after
`unlockCeStorage`, which LockSettings calls with the synthetic password, and the real PC
(Cuttlefish's `fileencryption=aes-256-xts:aes-256-hctr2`) on top needs the key itself. Seen
on 2026-11-26: the host's Waydroid, on the user's data, had waydroid-nvidia's
`FingerprintBootUnlock.apk` in its overlay; after a boot it sat at FallbackHome,
`RUNNING_LOCKED`, keyguard hidden.

### What the patch adds

`tools/patches/aosp17-boot-biometric-unlock.patch` (in `aosp/frameworks/base`, LockSettings
only), active when `config_strongAuthRequiredOnBoot` is `false`:

- Whenever the credential is verified (the PIN typed on the keyguard) or set, LockSettings keeps
  a copy of it, encrypted with AES-GCM under a keystore key of its own
  (`boot_unlock_credential_<user>`, LockSettings' keystore namespace, no user authentication),
  in `/data/system/boot.unlock.credential` (`/data/system/users/<user>/` for other users; DE
  storage, readable before the PIN). Removing the screen lock, or turning the setting off,
  deletes the file and the key at the next verification or change.
- When keyguard reports a successful strong biometric unlock
  (`reportSuccessfulBiometricUnlock`) and the user is not unlocked yet, LockSettings decrypts the
  copy and verifies it as if it had been typed: the synthetic password unlocks keystore and CE
  storage and the user goes to `RUNNING_UNLOCKED` (logs: `Verifying lockscreen credential`,
  `Unlocked CE storage for secured user 0`). A copy that no longer matches is deleted.
- Profiles with a unified lock are unlocked by their parent as usual; nothing is kept for them.

Until that first PIN entry, a touch on the sensor after a boot still hides the keyguard and
leaves the user at FallbackHome, `RUNNING_LOCKED`; lock the screen again and type the PIN.

So the PIN is needed once on a new install or after the screen lock was first set up with a
build without the patch; from then on the fingerprint alone opens Android after every boot. A
fingerprint lockout (5 failed touches) still asks for the PIN.

### Security tradeoff

The user asked for this; it gives up what the PIN protects after a reboot:

- **Anyone who can touch the sensor, or reach adb or `waydroid shell`, can unlock the device
  after a reboot**, including its storage. The sensor is the virtual HAL: `tools/fp.sh touch`,
  or a long press on the sensor spot while `vendor.fingerprint.virtual.enrollment_hit` names an
  enrolled finger (Waydroid's `waydroid.cfg` sets it to 1 at every start, so a press on the spot
  is enough there).
- **The PIN sits on the disk**, wrapped by a keystore key that needs no authentication. The
  PC's KeyMint is software (`com.android.hardware.keymint.rust_nonsecure` in Waydroid,
  Cuttlefish's on the real PC) and keeps its keys in `/data/misc/keystore`, so whoever has the
  disk (or the USB stick) and root can read the PIN and with it the CE storage. Disk encryption
  on the real PC then protects only against someone who has neither.
- Waydroid's `/data` is a host directory and was never encrypted; there the copy only adds that
  `/data/system/boot.unlock.credential` reveals the PIN to the host's root.

To get the stock behaviour back, set `config_strongAuthRequiredOnBoot` to `true` in
`aosp-device/pc/rro/res/values/config.xml` and rebuild `vendor.img`; the next PIN entry deletes
the stored copy.

### tools/fp.sh

`tools/fp.sh` drives the virtual sensor over adb (ported from waydroid-nvidia's
`fingerprint/fp.sh`): Waydroid on this host when its container runs (adb to the IP from
`waydroid status`), else `$ANDROID_SERIAL` or adb's only device, e.g. the real PC.

```bash
tools/fp.sh status
```

```bash
FP_PIN=1234 tools/fp.sh enroll
```

```bash
tools/fp.sh lock
```

```bash
tools/fp.sh touch
```

```bash
tools/fp.sh bad
```

`status` also shows the strong-auth flags and whether user 0 is `RUNNING_LOCKED`. `touch` waits
for the boot unlock and says whether the storage got unlocked. `bad` puts the previous
`enrollment_hit` back afterwards, so a later press on the spot still matches. `lock` calls
`IWindowManager.lockNow` (`service call window 75`, SDK 37). `enroll` opens Settings in
fullscreen (in a desktop window Settings refuses: "Exit split screen to set up Fingerprint
Unlock") and, on landscape screens, needs `tools/patches/aosp17-settings-udfps-enroll-landscape.patch`
(`packages/apps/Settings`): `UdfpsEnrollEnrollingView.adjustUdfpsVieWithFooterBar` cast
`suc_layout_status`, a `LinearLayout` in the landscape layout, to `FrameLayout`, and Settings
crashed on the enrolling screen.

### gts7l

`aosp_gts7l` is built from the same tree, so it carries the same three patches, and is set up
the same way so both products behave alike:

- `gts7l_overlay_frameworks_base_core` (`aosp-device/gts7l/rro`) sets
  `config_biometricUnlockInLockdown` to `true` and `config_strongAuthRequiredOnBoot` to `false`.
- Since 2026-11-27 the tablet has no fingerprint: it is a real device and uses only real
  drivers, and its side-key sensor is TrustZone's and out of reach (docs/gts7l.md §25). The
  RRO above and the patches stay and change nothing without a strong biometric. Before that,
  `aosp_gts7l.mk` added the virtual HAL as Cuttlefish does
  (`com.android.hardware.biometrics.fingerprint.virtual`,
  `android.hardware.biometrics.fingerprint-service.default`, `android.hardware.fingerprint.xml`),
  an under-display sensor at `800:2240:100` (natural orientation of the 1600x2560 panel), and
  `sepolicy-system_ext/platform_app.te` lets SystemUI reach it (Cuttlefish's system_ext rule).
- No `enrollment_hit` is preset, as on the real PC: a press on the spot matches only after
  `tools/fp.sh touch` set it. Over adb: `ANDROID_SERIAL=<serial> tools/fp.sh ...`.
- `/data` on the tablet is not encrypted (fstab.gts7l), so the security tradeoff above applies:
  adb (on, without key confirmation, also on TCP 5555) is enough to unlock after a reboot.

### Test (2026-11-26)

Waydroid, system and vendor `eng.caoanh.20261126.0922` (built with the patch and the RRO),
fresh test data (`XDG_DATA_HOME=~/.local/share/waydroid-test-fp`, test PIN set with
`locksettings set-pin`; finger 1 came from the user's `settings_fingerprint_5.xml` copied in,
because enrolling crashed Settings before the Settings patch):

- After a reboot: keyguard shown, strong-auth flags `0x0`, user 0 `RUNNING_LOCKED`, keyguard's
  `FingerprintAuthenticationClient` running.
- `tools/fp.sh bad`: rejected (`reject: 1`), keyguard still shown.
- `tools/fp.sh touch`: keyguard hidden, CE storage unlocked 0.1 s after the credential check
  began, user 0 `RUNNING_UNLOCKED`, HyprLauncher on top.
- `lock`, `bad` (rejected), `touch` (unlocked) while running.
- PIN changed with `locksettings set-pin --old`, reboot, `bad` rejected, `touch` unlocked: the
  stored copy follows a PIN change.
- System_ext and vendor rebuilt with the Settings patch: `FP_PIN=... tools/fp.sh enroll` enrolled
  finger 2 through Settings (no crash); `lock`, `touch 2` unlocked; after a reboot `bad` was
  rejected and `touch 2` unlocked the user.
- Without a stored copy (the older `waydroid-test` data, whose PIN was set before the patch):
  `touch` hides the keyguard and the user stays `RUNNING_LOCKED` at FallbackHome, as described
  above, until the PIN is typed once.

The real PC was not booted with this build: its unlock goes through the same LockSettings path
as a typed PIN (`unlockCeStorage` then hands the key to vold, as for every PIN unlock).

