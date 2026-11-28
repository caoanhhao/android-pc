# gts7l audio HAL check

## Update 2026-11-28: silent speakers on the merged build

Build `eng.caoanh.20261128.1007` (gts7l with this fix merged) had no sound. The card was
there (`/dev/snd/controlC0` from second 6 of boot, the HAL started at second 9), but
`dumpsys android.hardware.audio.core.IModule/default` showed `output: null Playback device`,
and `PRIMARY_TDM_RX_0 Audio Mixer MultiMedia1` was Off. The HAL log:

```
W ...: card 0 device 0 (Galaxy Tab S7+: MultiMedia1 (*), Analog): cannot probe Playback: /dev/snd/pcmC0D0p: Invalid argument (os error 22); using a null device
W ...: card 0 device 1 (Galaxy Tab S7+: MultiMedia2 (*), Analog): cannot probe Capture: /dev/snd/pcmC0D1c: Invalid argument (os error 22); using a null device
```

An ASoC DPCM front end with no back end routed to it fails to open with EINVAL, and the HAL
wrote the setup file's routes only to cards whose PCMs it had probed, i.e. never. The
tinyplay tests of 2026-11-25 set the routes by hand first, so they did not see it.

Fix (`config.rs`): the setup is written to the primary card before its PCMs are probed.
Workaround on the running build, checked: set the two routes with tinymix and restart the
HAL; it then found `card 0 device 0` / `device 1`, wrote every setup line, AudioTrack played
through it (`Frames written` 849920, speaker device, no ERROR) and the user heard the tone;
`AudioCheck rec1 2` recorded from the built-in mic through the HAL (`frames=96000 in 2.05s
nonzero=95534 rms=91.9 peak=1790`, the room with the user at the tablet):

```
adb root
adb shell 'tinymix -D 0 "PRIMARY_TDM_RX_0 Audio Mixer MultiMedia1" 1; tinymix -D 0 "MultiMedia2 Mixer VA_CODEC_DMA_TX_0" 1; stop vendor.audio-hal; start vendor.audio-hal'
```

The mixer state does not survive a reboot, so the workaround is needed after each boot until
an image with the fix is flashed.

## Update 2026-11-26: silent speakers on the merged build

On the merged build `eng.caoanh.20261126.2024` the user heard nothing from the speakers. The
card was there, but the HAL served null devices:

```
W android.hardware.audio.service.pc: main::config: card 0 device 0 (Galaxy Tab S7+: MultiMedia1 (*), Analog): cannot probe Playback: /dev/snd/pcmC0D0p: Invalid argument (os error 22); using a null device
W android.hardware.audio.service.pc: main::config: card 0 device 1 (Galaxy Tab S7+: MultiMedia2 (*), Analog): cannot probe Capture: /dev/snd/pcmC0D1c: Invalid argument (os error 22); using a null device
```

Even `cat /dev/snd/pcmC0D0p` failed with `EINVAL`. A DPCM front end refuses to open while no
back end is routed to it (`soc-pcm.c`: "no backend DAIs enabled", printed only once per boot),
and the HAL applied `audio_mixer_setup.conf` only to the cards it had probed successfully, so
the routes stayed off. The 2026-11-25 tests had set the routes with tinymix first.

Fix (`config.rs`, `Hardware::discover`): write the setup to the primary card before probing
its PCMs. Checked on the running build by setting the same routes with tinymix and restarting
`vendor.audio-hal` (no flash):

```
I main::config: Playback: card 0 device 0 (Galaxy Tab S7+: MultiMedia1 (*), Analog) Capabilities { formats: [S16Le], channels: [1, 2], rates: [8000, ..., 48000] }
I main::config: Capture: card 0 device 1 (Galaxy Tab S7+: MultiMedia2 (*), Analog) ...
$ AudioCheck play 6            -> end head=288000 written=288000 underruns=1
  dumpsys media.audio_flinger  -> Output devices: 0x2 (AUDIO_DEVICE_OUT_SPEAKER), Frames written: 235520
  host webcam mic              -> 1 kHz at amplitude 89 (0.2 with the tablet silent)
$ AudioCheck rec1 3            -> frames=144000 in 3.07s nonzero=143100 rms=121.9 peak=4887
```

