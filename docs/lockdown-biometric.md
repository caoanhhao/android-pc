# Fingerprint unlock in lockdown

Stock Android: "Lockdown" in the power menu locks the screen, hides notifications on the lock
screen, turns off Smart Lock (trust agents) and **requires the PIN/password** for the next
unlock: fingerprint and face stop working until the PIN is entered.

On `aosp_pc_x86_64` (the real PC and Waydroid) a **strong (Class 3) fingerprint still unlocks**
in lockdown. Everything else lockdown does stays. The fingerprint here is AOSP's virtual
fingerprint HAL (`com.android.hardware.biometrics.fingerprint.virtual`, an under-display sensor
configured by the `persist.vendor.fingerprint.virtual.*` properties in Waydroid's
`waydroid.cfg`); it reports itself as `STRONG`.

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
the permanent lockout), and a reboot still requires the PIN (`STRONG_AUTH_REQUIRED_AFTER_BOOT`
is not affected). In Waydroid the "fingerprint" is a virtual sensor, so anyone who can run
commands in the container (`waydroid shell`, adb) can trigger a match; lockdown with the stock
behaviour would have stopped that.

To get stock lockdown back, set `config_biometricUnlockInLockdown` to `false` in
`aosp-device/pc/rro/res/values/config.xml` and rebuild `vendor.img`.

## Test

2026-11-19, Waydroid (`images-aosp17`, system `eng.caoanh.20261118.2254`), on a test data
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
