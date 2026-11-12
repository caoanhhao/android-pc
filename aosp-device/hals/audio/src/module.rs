//! `IModule/default`: ports, port configurations, patches and streams of the primary
//! module, following AOSP's default implementation (`Module.cpp`) for a module whose
//! devices are all permanently attached.

use std::collections::hash_map::RandomState;
use std::collections::{BTreeMap, BTreeSet};
use std::hash::BuildHasher;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use android_hardware_audio_core::aidl::android::hardware::audio::core::{
    IBluetooth::IBluetooth, IBluetoothA2dp::IBluetoothA2dp, IBluetoothLe::IBluetoothLe,
    ITelephony::ITelephony,
};
use android_hardware_audio_core_sounddose::aidl::android::hardware::audio::core::sounddose::ISoundDose::ISoundDose;
use android_hardware_audio_effect::aidl::android::hardware::audio::effect::IEffect::IEffect;
use binder::{Interface, Strong};
use log::{debug, info};

use crate::aidl::{
    dup, frame_bytes, illegal_argument, illegal_state, same, unsupported, AudioDeviceType,
    AudioIoFlags, AudioMMapPolicy, AudioMMapPolicyInfo, AudioMMapPolicyType, AudioMode, AudioPatch,
    AudioPort, AudioPortConfig, AudioPortExt, AudioRoute, FlushFromFrameSupport, IModule,
    MicrophoneInfo, ModuleDebug, OpenInputStreamArguments, OpenInputStreamReturn,
    OpenOutputStreamArguments, OpenOutputStreamReturn, ScreenRotation, StreamDescriptor,
    SupportedPlaybackRateFactors, VendorParameter,
};
use crate::config::{self, Configuration, Endpoint, Hardware};
use crate::pcm;
use crate::stream::{self, Levels, SoundDevice, StreamIn, StreamOut, StreamParams, StreamShared};
use crate::worker::{Io, Settings};

/// Largest data FMQ the module allocates (as AOSP's default module).
const MAX_STREAM_BUFFER_BYTES: i64 = 1 << 30;

/// Buffer size for `latency_ms` at `rate`, as the framework sizes mixer bursts: a
/// multiple of 16 frames, and a power of two above 512 frames at 44.1 kHz and up.
pub fn buffer_frames_for(latency_ms: i32, rate: i32) -> i32 {
    let raw = ((i64::from(latency_ms) * i64::from(rate) + 999) / 1000) as i32;
    let multiple_of_16 = (raw + 15) & !15;
    if rate < 44100 || multiple_of_16 <= 512 {
        multiple_of_16
    } else {
        (multiple_of_16 as u32).next_power_of_two() as i32
    }
}

/// An open stream as the module tracks it.
struct OpenStream {
    port_id: i32,
    shared: Weak<StreamShared>,
}

impl OpenStream {
    fn live(&self) -> Option<Arc<StreamShared>> {
        self.shared.upgrade().filter(|s| s.is_open())
    }
}

struct State {
    config: Configuration,
    /// Patch ids by the port config ids (and port ids) they use.
    patch_users: BTreeMap<i32, BTreeSet<i32>>,
    /// Streams by mix port config id.
    streams: BTreeMap<i32, OpenStream>,
    debug: ModuleDebug,
}

impl State {
    fn prune_streams(&mut self) {
        self.streams.retain(|_, s| s.live().is_some());
    }

    fn open_streams_on_port(&mut self, port_id: i32) -> usize {
        self.prune_streams();
        self.streams.values().filter(|s| s.port_id == port_id).count()
    }

    fn has_stream(&mut self, port_config_id: i32) -> bool {
        self.prune_streams();
        self.streams.contains_key(&port_config_id)
    }

    fn register_patch(&mut self, patch: &AudioPatch) {
        for &config_id in patch.sourcePortConfigIds.iter().chain(&patch.sinkPortConfigIds) {
            if let Some(port_id) = self.config.port_config(config_id).map(|c| c.portId) {
                self.patch_users.entry(config_id).or_default().insert(patch.id);
                self.patch_users.entry(port_id).or_default().insert(patch.id);
            }
        }
    }

    fn unregister_patch(&mut self, patch_id: i32) {
        self.patch_users.retain(|_, patches| {
            patches.remove(&patch_id);
            !patches.is_empty()
        });
    }