The user heard the speakers on the tablet in this state (2026-11-26).

Also: the logcat buffers are 256 KiB, so the HAL's start-up lines are gone within a minute of
boot; read them with `logcat -G 16M` and a HAL restart.

## Update 2026-11-25: speakers and microphone (branch `claude/gts7l-fix-audio-8566a2`)

The sections after this one are the first check (build `eng.caoanh.20261124.1208`), kept as
the record of what failed. What changed since:

| Part | State | Evidence |
|---|---|---|
| Kernel ALSA card | **Works** (no kernel change) | modules loaded by hand on the running build: `/proc/asound/cards` = ` 0 [S7 ]: sm8250 - Galaxy Tab S7+`, PCMs `00-00 MultiMedia1` (playback + capture), `00-01 MultiMedia2` (capture), `00-02 MultiMedia3` (playback, DP) |
| Speakers (4x CS35L41, Primary TDM RX 0) | **Works** with tinyplay | DAPM on during playback (`FL ASPRX1`, `FL PCM Source`, `FL Main AMP`, `FL SPK` On on all four amps); the host's webcam microphone (UGREEN, next to the tablet) recorded the 1 kHz tone at amplitude 740 against 0.2 with the tablet silent |
| Built-in mic (VA macro DMIC, MultiMedia2) | **Works** with tinycap | real non-zero samples: quiet room rms 4-6 on DMIC0-3; with the speakers playing 1 kHz, DMIC0-3 each capture it (amplitude ~90 at 0 dB, 1841 at +26 dB `VA_DEC0 Volume`); with `VA DMIC MUX0 = ZERO` the capture returns no data |
| Mic through MultiMedia1 | Fails | rms ~5 and no trace of the tone while it plays: use MultiMedia2 |
| Rust HAL (`hals/audio`) on gts7l | **Works after the 2026-11-26 fix** | AudioTrack and AudioRecord through the HAL, see the update above |

### What was wrong

Only the module list. `kernel/modules.bp` loads the drivers in `src_filenames_to_load`
(`modules.load`), and none of the audio stack was there. Loading these (modprobe pulls in
their dependencies from `modules.dep`) brings the card up:

```
apr q6core q6afe q6asm q6adm q6routing q6afe-clocks q6afe-dai q6asm-dai
pinctrl-sm8250-lpass-lpi lpass-gfm-sm8250 snd-soc-lpass-va-macro snd-soc-cs35l41-i2c
snd-soc-hdmi-codec snd-soc-sm8250
```

`snd-soc-hdmi-codec` is needed although nothing plays to DisplayPort yet: the card waits for
every codec of its DAI links (`devices_deferred`: `snd-sm8250: DisplayPort Playback: codec dai
not found`). The four amps probe without a reset GPIO (`Cirrus Logic CS35L41 (35a40),
Revision: A0` at 11-0040..43); the stock DT's TDM setup (Primary TDM, 4 slots, data delay 1,
sync source 1) is what the S7+ series already has.

### Mixer routes

`aosp-device/gts7l/audio_mixer_setup.conf` (installed as `/vendor/etc/audio_mixer_setup.conf`):

```
PRIMARY_TDM_RX_0 Audio Mixer MultiMedia1 = On      # speakers
VA DMIC MUX0 = DMIC1                               # mic
VA DEC0 MUX = VA_DMIC
VA_AIF1_CAP Mixer DEC0 = On
VA_DEC0 Volume = 100                               # +16 dB (84 = 0 dB)
MultiMedia2 Mixer VA_CODEC_DMA_TX_0 = On
```

