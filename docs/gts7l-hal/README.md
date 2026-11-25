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
| Camera | CAMSS + 3 sensors + focus motor (raw frames, media graph), HAL pipeline (ISP/AE/AWB/AF) on all three, HAL finds 3 cameras | HAL under cameraserver, JPEG through the framework: need the full image | | [camera.md](camera.md) |
| Security, USB, media | KeyMint, Gatekeeper, SecureClock/SharedSecret (SELinux permissive), adb, ACM | NCM (no IP traffic on the tested build; fix in the USB HAL merge), Rust USB HAL (IUsb + IUsbGadget) and Codec2 V4L2 HAL merged but not yet run on the tablet; software Codec2 and qcom-iris decode proven | CAS is the ClearKey example | [security-usb-media.md](security-usb-media.md) |
