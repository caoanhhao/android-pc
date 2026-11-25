//! The primary module's ports, routes and initial port configurations, built from the
//! sound hardware found at start.
//!
//! The module has one output path (`primary output` mix port → `Speaker` device port,
//! the default output) and one input path (`Built-In Mic` → `primary input`). Each is
//! backed by an ALSA PCM of the built-in codec ([`cards::Primary`]); a path without
//! hardware (no sound card, or a codec without capture) is backed by a null device that
//! keeps time and plays silence, so the framework always has a speaker and a microphone.
//!
//! HDMI/DisplayPort audio is an external device: the `HDMI Out` template device port
//! (connection `hdmi`, fed by the `hdmi output` mix port) is connected by the framework when
//! a monitor with audio is plugged in (WiredAccessoryManager reads the HD Audio HDMI/DP
//! jacks as input switches). [`Endpoint::connect_hdmi`] then picks the PCM.

use std::time::{Duration, Instant};

use log::{info, warn};

use crate::aidl::{
    channel_layout, device_description, device_ext, dup, pcm_format, AudioChannelLayout,
    AudioDevice, AudioDeviceAddress, AudioDeviceDescription, AudioDeviceType, AudioIoFlags,
    AudioOutputFlags, AudioPatch, AudioPort, AudioPortConfig, AudioPortDeviceExt, AudioPortExt,
    AudioPortMixExt, AudioProfile, AudioRoute, Int, PcmType,
};
use crate::cards::{self, HdmiChoice, Kind, PcmInfo};
use crate::ctl;
use crate::pcm::{self, Capabilities, Direction, Format, PcmAddress};

/// Nominal latency of real codecs: the client's buffer (one burst) lasts this long.
const LATENCY_MS: i32 = 40;
/// Virtual sound cards (virtio-snd on Cuttlefish) are fed by a host process with coarse
/// timing; AOSP's Cuttlefish HAL uses this latency to pass CTS on them.
const VIRTUAL_LATENCY_MS: i32 = 85;

/// A board's mixer setup ([`ctl::parse_setup`]): the routes of a SoC card, written to the
/// primary card at start. Boards whose codec needs none have no file.
const MIXER_SETUP_PATH: &str = "/vendor/etc/audio_mixer_setup.conf";
/// How long the HAL waits for a sound card at start on a board with a mixer setup: a SoC
/// card only appears once its DSP has booted and announced its services.
const CARD_WAIT: Duration = Duration::from_secs(20);

/// Rates of the HDMI path: CEA-861 basic audio (stereo LPCM at 32, 44.1 and 48 kHz), which
/// every HDMI sink with audio takes.
const HDMI_RATES: [u32; 3] = [32000, 44100, 48000];

/// What the HDMI path offers: 16-bit stereo at [`HDMI_RATES`].
pub fn hdmi_caps() -> Capabilities {
    Capabilities { formats: vec![Format::S16Le], channels: vec![2], rates: HDMI_RATES.to_vec() }
}

/// The property that picks the HDMI PCM, or turns HDMI audio off ([`HdmiChoice`]).
pub const HDMI_PROPERTY: &str = "persist.vendor.audio.hdmi";

fn hdmi_choice() -> HdmiChoice {
    #[cfg(target_os = "android")]
    {
        use rustutils::android::system_properties;
        let value = system_properties::read(HDMI_PROPERTY).ok().flatten();
        HdmiChoice::parse(value.as_deref())
    }
    #[cfg(not(target_os = "android"))]
    HdmiChoice::Auto
}

/// The PCM devices a board asks for (`ro.vendor.audio.pc.playback_device`,
/// `ro.vendor.audio.pc.capture_device`), see [`cards::Preferred`].
fn preferred_devices() -> cards::Preferred {
    #[cfg(target_os = "android")]
    {
        use rustutils::android::system_properties;
        let read = |name: &str| {
            system_properties::read(name).ok().flatten().and_then(|value| value.trim().parse().ok())
        };
        cards::Preferred {
            playback: read("ro.vendor.audio.pc.playback_device"),
            capture: read("ro.vendor.audio.pc.capture_device"),
        }
    }
    #[cfg(not(target_os = "android"))]
    cards::Preferred::default()
}

