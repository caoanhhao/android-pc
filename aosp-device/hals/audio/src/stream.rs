//! `IStreamOut`, `IStreamIn` and `IStreamCommon`: one worker thread per stream serves
//! the command FMQ ([`worker::Worker`]) and moves audio between the data FMQ and the
//! ALSA PCM ([`SoundDevice`]).

use std::io;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use android_hardware_audio_common::aidl::android::hardware::audio::common::AudioOffloadMetadata::AudioOffloadMetadata;
use android_hardware_audio_core::aidl::android::hardware::audio::core::{
    IStreamCommon::{BnStreamCommon, IStreamCommon},
    IStreamIn::{BnStreamIn, IStreamIn, MicrophoneDirection::MicrophoneDirection},
    IStreamOut::{BnStreamOut, IStreamOut},
    MmapBufferDescriptor::MmapBufferDescriptor,
    StreamDescriptor::{
        AudioBuffer::AudioBuffer, Command::Command as AidlCommand, Position::Position,
        Reply::Reply as AidlReply, State::State as AidlState, StreamDescriptor,
    },
    VendorParameter::VendorParameter,
};
use android_hardware_audio_effect::aidl::android::hardware::audio::effect::IEffect::IEffect;
use android_media_audio_common_types::aidl::android::media::audio::common::{
    AudioDualMonoMode::AudioDualMonoMode,
    AudioLatencyMode::AudioLatencyMode,
    AudioPlaybackRate::AudioPlaybackRate,
    MicrophoneDynamicInfo::{ChannelMapping::ChannelMapping, MicrophoneDynamicInfo},
};
use binder::{BinderFeatures, Interface, Strong};
use log::{debug, error, warn};
use pcfmq::MessageQueue;

use crate::aidl::{
    illegal_argument, illegal_state, is_valid_codec_provenance, is_vendor_extension, unsupported,
    SinkMetadata, SourceMetadata,
};
use crate::pcm::{self, Pcm, PcmAddress};
use crate::worker::{self, DataQueue, Device, Io, Settings, Step, Worker};

/// Module-wide levels the streams apply in software: master volume and mute for
/// playback, microphone mute for capture.
#[derive(Debug)]
pub struct Levels {
    master_mute: AtomicBool,
    /// `f32` bits.
    master_volume: AtomicU32,
    mic_mute: AtomicBool,
}

impl Default for Levels {
    fn default() -> Self {
        Self {
            master_mute: AtomicBool::new(false),
            master_volume: AtomicU32::new(1.0f32.to_bits()),
            mic_mute: AtomicBool::new(false),
        }
    }
}

impl Levels {
    pub fn set_master_mute(&self, mute: bool) {
        self.master_mute.store(mute, Ordering::Relaxed);
    }
    pub fn master_mute(&self) -> bool {
        self.master_mute.load(Ordering::Relaxed)
    }
    pub fn set_master_volume(&self, volume: f32) {
        self.master_volume.store(volume.to_bits(), Ordering::Relaxed);
    }
    pub fn master_volume(&self) -> f32 {
        f32::from_bits(self.master_volume.load(Ordering::Relaxed))
    }
    pub fn set_mic_mute(&self, mute: bool) {
        self.mic_mute.store(mute, Ordering::Relaxed);
    }
    pub fn mic_mute(&self) -> bool {
        self.mic_mute.load(Ordering::Relaxed)
    }

    /// The gain to apply to a stream: 0 when muted.
    fn gain(&self, io: Io) -> f32 {
        match io {
            Io::Output if self.master_mute() => 0.0,
            Io::Output => self.master_volume(),
            Io::Input if self.mic_mute() => 0.0,
            Io::Input => 1.0,
        }
    }
}