DMIC0-3 all hear the speakers at about the same level, and the stock routing names no main
mic, so DMIC1 is a choice, not a measurement. The amps play the ASP input directly (`PCM
Source = ASP`); the speaker-protection DSP firmware and the `sec_efs` calibration (patch 0019)
are not loaded.

### Audio HAL

gts7l now uses android-pc's Rust HAL (`hals/audio`, added to `hals.list`) for
`IModule/default`; AOSP's default HAL keeps `IConfig`, `r_submix`, `bluetooth` and the effects,
with `hals/audio/audio_policy_configuration.xml` (no primary module) instead of Cuttlefish's.
Changes to the HAL, none of them visible on the PC (no setup file there):

- `ctl.rs`: enumerated controls (item names), and a board mixer setup file of `<control> =
  <value>` lines (On/Off, a number, an item name), written to the primary card at start.
- `config.rs`: with a setup file, the HAL waits up to 20 s for a sound card (the SoC card
  appears only once the ADSP has booted), then applies it.
- `cards.rs`: `ro.vendor.audio.pc.playback_device` / `capture_device` pick the PCM devices on
  the primary card; `aosp_gts7l.mk` sets `capture_device=1` (MultiMedia2). Host test
  `selects_the_preferred_soc_devices`.

### Reproduce on the running build (no flash)

```
adb -s $SERIAL root
adb -s $SERIAL shell 'for m in apr q6core q6afe q6asm q6adm q6routing q6afe-clocks q6afe-dai q6asm-dai pinctrl-sm8250-lpass-lpi lpass-gfm-sm8250 snd-soc-lpass-va-macro snd-soc-cs35l41-i2c snd-soc-hdmi-codec snd-soc-sm8250; do /vendor/bin/modprobe -d /vendor/lib/modules $m; done'
adb -s $SERIAL shell 'stop audioserver; tinymix -D 0 "PRIMARY_TDM_RX_0 Audio Mixer MultiMedia1" 1; tinyplay /data/local/tmp/tone12.wav -D 0 -d 0'
adb -s $SERIAL shell 'tinymix -D 0 "MultiMedia2 Mixer VA_CODEC_DMA_TX_0" 1; tinymix -D 0 "VA_AIF1_CAP Mixer DEC0" 1; tinymix -D 0 "VA DMIC MUX0" DMIC1; tinycap /data/local/tmp/mic.wav -D 0 -d 1 -c 1 -r 48000 -b 16 -T 3; start audioserver'
```

### Still to test after the merged flash

1. `/proc/asound/cards` lists the card at boot (modules from `modules.load`).
2. logcat `android.hardware.audio.service.pc`: `found card 0 ...`, `Playback: card 0 device 0`,
   `Capture: card 0 device 1`, the `card 0: <control> = <value>` lines of the setup file.
3. `AudioCheck play 4` (§3): `dumpsys media.audio_flinger` shows `Frames written` growing and no
   `ERROR` state.
4. `AudioCheck rec1 2`: `nonzero` > 0 and rms > 0.
5. The user listens (below).

Follow-ups: speaker-protection firmware and calibration (`cirrus/cs35l41-*` in `gts7l_fw`,
DSP1 firmware); which DMIC is the main mic (stereo capture from two DMICs for noise
suppression); DisplayPort audio (MultiMedia3, `DISPLAY_PORT_RX`); headset is absent
(`audio,no-earjack` in the stock DT).

---


Build `eng.caoanh.20261124.1208` (userdebug), tablet `<serial>`, checked 2026-11-25.
Check only: nothing in the image was changed. The tablet rebooted twice during the check
(other sessions); every result below was seen after a full boot (`sys.boot_completed=1`).

The image ships `com.android.hardware.audio` (AOSP's default AIDL audio core: `IModule`
`default`, `r_submix`, `bluetooth`, plus `IConfig`) and AOSP's default effect HAL, with
Cuttlefish's audio policy files (`aosp-device/gts7l/aosp_gts7l.mk`).