/// Logs the monitor state of every HDMI pin of `card` (`/proc/asound/card<N>/eld#*`).
fn log_eld(card: u32) {
    let Ok(entries) = std::fs::read_dir(format!("/proc/asound/card{card}")) else { return };
    let mut paths: Vec<_> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("eld#")))
        .collect();
    paths.sort();
    for path in paths {
        let eld = cards::parse_eld(&std::fs::read_to_string(&path).unwrap_or_default());
        info!(
            "{}: monitor_present {} eld_valid {} {:?}",
            path.display(),
            eld.monitor_present,
            eld.eld_valid,
            eld.monitor_name
        );
    }
}

/// The hardware behind one direction of the module.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    /// `None`: a null device.
    pub pcm: Option<PcmAddress>,
    pub caps: Capabilities,
    pub nominal_latency_ms: i32,
    /// Human-readable origin, for logs and dumps.
    pub description: String,
}

impl Endpoint {
    fn null(direction: Direction) -> Self {
        Self {
            pcm: None,
            caps: Capabilities {
                formats: vec![Format::S16Le],
                channels: vec![1, 2],
                rates: pcm::STANDARD_RATES.to_vec(),
            },
            nominal_latency_ms: LATENCY_MS,
            description: format!("null {direction:?} device"),
        }
    }

    /// Probes `info`; falls back to a null device if the PCM is unusable.
    fn probe(info: Option<&PcmInfo>, direction: Direction) -> Self {
        let Some(info) = info else {
            warn!("no {direction:?} PCM, using a null device");
            return Self::null(direction);
        };
        let address = PcmAddress { card: info.card, device: info.device, direction };
        let caps = match pcm::probe(&address) {
            Ok(caps) => caps,
            Err(err) => {
                warn!("{info}: cannot probe {direction:?}: {err}; using a null device");
                return Self::null(direction);
            }
        };
        // The HAL passes samples through, so it offers 16 bit PCM where the device has it
        // (the framework mixes into it anyway) and the device's own format otherwise.
        let formats: Vec<Format> = if caps.formats.contains(&Format::S16Le) {
            vec![Format::S16Le]
        } else {
            caps.formats.iter().copied().take(1).collect()
        };
        if formats.is_empty() || caps.channels.is_empty() || caps.rates.is_empty() {
            warn!("{info}: no usable {direction:?} configuration in {caps:?}; using a null device");
            return Self::null(direction);
        }
        let virtual_card = info.card_name.to_ascii_lowercase().contains("virtio");
        let endpoint = Self {
            pcm: Some(address),
            caps: Capabilities { formats, ..caps },
            nominal_latency_ms: if virtual_card { VIRTUAL_LATENCY_MS } else { LATENCY_MS },
            description: info.to_string(),
        };
        info!("{direction:?}: {} {:?}", endpoint.description, endpoint.caps);
        endpoint
    }

    /// The HDMI path when no hardware is used (connections simulated by VTS).
    pub fn hdmi_null() -> Self {
        Self { caps: hdmi_caps(), ..Self::null(Direction::Playback) }
    }

