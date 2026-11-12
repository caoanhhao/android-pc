//! `ICameraDeviceSession`: stream configuration, capture requests and the capture thread.
//!
//! Binder calls validate requests, import new buffers and queue the requests. One worker
//! thread per session owns the V4L2 stream: for each request it takes the next frame,
//! converts it to YUV 4:2:0 once, fills every output buffer of the request from it, and
//! sends the shutter notification and the capture result.

use std::collections::{HashMap, VecDeque};
use std::os::fd::{AsFd, OwnedFd};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

use android_hardware_camera_common::aidl::android::hardware::camera::common::Status::Status;
use android_hardware_camera_device::aidl::android::hardware::camera::device::{
    BufferCache::BufferCache, BufferStatus::BufferStatus, CameraBlobId::CameraBlobId,
    CameraMetadata::CameraMetadata, CameraOfflineSessionInfo::CameraOfflineSessionInfo,
    CaptureRequest::CaptureRequest, CaptureResult::CaptureResult,
    ConfigureStreamsRet::ConfigureStreamsRet, ErrorCode::ErrorCode, ErrorMsg::ErrorMsg,
    HalStream::HalStream, ICameraDeviceCallback::ICameraDeviceCallback,
    ICameraDeviceSession::ICameraDeviceSession, ICameraOfflineSession::ICameraOfflineSession,
    NotifyMsg::NotifyMsg, RequestTemplate::RequestTemplate, ShutterMsg::ShutterMsg,
    Stream::Stream as AidlStream, StreamBuffer::StreamBuffer,
    StreamConfiguration::StreamConfiguration, StreamConfigurationMode::StreamConfigurationMode,
    StreamRotation::StreamRotation, StreamType::StreamType,
};
use android_hardware_common::aidl::android::hardware::common::NativeHandle::NativeHandle;
use android_hardware_common_fmq::aidl::android::hardware::common::fmq::{
    MQDescriptor::MQDescriptor, SynchronizedReadWrite::SynchronizedReadWrite,
};
use android_hardware_graphics_common::aidl::android::hardware::graphics::common::{
    BufferUsage::BufferUsage, Dataspace::Dataspace, PixelFormat::PixelFormat,
};
use binder::{Interface, ParcelFileDescriptor, Strong};
use log::{error, info, warn};
use pccamera::image::{Size, Yuv420};
use pccamera::jpeg;
use pccamera::metadata::Metadata;
use pccamera::sensor::{
    self, OutputFormat, Sensor, Template, JPEG_BLOB_TRAILER_SIZE, MAX_PROCESSED_STREAMS,
    MAX_STALL_STREAMS, PIPELINE_DEPTH,
};
use pccamera::v4l2::{self, FourCc, Interval, Mode};

use crate::camera::Camera;
use crate::gralloc::Buffer;
use crate::service_error;

/// How long one wait for a frame may take before trying again.
const FRAME_TIMEOUT: Duration = Duration::from_secs(1);
/// Frames to try (timeouts and undecodable frames) before failing a request.
const FRAME_ATTEMPTS: usize = 4;
/// V4L2 buffers to stream with.
const V4L2_BUFFERS: u32 = 4;

/// A configured output stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Output {
    size: Size,
    format: OutputFormat,
    /// `Stream::bufferSize` for JPEG streams: where the `CameraBlob` trailer goes.
    buffer_size: usize,
}

/// One output buffer of a queued request.
#[derive(Debug)]
struct OutputBuffer {
    stream_id: i32,
    buffer_id: i64,
    output: Output,
    buffer: Arc<Buffer>,
    acquire_fence: Option<OwnedFd>,
}

/// A validated request waiting for the worker.
#[derive(Debug)]
struct Request {
    frame_number: i32,
    settings: Metadata,
    outputs: Vec<OutputBuffer>,
}

/// What the worker should capture.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Capture {
    mode: Mode,
    interval: Interval,
}

#[derive(Debug, Default)]
struct State {
    streams: HashMap<i32, Output>,
    /// Imported buffers by (stream id, buffer id); the framework sends a handle only the
    /// first time it uses a buffer.
    buffers: HashMap<(i32, i64), Arc<Buffer>>,
    queue: VecDeque<Request>,
    /// Requests queued or being captured.
    in_flight: usize,
    /// The latest settings: a request without settings repeats them.
    settings: Option<Metadata>,
    capture: Option<Capture>,
    flushing: bool,
    closed: bool,
}

