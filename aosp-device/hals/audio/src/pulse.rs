//! Playback to and capture from a PulseAudio server (the desktop session's PulseAudio or pipewire-pulse) over
//! its native protocol, for Waydroid's container: there is no sound card in it, and Waydroid
//! binds the session's socket at `/run/xdg/pulse/native` (docs/waydroid-aosp17.md).
//!
//! Only what one stream needs: AUTH, SET_CLIENT_NAME, then CREATE_PLAYBACK_STREAM with audio
//! sent as the server asks for it (REQUEST), DRAIN and FLUSH, or CREATE_RECORD_STREAM with
//! audio arriving as the server's data packets. Protocol version 13, no
//! shared memory: every sample goes through the socket. The wire format follows PulseAudio's
//! `src/pulsecore/{pstream,tagstruct}.c` and `src/pulse/stream.c`.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

/// Where Waydroid binds the session's PulseAudio socket in the container.
pub const WAYDROID_SOCKET: &str = "/run/xdg/pulse/native";

const PROTOCOL_VERSION: u32 = 13;
/// `PA_PSTREAM_DESCRIPTOR_CHANNEL` of a control packet.
const CONTROL_CHANNEL: u32 = u32::MAX;
const INVALID_INDEX: u32 = u32::MAX;

// `PA_COMMAND_*` (src/pulsecore/native-common.h).
const COMMAND_ERROR: u32 = 0;
const COMMAND_REPLY: u32 = 2;
const COMMAND_CREATE_PLAYBACK_STREAM: u32 = 3;
const COMMAND_DELETE_PLAYBACK_STREAM: u32 = 4;
const COMMAND_CREATE_RECORD_STREAM: u32 = 5;
const COMMAND_DELETE_RECORD_STREAM: u32 = 6;
const COMMAND_AUTH: u32 = 8;
const COMMAND_SET_CLIENT_NAME: u32 = 9;
const COMMAND_DRAIN_PLAYBACK_STREAM: u32 = 12;
const COMMAND_FLUSH_PLAYBACK_STREAM: u32 = 42;
const COMMAND_FLUSH_RECORD_STREAM: u32 = 59;
const COMMAND_REQUEST: u32 = 61;
const COMMAND_PLAYBACK_STREAM_KILLED: u32 = 64;
const COMMAND_RECORD_STREAM_KILLED: u32 = 65;

/// Captured audio kept when the reader falls behind; older audio is dropped.
const MAX_CAPTURED_MS: u32 = 500;

// `PA_TAG_*` (src/pulsecore/tagstruct.h).
const TAG_STRING: u8 = b't';
const TAG_STRING_NULL: u8 = b'N';
const TAG_U32: u8 = b'L';
const TAG_SAMPLE_SPEC: u8 = b'a';
const TAG_ARBITRARY: u8 = b'x';
const TAG_BOOLEAN_TRUE: u8 = b'1';
const TAG_BOOLEAN_FALSE: u8 = b'0';
const TAG_CHANNEL_MAP: u8 = b'm';
const TAG_CVOLUME: u8 = b'v';
const TAG_PROPLIST: u8 = b'P';

/// `PA_VOLUME_NORM`: the HAL applies its own gain to the samples.
const VOLUME_NORM: u32 = 0x10000;

/// `pa_sample_format_t` values of the formats the HAL plays, all little endian.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleFormat {
    S16 = 3,
    Float32 = 5,
    S32 = 7,
    /// Packed 24 bit.
    S24 = 9,
    /// 24 bit in the low bytes of 32.
    S24In32 = 11,
}

#[derive(Clone, Copy, Debug)]
pub struct Spec {
    pub format: SampleFormat,
    pub channels: u8,
    pub rate: u32,
}

impl Spec {
    fn frame_bytes(&self) -> u32 {
        let sample = match self.format {
            SampleFormat::S16 => 2,
            SampleFormat::S24 => 3,
            SampleFormat::Float32 | SampleFormat::S32 | SampleFormat::S24In32 => 4,
        };
        sample * u32::from(self.channels)
    }

    /// `ms` of audio in bytes, whole frames.
    fn bytes(&self, ms: u32) -> u32 {
        self.rate * ms / 1000 * self.frame_bytes()
    }
}

/// A tagstruct being built.
#[derive(Default)]
struct Tags(Vec<u8>);

