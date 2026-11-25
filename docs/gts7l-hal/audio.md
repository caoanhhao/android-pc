# gts7l audio HAL check

Build `eng.caoanh.20261124.1208` (userdebug), tablet `R52R504WFTK`, checked 2026-11-25.
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
(`dumpsys bluetooth_manager`: `State: ON`, `A2dpOffloadEnabled: false`, A2DP
`Connected count: 0`). No headset or speaker was paired, so no Bluetooth stream was opened.
Partial: needs a manual test (below).

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

Loa trong hiện **không thể** phát ra tiếng (kernel không có sound card nào), nên không có
bước nghe thử loa. Những việc dưới đây chỉ bạn làm được:

1. **Xác nhận loa im lặng (tùy chọn, để đối chiếu).** Trên máy tính, ở thư mục gốc repo
   (có `aosp/`), build và chạy chương trình thử:
   ```
   J=aosp/prebuilts/jdk/jdk21/linux-x86/bin; AJ=aosp/prebuilts/sdk/36/public/android.jar
   mkdir -p /tmp/ac/cls /tmp/ac/dex
   $J/javac --release 17 -cp $AJ -d /tmp/ac/cls docs/gts7l-hal/audio-check/AudioCheck.java
   $J/java -cp aosp/prebuilts/r8/r8.jar com.android.tools.r8.D8 --lib $AJ --output /tmp/ac/dex /tmp/ac/cls/*.class
   adb -s R52R504WFTK push /tmp/ac/dex/classes.dex /data/local/tmp/audiocheck.dex
   adb -s R52R504WFTK shell CLASSPATH=/data/local/tmp/audiocheck.dex app_process /system/bin AudioCheck play 4
   ```
   Áp tai gần bốn loa (hai cạnh ngắn của tablet) trong 4 giây. Kết quả mong đợi theo bằng
   chứng trên: **không nghe gì**. Nếu bạn nghe thấy tiếng bíp 1 kHz, hãy báo lại vì điều đó
   trái với log (`Frames written: 0`).
2. **Âm thanh Bluetooth (A2DP).** Trên tablet: Cài đặt → Thiết bị đã kết nối → Ghép nối
   thiết bị mới, ghép một tai nghe hoặc loa Bluetooth. Khi đã kết nối, chạy lệnh ở bước 1
   (hoặc `AudioCheck play 10` cho 10 giây). Mong đợi: nghe tiếng bíp 1 kHz đều trong tai
   nghe, không rè hay ngắt quãng. Ghi lại: có tiếng / không có tiếng / bị ngắt.
   Sau đó chạy `adb -s R52R504WFTK shell dumpsys media.audio_flinger | grep -A3 "Output devices"`
   trong lúc phát và gửi kết quả (phải thấy `AUDIO_DEVICE_OUT_BLUETOOTH_A2DP`).
3. **Tai nghe Bluetooth có micro (LE Audio, nếu có).** Chỉ cần nếu tai nghe hỗ trợ LE Audio:
   kết nối rồi chạy `AudioCheck rec1 3` trong khi nói vào micro tai nghe; gửi dòng
   `frames=... rms=...` (rms phải lớn hơn 0 rõ rệt).