## Summary

| HAL / module | Result | Short reason |
|---|---|---|
| Kernel ALSA (sm8250 card, 4x CS35L41, VA mic) | **Fails** | no sound card: none of the ASoC/qdsp6 modules is loaded |
| `IModule/default` speaker output | **Fails** | HAL opens ALSA `card 0 device 0`, which does not exist; stream goes to `ERROR`, 0 frames reach the HAL |
| `IModule/default` built-in mic | **Fails** | same `card 0` open fails for `PCM_IN`; `AudioRecord.read()` blocks forever |
| `IModule/r_submix` | **Works** | a 1 kHz tone played into "Remote Submix Out" is captured from `REMOTE_SUBMIX` at the expected level |
| `IModule/bluetooth` | **Partial** | registered, all A2DP/LE/hearing-aid ports in the policy; no BT audio sink was paired, so no stream was opened |
| Effect HAL (`IFactory/default`) | **Works** | 17 effects listed; LoudnessEnhancer +12 dB raised the captured signal ×3.96 (theory ×3.98) |
| audioserver / AudioFlinger | **Works** | starts, opens the primary output and mixes tracks; its output is lost because the HAL stream is in `ERROR` |

The default HAL is not a null sink: for the speaker and the built-in mic it really uses
tinyalsa on `card 0, device 0` (`hardware/interfaces/audio/aidl/default/primary/StreamPrimary.cpp`,
`PrimaryMixer::kAlsaCard = 0`, `kAlsaDevice = 0`). It only falls back to its stub driver for
telephony, FM, bus devices or the `ro.boot.audio.tinyalsa.*` properties, which are unset here.
So it would reach the speakers only if the kernel had a card 0 whose device 0 is
`MultiMedia1` **and** someone set the q6routing mixer (`PRIMARY_TDM_RX_0 Audio Mixer
MultiMedia1`) and the CS35L41 controls. The default HAL sets no mixer route (it only touches
master volume/mute and mic gain controls by name), so even with a card it would most likely
stay silent. A real gts7l audio HAL (routing + calibration, `docs/gts7l.md` §"audio") is
still needed.

## 1. Kernel: no ALSA card

```
$ adb shell cat /proc/asound/cards
--- no soundcards ---
$ adb shell ls -l /dev/snd
crw-rw---- 1 system audio 116,  33 1970-01-01 00:00 timer
$ adb shell 'lsmod | grep -i -E "snd|cs35|q6|apr"'      # at uptime 3 min
qcom_q6v5_pas  ...   qcom_pil_info ...   qcom_common ...   qcom_q6v5 ...   qcom_sysmon ...
```

The ADSP itself runs:

```
/sys/class/remoteproc/remoteproc2 adsp running qcom/sm8250/gts7xl/adsp.mbn
```

The drivers exist as modules (`CONFIG_SND_SOC=m`, `CONFIG_SND_SOC_SM8250=m`,
`CONFIG_SND_SOC_QDSP6*=m`, `CONFIG_QCOM_APR=m`, `CONFIG_SND_SOC_CS35L41_I2C=m`,
`CONFIG_SND_SOC_LPASS_VA_MACRO=m`) and are installed in `/vendor/lib/modules`
(`snd-soc-sm8250.ko`, `apr.ko`, `q6afe.ko`, `q6asm.ko`, `q6routing.ko`,
`snd-soc-cs35l41-i2c.ko`, ...), but nothing loads them:

```
$ adb shell wc -l /vendor/lib/modules/modules.load
19 /vendor/lib/modules/modules.load
# qmi_helpers qcom_pdr_msg pdr_interface qcom_pd_mapper qcom_glink_smem qcom_sysmon qcom_q6v5
# qcom_common qcom_pil_info qcom_q6v5_pas fastrpc pwrseq-qcom-wcn rfkill ecc ecdh_generic
# bluetooth btbcm btqca hci_uart     (no snd-*, q6*, apr, cs35l41)
$ adb shell ls /vendor/lib/modules | wc -l
1118
$ adb shell grep -i modalias /system/etc/ueventd.rc /vendor/etc/ueventd.rc
(nothing: modalias_handling is off, so ueventd does not load modules for uevents)
```