/// Scales 16 bit samples by `gain`; other formats are only silenced at gain 0.
fn apply_gain(data: &mut [u8], format: pcm::Format, gain: f32) {
    if gain >= 1.0 {
        return;
    }
    if gain <= 0.0 {
        data.fill(0);
        return;
    }
    if format == pcm::Format::S16Le {
        for sample in data.chunks_exact_mut(2) {
            let value = i16::from_le_bytes([sample[0], sample[1]]);
            // `gain` is in (0, 1), so the product stays in range.
            let scaled = (f32::from(value) * gain) as i16;
            sample.copy_from_slice(&scaled.to_le_bytes());
        }
    }
}

/// The device behind a stream: an ALSA PCM, opened while the stream is out of standby,
/// or a null device that keeps time.
pub struct SoundDevice {
    io: Io,
    address: Option<PcmAddress>,
    config: pcm::Config,
    pcm: Option<Pcm>,
    levels: Arc<Levels>,
}

impl SoundDevice {
    pub fn new(
        io: Io,
        address: Option<PcmAddress>,
        config: pcm::Config,
        levels: Arc<Levels>,
    ) -> Self {
        Self { io, address, config, pcm: None, levels }
    }

    fn pcm(&mut self) -> io::Result<Option<&mut Pcm>> {
        let Some(address) = &self.address else { return Ok(None) };
        if self.pcm.is_none() {
            let pcm = Pcm::open(address, self.config)?;
            debug!(
                "{}: opened with {:?}, {} frames buffer",
                address.path().display(),
                self.config,
                pcm.buffer_frames()
            );
            self.pcm = Some(pcm);
        }
        Ok(self.pcm.as_mut())
    }
}

impl Device for SoundDevice {
    fn start(&mut self) -> io::Result<()> {
        self.pcm().map(|_| ())
    }

    fn transfer(&mut self, buf: &mut [u8]) -> io::Result<()> {
        let gain = self.levels.gain(self.io);
        let format = self.config.format;
        let io = self.io;
        if io == Io::Output {
            apply_gain(buf, format, gain);
        }
        match self.pcm()? {
            Some(pcm) if io == Io::Output => pcm.write(buf)?,
            Some(pcm) => pcm.read(buf)?,
            None => {
                // Null device: take as long as the hardware would.
                let frames = buf.len() / self.config.frame_bytes();
                thread::sleep(Duration::from_micros(
                    frames as u64 * 1_000_000 / u64::from(self.config.rate),
                ));
                if io == Io::Input {
                    buf.fill(0);
                }
            }
        }
        if io == Io::Input {
            apply_gain(buf, format, gain);
        }
        Ok(())
    }

    fn drain(&mut self) {
        if let Some(pcm) = &mut self.pcm {
            pcm.drain();
        }
    }

    fn flush(&mut self) {
        if let Some(pcm) = &mut self.pcm {
            pcm.drop_frames();
        }
    }

    fn standby(&mut self) {
        self.pcm = None;
    }

    fn latency_ms(&self) -> Option<i32> {
        let pcm = self.pcm.as_ref()?;
        let frames = pcm.delay_frames()?;
        i32::try_from(frames * 1000 / u64::from(self.config.rate)).ok()
    }
}

/// The data FMQ (bytes), seen from the HAL.
struct DataFmq(MessageQueue<i8>);

impl DataQueue for DataFmq {
    // The queue has no "available" query in Rust; a transaction of n elements succeeds
    // only if n are available and a failed attempt changes nothing, so the largest
    // possible amount is taken in halving steps.
    fn read_up_to(&mut self, buf: &mut [u8]) -> usize {
        let mut done = 0;
        let mut chunk = buf.len();
        while chunk > 0 && done < buf.len() {
            let n = chunk.min(buf.len() - done);
            match self.0.read_many(n) {
                Some(mut completion) => {
                    for byte in &mut buf[done..done + n] {
                        *byte = completion.read().expect("the transaction holds n bytes") as u8;
                    }
                    done += n;
                }
                None => chunk /= 2,
            }
        }
        done
    }