    /// The HDMI PCM the framework's HDMI connection plays to: the one `persist.vendor.audio.hdmi`
    /// names, else the one whose jack reports a monitor, else the first HDMI PCM. Switches the
    /// card's IEC958 outputs on. `None`: no HDMI PCM, or HDMI audio is turned off.
    pub fn connect_hdmi() -> Option<Self> {
        let choice = hdmi_choice();
        let pcms = cards::discover();
        let mut hdmi_cards: Vec<u32> =
            pcms.iter().filter(|p| p.kind == Kind::Hdmi).map(|p| p.card).collect();
        hdmi_cards.dedup();
        let mut connected = Vec::new();
        for card in hdmi_cards {
            log_eld(card);
            connected.extend(ctl::hdmi_jacks(card).into_iter().map(|device| (card, device)));
        }
        let Some(info) = cards::select_hdmi(&pcms, &connected, choice) else {
            warn!("HDMI: no PCM for {choice:?} ({} PCMs, jacks on: {connected:?})", pcms.len());
            return None;
        };
        if !connected.contains(&(info.card, info.device)) {
            warn!(
                "HDMI: {info} has no monitor with a valid ELD; the codec only routes a PCM to a \
                 monitor's pin when the GPU reports one"
            );
        }
        ctl::unmute(info.card);
        let address =
            PcmAddress { card: info.card, device: info.device, direction: Direction::Playback };
        let caps = match pcm::probe(&address) {
            Ok(probed) => {
                let wanted = hdmi_caps();
                let keep = |have: &[u32], want: &[u32]| -> Vec<u32> {
                    want.iter().copied().filter(|v| have.contains(v)).collect()
                };
                let caps = Capabilities {
                    formats: wanted
                        .formats
                        .iter()
                        .copied()
                        .filter(|f| probed.formats.contains(f))
                        .collect(),
                    channels: keep(&probed.channels, &wanted.channels),
                    rates: keep(&probed.rates, &wanted.rates),
                };
                if caps.formats.is_empty() || caps.channels.is_empty() || caps.rates.is_empty() {
                    warn!("HDMI: {info} offers {probed:?}, trying {wanted:?}");
                    wanted
                } else {
                    caps
                }
            }
            Err(err) => {
                warn!("HDMI: cannot probe {info}: {err}");
                hdmi_caps()
            }
        };
        let endpoint = Self {
            pcm: Some(address),
            caps,
            nominal_latency_ms: LATENCY_MS,
            description: info.to_string(),
        };
        info!("HDMI ({choice:?}): {} {:?}", endpoint.description, endpoint.caps);
        Some(endpoint)
    }
}

/// Both directions of the module.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hardware {
    pub output: Endpoint,
    pub input: Endpoint,
}

impl Hardware {
    /// Finds the built-in codec in `/proc/asound` and probes its PCMs.
    pub fn discover() -> Self {
        let setup = std::fs::read_to_string(MIXER_SETUP_PATH)
            .map(|text| ctl::parse_setup(&text))
            .unwrap_or_default();
        let mut pcms = cards::discover();
        if !setup.is_empty() {
            let start = Instant::now();
            while pcms.is_empty() && start.elapsed() < CARD_WAIT {
                std::thread::sleep(Duration::from_millis(250));
                pcms = cards::discover();
            }
            info!("{} PCMs after {:?}", pcms.len(), start.elapsed());
        }
        for pcm in &pcms {
            info!("found {pcm}");
        }
        let primary = cards::Primary::select_preferred(&pcms, preferred_devices());
        let hardware = Self {
            output: Endpoint::probe(primary.playback.as_ref(), Direction::Playback),
            input: Endpoint::probe(primary.capture.as_ref(), Direction::Capture),
        };
        let mut cards: Vec<u32> = [&hardware.output.pcm, &hardware.input.pcm]
            .into_iter()
            .flatten()
            .map(|address| address.card)
            .collect();
        cards.dedup();
        for card in cards {
            ctl::unmute(card);
            if !setup.is_empty() {
                ctl::apply_setup(card, &setup);
            }
        }
        hardware
    }

    /// No sound hardware at all.
    #[cfg(test)]
    pub fn null() -> Self {
        Self {
            output: Endpoint::null(Direction::Playback),
            input: Endpoint::null(Direction::Capture),
        }
    }
}

pub fn pcm_type(format: Format) -> PcmType {
    match format {
        Format::S16Le => PcmType::INT_16_BIT,
        Format::S24Le => PcmType::FIXED_Q_8_24,
        Format::S32Le => PcmType::INT_32_BIT,
        Format::S24Packed => PcmType::INT_24_BIT,
        Format::FloatLe => PcmType::FLOAT_32_BIT,
    }
}

pub fn alsa_format(pcm: PcmType) -> Option<Format> {
    Format::ALL.into_iter().find(|&f| pcm_type(f) == pcm)
}

