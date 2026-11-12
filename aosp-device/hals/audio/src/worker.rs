//! The stream state machine of `StreamDescriptor` (non-MMap, synchronous I/O).
//!
//! The client writes one command into the command FMQ and waits for one reply; the worker
//! thread applies the command to [`Worker`], which moves data between the data FMQ
//! ([`DataQueue`]) and the device ([`Device`]) and returns the reply. The state diagrams
//! are in `hardware/interfaces/audio/aidl/android/hardware/audio/core/stream-*-sm.gv`;
//! this follows AOSP's default implementation (`Stream.cpp`) for streams without an
//! async callback, MMap or offload.
//!
//! No AIDL types here, so the whole state machine is unit tested on the host.

use std::io;
use std::thread;
use std::time::{Duration, Instant};

/// `StreamDescriptor.State`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Standby,
    Idle,
    Active,
    Paused,
    Draining,
    DrainPaused,
    Error,
}

/// `StreamDescriptor.DrainMode` values.
pub const DRAIN_UNSPECIFIED: i8 = 0;
pub const DRAIN_ALL: i8 = 1;
pub const DRAIN_EARLY_NOTIFY: i8 = 2;

/// `StreamDescriptor.Command`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    /// `halReservedExit`: the HAL's own request to stop the worker, carrying its cookie.
    Exit(i32),
    GetStatus,
    Start,
    Burst(i32),
    Drain(i8),
    Standby,
    Pause,
    Flush,
    FlushFromFrame(i32),
}

/// Reply status codes (`status_t`).
pub const STATUS_OK: i32 = 0;
pub const STATUS_BAD_VALUE: i32 = -libc::EINVAL;
pub const STATUS_INVALID_OPERATION: i32 = -libc::ENOSYS;
pub const STATUS_NOT_ENOUGH_DATA: i32 = -libc::ENODATA;

/// `StreamDescriptor.Position.UNKNOWN` and `LATENCY_UNKNOWN`.
pub const UNKNOWN: i64 = -1;

/// `StreamDescriptor.Reply` without the AIDL type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reply {
    pub status: i32,
    pub fmq_byte_count: i32,
    /// Frames transferred and when (`observable`), or [`UNKNOWN`].
    pub frames: i64,
    pub time_ns: i64,
    pub latency_ms: i32,
    pub state: State,
}

/// Which side of the data FMQ the worker is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Io {
    /// Playback: the client fills the data queue, the worker plays it.
    Output,
    /// Capture: the worker fills the data queue, the client drains it.
    Input,
}

/// The data FMQ, seen from the HAL.
pub trait DataQueue {
    /// Output: moves up to `buf.len()` bytes out of the queue, returns how many.
    fn read_up_to(&mut self, buf: &mut [u8]) -> usize;
    /// Input: moves up to `data.len()` bytes into the queue, returns how many.
    fn write_up_to(&mut self, data: &[u8]) -> usize;
}

/// The sound device behind a stream.
pub trait Device {
    /// Leaves standby (opens the device if needed).
    fn start(&mut self) -> io::Result<()>;
    /// Plays (output) or captures into (input) `buf`, whole frames, blocking.
    fn transfer(&mut self, buf: &mut [u8]) -> io::Result<()>;
    /// Plays out what is queued (output only).
    fn drain(&mut self);
    /// Drops what is queued.
    fn flush(&mut self);
    /// Enters standby (closes the device).
    fn standby(&mut self);
    /// Latency of the last transfer, or `None` if unknown.
    fn latency_ms(&self) -> Option<i32>;
}

/// Settings of one stream, fixed when it is opened.
#[derive(Clone, Copy, Debug)]
pub struct Settings {
    pub io: Io,
    pub frame_bytes: usize,
    /// Size of the data FMQ in bytes.
    pub buffer_bytes: usize,
    /// Reported when the device does not know its latency.
    pub nominal_latency_ms: i32,
    /// `ModuleDebug.streamTransientStateDelayMs`: minimum time in DRAINING.
    pub transient_state_delay: Duration,
    /// The cookie of [`Command::Exit`] the worker accepts.
    pub exit_cookie: i32,
}