    fn write_up_to(&mut self, data: &[u8]) -> usize {
        let mut done = 0;
        let mut chunk = data.len();
        while chunk > 0 && done < data.len() {
            let n = chunk.min(data.len() - done);
            match self.0.write_many(n) {
                Some(mut completion) => {
                    for &byte in &data[done..done + n] {
                        completion.write(byte as i8).expect("the transaction has room for n bytes");
                    }
                    done += n;
                }
                None => chunk /= 2,
            }
        }
        done
    }
}

fn from_aidl(command: &AidlCommand) -> worker::Command {
    match *command {
        AidlCommand::HalReservedExit(cookie) => worker::Command::Exit(cookie),
        AidlCommand::GetStatus(_) => worker::Command::GetStatus,
        AidlCommand::Start(_) => worker::Command::Start,
        AidlCommand::Burst(bytes) => worker::Command::Burst(bytes),
        AidlCommand::Drain(mode) => worker::Command::Drain(mode.0),
        AidlCommand::Standby(_) => worker::Command::Standby,
        AidlCommand::Pause(_) => worker::Command::Pause,
        AidlCommand::Flush(_) => worker::Command::Flush,
        AidlCommand::FlushFromFrame(frame) => worker::Command::FlushFromFrame(frame),
    }
}

fn to_aidl(reply: worker::Reply) -> AidlReply {
    let state = match reply.state {
        worker::State::Standby => AidlState::STANDBY,
        worker::State::Idle => AidlState::IDLE,
        worker::State::Active => AidlState::ACTIVE,
        worker::State::Paused => AidlState::PAUSED,
        worker::State::Draining => AidlState::DRAINING,
        worker::State::DrainPaused => AidlState::DRAIN_PAUSED,
        worker::State::Error => AidlState::ERROR,
    };
    AidlReply {
        status: reply.status,
        fmqByteCount: reply.fmq_byte_count,
        observable: Position { frames: reply.frames, timeNs: reply.time_ns },
        hardware: Position { frames: worker::UNKNOWN, timeNs: worker::UNKNOWN },
        latencyMs: reply.latency_ms,
        xrunFrames: 0,
        state,
        flushFromPosition: 0,
    }
}

/// `ANDROID_PRIORITY_AUDIO`: the nice value of the framework's audio threads.
const AUDIO_THREAD_NICE: libc::c_int = -16;

fn serve_commands<D: Device, Q: DataQueue>(
    mut commands: MessageQueue<AidlCommand>,
    mut replies: MessageQueue<AidlReply>,
    mut worker: Worker<D, Q>,
    connected: &AtomicBool,
) {
    // SAFETY: setpriority with PRIO_PROCESS and who = 0 changes the calling thread only.
    if unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, AUDIO_THREAD_NICE) } != 0 {
        warn!("cannot raise the worker priority: {}", io::Error::last_os_error());
    }
    let mut command = [AidlCommand::default()];
    loop {
        if !commands.read_blocking(&mut command, None) {
            error!("reading a command from the FMQ failed");
            return;
        }
        match worker.handle(from_aidl(&command[0]), connected.load(Ordering::Relaxed)) {
            Step::Reply(reply) => {
                if !replies.write_blocking(&[to_aidl(reply)], None) {
                    error!("writing a reply to the FMQ failed");
                    return;
                }
            }
            Step::Ignore => {}
            Step::Exit => return,
        }
    }
}

/// Everything needed to start a stream.
pub struct StreamParams {
    pub settings: Settings,
    pub buffer_frames: i64,
    pub device: SoundDevice,
    pub channel_count: usize,
}

/// The worker thread and the HAL's own handle on the command queue, used to stop it.
struct Running {
    thread: JoinHandle<()>,
    exit_queue: MessageQueue<AidlCommand>,
    cookie: i32,
}