`init.gts7l.rc` loads only `modules.load` (`modprobe -a --all=.../modules.load`). The four
amplifiers are on i2c-11 with no driver bound:

```
/sys/bus/i2c/devices/11-0040 cs35l41 of:Nspeaker-ampT(null)Ccirrus,cs35l41 drv=
/sys/bus/i2c/devices/11-0041 cs35l41 ... drv=
/sys/bus/i2c/devices/11-0042 cs35l41 ... drv=
/sys/bus/i2c/devices/11-0043 cs35l41 ... drv=
```

`tinymix` and `tinypcminfo` are in `/system/bin` but have no card to open.

Cause: `tools/gts7l-kernel.sh` writes a `modules.load` with every module, but the one in the
vendor image has 19 entries, so the audio stack (and its dependencies `apr`, `q6core`,
`q6afe`, `q6asm`, `q6adm`, `q6routing`, `snd-soc-qcom-common`, `snd-soc-sm8250`,
`snd-soc-cs35l41-i2c`, `snd-soc-lpass-va-macro`, `pinctrl-sm8250-lpass-lpi`,
`lpass-gfm-sm8250`, ...) never loads. Fix belongs in the image (not done here: check only).

## 2. HAL services

```
$ adb shell service list | grep -i -E "audio|effect"
android.hardware.audio.core.IConfig/default
android.hardware.audio.core.IModule/bluetooth
android.hardware.audio.core.IModule/default
android.hardware.audio.core.IModule/r_submix
android.hardware.audio.effect.IFactory/default
android.hardware.bluetooth.audio.IBluetoothAudioProviderFactory/default
media.audio_flinger, media.audio_policy, media.aaudio, audio
$ adb shell ps -A | grep audio
audioserver  android.hardware.audio.service-aidl.example
audioserver  android.hardware.audio.effect.service-aidl.example
audioserver  audioserver
```

`dumpsys media.audio_policy` lists three HW modules: `primary` (Speaker, Telephony Tx,
Built-In Mic, Telephony Rx, FM Tuner), `r_submix` (Remote Submix Out/In, address `0`) and
`bluetooth` (BT A2DP Out/Headphones/Speaker, BT Hearing Aid Out, BLE Headset/Speaker/
Broadcast Out, BLE Headset In). Available output devices: Speaker, Telephony Tx.

## 3. Playback test (speaker)

No player app or media files are in the image, so playback and recording used a small test
program, [`audio-check/AudioCheck.java`](audio-check/AudioCheck.java), run with `app_process`
from `/data/local/tmp` (no APK installed). It writes a 1 kHz stereo sine (amplitude 8000,
48 kHz, `USAGE_MEDIA`) through `AudioTrack`.

Build and run:

```
J=aosp/prebuilts/jdk/jdk21/linux-x86/bin; AJ=aosp/prebuilts/sdk/36/public/android.jar
$J/javac --release 17 -cp $AJ -d cls docs/gts7l-hal/audio-check/AudioCheck.java
$J/java -cp aosp/prebuilts/r8/r8.jar com.android.tools.r8.D8 --lib $AJ --output dex cls/*.class
adb push dex/classes.dex /data/local/tmp/audiocheck.dex
adb shell CLASSPATH=/data/local/tmp/audiocheck.dex app_process /system/bin AudioCheck play 4
```

The client side looks fine (AudioFlinger consumes the data on time):

```
track state=1 routed=AudioDeviceInfo: type: speaker addr:  id: 2
t=1s wrote=9600 head=49152 underruns=0 routed=2:android-pc Galaxy Tab S7 LTE
t=3s wrote=9600 head=143360 underruns=0 routed=2:android-pc Galaxy Tab S7 LTE
end head=192000 written=192000 underruns=1
```