impl Tags {
    fn command(command: u32, tag: u32) -> Self {
        let mut t = Self::default();
        t.u32(command).u32(tag);
        t
    }

    fn u32(&mut self, v: u32) -> &mut Self {
        self.0.push(TAG_U32);
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }

    fn bool(&mut self, v: bool) -> &mut Self {
        self.0.push(if v { TAG_BOOLEAN_TRUE } else { TAG_BOOLEAN_FALSE });
        self
    }

    fn string(&mut self, v: Option<&str>) -> &mut Self {
        match v {
            Some(s) => {
                self.0.push(TAG_STRING);
                self.0.extend_from_slice(s.as_bytes());
                self.0.push(0);
            }
            None => self.0.push(TAG_STRING_NULL),
        }
        self
    }

    fn arbitrary(&mut self, v: &[u8]) -> &mut Self {
        self.0.push(TAG_ARBITRARY);
        self.0.extend_from_slice(&(v.len() as u32).to_be_bytes());
        self.0.extend_from_slice(v);
        self
    }

    fn sample_spec(&mut self, spec: &Spec) -> &mut Self {
        self.0.extend_from_slice(&[TAG_SAMPLE_SPEC, spec.format as u8, spec.channels]);
        self.0.extend_from_slice(&spec.rate.to_be_bytes());
        self
    }

    /// The default map of 1 or 2 channels (mono; front left and right).
    fn channel_map(&mut self, channels: u8) -> &mut Self {
        self.0.extend_from_slice(&[TAG_CHANNEL_MAP, channels]);
        match channels {
            1 => self.0.push(0),
            _ => self.0.extend((1..=channels).map(|c| c.min(2))),
        }
        self
    }

    fn cvolume(&mut self, channels: u8, volume: u32) -> &mut Self {
        self.0.extend_from_slice(&[TAG_CVOLUME, channels]);
        for _ in 0..channels {
            self.0.extend_from_slice(&volume.to_be_bytes());
        }
        self
    }

    /// String properties, each stored with its terminating NUL as PulseAudio does.
    fn proplist(&mut self, props: &[(&str, &str)]) -> &mut Self {
        self.0.push(TAG_PROPLIST);
        for (key, value) in props {
            let mut data = value.as_bytes().to_vec();
            data.push(0);
            self.string(Some(key)).u32(data.len() as u32).arbitrary(&data);
        }
        self.string(None)
    }
}

/// A received tagstruct being read.
struct Reader {
    data: Vec<u8>,
    pos: usize,
}

impl Reader {
    fn u32(&mut self) -> io::Result<u32> {
        match self.data[self.pos..] {
            [TAG_U32, a, b, c, d, ..] => {
                self.pos += 5;
                Ok(u32::from_be_bytes([a, b, c, d]))
            }
            _ => Err(protocol("expected a u32")),
        }
    }
}

fn protocol(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("pulse: {what}"))
}

/// One playback or record stream on its own connection.
pub struct PulseStream {
    socket: UnixStream,
    next_tag: u32,
    record: bool,
    /// The stream's number on this connection: data packets go to and come from it, and the
    /// stream commands name it.
    channel: u32,
    /// Playback: bytes the server asked for and has not received yet.
    requested: usize,
    /// Record: audio received and not read yet, at most `max_captured` bytes.
    captured: VecDeque<u8>,
    max_captured: usize,
}

impl PulseStream {
    fn connect(socket: &Path, spec: &Spec, record: bool) -> io::Result<Self> {
        let mut s = Self {
            socket: UnixStream::connect(socket)?,
            next_tag: 0,
            record,
            channel: 0,
            requested: 0,
            captured: VecDeque::new(),
            max_captured: spec.bytes(MAX_CAPTURED_MS) as usize,
        };
        // No cookie: the server trusts local clients on its socket (pipewire-pulse ignores
        // it; PulseAudio's module-native-protocol-unix accepts the socket's uid by default).
        s.call(COMMAND_AUTH, |t| {
            t.u32(PROTOCOL_VERSION).arbitrary(&[0; 256]);
        })?;
        s.call(COMMAND_SET_CLIENT_NAME, |t| {
            t.proplist(&[("application.name", "Android"), ("application.id", "android-pc")]);
        })?;
        Ok(s)
    }