impl Running {
    fn stop(mut self) {
        if !self.exit_queue.write_blocking(
            &[AidlCommand::HalReservedExit(self.cookie)],
            Some(Duration::from_secs(1)),
        ) {
            error!("cannot send the exit command to the stream worker");
            return;
        }
        if self.thread.join().is_err() {
            error!("the stream worker panicked");
        }
    }
}

/// State shared by a stream's binder objects and its module.
pub struct StreamShared {
    io: Io,
    channel_count: usize,
    /// Whether a patch connects the stream to a device port.
    connected: Arc<AtomicBool>,
    /// Microphones (`MicrophoneInfo.id`) of the connected device ports.
    microphones: Mutex<Vec<String>>,
    /// `None` once closed.
    running: Mutex<Option<Running>>,
}

impl StreamShared {
    pub fn is_open(&self) -> bool {
        self.running.lock().expect("lock poisoned").is_some()
    }

    pub fn set_connected(&self, connected: bool, microphones: Vec<String>) {
        self.connected.store(connected, Ordering::Relaxed);
        *self.microphones.lock().expect("lock poisoned") = microphones;
    }

    fn close(&self) -> binder::Result<()> {
        let running = self.running.lock().expect("lock poisoned").take();
        match running {
            Some(running) => {
                running.stop();
                Ok(())
            }
            None => Err(illegal_state("stream is already closed")),
        }
    }

    fn check_open(&self) -> binder::Result<()> {
        if self.is_open() {
            Ok(())
        } else {
            Err(illegal_state("stream is closed"))
        }
    }
}

impl Drop for StreamShared {
    fn drop(&mut self) {
        if let Some(running) = self.running.get_mut().expect("lock poisoned").take() {
            warn!("stream released without close()");
            running.stop();
        }
    }
}

/// Starts the worker of a new stream; returns its shared state and the descriptor for
/// the client.
///
/// # Errors
///
/// The worker thread cannot be started.
pub fn start(params: StreamParams) -> io::Result<(Arc<StreamShared>, StreamDescriptor)> {
    let frame_bytes = params.settings.frame_bytes;
    let commands: MessageQueue<AidlCommand> = MessageQueue::new(1, true);
    let replies: MessageQueue<AidlReply> = MessageQueue::new(1, true);
    let data: MessageQueue<i8> = MessageQueue::new(params.settings.buffer_bytes, false);
    let command_desc = commands.dupe_desc();
    let descriptor = StreamDescriptor {
        command: commands.dupe_desc(),
        reply: replies.dupe_desc(),
        frameSizeBytes: i32::try_from(frame_bytes).expect("frame sizes are small"),
        bufferSizeFrames: params.buffer_frames,
        audio: AudioBuffer::Fmq(data.dupe_desc()),
    };
    let exit_queue = MessageQueue::from_desc(&command_desc, false);
    let cookie = params.settings.exit_cookie;
    let connected = Arc::new(AtomicBool::new(false));
    let worker = Worker::new(params.settings, params.device, DataFmq(data));
    let thread = {
        let connected = Arc::clone(&connected);
        thread::Builder::new()
            .name(match params.settings.io {
                Io::Output => "writer".into(),
                Io::Input => "reader".into(),
            })
            .spawn(move || serve_commands(commands, replies, worker, &connected))?
    };
    let shared = StreamShared {
        io: params.settings.io,
        channel_count: params.channel_count,
        connected,
        microphones: Mutex::new(Vec::new()),
        running: Mutex::new(Some(Running { thread, exit_queue, cookie })),
    };
    Ok((Arc::new(shared), descriptor))
}

/// `IStreamCommon` of a stream.
struct StreamCommon(Arc<StreamShared>);

impl Interface for StreamCommon {}

impl IStreamCommon for StreamCommon {
    fn close(&self) -> binder::Result<()> {
        debug!("{:?} stream: close", self.0.io);
        self.0.close()
    }

    fn prepareToClose(&self) -> binder::Result<()> {
        self.0.check_open()
    }

