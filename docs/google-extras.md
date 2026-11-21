# Google extras: Play Store and ARM64 apps

Google Play Store (`com.android.vending`) and Google's ARM64 → x86_64 translator
(`libndk_translation`, docs/arm-translation.md) are proprietary Google software. The
android-pc image may not contain them, so it is published without them (docs/release.md),
and every user adds them to their own stick from Google's official Android Emulator
system images. Without them Android runs normally: microG, F-Droid and x86_64 apps work,
apps that ship only `arm64-v8a` code do not start, and there is no Play Store.

Google's Android System WebView is not used any more: AOSP's WebView
(`com.android.webview`) is the only provider.

## Adding them

On a Linux computer, with the stick image or the stick:

    tools/add-google-extras.sh out/aosp17-usb.img      # an image, before writing it
    tools/add-google-extras.sh /dev/sdX                # a stick (asks for sudo)

It downloads two emulator images from `dl.google.com` (2.1 GB each, kept in
`~/.cache/android-pc-extras`), checks their SHA-1 against the values Google publishes,
asks you to accept the Android SDK License Agreement, and copies out:

| From | Files |
|---|---|
| `google_apis/x86_64-37.0_r08.zip`, `system` | `system/lib64/libndk_translation.so`, 21 `system/lib64/libproxy_arm64_to_x86_64_*.so`, `system/bin/ndk_translation_program_runner_binfmt_misc_arm64` |
| `google_apis_playstore/x86_64-37.0_r08.zip`, `product` | `product/priv-app/Phonesky/Phonesky.apk`, `product/etc/permissions/privapp-permissions-phonesky.xml` and `product/etc/sysconfig/sysconfig-phonesky.xml` (generated from the image's own entries) |

They go to `android/extras/` on the stick's ESP with a `VERSION` and a `SHA256SUMS` list
(about 110 MB). `--only playstore` or `--only ndk` adds one of the two. `--dir DIR`
writes the same tree to a directory instead, e.g. Waydroid's
`~/.local/share/waydroid/data/vendor/pc-extras` (step 2 below then runs without step 1).

## What Android does with them

The image has empty placeholders at every one of those paths
(`aosp-device/ndk_translation.mk`, `aosp-device/google_extras/extras.mk`).
`init.pcextras.rc` runs `pcextras.sh` in `post-fs-data`, after init has switched to the
default mount namespace and before zygote starts:

1. If the ESP has `android/extras/SHA256SUMS` and it differs from the one in
   `/data/vendor/pc-extras`, the extras are copied to `/data/vendor/pc-extras`, checked
   with `sha256sum -c`, and replace the old copy. A failed copy keeps the old one.
2. Every boot, each file is labelled (`system_file`, the libraries `system_lib_file`)
   and bind-mounted over its placeholder; Play Store's directory is mounted as a whole on
   `/product/priv-app/Phonesky`.

PackageManager then finds the Play Store as a privileged `/product` app with its
permissions, and `libpc_native_bridge` loads the translator as if it were built in. The
log line is `pcextras: mounted 26 files` in the kernel log (pclog's `dmesg`).

## Updates

`tools/update-usb-aosp17.sh` rewrites the ESP. It copies the stick's `android/extras`
back afterwards unless the new image brings its own, and it stops if neither has them
(the new build would boot without Play Store, and PackageManager would drop its data)
unless `--no-extras` is given. `/data/vendor/pc-extras` survives updates in any case.

Removing them: delete `android/extras` from the ESP and `/data/vendor/pc-extras` (as root),
then reboot.

## Checked (2026-11-19, build `eng.builder.20261119.0009`, commit `351a894`)

- QEMU, the stick image with `add-google-extras.sh` applied (default graphics entry, QEMU
  has no NVIDIA GPU): `pcextras: installed ...` then `mounted 26 files` 7.5 s after boot,
  `sys.boot_completed=1`. `pm path com.android.vending` is
  `/product/priv-app/Phonesky/Phonesky.apk`, `PRIVILEGED PRODUCT`, 50.7.34-31, 102
  permissions granted. `/system/lib64/libndk_translation.so` is the real 8.7 MB file
  (`system_lib_file`). The arm64 guest-hook test app (`tools/guest-hook-test`) passes
  through `libpc_native_bridge` and the translator. WebView provider: `com.android.webview`.
- `update-usb-aosp17.sh` from the clean release image onto a copy of that stick image:
  `Google extras written back to the ESP`, same `VERSION`. Onto a clean copy: refuses
  without `--no-extras`.
- Not checked yet: the real PC; an image with no extras at all on the real PC (pcextras.sh
  exits at once, as on Cuttlefish).