    /// Connects to the server at `socket` and creates a record stream from the default source,
    /// delivering about every `latency_ms`.
    pub fn open_record(socket: &Path, spec: Spec, latency_ms: u32) -> io::Result<Self> {
        let mut s = Self::connect(socket, &spec, true)?;
        let mut reply = s.call(COMMAND_CREATE_RECORD_STREAM, |t| {
            t.sample_spec(&spec)
                .channel_map(spec.channels)
                .u32(INVALID_INDEX) // source index
                .string(None) // source name: the default source
                .u32(u32::MAX) // maxlength: the server's default
                .bool(false) // corked
                .u32(spec.bytes(latency_ms)); // fragsize
            // v12: no_remap, no_remix, fix_format, fix_rate, fix_channels, no_move,
            // variable_rate.
            for _ in 0..7 {
                t.bool(false);
            }
            // v13: peak_detect, adjust_latency, proplist, direct_on_input.
            t.bool(false)
                .bool(true)
                .proplist(&[("media.name", "Android"), ("media.role", "game")])
                .u32(INVALID_INDEX);
        })?;
        s.channel = reply.u32()?;
        Ok(s)
    }

    /// Connects to the server at `socket` and creates a playback stream to the default sink,
    /// buffering about `latency_ms`.
    pub fn open(socket: &Path, spec: Spec, latency_ms: u32) -> io::Result<Self> {
        let mut s = Self::connect(socket, &spec, false)?;
        let tlength = spec.bytes(latency_ms);
        let mut reply = s.call(COMMAND_CREATE_PLAYBACK_STREAM, |t| {
            t.sample_spec(&spec)
                .channel_map(spec.channels)
                .u32(INVALID_INDEX) // sink index
                .string(None) // sink name: the default sink
                .u32(u32::MAX) // maxlength: the server's default
                .bool(false) // corked
                .u32(tlength)
                .u32(u32::MAX) // prebuf: the server's default (tlength)
                .u32(spec.bytes(latency_ms / 4)) // minreq
                .u32(0) // syncid
                .cvolume(spec.channels, VOLUME_NORM);
            // v12: no_remap, no_remix, fix_format, fix_rate, fix_channels, no_move,
            // variable_rate.
            for _ in 0..7 {
                t.bool(false);
            }
            // v13: start_muted, adjust_latency, proplist.
            t.bool(false).bool(true).proplist(&[("media.name", "Android"), ("media.role", "game")]);
        })?;
        s.channel = reply.u32()?;
        let _sink_input = reply.u32()?;
        s.requested = reply.u32()? as usize;
        Ok(s)
    }

    /// Sends all of `data` (whole frames), waiting for the server to ask for it: this is what
    /// paces playback.
    pub fn write(&mut self, mut data: &[u8]) -> io::Result<()> {
        while !data.is_empty() {
            while self.requested == 0 {
                self.receive(None)?;
            }
            let n = data.len().min(self.requested);
            self.send_packet(self.channel, &data[..n])?;
            self.requested -= n;
            data = &data[n..];
        }
        Ok(())
    }

    /// Fills `buf` with captured audio (whole frames), waiting for the server to send it.
    pub fn read(&mut self, buf: &mut [u8]) -> io::Result<()> {
        while self.captured.len() < buf.len() {
            self.receive(None)?;
        }
        let n = buf.len();
        for (dst, src) in buf.iter_mut().zip(self.captured.drain(..n)) {
            *dst = src;
        }
        Ok(())
    }

    /// Waits until the server has played everything sent.
    pub fn drain(&mut self) -> io::Result<()> {
        let channel = self.channel;
        self.call(COMMAND_DRAIN_PLAYBACK_STREAM, |t| {
            t.u32(channel);
        })
        .map(|_| ())
    }

    /// Drops what the server and this client have buffered.
    pub fn flush(&mut self) -> io::Result<()> {
        let channel = self.channel;
        self.captured.clear();
        let command =
            if self.record { COMMAND_FLUSH_RECORD_STREAM } else { COMMAND_FLUSH_PLAYBACK_STREAM };
        self.call(command, |t| {
            t.u32(channel);
        })
        .map(|_| ())
    }