/// What the worker thread does after a command.
#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    /// Send this reply and wait for the next command.
    Reply(Reply),
    /// Wait for the next command without replying (a foreign exit cookie).
    Ignore,
    /// Stop the worker (the HAL's own exit command).
    Exit,
}

/// Simulated transfer time when the stream is not connected to any device.
const DISCONNECTED_TRANSFER: Duration = Duration::from_millis(3);

/// The state machine of one stream, owned by its worker thread.
pub struct Worker<D, Q> {
    settings: Settings,
    device: D,
    data: Q,
    buffer: Vec<u8>,
    state: State,
    frames: i64,
    transient_since: Instant,
}

impl<D: Device, Q: DataQueue> Worker<D, Q> {
    pub fn new(settings: Settings, device: D, data: Q) -> Self {
        Self {
            buffer: vec![0; settings.buffer_bytes],
            settings,
            device,
            data,
            state: State::Standby,
            frames: 0,
            transient_since: Instant::now(),
        }
    }

    /// Applies `command`. `connected` says whether a patch connects the stream to a
    /// device; without one, transfers are simulated (silence, discarded output), as the
    /// framework expects of a stream that is open but not routed.
    pub fn handle(&mut self, command: Command, connected: bool) -> Step {
        self.leave_draining_after_delay();
        let mut reply = Reply {
            status: STATUS_BAD_VALUE,
            fmq_byte_count: 0,
            frames: UNKNOWN,
            time_ns: UNKNOWN,
            latency_ms: 0,
            state: self.state,
        };
        match command {
            Command::Exit(cookie) => {
                if cookie == self.settings.exit_cookie {
                    self.device.standby();
                    return Step::Exit;
                }
                if cookie != 0 {
                    log::warn!("exit command with a foreign cookie {cookie}");
                    return Step::Ignore;
                }
                // A client's halReservedExit(0) is answered with BAD_VALUE.
            }
            Command::GetStatus => self.ok(&mut reply, connected),
            Command::Start => self.start(&mut reply, connected),
            Command::Burst(bytes) => match usize::try_from(bytes) {
                Ok(bytes) => self.burst(bytes, &mut reply, connected),
                Err(_) => log::warn!("burst with a negative byte count {bytes}"),
            },
            Command::Drain(mode) => self.drain(mode, &mut reply, connected),
            Command::Standby => {
                if self.state == State::Idle {
                    self.ok(&mut reply, connected);
                    self.device.standby();
                    self.state = State::Standby;
                } else {
                    self.wrong_state(&mut reply, command);
                }
            }
            Command::Pause => {
                let next = match (self.settings.io, self.state) {
                    (_, State::Active) => Some(State::Paused),
                    (Io::Output, State::Draining) => Some(State::DrainPaused),
                    _ => None,
                };
                match next {
                    Some(next) => {
                        self.ok(&mut reply, connected);
                        self.state = next;
                    }
                    None => self.wrong_state(&mut reply, command),
                }
            }
            Command::Flush => {
                let next = match (self.settings.io, self.state) {
                    (Io::Output, State::Paused | State::DrainPaused) => Some(State::Idle),
                    (Io::Input, State::Paused) => Some(State::Standby),
                    _ => None,
                };
                match next {
                    Some(next) => {
                        self.device.flush();
                        if next == State::Standby {
                            self.device.standby();
                        }
                        self.ok(&mut reply, connected);
                        self.state = next;
                    }
                    None => self.wrong_state(&mut reply, command),
                }
            }
            Command::FlushFromFrame(_) => {
                // Only PCM offload streams support it.
                log::warn!("flushFromFrame is not supported");
                reply.status = STATUS_INVALID_OPERATION;
            }
        }
        reply.state = self.state;
        Step::Reply(reply)
    }

    /// DRAINING ends on the next command once the transient delay has passed (there is no
    /// async callback to tell the client earlier).
    fn leave_draining_after_delay(&mut self) {
        if self.settings.io == Io::Output
            && self.state == State::Draining
            && self.transient_since.elapsed() >= self.settings.transient_state_delay
        {
            self.state = State::Idle;
        }
    }

    fn enter_transient(&mut self, state: State) {
        self.state = state;
        self.transient_since = Instant::now();
    }