    fn updateHwAvSyncId(&self, _id: i32) -> binder::Result<()> {
        Err(unsupported())
    }

    fn getVendorParameters(&self, _ids: &[String]) -> binder::Result<Vec<VendorParameter>> {
        Err(unsupported())
    }

    fn setVendorParameters(
        &self,
        _parameters: &[VendorParameter],
        _async: bool,
    ) -> binder::Result<()> {
        Err(unsupported())
    }

    fn addEffect(&self, _effect: &Strong<dyn IEffect>) -> binder::Result<()> {
        Err(unsupported())
    }

    fn removeEffect(&self, _effect: &Strong<dyn IEffect>) -> binder::Result<()> {
        Err(unsupported())
    }

    fn createMmapBuffer(&self) -> binder::Result<MmapBufferDescriptor> {
        Err(unsupported())
    }
}

fn common_binder(shared: &Arc<StreamShared>) -> Strong<dyn IStreamCommon> {
    BnStreamCommon::new_binder(StreamCommon(Arc::clone(shared)), BinderFeatures::default())
}

fn check_tags<'a>(tags: impl IntoIterator<Item = &'a String>) -> binder::Result<()> {
    match tags.into_iter().find(|tag| !is_vendor_extension(tag)) {
        Some(tag) => {
            Err(illegal_argument(&format!("metadata tag {tag:?} is not a vendor extension")))
        }
        None => Ok(()),
    }
}

fn check_codecs(metadata: &SourceMetadata) -> binder::Result<()> {
    match metadata.tracks.iter().find(|t| !is_valid_codec_provenance(t.codecProvenance.as_deref()))
    {
        Some(track) => {
            Err(illegal_argument(&format!("invalid codec provenance {:?}", track.codecProvenance)))
        }
        None => Ok(()),
    }
}

/// Checks the codecs and tags of playback metadata.
pub fn check_source_metadata(metadata: &SourceMetadata) -> binder::Result<()> {
    check_codecs(metadata)?;
    check_tags(metadata.tracks.iter().flat_map(|t| &t.tags))
}

/// Checks the tags of recording metadata.
pub fn check_sink_metadata(metadata: &SinkMetadata) -> binder::Result<()> {
    check_tags(metadata.tracks.iter().flat_map(|t| &t.tags))
}

/// `IStreamOut` of a playback stream.
pub struct StreamOut {
    shared: Arc<StreamShared>,
    common: Strong<dyn IStreamCommon>,
}

impl StreamOut {
    pub fn new_binder(shared: &Arc<StreamShared>) -> Strong<dyn IStreamOut> {
        let stream = Self { shared: Arc::clone(shared), common: common_binder(shared) };
        BnStreamOut::new_binder(stream, BinderFeatures::default())
    }
}

impl Interface for StreamOut {}

impl IStreamOut for StreamOut {
    fn getStreamCommon(&self) -> binder::Result<Strong<dyn IStreamCommon>> {
        Ok(self.common.clone())
    }

    fn updateMetadata(&self, metadata: &SourceMetadata) -> binder::Result<()> {
        // Invalid codecs are rejected even on a closed stream (as AOSP's default HAL does).
        check_codecs(metadata)?;
        self.shared.check_open()?;
        check_tags(metadata.tracks.iter().flat_map(|t| &t.tags))
    }

    fn updateOffloadMetadata(&self, _metadata: &AudioOffloadMetadata) -> binder::Result<()> {
        self.shared.check_open()?;
        Err(unsupported())
    }

    fn getHwVolume(&self) -> binder::Result<Vec<f32>> {
        Err(unsupported())
    }

    fn setHwVolume(&self, _volumes: &[f32]) -> binder::Result<()> {
        Err(unsupported())
    }

    fn getAudioDescriptionMixLevel(&self) -> binder::Result<f32> {
        Err(unsupported())
    }

    fn setAudioDescriptionMixLevel(&self, _level_db: f32) -> binder::Result<()> {
        Err(unsupported())
    }