#[derive(Debug)]
struct Shared {
    camera: Arc<Camera>,
    callback: Strong<dyn ICameraDeviceCallback>,
    state: Mutex<State>,
    /// Signalled when the worker has something to do.
    work: Condvar,
    /// Signalled when a request completes.
    done: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().expect("session state lock poisoned")
    }
}

/// An open camera session.
#[derive(Debug)]
pub struct Session {
    shared: Arc<Shared>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl Interface for Session {}

impl Session {
    /// Opens the camera's V4L2 node and starts the capture thread.
    ///
    /// # Errors
    ///
    /// Returns `CAMERA_DISCONNECTED` if the node cannot be opened.
    pub fn open(
        camera: Arc<Camera>,
        callback: Strong<dyn ICameraDeviceCallback>,
    ) -> binder::Result<Self> {
        let device = v4l2::Device::open(camera.path()).map_err(|err| {
            error!("{}: {err}", camera.path().display());
            service_error(Status::CAMERA_DISCONNECTED)
        })?;
        let shared = Arc::new(Shared {
            camera,
            callback,
            state: Mutex::default(),
            work: Condvar::new(),
            done: Condvar::new(),
        });
        let worker = Worker::new(Arc::clone(&shared), device);
        let handle = std::thread::Builder::new()
            .name("camera-capture".into())
            .spawn(move || worker.run())
            .map_err(|err| {
                error!("cannot start the capture thread: {err}");
                service_error(Status::INTERNAL_ERROR)
            })?;
        info!("{}: session open", shared.camera.id());
        Ok(Self { shared, worker: Mutex::new(Some(handle)) })
    }

    fn configure(&self, config: &StreamConfiguration) -> binder::Result<Vec<HalStream>> {
        let outputs = validate(self.shared.camera.sensor(), config)?;
        let sensor = self.shared.camera.sensor();
        let mut state = self.shared.lock();
        if state.closed {
            return Err(service_error(Status::INTERNAL_ERROR));
        }
        if state.in_flight > 0 {
            error!("configureStreams with {} requests in flight", state.in_flight);
            return Err(service_error(Status::INTERNAL_ERROR));
        }
        let streams: HashMap<i32, Output> = config
            .streams
            .iter()
            .zip(&outputs)
            .map(|(stream, output)| (stream.id, *output))
            .collect();
        state.buffers.retain(|(stream_id, _), _| streams.contains_key(stream_id));
        state.streams = streams;
        let sizes: Vec<Size> = outputs.iter().map(|output| output.size).collect();
        let mode = sensor.mode_for(&sizes).clone();
        let interval =
            Sensor::interval_for(&mode, state.settings.as_ref().unwrap_or(&Metadata::new()));
        info!(
            "{}: {} streams from {:?} {}x{} at {:.1} fps",
            self.shared.camera.id(),
            sizes.len(),
            mode.fourcc,
            mode.size.width,
            mode.size.height,
            interval.fps()
        );
        state.capture = Some(Capture { mode, interval });
        drop(state);
        self.shared.work.notify_all();

        Ok(config
            .streams
            .iter()
            .map(|stream| HalStream {
                id: stream.id,
                overrideFormat: if stream.format == PixelFormat::IMPLEMENTATION_DEFINED {
                    PixelFormat::YCBCR_420_888
                } else {
                    stream.format
                },
                producerUsage: BufferUsage(
                    stream.usage.0 | BufferUsage::CPU_WRITE_OFTEN.0 | BufferUsage::CAMERA_OUTPUT.0,
                ),
                consumerUsage: BufferUsage(0),
                maxBuffers: i32::from(PIPELINE_DEPTH),
                overrideDataSpace: stream.dataSpace,
                ..Default::default()
            })
            .collect())
    }