but the HAL never opens the hardware (logcat):

```
D AHAL_Stream: cycle: received command Command{start: Void{}} in writer
D alsa_device_proxy: proxy_open(card:0 device:0 PCM_OUT)
E alsa_device_proxy:   proxy_open() pcm_is_ready() failed: cannot open device (0) for card (0): No such file or directory
E AHAL_AlsaUtils: openProxyForAttachedDevice: failed to open device, address=<0,0> error=-12
E AHAL_StreamPrimary: Function: start Line: 88 Failed
E AHAL_Stream: cycle: start failed: -19
D AHAL_Stream: cycle: writing reply Reply{status: -22, ... state: ERROR, ...}
W AHAL_Stream: command 'burst' can not be handled in the state ERROR     (repeats every write)
```

`dumpsys media.audio_flinger` taken 2.5 s into the tone:

```
Output thread ..., name AudioOut_D, type 0 (MIXER):
  Standby: no
  Output devices: 0x2 (AUDIO_DEVICE_OUT_SPEAKER)
  Timestamp stats: n=42 disc=1 cold=40 nRdy=0 err=57 ...
  Total writes: 56
  AudioStreamOut: ... flags 0x2 (AUDIO_OUTPUT_FLAG_PRIMARY)
  Frames written: 0
  1 Tracks of which 1 are active
    Id Active Client(pid/uid) ... SRate ST Usg CT  G db ... Server FrmCnt FrmRdy F Underruns
    55    yes    3103/   1000 ... 48000  3   1  2   -33 ... 0002EE00   8196  0 A     33280
```

AudioFlinger mixes the active track (56 writes), but `Frames written: 0` on the HAL stream,
every timestamp query fails (`err=57`) and the track shows 33280 underrun frames. Nothing
reaches a speaker.

## 4. Recording test

Built-in mic (`AudioCheck rec1 2`, `AudioSource.MIC`):

```
source=1 minbuf=16384 state=1
recordingState=3
(no further output: read() blocked; killed by timeout after 20 s)
```

```
D AHAL_StreamPrimary: setConnectedDevices: [AudioDevice{type: IN_MICROPHONE ... id: bottom}]
D alsa_device_proxy: proxy_open(card:0 device:0 PCM_IN)
E alsa_device_proxy:   proxy_open() pcm_is_ready() failed: cannot open device (0) for card (0): No such file or directory
E AHAL_Stream: cycle: start failed: -19
E StreamHalAidl: [in|ioHandle:46] standby: not supported from input stream state ERROR
```

Remote submix with nothing playing into it (`AudioCheck rec8 2`): the stream opens and
delivers data at the real-time rate, all zeros:

```
source=8 minbuf=16384 state=1
recordingState=3
frames=96000 in 2.00s nonzero=0 rms=0.0 peak=0 routed=25:android-pc Galaxy Tab S7 LTE
```

Remote submix loopback (`AudioCheck loop 3`): one thread records `REMOTE_SUBMIX`, the other
plays the tone with `setPreferredDevice` = "Remote Submix Out" (address `0`):

```
track state=1 routed=AudioDeviceInfo: type: remote_submix addr: 0 id: 28
frames=144000 in 3.01s nonzero=132112 rms=131.0 peak=189 routed=25:android-pc Galaxy Tab S7 LTE
end head=144000 written=144000 underruns=0
```

```
D DeviceHalAidl: [r_submix] openInputStream: handle: 46 devices 80000100 ...
D AHAL_Module: openInputStream: r_submix: port config id 10, buffer size 1024 frames
D AHAL_Module: connectExternalDevice: r_submix: ... OUT_SUBMIX ... address: AudioDeviceAddress{id: 0}
```

The captured peak (189) matches the tone after the media stream volume AudioFlinger applied
(`G db -33`: 8000 × 10^(−33/20) ≈ 179). The r_submix module carries audio end to end.

