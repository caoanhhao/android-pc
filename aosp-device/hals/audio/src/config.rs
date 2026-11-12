//! The primary module's ports, routes and initial port configurations, built from the
//! sound hardware found at start.
//!
//! The module has one output path (`primary output` mix port → `Speaker` device port,
//! the default output) and one input path (`Built-In Mic` → `primary input`). Each is
//! backed by an ALSA PCM of the built-in codec ([`cards::Primary`]); a path without
//! hardware (no sound card, or a codec without capture) is backed by a null device that
//! keeps time and plays silence, so the framework always has a speaker and a microphone.

use log::{info, warn};

use crate::aidl::{
    channel_layout, device_ext, dup, pcm_format, AudioChannelLayout, AudioDevice,
    AudioDeviceAddress, AudioDeviceDescription, AudioDeviceType, AudioIoFlags, AudioOutputFlags,
    AudioPatch, AudioPort, AudioPortConfig, AudioPortDeviceExt, AudioPortExt, AudioPortMixExt,
    AudioProfile, AudioRoute, Int, PcmType,
};
use crate::cards::{self, PcmInfo};
use crate::ctl;
use crate::pcm::{self, Capabilities, Direction, Format, PcmAddress};

/// Nominal latency of real codecs: the client's buffer (one burst) lasts this long.
const LATENCY_MS: i32 = 40;
/// Virtual sound cards (virtio-snd on Cuttlefish) are fed by a host process with coarse
/// timing; AOSP's Cuttlefish HAL uses this latency to pass CTS on them.
const VIRTUAL_LATENCY_MS: i32 = 85;

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
        let pcms = cards::discover();
        for pcm in &pcms {
            info!("found {pcm}");
        }
        let primary = cards::Primary::select(&pcms);
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

fn profiles(caps: &Capabilities) -> Vec<AudioProfile> {
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

fn device_port(
    id: i32,
    name: &str,
    kind: AudioDeviceType,
    address: &str,
    flags: AudioIoFlags,
    profiles: Vec<AudioProfile>,
    default: bool,
) -> AudioPort {
    let ext = AudioPortDeviceExt {
        device: AudioDevice {
            r#type: AudioDeviceDescription { r#type: kind, connection: String::new() },
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
            output,
            profiles(&hardware.output.caps),
            true,
        );
        let mic = device_port(
            2,
            MICROPHONE,
            AudioDeviceType::IN_MICROPHONE,
            "bottom",
            input,
            profiles(&hardware.input.caps),
            false,
        );
        let mix_out = mix_port(3, PRIMARY_OUTPUT, primary, profiles(&hardware.output.caps));
        let mix_in =
            mix_port(4, PRIMARY_INPUT, AudioIoFlags::Input(0), profiles(&hardware.input.caps));
        let routes = vec![
            AudioRoute {
                sourcePortIds: vec![mix_out.id],
                sinkPortId: speaker.id,
                isExclusive: false,
            },
            AudioRoute { sourcePortIds: vec![mic.id], sinkPortId: mix_in.id, isExclusive: false },
        ];
        let port_configs = vec![initial_config(5, &speaker), initial_config(6, &mic)];
        let initial_configs = port_configs.iter().map(dup).collect();
        Self {
            ports: vec![speaker, mic, mix_out, mix_in],
            routes,
            port_configs,
            initial_configs,
            patches: Vec::new(),
            next_port_id: 7,
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
        assert_eq!(config.ports.len(), 4);
        assert_eq!(config.port_configs.len(), 2);
        let output =
            config.ports.iter().find(|p| p.name == PRIMARY_OUTPUT).expect("output mix port");
        assert_eq!(config.routes_for(output.id).count(), 1);
        let speaker = config.port(1).expect("speaker");
        assert!(matches!(&speaker.ext, AudioPortExt::Device(d) if d.flags == 1));
    }

    #[test]
    fn formats_round_trip() {
        for format in Format::ALL {
            assert_eq!(alsa_format(pcm_type(format)), Some(format));
        }
    }
}