    fn ok(&self, reply: &mut Reply, connected: bool) {
        reply.status = STATUS_OK;
        if connected {
            reply.frames = self.frames;
            reply.time_ns = monotonic_ns();
        }
    }

    fn wrong_state(&self, reply: &mut Reply, command: Command) {
        // Debug only: after a device error the framework retries bursts every few ms.
        log::debug!("{command:?} is not accepted in state {:?}", self.state);
        reply.status = STATUS_INVALID_OPERATION;
    }

    fn start(&mut self, reply: &mut Reply, connected: bool) {
        let next = match (self.settings.io, self.state) {
            (_, State::Standby) => State::Idle,
            (Io::Output, State::Paused) => State::Active,
            (Io::Output, State::DrainPaused) => State::Draining,
            (Io::Input, State::Draining) => State::Active,
            _ => return self.wrong_state(reply, Command::Start),
        };
        if connected {
            if let Err(err) = self.device.start() {
                log::error!("start failed: {err}");
                self.state = State::Error;
                return;
            }
        }
        self.ok(reply, connected);
        if next == State::Draining {
            self.enter_transient(next);
        } else {
            self.state = next;
        }
    }

    fn burst(&mut self, client_bytes: usize, reply: &mut Reply, connected: bool) {
        let accepted = match self.settings.io {
            Io::Output => self.state != State::Error,
            Io::Input => {
                matches!(self.state, State::Idle | State::Active | State::Paused | State::Draining)
            }
        };
        if !accepted {
            return self.wrong_state(reply, Command::Burst(client_bytes as i32));
        }
        let result = match self.settings.io {
            Io::Output => self.write(client_bytes, reply, connected),
            Io::Input => self.read(client_bytes, reply, connected),
        };
        if let Err(err) = result {
            log::error!("transfer failed: {err}");
            self.state = State::Error;
            return;
        }
        self.state = match (self.settings.io, self.state) {
            (Io::Output, State::Standby | State::Paused | State::DrainPaused) => State::Paused,
            (Io::Output, State::Idle | State::Active | State::Draining) => State::Active,
            (Io::Input, State::Idle | State::Paused) => State::Active,
            (Io::Input, State::Draining) => State::Standby,
            (_, state) => state,
        };
    }

    /// Output burst: consumes everything the client queued, plays whole frames of it.
    fn write(&mut self, client_bytes: usize, reply: &mut Reply, connected: bool) -> io::Result<()> {
        let queued = self.data.read_up_to(&mut self.buffer);
        let frame_bytes = self.settings.frame_bytes;
        let bytes = client_bytes.min(queued) / frame_bytes * frame_bytes;
        if bytes > 0 {
            if connected {
                if self.state == State::Standby {
                    // Priming a stream in standby opens the device.
                    self.device.start()?;
                }
                self.device.transfer(&mut self.buffer[..bytes])?;
            } else {
                thread::sleep(DISCONNECTED_TRANSFER);
            }
        }
        self.count(bytes, reply, connected);
        Ok(())
    }

    /// Input burst: captures what the client asked for and fits into the queue.
    fn read(&mut self, client_bytes: usize, reply: &mut Reply, connected: bool) -> io::Result<()> {
        let frame_bytes = self.settings.frame_bytes;
        let bytes = client_bytes.min(self.buffer.len()) / frame_bytes * frame_bytes;
        let buf = &mut self.buffer[..bytes];
        if connected {
            self.device.transfer(buf)?;
        } else {
            buf.fill(0);
            thread::sleep(DISCONNECTED_TRANSFER);
        }
        let written = self.data.write_up_to(buf);
        if written < bytes {
            log::warn!("data queue had room for {written} of {bytes} bytes");
            reply.status = STATUS_NOT_ENOUGH_DATA;
            return Ok(());
        }
        self.count(bytes, reply, connected);
        Ok(())
    }

    fn count(&mut self, bytes: usize, reply: &mut Reply, connected: bool) {
        reply.fmq_byte_count =
            i32::try_from(bytes).expect("burst sizes fit the i32 of the command");
        self.frames += (bytes / self.settings.frame_bytes) as i64;
        self.ok(reply, connected);
        reply.latency_ms = self
            .device
            .latency_ms()
            .filter(|_| connected)
            .unwrap_or(self.settings.nominal_latency_ms);
    }