    /// Validates one request and turns it into a queued [`Request`].
    fn prepare(state: &mut State, request: &CaptureRequest) -> Result<Request, Status> {
        if request.inputBuffer.streamId != -1 {
            error!("frame {}: reprocessing is not supported", request.frameNumber);
            return Err(Status::ILLEGAL_ARGUMENT);
        }
        if state.capture.is_none() || request.outputBuffers.is_empty() {
            error!("frame {}: no output buffers or no streams", request.frameNumber);
            return Err(Status::ILLEGAL_ARGUMENT);
        }
        if request.fmqSettingsSize > 0 {
            // getCaptureRequestMetadataQueue hands out no queue.
            error!("frame {}: settings in an FMQ", request.frameNumber);
            return Err(Status::ILLEGAL_ARGUMENT);
        }
        let settings = if request.settings.metadata.is_empty() {
            state.settings.clone().ok_or_else(|| {
                error!("frame {}: the first request has no settings", request.frameNumber);
                Status::ILLEGAL_ARGUMENT
            })?
        } else {
            let settings = Metadata::from_bytes(&request.settings.metadata).map_err(|err| {
                error!("frame {}: {err}", request.frameNumber);
                Status::ILLEGAL_ARGUMENT
            })?;
            state.settings = Some(settings.clone());
            settings
        };

        let mut outputs = Vec::with_capacity(request.outputBuffers.len());
        for stream_buffer in &request.outputBuffers {
            let key = (stream_buffer.streamId, stream_buffer.bufferId);
            let Some(output) = state.streams.get(&key.0).copied() else {
                error!("frame {}: unknown stream {}", request.frameNumber, key.0);
                return Err(Status::ILLEGAL_ARGUMENT);
            };
            let buffer = if stream_buffer.buffer.fds.is_empty() {
                state.buffers.get(&key).cloned().ok_or_else(|| {
                    error!("frame {}: buffer {key:?} was never sent", request.frameNumber);
                    Status::ILLEGAL_ARGUMENT
                })?
            } else {
                let buffer = Arc::new(Buffer::import(&stream_buffer.buffer).map_err(|err| {
                    error!("frame {}: {err}", request.frameNumber);
                    Status::ILLEGAL_ARGUMENT
                })?);
                state.buffers.insert(key, Arc::clone(&buffer));
                buffer
            };
            let acquire_fence = match stream_buffer.acquireFence.fds.first() {
                Some(fence) => Some(fence.as_ref().try_clone().map_err(|err| {
                    error!("frame {}: acquire fence: {err}", request.frameNumber);
                    Status::INTERNAL_ERROR
                })?),
                None => None,
            };
            outputs.push(OutputBuffer {
                stream_id: key.0,
                buffer_id: key.1,
                output,
                buffer,
                acquire_fence,
            });
        }
        Ok(Request { frame_number: request.frameNumber, settings, outputs })
    }