## 5. Effect HAL

`AudioCheck fx` (`AudioEffect.queryEffects()`) lists 17 effects from the HAL: Automatic Gain
Control V2, Insert/Auxiliary Environmental Reverb, Visualizer, ExtensionEffectExample,
Loudness Enhancer, Dynamic Bass Boost, Volume, EqualizerBundle, Haptic Generator,
Virtualizer, Multichannel Downmix To Stereo, Insert/Auxiliary Preset Reverb, Noise
Suppression, DynamicsProcessing, Acoustic Echo Canceler.

Processing check: the same submix loopback with a `LoudnessEnhancer` (target gain 1200 mB)
on the track's session (`AudioCheck loopfx1200 3`):

```
LoudnessEnhancer enable=0 gain=1200.0 enabled=true
frames=144000 in 3.01s nonzero=131130 rms=518.9 peak=753 routed=25:...
```

rms 131.0 → 518.9 (×3.96) and peak 189 → 753 (×3.98); +12 dB is ×3.98. The effect HAL
instantiates effects and processes audio in the playback path.

## 6. Bluetooth module

`IModule/bluetooth` is registered and its ports are in the policy (§2); Bluetooth is on
(`dumpsys bluetooth_manager`: `State: ON`, `A2dpOffloadEnabled: false`).

### Update 2026-11-26: headset connected, dropped, no sound

**Symptom.** A Bluetooth headset (EDIFIER X2s, bonded over BR/EDR, class 0x2404 headset)
connected, disconnected again after a few seconds, over and over, and never played sound.

**Cause.** The Bluetooth app starts only the profiles whose `bluetooth.profile.*.enabled`
property is set, and `aosp_gts7l.mk` set only `gatt` and `hid.host` (docs/gts7l.md §20,
written before the audio HAL existed). On build `eng.caoanh.20261126.2024`:

```
Profile: GattService
Profile: HearingAidService
Profile: HidHostService
A2DP Source State: Disabled
AVRCP Target Service not started
```