    fn drain(&mut self, mode: i8, reply: &mut Reply, connected: bool) {
        let valid = match self.settings.io {
            Io::Output => mode == DRAIN_ALL || mode == DRAIN_EARLY_NOTIFY,
            Io::Input => mode == DRAIN_UNSPECIFIED,
        };
        if !valid {
            log::warn!("invalid drain mode {mode}");
            return;
        }
        if self.state != State::Active {
            return self.wrong_state(reply, Command::Drain(mode));
        }
        if connected {
            self.device.drain();
        }
        self.ok(reply, connected);
        self.enter_transient(State::Draining);
    }
}

/// `CLOCK_MONOTONIC` in nanoseconds (Android's `uptimeNanos`, which the framework uses for
/// stream positions).
pub fn monotonic_ns() -> i64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: `ts` is a valid timespec for the kernel to fill.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec * 1_000_000_000 + ts.tv_nsec
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    #[derive(Default)]
    struct FakeDevice {
        started: usize,
        played: Vec<u8>,
        standbys: usize,
        fail: bool,
    }

    impl Device for FakeDevice {
        fn start(&mut self) -> io::Result<()> {
            self.started += 1;
            Ok(())
        }
        fn transfer(&mut self, buf: &mut [u8]) -> io::Result<()> {
            if self.fail {
                return Err(io::Error::other("unplugged"));
            }
            self.played.extend_from_slice(buf);
            buf.fill(7);
            Ok(())
        }
        fn drain(&mut self) {}
        fn flush(&mut self) {}
        fn standby(&mut self) {
            self.standbys += 1;
        }
        fn latency_ms(&self) -> Option<i32> {
            Some(20)
        }
    }

    #[derive(Default)]
    struct FakeQueue {
        bytes: VecDeque<u8>,
        capacity: usize,
    }

    impl DataQueue for FakeQueue {
        fn read_up_to(&mut self, buf: &mut [u8]) -> usize {
            let n = buf.len().min(self.bytes.len());
            for b in &mut buf[..n] {
                *b = self.bytes.pop_front().expect("n <= len");
            }
            n
        }
        fn write_up_to(&mut self, data: &[u8]) -> usize {
            let n = data.len().min(self.capacity - self.bytes.len());
            self.bytes.extend(&data[..n]);
            n
        }
    }

    const COOKIE: i32 = 0x1234;

    fn worker(io: Io) -> Worker<FakeDevice, FakeQueue> {
        let settings = Settings {
            io,
            frame_bytes: 4,
            buffer_bytes: 64,
            nominal_latency_ms: 85,
            transient_state_delay: Duration::ZERO,
            exit_cookie: COOKIE,
        };
        Worker::new(
            settings,
            FakeDevice::default(),
            FakeQueue { capacity: 64, ..Default::default() },
        )
    }

    fn reply(step: Step) -> Reply {
        match step {
            Step::Reply(reply) => reply,
            other => panic!("expected a reply, got {other:?}"),
        }
    }

    #[test]
    fn output_plays_bursts() {
        let mut w = worker(Io::Output);
        assert_eq!(reply(w.handle(Command::Start, true)).state, State::Idle);
        w.data.bytes.extend([1u8; 10]);
        let r = reply(w.handle(Command::Burst(10), true));
        // Whole frames only; the partial frame is consumed from the queue all the same.
        assert_eq!(
            (r.status, r.fmq_byte_count, r.frames, r.state),
            (STATUS_OK, 8, 2, State::Active)
        );
        assert!(w.data.bytes.is_empty());
        assert_eq!(w.device.played.len(), 8);
        assert_eq!(r.latency_ms, 20);
    }

    #[test]
    fn output_drain_returns_to_idle() {
        let mut w = worker(Io::Output);
        w.handle(Command::Start, true);
        w.data.bytes.extend([0u8; 8]);
        w.handle(Command::Burst(8), true);
        assert_eq!(reply(w.handle(Command::Drain(DRAIN_ALL), true)).state, State::Draining);
        assert_eq!(reply(w.handle(Command::GetStatus, true)).state, State::Idle);
        assert_eq!(reply(w.handle(Command::Standby, true)).state, State::Standby);
        assert_eq!(w.device.standbys, 1);
    }

    #[test]
    fn output_pause_flush() {
        let mut w = worker(Io::Output);
        w.handle(Command::Start, true);
        w.data.bytes.extend([0u8; 8]);
        w.handle(Command::Burst(8), true);
        assert_eq!(reply(w.handle(Command::Pause, true)).state, State::Paused);
        assert_eq!(reply(w.handle(Command::Flush, true)).state, State::Idle);
        let r = reply(w.handle(Command::Flush, true));
        assert_eq!((r.status, r.state), (STATUS_INVALID_OPERATION, State::Idle));
    }

    #[test]
    fn output_burst_in_standby_primes_into_paused() {
        let mut w = worker(Io::Output);
        w.data.bytes.extend([0u8; 8]);
        assert_eq!(reply(w.handle(Command::Burst(8), true)).state, State::Paused);
        assert_eq!(w.device.started, 1);
        assert_eq!(reply(w.handle(Command::Start, true)).state, State::Active);
    }

    #[test]
    fn invalid_commands_keep_the_state() {
        let mut w = worker(Io::Output);
        w.handle(Command::Start, true);
        for command in
            [Command::Burst(-1), Command::Burst(i32::MIN), Command::Drain(DRAIN_UNSPECIFIED)]
        {
            let r = reply(w.handle(command, true));
            assert_eq!((r.status, r.state), (STATUS_BAD_VALUE, State::Idle), "{command:?}");
        }
        let r = reply(w.handle(Command::FlushFromFrame(0), true));
        assert_eq!(r.status, STATUS_INVALID_OPERATION);
        assert_eq!(reply(w.handle(Command::Exit(0), true)).status, STATUS_BAD_VALUE);
        assert_eq!(w.handle(Command::Exit(1), true), Step::Ignore);
        assert_eq!(w.handle(Command::Exit(COOKIE), true), Step::Exit);
    }

    #[test]
    fn input_captures_into_the_queue() {
        let mut w = worker(Io::Input);
        w.handle(Command::Start, true);
        let r = reply(w.handle(Command::Burst(16), true));
        assert_eq!((r.status, r.fmq_byte_count, r.state), (STATUS_OK, 16, State::Active));
        assert_eq!(w.data.bytes.iter().copied().collect::<Vec<u8>>(), vec![7; 16]);
        assert_eq!(reply(w.handle(Command::Drain(DRAIN_UNSPECIFIED), true)).state, State::Draining);
        w.data.bytes.clear();
        assert_eq!(reply(w.handle(Command::Burst(16), true)).state, State::Standby);
        let r = reply(w.handle(Command::Drain(DRAIN_ALL), true));
        assert_eq!(r.status, STATUS_BAD_VALUE);
    }

    #[test]
    fn input_pause_flush_goes_to_standby() {
        let mut w = worker(Io::Input);
        w.handle(Command::Start, true);
        w.handle(Command::Burst(4), true);
        assert_eq!(reply(w.handle(Command::Pause, true)).state, State::Paused);
        assert_eq!(reply(w.handle(Command::Flush, true)).state, State::Standby);
    }

    #[test]
    fn disconnected_streams_simulate_io() {
        let mut w = worker(Io::Input);
        w.handle(Command::Start, false);
        let r = reply(w.handle(Command::Burst(8), false));
        assert_eq!((r.frames, r.time_ns, r.fmq_byte_count), (UNKNOWN, UNKNOWN, 8));
        assert_eq!(w.device.started, 0);
        assert_eq!(w.data.bytes.iter().copied().collect::<Vec<u8>>(), vec![0; 8]);
    }

    #[test]
    fn device_failure_is_the_error_state() {
        let mut w = worker(Io::Output);
        w.handle(Command::Start, true);
        w.device.fail = true;
        w.data.bytes.extend([0u8; 8]);
        assert_eq!(reply(w.handle(Command::Burst(8), true)).state, State::Error);
        assert_eq!(reply(w.handle(Command::Start, true)).status, STATUS_INVALID_OPERATION);
    }
}