    fn shutdown(&self) {
        {
            let mut state = self.shared.lock();
            if state.closed {
                return;
            }
            state.closed = true;
        }
        self.shared.work.notify_all();
        let worker = self.worker.lock().expect("worker handle lock poisoned").take();
        if let Some(worker) = worker {
            if worker.join().is_err() {
                error!("the capture thread panicked");
            }
        }
        self.shared.lock().buffers.clear();
        self.shared.camera.end_session();
        info!("{}: session closed", self.shared.camera.id());
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Checks a stream configuration and describes each stream.
///
/// # Errors
///
/// Returns `ILLEGAL_ARGUMENT` for anything a webcam cannot produce.
pub fn validate(
    sensor: &Sensor,
    config: &StreamConfiguration,
) -> Result<Vec<Output>, binder::Status> {
    let illegal = |why: String| {
        warn!("unsupported stream configuration: {why}");
        service_error(Status::ILLEGAL_ARGUMENT)
    };
    if config.operationMode != StreamConfigurationMode::NORMAL_MODE {
        return Err(illegal(format!("operation mode {:?}", config.operationMode)));
    }
    if config.streams.is_empty() {
        return Err(illegal("no streams".into()));
    }
    let outputs = config
        .streams
        .iter()
        .map(|stream| output_for(sensor, stream).map_err(&illegal))
        .collect::<Result<Vec<Output>, _>>()?;
    let jpegs = outputs.iter().filter(|output| output.format == OutputFormat::Jpeg).count();
    let yuvs = outputs.len() - jpegs;
    if jpegs > MAX_STALL_STREAMS as usize || yuvs > MAX_PROCESSED_STREAMS as usize {
        return Err(illegal(format!("{yuvs} YUV and {jpegs} JPEG streams")));
    }
    Ok(outputs)
}

fn output_for(sensor: &Sensor, stream: &AidlStream) -> Result<Output, String> {
    if stream.streamType != StreamType::OUTPUT {
        return Err(format!("stream {} is an input", stream.id));
    }
    if stream.rotation != StreamRotation::ROTATION_0 {
        return Err(format!("stream {} is rotated", stream.id));
    }
    if stream.useCase.0 != 0 {
        return Err(format!("stream {} has use case {}", stream.id, stream.useCase.0));
    }
    let size = match (u32::try_from(stream.width), u32::try_from(stream.height)) {
        (Ok(width), Ok(height)) => Size { width, height },
        _ => return Err(format!("stream {} is {}x{}", stream.id, stream.width, stream.height)),
    };
    if !sensor.supports(size) {
        return Err(format!("{}x{} is not an output size", size.width, size.height));
    }
    let format = match stream.format {
        PixelFormat::BLOB if stream.dataSpace == Dataspace::JFIF => OutputFormat::Jpeg,
        PixelFormat::YCBCR_420_888 | PixelFormat::IMPLEMENTATION_DEFINED => OutputFormat::Ycbcr420,
        other => return Err(format!("format {other:?} with data space {:?}", stream.dataSpace)),
    };
    let buffer_size = usize::try_from(stream.bufferSize).unwrap_or(0);
    Ok(Output { size, format, buffer_size })
}

/// The default settings for a `RequestTemplate`.
///
/// # Errors
///
/// Returns `ILLEGAL_ARGUMENT` for MANUAL, ZERO_SHUTTER_LAG and vendor templates.
pub fn default_settings(
    sensor: &Sensor,
    template: RequestTemplate,
) -> binder::Result<CameraMetadata> {
    let template =
        Template::from_aidl(template.0).ok_or_else(|| service_error(Status::ILLEGAL_ARGUMENT))?;
    Ok(CameraMetadata { metadata: sensor.default_request(template).to_bytes() })
}

impl ICameraDeviceSession for Session {
    fn close(&self) -> binder::Result<()> {
        self.shutdown();
        Ok(())
    }

    fn configureStreams(&self, config: &StreamConfiguration) -> binder::Result<Vec<HalStream>> {
        self.configure(config)
    }

    fn configureStreamsV2(
        &self,
        config: &StreamConfiguration,
    ) -> binder::Result<ConfigureStreamsRet> {
        Ok(ConfigureStreamsRet { halStreams: self.configure(config)? })
    }

    fn constructDefaultRequestSettings(
        &self,
        template: RequestTemplate,
    ) -> binder::Result<CameraMetadata> {
        default_settings(self.shared.camera.sensor(), template)
    }

    fn flush(&self) -> binder::Result<()> {
        let mut state = self.shared.lock();
        state.flushing = true;
        self.shared.work.notify_all();
        while state.in_flight > 0 && !state.closed {
            state = self.shared.done.wait(state).expect("session state lock poisoned");
        }
        state.flushing = false;
        Ok(())
    }

    fn getCaptureRequestMetadataQueue(
        &self,
    ) -> binder::Result<MQDescriptor<i8, SynchronizedReadWrite>> {
        // No queue: settings come inline in each request.
        Ok(MQDescriptor::default())
    }

    fn getCaptureResultMetadataQueue(
        &self,
    ) -> binder::Result<MQDescriptor<i8, SynchronizedReadWrite>> {
        // No queue: results carry their metadata inline.
        Ok(MQDescriptor::default())
    }

    fn isReconfigurationRequired(
        &self,
        old: &CameraMetadata,
        new: &CameraMetadata,
    ) -> binder::Result<bool> {
        Ok(old.metadata != new.metadata)
    }

    fn processCaptureRequest(
        &self,
        requests: &[CaptureRequest],
        caches_to_remove: &[BufferCache],
    ) -> binder::Result<i32> {
        let mut state = self.shared.lock();
        for cache in caches_to_remove {
            state.buffers.remove(&(cache.streamId, cache.bufferId));
        }
        if state.closed {
            return Err(service_error(Status::INTERNAL_ERROR));
        }
        let mut accepted = 0;
        for request in requests {
            let request = Self::prepare(&mut state, request).map_err(service_error)?;
            state.queue.push_back(request);
            state.in_flight += 1;
            accepted += 1;
            self.shared.work.notify_all();
        }
        Ok(accepted)
    }

    fn signalStreamFlush(
        &self,
        _stream_ids: &[i32],
        _stream_config_counter: i32,
    ) -> binder::Result<()> {
        // Buffers are only held while their request is captured; nothing to return.
        Ok(())
    }

    fn switchToOffline(
        &self,
        _streams_to_keep: &[i32],
        _info: &mut CameraOfflineSessionInfo,
    ) -> binder::Result<Strong<dyn ICameraOfflineSession>> {
        Err(service_error(Status::ILLEGAL_ARGUMENT))
    }

    fn repeatingRequestEnd(&self, _frame_number: i32, _stream_ids: &[i32]) -> binder::Result<()> {
        Ok(())
    }
}

/// The capture thread's state.
struct Worker {
    shared: Arc<Shared>,
    device: v4l2::Device,
    stream: Option<v4l2::Stream>,
    /// Whether an AF trigger "locked" focus (reported in results until cancelled).
    af_locked: bool,
}

/// Why a request produced no frame.
enum CaptureFailure {
    /// This request failed; later ones may work.
    Request(String),
    /// The camera is gone.
    Device(String),
}

impl Worker {
    fn new(shared: Arc<Shared>, device: v4l2::Device) -> Self {
        Self { shared, device, stream: None, af_locked: false }
    }

    fn run(mut self) {
        loop {
            let (request, capture, flushing) = {
                let mut state = self.shared.lock();
                loop {
                    if state.closed {
                        return;
                    }
                    if let Some(request) = state.queue.pop_front() {
                        break (Some(request), state.capture.clone(), state.flushing);
                    }
                    let idle_mode = state.capture.clone();
                    if idle_mode.is_some() && !self.streams(idle_mode.as_ref()) {
                        // Start streaming right after configureStreams: the first frames of a
                        // webcam take a while.
                        break (None, idle_mode, false);
                    }
                    state = self.shared.work.wait(state).expect("session state lock poisoned");
                }
            };

            let Some(capture) = capture else { continue };
            let Some(request) = request else {
                if let Err(err) = self.start(&capture) {
                    error!("{err}");
                    // Wait for a request (which will fail) instead of retrying in a loop.
                    let mut state = self.shared.lock();
                    if state.queue.is_empty() && !state.closed {
                        state = self.shared.work.wait(state).expect("session state lock poisoned");
                    }
                    drop(state);
                }
                continue;
            };

            if flushing {
                self.fail(request, ErrorCode::ERROR_REQUEST);
            } else {
                self.capture(request, &capture);
            }
            let mut state = self.shared.lock();
            state.in_flight -= 1;
            drop(state);
            self.shared.done.notify_all();
        }
    }

    /// Whether the running stream is `capture`'s mode.
    fn streams(&self, capture: Option<&Capture>) -> bool {
        match (&self.stream, capture) {
            (Some(stream), Some(capture)) => stream.mode() == &capture.mode,
            _ => false,
        }
    }

    fn start(&mut self, capture: &Capture) -> Result<(), String> {
        let wanted = |stream: &v4l2::Stream| {
            stream.mode() == &capture.mode && stream.interval() == capture.interval
        };
        if self.stream.as_ref().is_some_and(wanted) {
            return Ok(());
        }
        // Stop the old stream before the device accepts a new format.
        self.stream = None;
        let stream =
            v4l2::Stream::start(&self.device, &capture.mode, capture.interval, V4L2_BUFFERS)
                .map_err(|err| format!("{}: cannot stream: {err}", self.device.path().display()))?;
        self.stream = Some(stream);
        Ok(())
    }

    fn capture(&mut self, request: Request, capture: &Capture) {
        let capture = Capture {
            interval: Sensor::interval_for(&capture.mode, &request.settings),
            mode: capture.mode.clone(),
        };
        let (frame, timestamp) = match self
            .start(&capture)
            .map_err(CaptureFailure::Request)
            .and_then(|()| self.next_frame())
        {
            Ok(frame) => frame,
            Err(CaptureFailure::Request(why)) => {
                warn!("frame {}: {why}", request.frame_number);
                self.fail(request, ErrorCode::ERROR_REQUEST);
                return;
            }
            Err(CaptureFailure::Device(why)) => {
                error!("frame {}: {why}", request.frame_number);
                self.notify(NotifyMsg::Error(ErrorMsg {
                    frameNumber: 0,
                    errorStreamId: -1,
                    errorCode: ErrorCode::ERROR_DEVICE,
                }));
                self.stream = None;
                self.fail(request, ErrorCode::ERROR_REQUEST);
                return;
            }
        };

        self.notify(NotifyMsg::Shutter(ShutterMsg {
            frameNumber: request.frame_number,
            timestamp,
            readoutTimestamp: timestamp,
            ..Default::default()
        }));

        let mut buffers = Vec::with_capacity(request.outputs.len());
        for output in &request.outputs {
            let status = match self.fill(&frame, output, &request.settings) {
                Ok(()) => BufferStatus::OK,
                Err(why) => {
                    warn!("frame {} stream {}: {why}", request.frame_number, output.stream_id);
                    self.notify(NotifyMsg::Error(ErrorMsg {
                        frameNumber: request.frame_number,
                        errorStreamId: output.stream_id,
                        errorCode: ErrorCode::ERROR_BUFFER,
                    }));
                    BufferStatus::ERROR
                }
            };
            buffers.push(returned(output, status, None));
        }

        self.af_locked = sensor::af_locked_after(&request.settings, self.af_locked);
        let result =
            self.shared.camera.sensor().result(&request.settings, timestamp, self.af_locked);
        self.send(CaptureResult {
            frameNumber: request.frame_number,
            result: CameraMetadata { metadata: result.to_bytes() },
            outputBuffers: buffers,
            inputBuffer: no_input(),
            partialResult: 1,
            ..Default::default()
        });
    }

    /// The next frame as YUV 4:2:0 with its capture time (`CLOCK_MONOTONIC`).
    fn next_frame(&mut self) -> Result<(Yuv420, i64), CaptureFailure> {
        let stream =
            self.stream.as_mut().ok_or_else(|| CaptureFailure::Request("not streaming".into()))?;
        let mode = stream.mode().clone();
        let mut last_problem = String::from("no frame");
        for _ in 0..FRAME_ATTEMPTS {
            let frame = match stream.next_frame(FRAME_TIMEOUT) {
                Ok(Some(frame)) => frame,
                Ok(None) => {
                    last_problem = format!("no frame within {FRAME_TIMEOUT:?}");
                    continue;
                }
                Err(err) => return Err(CaptureFailure::Device(format!("capture: {err}"))),
            };
            let timestamp = monotonic_now();
            let converted = if mode.fourcc == FourCc::MJPEG {
                jpeg::decode(frame.data(), mode.size).map_err(|err| err.to_string())
            } else {
                Yuv420::from_yuyv(frame.data(), mode.size).map_err(|err| err.to_string())
            };
            match converted {
                Ok(image) => return Ok((image, timestamp)),
                // Webcams send a broken frame now and then (often the first one).
                Err(why) => last_problem = why,
            }
        }
        Err(CaptureFailure::Request(last_problem))
    }

    /// Writes the frame into one output buffer.
    fn fill(
        &mut self,
        frame: &Yuv420,
        output: &OutputBuffer,
        settings: &Metadata,
    ) -> Result<(), String> {
        let fence = output.acquire_fence.as_ref().map(AsFd::as_fd);
        let Output { size, format, buffer_size } = output.output;
        match format {
            OutputFormat::Ycbcr420 => {
                let mut locked = output
                    .buffer
                    .lock(size.width, size.height, fence)
                    .map_err(|err| err.to_string())?;
                let layout = locked.ycbcr_layout().ok_or("buffer is not 8-bit YCbCr")?;
                frame.write_to(locked.bytes_mut(), &layout, size).map_err(|err| err.to_string())
            }
            OutputFormat::Jpeg => {
                let jpeg = jpeg::encode(&frame.scaled(size), sensor::jpeg_quality(settings))
                    .map_err(|err| err.to_string())?;
                let width = u32::try_from(buffer_size).unwrap_or(u32::MAX);
                let mut locked =
                    output.buffer.lock(width, 1, fence).map_err(|err| err.to_string())?;
                write_jpeg_blob(locked.bytes_mut(), buffer_size, &jpeg)
            }
        }
    }

    /// Fails a request: ERROR_REQUEST (or the given code), every buffer back with ERROR.
    fn fail(&self, request: Request, code: ErrorCode) {
        self.notify(NotifyMsg::Error(ErrorMsg {
            frameNumber: request.frame_number,
            errorStreamId: -1,
            errorCode: code,
        }));
        let buffers = request
            .outputs
            .into_iter()
            .map(|mut output| {
                let fence = output.acquire_fence.take();
                returned(&output, BufferStatus::ERROR, fence)
            })
            .collect();
        self.send(CaptureResult {
            frameNumber: request.frame_number,
            outputBuffers: buffers,
            inputBuffer: no_input(),
            partialResult: 0,
            ..Default::default()
        });
    }

    fn notify(&self, message: NotifyMsg) {
        if let Err(err) = self.shared.callback.notify(&[message]) {
            warn!("notify: {err}");
        }
    }

    fn send(&self, result: CaptureResult) {
        if let Err(err) = self.shared.callback.processCaptureResult(&[result]) {
            warn!("processCaptureResult: {err}");
        }
    }
}

/// Writes `jpeg` at the start of a BLOB buffer and the `CameraBlob` trailer in the last
/// bytes of its `buffer_size` (the whole mapping if the stream gave no size).
fn write_jpeg_blob(bytes: &mut [u8], buffer_size: usize, jpeg: &[u8]) -> Result<(), String> {
    let size = if buffer_size == 0 { bytes.len() } else { buffer_size.min(bytes.len()) };
    if jpeg.len() + JPEG_BLOB_TRAILER_SIZE > size {
        return Err(format!("JPEG of {} bytes does not fit {size}", jpeg.len()));
    }
    bytes[..jpeg.len()].copy_from_slice(jpeg);
    let trailer = &mut bytes[size - JPEG_BLOB_TRAILER_SIZE..size];
    trailer[..4].copy_from_slice(&CameraBlobId::JPEG.0.to_ne_bytes());
    let len = i32::try_from(jpeg.len()).map_err(|err| err.to_string())?;
    trailer[4..].copy_from_slice(&len.to_ne_bytes());
    Ok(())
}

/// The `StreamBuffer` returning `output` to the framework; a failed buffer carries its
/// unwaited acquire fence back as the release fence.
fn returned(output: &OutputBuffer, status: BufferStatus, fence: Option<OwnedFd>) -> StreamBuffer {
    StreamBuffer {
        streamId: output.stream_id,
        bufferId: output.buffer_id,
        status,
        releaseFence: NativeHandle {
            fds: fence.map(ParcelFileDescriptor::new).into_iter().collect(),
            ints: Vec::new(),
        },
        ..Default::default()
    }
}

fn no_input() -> StreamBuffer {
    StreamBuffer { streamId: -1, status: BufferStatus::ERROR, ..Default::default() }
}

fn monotonic_now() -> i64 {
    let mut now = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: `now` is a valid out-pointer; CLOCK_MONOTONIC always exists.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) };
    now.tv_sec * 1_000_000_000 + now.tv_nsec
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jpeg_blob_has_trailer_at_buffer_end() {
        let mut bytes = vec![0u8; 64];
        write_jpeg_blob(&mut bytes, 32, &[0xff, 0xd8, 0xff, 0xd9]).unwrap();
        assert_eq!(&bytes[..4], &[0xff, 0xd8, 0xff, 0xd9]);
        assert_eq!(&bytes[24..28], &0x00ff_i32.to_ne_bytes());
        assert_eq!(&bytes[28..32], &4_i32.to_ne_bytes());
        assert!(write_jpeg_blob(&mut bytes, 10, &[0; 4]).is_err());
    }
}