With no A2DP source, AVRCP target or HFP AG there was no SDP record and no profile for the
headset to connect: the ACL link came up, the headset found nothing to talk to and dropped
it. Nothing was wrong below the stack: the audio HAL's `IBluetoothAudioProviderFactory`
(AOSP's default AIDL audio service, `com.android.hardware.audio`) was registered, the
controller and UART were fine (the HID mouse and keyboard work over the same link).

**Fix.** `aosp_gts7l.mk` also sets `bluetooth.profile.a2dp.source.enabled`,
`bluetooth.profile.avrcp.target.enabled` and `bluetooth.profile.hfp.ag.enabled` (product
properties, as Cuttlefish's phone and goldfish do). A2DP stays on software encoding:
`ro.bluetooth.a2dp_offload.supported` is unset, so the stack opens the provider's
`A2DP_SOFTWARE_ENCODING_DATAPATH` session (encoding in the Bluetooth app, PCM from the audio
HAL over an FMQ). LE audio profiles stay off (untested on the QCA6390).

**Proof without flashing** (the properties are not read-only, the app reads them when the
adapter starts):

```
adb shell 'setprop bluetooth.profile.a2dp.source.enabled true
  setprop bluetooth.profile.avrcp.target.enabled true
  setprop bluetooth.profile.hfp.ag.enabled true
  cmd bluetooth_manager disable; cmd bluetooth_manager wait-for-state:STATE_OFF
  am force-stop com.android.bluetooth
  cmd bluetooth_manager enable; cmd bluetooth_manager wait-for-state:STATE_ON'
```

- `A2dpService`, `AvrcpTargetService`, `HeadsetService` start; `A2DP Source State: Enabled`.
- Connecting the headset from Settings: A2DP and HEADSET `STATE_CONNECTED` (one A2DP
  connect/connect collision retried at once), `active_a2dp_devices` and
  `active_hfp_devices` set, codec SBC 44.1 kHz 16-bit stereo (the headset offers no
  optional codec), still connected a minute later with no new state change.
- Audio policy opens the `bluetooth` module's `a2dp output` on `BT A2DP Out`; the HAL logs
  `updateSourceMetadata: session_type=A2DP_SOFTWARE_ENCODING_DATAPATH ... state=STARTED`.
- `AudioCheck play 10` (§3): the track is routed to `bt_a2dp` "EDIFIER X2s", the playback
  head advances at 48 kHz for 10 s (`end head=480000 written=480000`), underruns 1 (start).

The user heard the 1 kHz tone in the headset (2026-11-26, runtime setprop on build
`eng.caoanh.20261126.2024`). Not proven yet: HFP voice: SCO goes over HCI (the HAL passes SCO
packets, `hals/bluetooth/src/hci.rs`), untested on the QCA6390.

SELinux: the FMQ memfd is labelled `tmpfs`, so `hal_audio_default` and `bluetooth` log
`read write map` denials on `tmpfs:file`. The tablet boots with
`androidboot.selinux=permissive`, so they do not block anything today; an enforcing build
needs them looked at.

## Follow-ups (not done: check only)

- Load the audio modules in the image: put the ASoC/qdsp6/CS35L41/LPASS modules (with their
  dependencies) into `modules.load`, or turn on `modalias_handling enabled` in
  `/vendor/etc/ueventd.rc`. Then check `/proc/asound/cards`, `tinymix` and the CS35L41
  firmware/calibration load in `dmesg`.
- Even with a card, the default HAL sets no ALSA mixer routes; the speakers need a gts7l HAL
  (or mixer setup) that enables `PRIMARY_TDM_RX_0 Audio Mixer MultiMedia1` and the amps, and
  the mic needs the VA macro / `VA_CODEC_DMA_TX_0` route.
- USB audio: `snd-usb-audio.ko` is not loaded and the policy has no `usb` module.

## Cần bạn tự kiểm tra

Sau khi phiên chính gộp mọi nhánh và flash một lần (nhánh này không tự flash):

1. **Nghe loa.** Ở thư mục gốc repo, build và chạy chương trình thử như ở §3:
   ```
   adb -s $SERIAL shell CLASSPATH=/data/local/tmp/audiocheck.dex app_process /system/bin AudioCheck play 4
   ```
   Áp tai gần bốn loa (hai cạnh ngắn của tablet) trong 4 giây. Mong đợi: tiếng bíp 1 kHz đều,
   không rè, không ngắt, nghe được từ cả hai cạnh. Ghi lại: có tiếng / không / rè / chỉ một bên.
   (Trước đó, khi phát bằng tinyplay, micro webcam đã ghi được tiếng bíp; nhưng chưa ai nghe
   bằng tai và chưa phát qua HAL.)
2. **Nghe âm lượng.** Bấm nút âm lượng lên/xuống trong lúc phát `AudioCheck play 10`. Mong đợi:
   tiếng to/nhỏ theo nút.
3. **Mic.** Chạy `AudioCheck rec1 3` rồi nói to gần tablet trong 3 giây; gửi dòng
   `frames=... rms=...` (rms phải lớn hơn rõ rệt so với khi im lặng, khoảng vài chục trở lên).
4. **Tai nghe Bluetooth (A2DP)** (§6): bật tai nghe EDIFIER X2s, kết nối trong Settings →
   Connected devices, rồi chạy
   ```
   adb -s R52R504WFTK shell CLASSPATH=/data/local/tmp/audiocheck.dex app_process /system/bin AudioCheck play 10
   ```
   Mong đợi: tai nghe không tự ngắt, nghe tiếng bíp 1 kHz đều trong 10 giây ở cả hai bên, loa
   tablet im lặng. Sau đó mở YouTube/nhạc và nghe thử vài phút; ghi lại nếu bị ngắt/rè/giật.
