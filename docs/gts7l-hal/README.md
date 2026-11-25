# gts7l HAL status

Every HAL of `aosp_gts7l` checked on the tablet (SM-T875, build
`eng.caoanh.20261124.1208`) on 2026-11-25, with the commands and output as proof in the file
of each group. Wi-Fi and Bluetooth are not here: their own work is in `docs/gts7l.md`.

Each file ends with "Cần bạn tự kiểm tra": the checks that need a person at the tablet (touch,
keys, pen, cover, listening, watching the panel), with the exact commands and what to expect.

| Group | Works | Partial | Fails / absent | File |
|---|---|---|---|---|
| Graphics | allocator (minigbm), freedreno GLES 3.2, Turnip Vulkan 1.3 | composer3: every frame reaches KMS, but all layers are CLIENT (no DPU plane offload) | | [graphics.md](graphics.md) |
| Audio | r_submix, effect HAL, audioserver; speakers and mic with tinyplay/tinycap (2026-11-25, modules loaded by hand) | `IModule/bluetooth` (no sink paired); speaker and mic through the Rust HAL built, not yet flashed | — | [audio.md](audio.md) |
| Power | ILights (backlight); IHealth, IThermal, IPower after the fuel-gauge branch (`claude/gts7l-fix-fuelgauge-a44bc2`: real battery level/voltage/current/temperature, typed thermal sensors with a skin sensor, cpufreq floor/cap hints) | | IPowerStats (AOSP example data) | [power.md](power.md) |
| Sensors, input | ISensors, accelerometer, auto-rotation, touchscreen | gyro/mag/light (the DSP has them, the HAL exposes only the accelerometer), S Pen, keys, Book Cover (present, need a human) | proximity, pressure | [sensors-input.md](sensors-input.md) |
| Security, USB, media | KeyMint, Gatekeeper, SecureClock/SharedSecret (SELinux permissive), adb, ACM | NCM (link up, no IP traffic; fix and Rust USB HAL on branch claude/gts7l-usb-hal-5d9a0d, untested) | Codec2 store empty, no Venus, no IUsb HAL on this build, no camera; CAS is the ClearKey example | [security-usb-media.md](security-usb-media.md) |