pub fn profiles(caps: &Capabilities) -> Vec<AudioProfile> {
    // Stereo and 48 kHz first: the framework's default picks come from the front.
    let mut channels = caps.channels.clone();
    channels.sort_unstable_by(|a, b| b.cmp(a));
    let mut rates: Vec<i32> = caps.rates.iter().map(|&r| r as i32).collect();
    rates.sort_unstable_by_key(|&r| (r != 48000, r));
    caps.formats
        .iter()
        .map(|&format| AudioProfile {
            name: String::new(),
            format: pcm_format(pcm_type(format)),
            channelMasks: channels
                .iter()
                .map(|&c| {
                    AudioChannelLayout::LayoutMask(if c == 1 {
                        channel_layout::LAYOUT_MONO
                    } else {
                        channel_layout::LAYOUT_STEREO
                    })
                })
                .collect(),
            sampleRates: rates.clone(),
            ..Default::default()
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn device_port(
    id: i32,
    name: &str,
    kind: AudioDeviceType,
    connection: &str,
    address: &str,
    flags: AudioIoFlags,
    profiles: Vec<AudioProfile>,
    default: bool,
) -> AudioPort {
    let ext = AudioPortDeviceExt {
        device: AudioDevice {
            r#type: AudioDeviceDescription { r#type: kind, connection: connection.to_owned() },
            address: AudioDeviceAddress::Id(address.to_owned()),
        },
        flags: if default { 1 << device_ext::FLAG_INDEX_DEFAULT_DEVICE } else { 0 },
        ..Default::default()
    };
    AudioPort {
        id,
        name: name.to_owned(),
        profiles,
        flags,
        ext: AudioPortExt::Device(ext),
        ..Default::default()
    }
}

fn mix_port(id: i32, name: &str, flags: AudioIoFlags, profiles: Vec<AudioProfile>) -> AudioPort {
    let ext =
        AudioPortMixExt { maxOpenStreamCount: 1, maxActiveStreamCount: 1, ..Default::default() };
    AudioPort {
        id,
        name: name.to_owned(),
        profiles,
        flags,
        ext: AudioPortExt::Mix(ext),
        ..Default::default()
    }
}

/// The initial configuration of an attached device port: its first profile.
fn initial_config(id: i32, port: &AudioPort) -> AudioPortConfig {
    let profile = port.profiles.first().expect("device ports have a profile");
    AudioPortConfig {
        id,
        portId: port.id,
        sampleRate: Some(Int { value: profile.sampleRates[0] }),
        channelMask: Some(dup(&profile.channelMasks[0])),
        format: Some(dup(&profile.format)),
        gain: None,
        flags: Some(dup(&port.flags)),
        ext: dup(&port.ext),
    }
}

pub const SPEAKER: &str = "Speaker";
pub const MICROPHONE: &str = "Built-In Mic";
pub const PRIMARY_OUTPUT: &str = "primary output";
pub const PRIMARY_INPUT: &str = "primary input";
pub const HDMI: &str = "HDMI Out";
pub const HDMI_OUTPUT: &str = "hdmi output";

/// The module's mutable configuration: what `getAudioPorts`, `getAudioRoutes`,
/// `getAudioPortConfigs` and `getAudioPatches` return. Ports and port configs share one
/// id space.
#[derive(Debug)]
pub struct Configuration {
    pub ports: Vec<AudioPort>,
    pub routes: Vec<AudioRoute>,
    pub port_configs: Vec<AudioPortConfig>,
    /// Port configs that exist from the start; `resetAudioPortConfig` restores them.
    pub initial_configs: Vec<AudioPortConfig>,
    pub patches: Vec<AudioPatch>,
    pub next_port_id: i32,
    pub next_patch_id: i32,
}

impl Configuration {
    pub fn new(hardware: &Hardware) -> Self {
        let output = AudioIoFlags::Output(0);
        let input = AudioIoFlags::Input(0);
        let primary = AudioIoFlags::Output(1 << AudioOutputFlags::PRIMARY.0);
        let speaker = device_port(
            1,
            SPEAKER,
            AudioDeviceType::OUT_SPEAKER,
            "",
            "",
            output,
            profiles(&hardware.output.caps),
            true,
        );
        let mic = device_port(
            2,
            MICROPHONE,
            AudioDeviceType::IN_MICROPHONE,
            "",
            "bottom",
            input,
            profiles(&hardware.input.caps),
            false,
        );
        let mix_out = mix_port(3, PRIMARY_OUTPUT, primary, profiles(&hardware.output.caps));
        let mix_in =
            mix_port(4, PRIMARY_INPUT, AudioIoFlags::Input(0), profiles(&hardware.input.caps));
        // Template: no profiles until a device is connected (Module::connectExternalDevice).
        let hdmi = device_port(
            5,
            HDMI,
            AudioDeviceType::OUT_DEVICE,
            device_description::CONNECTION_HDMI,
            "",
            AudioIoFlags::Output(0),
            Vec::new(),
            false,
        );
        let mix_hdmi = mix_port(6, HDMI_OUTPUT, AudioIoFlags::Output(0), profiles(&hdmi_caps()));
        let routes = vec![
            AudioRoute {
                sourcePortIds: vec![mix_out.id],
                sinkPortId: speaker.id,
                isExclusive: false,
            },
            AudioRoute { sourcePortIds: vec![mic.id], sinkPortId: mix_in.id, isExclusive: false },
            AudioRoute {
                sourcePortIds: vec![mix_hdmi.id],
                sinkPortId: hdmi.id,
                isExclusive: false,
            },
        ];
        let port_configs = vec![initial_config(7, &speaker), initial_config(8, &mic)];
        let initial_configs = port_configs.iter().map(dup).collect();
        Self {
            ports: vec![speaker, mic, mix_out, mix_in, hdmi, mix_hdmi],
            routes,
            port_configs,
            initial_configs,
            patches: Vec::new(),
            next_port_id: 9,
            next_patch_id: 1,
        }
    }

    pub fn port(&self, id: i32) -> Option<&AudioPort> {
        self.ports.iter().find(|p| p.id == id)
    }

    pub fn port_config(&self, id: i32) -> Option<&AudioPortConfig> {
        self.port_configs.iter().find(|c| c.id == id)
    }

    pub fn initial_config(&self, id: i32) -> Option<&AudioPortConfig> {
        self.initial_configs.iter().find(|c| c.id == id)
    }

    pub fn patch(&self, id: i32) -> Option<&AudioPatch> {
        self.patches.iter().find(|p| p.id == id)
    }

    /// Routes that `port_id` is the sink or one of the sources of.
    pub fn routes_for(&self, port_id: i32) -> impl Iterator<Item = &AudioRoute> {
        self.routes
            .iter()
            .filter(move |r| r.sinkPortId == port_id || r.sourcePortIds.contains(&port_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_put_the_defaults_first() {
        let caps = Capabilities {
            formats: vec![Format::S16Le],
            channels: vec![1, 2],
            rates: vec![44100, 48000, 96000],
        };
        let profiles = profiles(&caps);
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].sampleRates, vec![48000, 44100, 96000]);
        assert!(matches!(
            profiles[0].channelMasks[0],
            AudioChannelLayout::LayoutMask(channel_layout::LAYOUT_STEREO)
        ));
    }

    #[test]
    fn null_hardware_still_has_both_paths() {
        let config = Configuration::new(&Hardware::null());
        assert_eq!(config.ports.len(), 6);
        assert_eq!(config.port_configs.len(), 2);
        let output =
            config.ports.iter().find(|p| p.name == PRIMARY_OUTPUT).expect("output mix port");
        assert_eq!(config.routes_for(output.id).count(), 1);
        let speaker = config.port(1).expect("speaker");
        assert!(matches!(&speaker.ext, AudioPortExt::Device(d) if d.flags == 1));
    }

    #[test]
    fn hdmi_is_a_template_fed_by_its_own_mix_port() {
        let config = Configuration::new(&Hardware::null());
        let hdmi = config.ports.iter().find(|p| p.name == HDMI).expect("HDMI port");
        assert!(hdmi.profiles.is_empty());
        assert!(matches!(&hdmi.ext, AudioPortExt::Device(d)
            if d.device.r#type.connection == device_description::CONNECTION_HDMI));
        let mix = config.ports.iter().find(|p| p.name == HDMI_OUTPUT).expect("HDMI mix port");
        let routes: Vec<_> = config.routes_for(hdmi.id).collect();
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].sourcePortIds, vec![mix.id]);
        assert_eq!(mix.profiles[0].sampleRates, vec![48000, 32000, 44100]);
    }

    #[test]
    fn formats_round_trip() {
        for format in Format::ALL {
            assert_eq!(alsa_format(pcm_type(format)), Some(format));
        }
    }
}