    /// Tells every open stream whether a patch connects it, and to which microphones.
    fn update_stream_connections(&mut self) {
        self.prune_streams();
        for (&mix_config_id, stream) in &self.streams {
            let Some(shared) = stream.live() else { continue };
            let devices: Vec<i32> = self
                .config
                .patches
                .iter()
                .filter_map(|p| {
                    if p.sourcePortConfigIds.contains(&mix_config_id) {
                        Some(p.sinkPortConfigIds.clone())
                    } else if p.sinkPortConfigIds.contains(&mix_config_id) {
                        Some(p.sourcePortConfigIds.clone())
                    } else {
                        None
                    }
                })
                .flatten()
                .collect();
            let microphones = devices
                .iter()
                .filter_map(|&id| self.config.port_config(id))
                .filter_map(|c| self.config.port(c.portId))
                .filter(|p| is_microphone(p))
                .map(|p| p.name.clone())
                .collect();
            shared.set_connected(!devices.is_empty(), microphones);
        }
    }
}

fn is_microphone(port: &AudioPort) -> bool {
    matches!(&port.ext, AudioPortExt::Device(d)
        if matches!(d.device.r#type.r#type, AudioDeviceType::IN_MICROPHONE | AudioDeviceType::IN_MICROPHONE_BACK))
}

fn is_dynamic_profile(profile: &crate::aidl::AudioProfile) -> bool {
    same(&profile.format, &Default::default())
        || profile.channelMasks.is_empty()
        || profile.sampleRates.is_empty()
}

/// The primary module.
pub struct Module {
    state: Mutex<State>,
    hardware: Hardware,
    levels: Arc<Levels>,
    random: RandomState,
}

impl Module {
    pub fn new(hardware: Hardware) -> Self {
        let config = Configuration::new(&hardware);
        info!("module has {} ports", config.ports.len());
        Self {
            state: Mutex::new(State {
                config,
                patch_users: BTreeMap::new(),
                streams: BTreeMap::new(),
                debug: ModuleDebug::default(),
            }),
            hardware,
            levels: Arc::new(Levels::default()),
            random: RandomState::new(),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().expect("lock poisoned")
    }

    fn microphones(config: &Configuration) -> Vec<MicrophoneInfo> {
        config
            .ports
            .iter()
            .filter(|p| is_microphone(p))
            .filter_map(|p| match &p.ext {
                AudioPortExt::Device(d) => Some(MicrophoneInfo {
                    id: p.name.clone(),
                    device: dup(&d.device),
                    group: 0,
                    indexInTheGroup: 0,
                    ..Default::default()
                }),
                _ => None,
            })
            .collect()
    }

    /// The mix port config a new stream opens on, after the checks every stream needs.
    fn stream_port(
        state: &mut State,
        port_config_id: i32,
        io: Io,
    ) -> binder::Result<(AudioPortConfig, AudioPort)> {
        let config =
            state.config.port_config(port_config_id).map(dup).ok_or_else(|| {
                illegal_argument(&format!("port config {port_config_id} not found"))
            })?;
        let port = state
            .config
            .port(config.portId)
            .map(dup)
            .ok_or_else(|| illegal_argument(&format!("port {} not found", config.portId)))?;
        if state.has_stream(port_config_id) {
            return Err(illegal_state(&format!(
                "port config {port_config_id} already has a stream"
            )));
        }
        let AudioPortExt::Mix(mix) = &port.ext else {
            return Err(illegal_argument(&format!(
                "port config {port_config_id} is not a mix port"
            )));
        };
        let max_open = usize::try_from(mix.maxOpenStreamCount).unwrap_or(0);
        if max_open != 0 && state.open_streams_on_port(port.id) >= max_open {
            return Err(illegal_state(&format!(
                "port {} has {max_open} open streams already",
                port.id
            )));
        }
        let direction_matches = matches!(
            (io, &port.flags),
            (Io::Output, AudioIoFlags::Output(_)) | (Io::Input, AudioIoFlags::Input(_))
        );
        if !direction_matches {
            return Err(illegal_argument(&format!(
                "port config {port_config_id} is not an {io:?} mix port"
            )));
        }
        Ok((config, port))
    }

    fn endpoint(&self, io: Io) -> &Endpoint {
        match io {
            Io::Output => &self.hardware.output,
            Io::Input => &self.hardware.input,
        }
    }

    /// Creates the stream on `port_config_id` and registers it.
    fn open_stream(
        &self,
        port_config_id: i32,
        buffer_frames: i64,
        io: Io,
    ) -> binder::Result<(Arc<StreamShared>, StreamDescriptor)> {
        let mut state = self.state();
        let (config, port) = Self::stream_port(&mut state, port_config_id, io)?;
        let endpoint = self.endpoint(io);
        if buffer_frames <= 0 {
            return Err(illegal_argument(&format!("non-positive buffer size {buffer_frames}")));
        }
        let (Some(format), Some(layout), Some(rate)) =
            (&config.format, &config.channelMask, &config.sampleRate)
        else {
            return Err(illegal_state("stream port config is not fully specified"));
        };
        let minimum = buffer_frames_for(endpoint.nominal_latency_ms, rate.value);
        if buffer_frames < i64::from(minimum) {
            return Err(illegal_argument(&format!(
                "buffer of {buffer_frames} frames, need at least {minimum}"
            )));
        }
        let frame_size = frame_bytes(format, layout)
            .ok_or_else(|| illegal_argument("stream port config is not linear PCM"))?;
        if buffer_frames > MAX_STREAM_BUFFER_BYTES / frame_size as i64 {
            return Err(illegal_argument(&format!(
                "buffer of {buffer_frames} frames is too large"
            )));
        }
        let alsa_format = config::alsa_format(format.pcm)
            .ok_or_else(|| illegal_argument("unsupported PCM type"))?;
        let channel_count =
            crate::aidl::channel_count(layout).expect("frame_bytes checked the layout");
        let pcm_config = pcm::Config {
            format: alsa_format,
            channels: channel_count as u32,
            rate: u32::try_from(rate.value)
                .map_err(|_| illegal_argument("negative sample rate"))?,
            burst_frames: u32::try_from(buffer_frames).unwrap_or(u32::MAX),
        };
        let cookie = (self.random.hash_one((port_config_id, buffer_frames)) as i32) | 1;
        let settings = Settings {
            io,
            frame_bytes: frame_size,
            buffer_bytes: frame_size * buffer_frames as usize,
            nominal_latency_ms: endpoint.nominal_latency_ms,
            transient_state_delay: Duration::from_millis(
                u64::try_from(state.debug.streamTransientStateDelayMs).unwrap_or(0),
            ),
            exit_cookie: cookie,
        };
        let device =
            SoundDevice::new(io, endpoint.pcm.clone(), pcm_config, Arc::clone(&self.levels));
        let params = StreamParams { settings, buffer_frames, device, channel_count };
        let (shared, descriptor) = stream::start(params)
            .map_err(|e| illegal_state(&format!("cannot start the stream worker: {e}")))?;
        state.streams.insert(
            port_config_id,
            OpenStream { port_id: port.id, shared: Arc::downgrade(&shared) },
        );
        state.update_stream_connections();
        debug!("opened {io:?} stream on port config {port_config_id}, {buffer_frames} frames");
        Ok((shared, descriptor))
    }

    fn set_audio_port_config_impl(
        &self,
        requested: &AudioPortConfig,
        suggested: &mut AudioPortConfig,
    ) -> binder::Result<bool> {
        let mut state = self.state();
        let config = &mut state.config;
        let existing = if requested.id != 0 {
            Some(config.port_configs.iter().position(|c| c.id == requested.id).ok_or_else(
                || illegal_argument(&format!("port config {} not found", requested.id)),
            )?)
        } else {
            None
        };
        let port_id = existing.map_or(requested.portId, |i| config.port_configs[i].portId);
        if port_id == 0 {
            return Err(illegal_argument("requested port config has no port id"));
        }
        let port = config
            .port(port_id)
            .ok_or_else(|| illegal_argument(&format!("port {port_id} not found")))?;
        if let AudioPortExt::Device(d) = &port.ext {
            if !d.device.r#type.connection.is_empty() {
                return Err(illegal_argument(&format!("port {port_id} is a template device port")));
            }
        }
        *suggested = match existing {
            Some(i) => dup(&config.port_configs[i]),
            None => default_port_config(port).ok_or_else(|| {
                illegal_argument(&format!("cannot make a default config for port {port_id}"))
            })?,
        };
        let mut valid = true;
        let mut fully_specified = true;
        match &requested.flags {
            Some(flags) if !same(flags, &port.flags) => valid = false,
            Some(_) => {}
            None => fully_specified = false,
        }
        match &requested.format {
            Some(format) if port.profiles.iter().any(|p| same(&p.format, format)) => {
                suggested.format = Some(dup(format))
            }
            Some(_) => valid = false,
            None => fully_specified = false,
        }
        let suggested_format = suggested.format.as_ref().expect("suggested configs have a format");
        let Some(profile) = port.profiles.iter().find(|p| same(&p.format, suggested_format)) else {
            return Err(illegal_argument(&format!(
                "port {port_id} does not support the format any more"
            )));
        };
        match &requested.channelMask {
            Some(mask) if profile.channelMasks.iter().any(|m| same(m, mask)) => {
                suggested.channelMask = Some(dup(mask))
            }
            Some(_) => valid = false,
            None => fully_specified = false,
        }
        match &requested.sampleRate {
            Some(rate) if profile.sampleRates.contains(&rate.value) => {
                suggested.sampleRate = Some(dup(rate))
            }
            Some(_) => valid = false,
            None => fully_specified = false,
        }
        if let Some(gain) = &requested.gain {
            // No port has gain controls.
            return Err(illegal_argument(&format!("port {port_id} has no gain {}", gain.index)));
        }
        match (&requested.ext, &mut suggested.ext) {
            (AudioPortExt::Unspecified(_), _) => {}
            (AudioPortExt::Mix(src), AudioPortExt::Mix(dst)) => {
                dst.handle = src.handle;
                dst.usecase = dup(&src.usecase);
            }
            (AudioPortExt::Device(_), AudioPortExt::Device(_)) => {}
            _ => valid = false,
        }
        let applied = match existing {
            None if valid && fully_specified => {
                suggested.id = config.next_port_id;
                config.next_port_id += 1;
                config.port_configs.push(dup(suggested));
                true
            }
            Some(i) if valid => {
                config.port_configs[i] = dup(suggested);
                true
            }
            _ => false,
        };
        debug!("setAudioPortConfig: {suggested:?} applied: {applied}");
        Ok(applied)
    }

    fn set_audio_patch_impl(&self, requested: &AudioPatch) -> binder::Result<AudioPatch> {
        let unique = |ids: &[i32]| ids.iter().collect::<BTreeSet<_>>().len() == ids.len();
        if requested.sourcePortConfigIds.is_empty() || requested.sinkPortConfigIds.is_empty() {
            return Err(illegal_argument("patch with no sources or no sinks"));
        }
        if !unique(&requested.sourcePortConfigIds) || !unique(&requested.sinkPortConfigIds) {
            return Err(illegal_argument("patch with duplicate port config ids"));
        }
        let mut state = self.state();
        let lookup = |state: &State, ids: &[i32]| -> binder::Result<Vec<AudioPortConfig>> {
            ids.iter()
                .map(|&id| {
                    state
                        .config
                        .port_config(id)
                        .map(dup)
                        .ok_or_else(|| illegal_argument(&format!("port config {id} not found")))
                })
                .collect()
        };
        let sources = lookup(&state, &requested.sourcePortConfigIds)?;
        let sinks = lookup(&state, &requested.sinkPortConfigIds)?;
        // Sink port id -> whether some route to it is non-exclusive.
        let mut allowed_sinks: BTreeMap<i32, bool> = BTreeMap::new();
        for source in &sources {
            for route in
                state.config.routes.iter().filter(|r| r.sourcePortIds.contains(&source.portId))
            {
                *allowed_sinks.entry(route.sinkPortId).or_default() |= !route.isExclusive;
            }
        }
        if let Some(sink) = sinks.iter().find(|s| !allowed_sinks.contains_key(&s.portId)) {
            return Err(illegal_argument(&format!("no route to sink port {}", sink.portId)));
        }
        let existing =
            if requested.id != 0 {
                Some(state.config.patch(requested.id).map(dup).ok_or_else(|| {
                    illegal_argument(&format!("patch {} not found", requested.id))
                })?)
            } else {
                None
            };
        let users_backup = state.patch_users.clone();
        if let Some(old) = &existing {
            state.unregister_patch(old.id);
        }
        if let Some((&sink, _)) = allowed_sinks
            .iter()
            .find(|(sink, non_exclusive)| !**non_exclusive && state.patch_users.contains_key(sink))
        {
            state.patch_users = users_backup;
            return Err(illegal_state(&format!("exclusive sink port {sink} is in use")));
        }
        let mixes = if matches!(sources[0].ext, AudioPortExt::Mix(_)) { &sources } else { &sinks };
        let fastest = mixes
            .iter()
            .filter_map(|c| c.sampleRate.as_ref().map(|r| r.value))
            .max()
            .unwrap_or(48000);
        let io =
            if matches!(sources[0].ext, AudioPortExt::Mix(_)) { Io::Output } else { Io::Input };
        let latency_ms = self.endpoint(io).nominal_latency_ms;
        let mut patch = dup(requested);
        patch.minimumStreamBufferSizeFrames = buffer_frames_for(latency_ms, fastest);
        patch.latenciesMs = vec![latency_ms; patch.sinkPortConfigIds.len()];
        match existing {
            Some(_) => {
                let slot = state
                    .config
                    .patches
                    .iter_mut()
                    .find(|p| p.id == patch.id)
                    .expect("patch exists");
                *slot = dup(&patch);
            }
            None => {
                patch.id = state.config.next_patch_id;
                state.config.next_patch_id += 1;
                state.config.patches.push(dup(&patch));
            }
        }
        state.register_patch(&patch);
        state.update_stream_connections();
        debug!("setAudioPatch: {patch:?}");
        Ok(patch)
    }
}

/// The module has no vendor parameters; the framework and VTS probe for AOSP's debug ones
/// (`aosp.*`) all the time, so this is not logged as an error.
fn unknown_vendor_parameter(id: &str) -> binder::Status {
    debug!("unknown vendor parameter {id:?}");
    binder::Status::new_exception(binder::ExceptionCode::ILLEGAL_ARGUMENT, None)
}

/// The default config of a port: its first fully specified profile.
fn default_port_config(port: &AudioPort) -> Option<AudioPortConfig> {
    let profile = port.profiles.iter().find(|p| !is_dynamic_profile(p))?;
    Some(AudioPortConfig {
        id: 0,
        portId: port.id,
        format: Some(dup(&profile.format)),
        channelMask: Some(dup(&profile.channelMasks[0])),
        sampleRate: Some(crate::aidl::Int { value: profile.sampleRates[0] }),
        gain: None,
        flags: Some(dup(&port.flags)),
        ext: dup(&port.ext),
    })
}

impl Interface for Module {
    fn dump(
        &self,
        writer: &mut dyn std::io::Write,
        _args: &[&std::ffi::CStr],
    ) -> Result<(), binder::StatusCode> {
        let state = self.state();
        let lines = [
            format!("output: {}", self.hardware.output.description),
            format!("input: {}", self.hardware.input.description),
            format!("port configs: {}", state.config.port_configs.len()),
            format!("patches: {}", state.config.patches.len()),
            format!(
                "open streams: {}",
                state.streams.values().filter(|s| s.live().is_some()).count()
            ),
        ];
        for line in lines {
            writeln!(writer, "{line}").map_err(|_| binder::StatusCode::UNKNOWN_ERROR)?;
        }
        Ok(())
    }
}

impl IModule for Module {
    fn setModuleDebug(&self, debug: &ModuleDebug) -> binder::Result<()> {
        if debug.streamTransientStateDelayMs < 0 {
            return Err(illegal_argument("negative streamTransientStateDelayMs"));
        }
        // There are no external devices, so connection simulation changes nothing.
        self.state().debug = dup(debug);
        Ok(())
    }

    fn getTelephony(&self) -> binder::Result<Option<Strong<dyn ITelephony>>> {
        Ok(None)
    }

    fn getBluetooth(&self) -> binder::Result<Option<Strong<dyn IBluetooth>>> {
        Ok(None)
    }

    fn getBluetoothA2dp(&self) -> binder::Result<Option<Strong<dyn IBluetoothA2dp>>> {
        Ok(None)
    }

    fn getBluetoothLe(&self) -> binder::Result<Option<Strong<dyn IBluetoothLe>>> {
        Ok(None)
    }

    fn connectExternalDevice(&self, template: &AudioPort) -> binder::Result<AudioPort> {
        let state = self.state();
        match state.config.port(template.id) {
            None => Err(illegal_argument(&format!("port {} not found", template.id))),
            Some(port) if !matches!(port.ext, AudioPortExt::Device(_)) => {
                Err(illegal_argument(&format!("port {} is not a device port", template.id)))
            }
            Some(_) => {
                Err(illegal_argument(&format!("port {} is permanently attached", template.id)))
            }
        }
    }

    fn disconnectExternalDevice(&self, port_id: i32) -> binder::Result<()> {
        Err(illegal_argument(&format!("port {port_id} is not a connected external device port")))
    }

    fn prepareToDisconnectExternalDevice(&self, port_id: i32) -> binder::Result<()> {
        Err(illegal_argument(&format!("port {port_id} is not a connected external device port")))
    }

    fn getAudioPatches(&self) -> binder::Result<Vec<AudioPatch>> {
        Ok(self.state().config.patches.iter().map(dup).collect())
    }

    fn getAudioPort(&self, port_id: i32) -> binder::Result<AudioPort> {
        self.state()
            .config
            .port(port_id)
            .map(dup)
            .ok_or_else(|| illegal_argument(&format!("port {port_id} not found")))
    }

    fn getAudioPortConfigs(&self) -> binder::Result<Vec<AudioPortConfig>> {
        Ok(self.state().config.port_configs.iter().map(dup).collect())
    }

    fn getAudioPorts(&self) -> binder::Result<Vec<AudioPort>> {
        Ok(self.state().config.ports.iter().map(dup).collect())
    }

    fn getAudioRoutes(&self) -> binder::Result<Vec<AudioRoute>> {
        Ok(self.state().config.routes.iter().map(dup).collect())
    }

    fn getAudioRoutesForAudioPort(&self, port_id: i32) -> binder::Result<Vec<AudioRoute>> {
        let state = self.state();
        if state.config.port(port_id).is_none() {
            return Err(illegal_argument(&format!("port {port_id} not found")));
        }
        Ok(state.config.routes_for(port_id).map(dup).collect())
    }

    fn openInputStream(
        &self,
        args: &OpenInputStreamArguments,
    ) -> binder::Result<OpenInputStreamReturn> {
        stream::check_sink_metadata(&args.sinkMetadata)?;
        let (shared, desc) =
            self.open_stream(args.portConfigId, args.bufferSizeFrames, Io::Input)?;
        Ok(OpenInputStreamReturn { stream: Some(StreamIn::new_binder(&shared)), desc })
    }

    fn openOutputStream(
        &self,
        args: &OpenOutputStreamArguments,
    ) -> binder::Result<OpenOutputStreamReturn> {
        stream::check_source_metadata(&args.sourceMetadata)?;
        // No port has COMPRESS_OFFLOAD or NON_BLOCKING, so offload info and the async
        // callback are not needed (and ignored if given).
        let (shared, desc) =
            self.open_stream(args.portConfigId, args.bufferSizeFrames, Io::Output)?;
        Ok(OpenOutputStreamReturn { stream: Some(StreamOut::new_binder(&shared)), desc })
    }

    fn getSupportedPlaybackRateFactors(&self) -> binder::Result<SupportedPlaybackRateFactors> {
        Err(unsupported())
    }

    fn setAudioPatch(&self, requested: &AudioPatch) -> binder::Result<AudioPatch> {
        self.set_audio_patch_impl(requested)
    }

    fn setAudioPortConfig(
        &self,
        requested: &AudioPortConfig,
        suggested: &mut AudioPortConfig,
    ) -> binder::Result<bool> {
        self.set_audio_port_config_impl(requested, suggested)
    }

    fn resetAudioPatch(&self, patch_id: i32) -> binder::Result<()> {
        let mut state = self.state();
        let Some(index) = state.config.patches.iter().position(|p| p.id == patch_id) else {
            return Err(illegal_argument(&format!("patch {patch_id} not found")));
        };
        state.unregister_patch(patch_id);
        state.config.patches.remove(index);
        state.update_stream_connections();
        Ok(())
    }

    fn resetAudioPortConfig(&self, port_config_id: i32) -> binder::Result<()> {
        let mut state = self.state();
        let Some(index) = state.config.port_configs.iter().position(|c| c.id == port_config_id)
        else {
            return Err(illegal_argument(&format!("port config {port_config_id} not found")));
        };
        if state.has_stream(port_config_id) {
            return Err(illegal_state(&format!("port config {port_config_id} has an open stream")));
        }
        if state.patch_users.contains_key(&port_config_id) {
            return Err(illegal_state(&format!("port config {port_config_id} is used by a patch")));
        }
        match state.config.initial_config(port_config_id).map(dup) {
            Some(initial) => state.config.port_configs[index] = initial,
            None => {
                state.config.port_configs.remove(index);
            }
        }
        Ok(())
    }

    fn getMasterMute(&self) -> binder::Result<bool> {
        Ok(self.levels.master_mute())
    }

    fn setMasterMute(&self, mute: bool) -> binder::Result<()> {
        self.levels.set_master_mute(mute);
        Ok(())
    }

    fn getMasterVolume(&self) -> binder::Result<f32> {
        Ok(self.levels.master_volume())
    }

    fn setMasterVolume(&self, volume: f32) -> binder::Result<()> {
        if !(0.0..=1.0).contains(&volume) {
            return Err(illegal_argument(&format!("invalid master volume {volume}")));
        }
        self.levels.set_master_volume(volume);
        Ok(())
    }

    fn getMicMute(&self) -> binder::Result<bool> {
        Ok(self.levels.mic_mute())
    }

    fn setMicMute(&self, mute: bool) -> binder::Result<()> {
        self.levels.set_mic_mute(mute);
        Ok(())
    }

    fn getMicrophones(&self) -> binder::Result<Vec<MicrophoneInfo>> {
        Ok(Self::microphones(&self.state().config))
    }

    fn updateAudioMode(&self, mode: AudioMode) -> binder::Result<()> {
        // The SYS_RESERVED_* modes are not valid requests.
        if !(AudioMode::NORMAL.0..=AudioMode::CALL_SCREEN.0).contains(&mode.0) {
            return Err(illegal_argument(&format!("invalid audio mode {mode:?}")));
        }
        Ok(())
    }

    fn updateScreenRotation(&self, _rotation: ScreenRotation) -> binder::Result<()> {
        Ok(())
    }

    fn updateScreenState(&self, _on: bool) -> binder::Result<()> {
        Ok(())
    }

    fn getSoundDose(&self) -> binder::Result<Option<Strong<dyn ISoundDose>>> {
        Ok(None)
    }

    fn generateHwAvSyncId(&self) -> binder::Result<i32> {
        Err(unsupported())
    }

    fn getVendorParameters(&self, ids: &[String]) -> binder::Result<Vec<VendorParameter>> {
        match ids.first() {
            Some(id) => Err(unknown_vendor_parameter(id)),
            None => Ok(Vec::new()),
        }
    }

    fn setVendorParameters(
        &self,
        parameters: &[VendorParameter],
        _async: bool,
    ) -> binder::Result<()> {
        match parameters.first() {
            Some(p) => Err(unknown_vendor_parameter(&p.id)),
            None => Ok(()),
        }
    }

    fn addDeviceEffect(
        &self,
        _port_config_id: i32,
        _effect: &Strong<dyn IEffect>,
    ) -> binder::Result<()> {
        Err(unsupported())
    }

    fn removeDeviceEffect(
        &self,
        _port_config_id: i32,
        _effect: &Strong<dyn IEffect>,
    ) -> binder::Result<()> {
        Err(unsupported())
    }

    fn getMmapPolicyInfos(
        &self,
        _policy_type: AudioMMapPolicyType,
    ) -> binder::Result<Vec<AudioMMapPolicyInfo>> {
        // No MMAP_NOIRQ mix port.
        Ok(vec![AudioMMapPolicyInfo { mmapPolicy: AudioMMapPolicy::NEVER, ..Default::default() }])
    }

    fn supportsVariableLatency(&self) -> binder::Result<bool> {
        Ok(false)
    }

    fn getAAudioMixerBurstCount(&self) -> binder::Result<i32> {
        Err(unsupported())
    }

    fn getAAudioHardwareBurstMinUsec(&self) -> binder::Result<i32> {
        Err(unsupported())
    }

    fn getFlushFromFrameSupport(
        &self,
        config: &AudioPortConfig,
    ) -> binder::Result<FlushFromFrameSupport> {
        if config.flags.is_none() || config.format.is_none() {
            return Err(illegal_argument("port config without flags or format"));
        }
        Ok(FlushFromFrameSupport::UNSUPPORTED)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aidl::{has_flag, AudioOutputFlags, AudioPortMixExt};

    /// Flags that would need offload info, an async callback or MMap support.
    const UNSUPPORTED_OUTPUT_FLAGS: [AudioOutputFlags; 3] = [
        AudioOutputFlags::COMPRESS_OFFLOAD,
        AudioOutputFlags::NON_BLOCKING,
        AudioOutputFlags::MMAP_NOIRQ,
    ];

    fn port_named(config: &Configuration, name: &str) -> i32 {
        config.ports.iter().find(|p| p.name == name).expect("port exists").id
    }

    fn module() -> Module {
        Module::new(Hardware::null())
    }

    fn mix_config(module: &Module, port_id: i32) -> AudioPortConfig {
        let requested = AudioPortConfig {
            portId: port_id,
            ext: AudioPortExt::Mix(AudioPortMixExt::default()),
            ..Default::default()
        };
        let mut suggested = AudioPortConfig::default();
        assert!(!module.setAudioPortConfig(&requested, &mut suggested).expect("suggestion"));
        let mut applied = AudioPortConfig::default();
        assert!(module.setAudioPortConfig(&suggested, &mut applied).expect("set"));
        applied
    }

    #[test]
    fn buffer_sizes_follow_the_framework_rules() {
        assert_eq!(buffer_frames_for(85, 48000), 4096);
        assert_eq!(buffer_frames_for(40, 48000), 2048);
        assert_eq!(buffer_frames_for(10, 8000), 80);
        assert_eq!(buffer_frames_for(10, 48000), 480);
    }

    #[test]
    fn ports_have_no_unsupported_flags() {
        let module = module();
        for port in module.getAudioPorts().expect("ports") {
            if let AudioIoFlags::Output(flags) = port.flags {
                for flag in UNSUPPORTED_OUTPUT_FLAGS {
                    assert!(!has_flag(flags, flag.0), "{port:?}");
                }
            }
        }
    }

    #[test]
    fn port_config_patch_and_reset() {
        let module = module();
        let state = module.state();
        let (out_port, speaker_config) =
            (port_named(&state.config, config::PRIMARY_OUTPUT), state.config.port_configs[0].id);
        drop(state);
        let mix = mix_config(&module, out_port);
        assert_ne!(mix.id, 0);
        let patch = AudioPatch {
            sourcePortConfigIds: vec![mix.id],
            sinkPortConfigIds: vec![speaker_config],
            ..Default::default()
        };
        let patch = module.setAudioPatch(&patch).expect("patch");
        assert_eq!(patch.minimumStreamBufferSizeFrames, 2048);
        assert!(module.resetAudioPortConfig(mix.id).is_err(), "used by a patch");
        module.resetAudioPatch(patch.id).expect("reset patch");
        module.resetAudioPortConfig(mix.id).expect("reset config");
        assert_eq!(module.getAudioPortConfigs().expect("configs").len(), 2);
    }

    #[test]
    fn rejects_patches_without_routes() {
        let module = module();
        let in_port = port_named(&module.state().config, config::PRIMARY_INPUT);
        let mix = mix_config(&module, in_port);
        let speaker_config = module.state().config.port_configs[0].id;
        let patch = AudioPatch {
            sourcePortConfigIds: vec![mix.id],
            sinkPortConfigIds: vec![speaker_config],
            ..Default::default()
        };
        assert!(module.setAudioPatch(&patch).is_err());
    }

    #[test]
    fn master_volume_range() {
        let module = module();
        assert!(module.setMasterVolume(1.5).is_err());
        module.setMasterVolume(0.5).expect("in range");
        assert_eq!(module.getMasterVolume().expect("volume"), 0.5);
    }
}