    fn getDualMonoMode(&self) -> binder::Result<AudioDualMonoMode> {
        Err(unsupported())
    }

    fn setDualMonoMode(&self, _mode: AudioDualMonoMode) -> binder::Result<()> {
        Err(unsupported())
    }

    fn getRecommendedLatencyModes(&self) -> binder::Result<Vec<AudioLatencyMode>> {
        Err(unsupported())
    }

    fn setLatencyMode(&self, _mode: AudioLatencyMode) -> binder::Result<()> {
        Err(unsupported())
    }

    fn getPlaybackRateParameters(&self) -> binder::Result<AudioPlaybackRate> {
        Err(unsupported())
    }

    fn setPlaybackRateParameters(&self, _rate: &AudioPlaybackRate) -> binder::Result<()> {
        Err(unsupported())
    }

    fn selectPresentation(&self, _presentation_id: i32, _program_id: i32) -> binder::Result<()> {
        Err(unsupported())
    }
}

/// `IStreamIn` of a capture stream.
pub struct StreamIn {
    shared: Arc<StreamShared>,
    common: Strong<dyn IStreamCommon>,
}

impl StreamIn {
    pub fn new_binder(shared: &Arc<StreamShared>) -> Strong<dyn IStreamIn> {
        let stream = Self { shared: Arc::clone(shared), common: common_binder(shared) };
        BnStreamIn::new_binder(stream, BinderFeatures::default())
    }
}

impl Interface for StreamIn {}

impl IStreamIn for StreamIn {
    fn getStreamCommon(&self) -> binder::Result<Strong<dyn IStreamCommon>> {
        Ok(self.common.clone())
    }

    fn getActiveMicrophones(&self) -> binder::Result<Vec<MicrophoneDynamicInfo>> {
        let microphones = self.shared.microphones.lock().expect("lock poisoned");
        Ok(microphones
            .iter()
            .map(|id| MicrophoneDynamicInfo {
                id: id.clone(),
                channelMapping: vec![ChannelMapping::DIRECT; self.shared.channel_count],
            })
            .collect())
    }

    fn getMicrophoneDirection(&self) -> binder::Result<MicrophoneDirection> {
        Err(unsupported())
    }

    fn setMicrophoneDirection(&self, _direction: MicrophoneDirection) -> binder::Result<()> {
        Err(unsupported())
    }

    fn getMicrophoneFieldDimension(&self) -> binder::Result<f32> {
        Err(unsupported())
    }

    fn setMicrophoneFieldDimension(&self, _zoom: f32) -> binder::Result<()> {
        Err(unsupported())
    }

    fn updateMetadata(&self, metadata: &SinkMetadata) -> binder::Result<()> {
        self.shared.check_open()?;
        check_sink_metadata(metadata)
    }

    fn getHwGain(&self) -> binder::Result<Vec<f32>> {
        Err(unsupported())
    }

    fn setHwGain(&self, _gains: &[f32]) -> binder::Result<()> {
        Err(unsupported())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gain_scales_16_bit_samples() {
        let mut data = [0x00, 0x40, 0x00, 0xc0]; // 16384, -16384
        apply_gain(&mut data, pcm::Format::S16Le, 0.5);
        assert_eq!(data, [0x00, 0x20, 0x00, 0xe0]);
        apply_gain(&mut data, pcm::Format::S16Le, 0.0);
        assert_eq!(data, [0; 4]);
    }

    #[test]
    fn levels_mute() {
        let levels = Levels::default();
        assert_eq!(levels.gain(Io::Output), 1.0);
        levels.set_master_volume(0.25);
        assert_eq!(levels.gain(Io::Output), 0.25);
        levels.set_master_mute(true);
        assert_eq!(levels.gain(Io::Output), 0.0);
        levels.set_mic_mute(true);
        assert_eq!(levels.gain(Io::Input), 0.0);
    }
}