    /// Sends a command and waits for its reply, handling the server's other messages.
    fn call(&mut self, command: u32, args: impl FnOnce(&mut Tags)) -> io::Result<Reader> {
        let tag = self.next_tag;
        self.next_tag = self.next_tag.wrapping_add(1);
        let mut t = Tags::command(command, tag);
        args(&mut t);
        self.send_packet(CONTROL_CHANNEL, &t.0)?;
        loop {
            if let Some(reply) = self.receive(Some(tag))? {
                return Ok(reply);
            }
        }
    }

    /// Reads one packet. Returns the arguments of the reply to `tag`; takes note of requests.
    fn receive(&mut self, tag: Option<u32>) -> io::Result<Option<Reader>> {
        let mut descriptor = [0u8; 20];
        self.socket.read_exact(&mut descriptor)?;
        let word = |i: usize| u32::from_be_bytes(descriptor[i * 4..i * 4 + 4].try_into().unwrap());
        let mut data = vec![0u8; word(0) as usize];
        self.socket.read_exact(&mut data)?;
        if word(1) != CONTROL_CHANNEL {
            if self.record && word(1) == self.channel {
                self.captured.extend(&data);
                // Keep whole frames when dropping: the excess is a multiple of the frame
                // size as long as both the limit and every packet are.
                let excess = self.captured.len().saturating_sub(self.max_captured);
                self.captured.drain(..excess);
            }
            return Ok(None);
        }
        let mut r = Reader { data, pos: 0 };
        let command = r.u32()?;
        let packet_tag = r.u32()?;
        match command {
            COMMAND_REPLY if Some(packet_tag) == tag => Ok(Some(r)),
            COMMAND_ERROR if Some(packet_tag) == tag => {
                let code = r.u32().unwrap_or(0);
                Err(io::Error::other(format!("pulse: server error {code}")))
            }
            COMMAND_REQUEST => {
                let (channel, bytes) = (r.u32()?, r.u32()?);
                if channel == self.channel {
                    self.requested += bytes as usize;
                }
                Ok(None)
            }
            COMMAND_PLAYBACK_STREAM_KILLED | COMMAND_RECORD_STREAM_KILLED => {
                Err(io::Error::other("pulse: stream killed"))
            }
            // Underflow, started, buffer attribute and other notifications.
            _ => Ok(None),
        }
    }

    fn send_packet(&mut self, channel: u32, payload: &[u8]) -> io::Result<()> {
        let mut packet = Vec::with_capacity(20 + payload.len());
        // length, channel, offset hi, offset lo, flags (PA_SEEK_RELATIVE).
        for word in [payload.len() as u32, channel, 0, 0, 0] {
            packet.extend_from_slice(&word.to_be_bytes());
        }
        packet.extend_from_slice(payload);
        self.socket.write_all(&packet)
    }
}

impl Drop for PulseStream {
    fn drop(&mut self) {
        // Best effort: closing the connection deletes the stream anyway.
        let command =
            if self.record { COMMAND_DELETE_RECORD_STREAM } else { COMMAND_DELETE_PLAYBACK_STREAM };
        let mut t = Tags::command(command, self.next_tag);
        t.u32(self.channel);
        let _ = self.send_packet(CONTROL_CHANNEL, &t.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tagstruct_encoding() {
        let mut t = Tags::command(COMMAND_AUTH, 7);
        t.bool(true).string(Some("ab")).string(None);
        assert_eq!(
            t.0,
            [b'L', 0, 0, 0, 8, b'L', 0, 0, 0, 7, b'1', b't', b'a', b'b', 0, b'N'].to_vec()
        );
    }

    #[test]
    fn spec_and_map() {
        let spec = Spec { format: SampleFormat::S16, channels: 2, rate: 48000 };
        let mut t = Tags::default();
        t.sample_spec(&spec).channel_map(2).cvolume(2, VOLUME_NORM);
        assert_eq!(
            t.0,
            [
                b'a', 3, 2, 0, 0, 0xbb, 0x80, b'm', 2, 1, 2, b'v', 2, 0, 1, 0, 0, 0, 1, 0, 0
            ]
            .to_vec()
        );
        assert_eq!(spec.bytes(10), 1920);
    }

    #[test]
    fn proplist_ends_with_null_string() {
        let mut t = Tags::default();
        t.proplist(&[("k", "v")]);
        assert_eq!(
            t.0,
            [b'P', b't', b'k', 0, b'L', 0, 0, 0, 2, b'x', 0, 0, 0, 2, b'v', 0, b'N'].to_vec()
        );
    }
}
