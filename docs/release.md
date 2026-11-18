# Releasing the AOSP 17 image

How a GitHub release of `aosp_pc_x86_64-cp2a-userdebug` is made, and what currently
keeps the image from being published.

## Status (2026-11-18): the image cannot be published as built

The release check (`tools/make-release.sh --check-only`) was run against the images in
`aosp/out/target/product/pc_x86_64` (built 2026-11-18 from `main` at `98dee97` plus the
uncommitted lockdown-biometric change). Three pieces of the image are proprietary Google
software that we may not redistribute. Each is fetched on the build machine and kept out
of git, and the fetch scripts already say "personal use only":

| Piece | Where in the image | Source | Licence | Redistributable |
|---|---|---|---|---|
| Google Play Store (Phonesky, `com.android.vending`) + its privapp permissions and sysconfig | `product/priv-app/Phonesky` | Android Emulator image "Google Play, x86_64, API 37" (`tools/fetch-apps.sh playstore`) | Google proprietary; the emulator images are licensed for development use only | **No** |
| Android System WebView (`com.google.android.webview`) | `product/app/WebViewGoogle` (+ the default-provider overlay from `aosp-device/webview/`) | the same emulator image (`tools/fetch-apps.sh webview`) | Google proprietary | **No** |
| `libndk_translation` (ARM64 translator), its 21 `libproxy_arm64_to_x86_64_*` libraries, `ndk_translation_program_runner_binfmt_misc_arm64` | `system/lib64`, `system/bin` | Android Emulator image "Google APIs, x86_64, API 37" (`tools/fetch-ndk-translation.sh`) | Google proprietary, development use (docs/arm-translation.md) | **No** |

To publish an image, a release build has to leave these out (no Play Store, AOSP's own
WebView as the only provider, no ARM64 apps), for example by building with the three
fetch directories absent (`aosp/device/android-pc/apps/{Phonesky,WebViewGoogle}` and
`aosp/device/android-pc/ndk_translation`; the makefiles then skip them) and rebuilding
`system.img`, `product.img` and `super.img`. The release notes would then have to drop
ARM64 apps and the Play Store from the feature list; users could still add Aurora Store
or F-Droid apps themselves.

## Everything else in the image

| Piece | Licence | Redistributable |
|---|---|---|
| AOSP 17 (`android-17.0.0_r1`), our Rust HALs, device tree | Apache 2.0 / BSD / GPL (kernel) | Yes. GPL parts (GKI kernel, nvidia-open kmods, glibc in the sidecar) need a source offer: link this repo and the AOSP/kernel tags in the release notes |
| GKI `6.18` kernel and modules | GPL-2.0 | Yes, with source (AOSP kernel/common `android17-6.18`) |
| nvidia-open kernel modules 620.70.06 | MIT / GPL-2.0 dual | Yes |
| NVIDIA user-space 620.70.06 (`libnvidia-*`, `libEGL_nvidia`, `libGLX_nvidia`, `libnvidia-ml`; vendor and sidecar copies) | NVIDIA Software License, "Linux/FreeBSD exception": may be redistributed if the binaries are not modified | Yes, conditionally: `cmp` against the host's `/usr/lib` copies shows them byte-identical. Ship NVIDIA's `LICENSE` with the release and say they are unmodified |
| NVIDIA GSP firmware (`gsp_ga10x.bin`, `ucodes_ga10x.bin`) | linux-firmware `LICENCE.nvidia` (redistribution allowed, unmodified) | Yes, identical to the host's `/usr/lib/firmware` copies |
| Intel AX211 Wi-Fi/BT firmware, `ibt-1040-0041`, Realtek `rtl8125*` | linux-firmware licences (`LICENCE.iwlwifi_firmware`, `LICENCE.ibt_firmware`, `LICENCE.rtlwifi_firmware`-style) | Yes, unmodified |
| glibc, libglvnd, libdrm, libepoxy, expat, libgbm, virglrenderer (nvsidecar) | LGPL / MIT | Yes (LGPL: point to the Arch packages' sources) |
| microG GmsCore | Apache 2.0 | Yes |
| Chromium (AndroidDesktop_x64 snapshot) | BSD + third-party open licences | Yes |
| F-Droid, Termux, Termux:API, Termux:Boot, Smart Dock | GPL-3.0 | Yes (F-Droid builds; link the sources) |
| AOSP test keys (platform/APK `testkey`, AVB `testkey_rsa4096`) | Public, in AOSP | See "Secrets" |

## Secrets

- **AVB:** every vbmeta is signed with `external/avb/test/data/testkey_rsa4096.pem`
  (public key SHA-1 `2597c218aae470a130f61162feaae70afd97f011`, rollback index
  `1780617600`). That key is public in AOSP, so nothing secret is shipped, but anyone can
  sign an image the stick accepts. The release notes say so. A private AVB key would also
  change the key `tools/update-usb-aosp17.sh` checks, so existing sticks would need one
  full reflash.
- **APK/platform keys:** AOSP's public test keys (`build/make/target/product/security`).
- **No private keys in the repo or the fetched trees:** `git grep 'PRIVATE KEY'` and a
  search of `aosp-device/` and `aosp/device/android-pc/` (apps excluded) find none, and
  no `.pem/.pk8/.key/.jks` file is tracked. `tools/make-release.sh` repeats a key search
  on the release directory before uploading.

## Making a release (once the image is publishable)

1. Commit the change set; no other build running (`pgrep -x soong_ui`).
2. Build with a fresh build number (`tools/aosp17-build.sh` sets one), never lowering the
   security patch level or the AVB rollback index.
3. `tools/mkusb-aosp17.sh` (default 4 GiB userdata, not `--disk`).
4. `tools/make-release.sh` writes `out/release/aosp17-<date>/`: the `xz`-compressed image
   split into parts under 2 GiB, `build.txt`, `SHA256SUMS`, `INSTALL.md`, `NOTES.md`. It
   refuses when the images contain any piece from the table above.
5. `tools/make-release.sh --upload` tags `aosp17-<date>` and creates a **draft** release;
   the user reviews and publishes it.
