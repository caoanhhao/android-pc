//! ALSA PCM devices (`/dev/snd/pcmC<card>D<device>{p,c}`) through the kernel's ioctl ABI.
//!
//! Only interleaved read/write access is used: [`Pcm::open`] negotiates the hardware
//! parameters with `HW_REFINE`/`HW_PARAMS`, sets the software parameters, and
//! [`Pcm::write`] / [`Pcm::read`] move whole frames, recovering from xruns by preparing
//! the stream again. [`probe`] asks a device which sample formats, channel counts and
//! rates it accepts.

use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::path::PathBuf;

use log::warn;

/// Direction of a PCM device: the `p`/`c` suffix of its device node.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Direction {
    Playback,
    Capture,
}

/// Sample formats the HAL can pass through unchanged (`SNDRV_PCM_FORMAT_*`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Format {
    /// Signed 16 bit, little endian.
    S16Le,
    /// Signed 24 bit in the low bits of 32, little endian (AIDL `FIXED_Q_8_24`).
    S24Le,
    /// Signed 32 bit, little endian.
    S32Le,
    /// Signed 24 bit packed in 3 bytes, little endian.
    S24Packed,
    /// IEEE float, little endian.
    FloatLe,
}

impl Format {
    pub const ALL: [Format; 5] =
        [Format::S16Le, Format::S24Le, Format::S32Le, Format::S24Packed, Format::FloatLe];

    /// The kernel's `snd_pcm_format_t` value.
    fn to_kernel(self) -> u32 {
        match self {
            Format::S16Le => 2,
            Format::S24Le => 6,
            Format::S32Le => 10,
            Format::FloatLe => 14,
            Format::S24Packed => 32,
        }
    }

    /// Bytes per sample.
    pub fn bytes(self) -> usize {
        match self {
            Format::S16Le => 2,
            Format::S24Packed => 3,
            Format::S24Le | Format::S32Le | Format::FloatLe => 4,
        }
    }
}

/// Sample rates offered to the framework, if the device accepts them. Higher rates are
/// left out: the framework mixes at the highest rate a mix port offers, and on an HDA codec
/// that would be 192 kHz for no audible gain.
pub const STANDARD_RATES: [u32; 7] = [8000, 11025, 16000, 22050, 32000, 44100, 48000];

/// One ALSA PCM device node.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PcmAddress {
    pub card: u32,
    pub device: u32,
    pub direction: Direction,
}

impl PcmAddress {
    pub fn path(&self) -> PathBuf {
        let suffix = match self.direction {
            Direction::Playback => 'p',
            Direction::Capture => 'c',
        };
        PathBuf::from(format!("/dev/snd/pcmC{}D{}{suffix}", self.card, self.device))
    }
}

/// What a device accepts, from `HW_REFINE`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Capabilities {
    pub formats: Vec<Format>,
    /// Supported channel counts out of 1 and 2 (the only layouts the HAL offers).
    pub channels: Vec<u32>,
    pub rates: Vec<u32>,
}

/// Parameters of an open stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    pub format: Format,
    pub channels: u32,
    pub rate: u32,
    /// Frames the client moves per burst; the ALSA buffer holds two of them.
    pub burst_frames: u32,
}

impl Config {
    pub fn frame_bytes(&self) -> usize {
        self.format.bytes() * self.channels as usize
    }
}

// Kernel ABI (include/uapi/sound/asound.h), x86_64 and arm64 layouts.

const PARAM_ACCESS: usize = 0;
const PARAM_FORMAT: usize = 1;
const PARAM_SUBFORMAT: usize = 2;
const MASK_COUNT: usize = 3;
const PARAM_FIRST_INTERVAL: usize = 8;
const PARAM_CHANNELS: usize = 10;
const PARAM_RATE: usize = 11;
const PARAM_PERIOD_SIZE: usize = 13;
const PARAM_BUFFER_SIZE: usize = 17;
const INTERVAL_COUNT: usize = 12;
const ACCESS_RW_INTERLEAVED: u32 = 3;
const SUBFORMAT_STD: u32 = 0;

#[repr(C)]
#[derive(Clone, Copy)]
struct Mask {
    bits: [u32; 8],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Interval {
    min: u32,
    max: u32,
    /// `openmin:1, openmax:1, integer:1, empty:1`.
    flags: u32,
}

const INTERVAL_EMPTY: u32 = 1 << 3;

#[repr(C)]
#[derive(Clone, Copy)]
struct HwParams {
    flags: u32,
    masks: [Mask; MASK_COUNT],
    mres: [Mask; 5],
    intervals: [Interval; INTERVAL_COUNT],
    ires: [Interval; 9],
    rmask: u32,
    cmask: u32,
    info: u32,
    msbits: u32,
    rate_num: u32,
    rate_den: u32,
    fifo_size: libc::c_ulong,
    reserved: [u8; 64],
}

impl HwParams {
    /// Everything allowed, as `snd_pcm_hw_params_any` does.
    fn any() -> Self {
        let full = Mask { bits: [u32::MAX; 8] };
        let any = Interval { min: 0, max: u32::MAX, flags: 0 };
        Self {
            flags: 0,
            masks: [full; MASK_COUNT],
            mres: [Mask { bits: [0; 8] }; 5],
            intervals: [any; INTERVAL_COUNT],
            ires: [Interval { min: 0, max: 0, flags: 0 }; 9],
            rmask: u32::MAX,
            cmask: 0,
            info: 0,
            msbits: 0,
            rate_num: 0,
            rate_den: 0,
            fifo_size: 0,
            reserved: [0; 64],
        }
    }

    fn set_mask(&mut self, param: usize, value: u32) {
        let mask = &mut self.masks[param];
        mask.bits = [0; 8];
        mask.bits[(value / 32) as usize] = 1 << (value % 32);
    }

    fn mask_has(&self, param: usize, value: u32) -> bool {
        self.masks[param].bits[(value / 32) as usize] & (1 << (value % 32)) != 0
    }

    fn interval_mut(&mut self, param: usize) -> &mut Interval {
        &mut self.intervals[param - PARAM_FIRST_INTERVAL]
    }

    fn interval(&self, param: usize) -> Interval {
        self.intervals[param - PARAM_FIRST_INTERVAL]
    }

    fn set_range(&mut self, param: usize, min: u32, max: u32) {
        *self.interval_mut(param) = Interval { min, max, flags: 0 };
    }

    fn set_interleaved(&mut self) {
        self.set_mask(PARAM_ACCESS, ACCESS_RW_INTERLEAVED);
        self.set_mask(PARAM_SUBFORMAT, SUBFORMAT_STD);
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SwParams {
    tstamp_mode: i32,
    period_step: u32,
    sleep_min: u32,
    avail_min: libc::c_ulong,
    xfer_align: libc::c_ulong,
    start_threshold: libc::c_ulong,
    stop_threshold: libc::c_ulong,
    silence_threshold: libc::c_ulong,
    silence_size: libc::c_ulong,
    boundary: libc::c_ulong,
    proto: u32,
    tstamp_type: u32,
    reserved: [u8; 56],
}

#[repr(C)]
struct XferI {
    result: libc::c_long,
    buf: *mut libc::c_void,
    frames: libc::c_ulong,
}

const fn ioc(dir: u32, nr: u32, size: usize) -> u32 {
    (dir << 30) | ((size as u32) << 16) | ((b'A' as u32) << 8) | nr
}
const IOC_WRITE: u32 = 1;
const IOC_READ: u32 = 2;

const IOCTL_HW_REFINE: u32 = ioc(IOC_READ | IOC_WRITE, 0x10, size_of::<HwParams>());
const IOCTL_HW_PARAMS: u32 = ioc(IOC_READ | IOC_WRITE, 0x11, size_of::<HwParams>());
const IOCTL_SW_PARAMS: u32 = ioc(IOC_READ | IOC_WRITE, 0x13, size_of::<SwParams>());
const IOCTL_DELAY: u32 = ioc(IOC_READ, 0x21, size_of::<libc::c_long>());
const IOCTL_PREPARE: u32 = ioc(0, 0x40, 0);
const IOCTL_DROP: u32 = ioc(0, 0x43, 0);
const IOCTL_DRAIN: u32 = ioc(0, 0x44, 0);
const IOCTL_WRITEI_FRAMES: u32 = ioc(IOC_WRITE, 0x50, size_of::<XferI>());
const IOCTL_READI_FRAMES: u32 = ioc(IOC_READ, 0x51, size_of::<XferI>());

const _: () = assert!(size_of::<HwParams>() == 608, "struct snd_pcm_hw_params");
const _: () = assert!(size_of::<SwParams>() == 136, "struct snd_pcm_sw_params");

/// `ioctl(2)` on `file`, mapping -1 to the errno.
///
/// # Safety
///
/// `arg` must point to memory of the type and size `request` encodes, valid for the
/// kernel to read and/or write for the duration of the call.
unsafe fn ioctl(file: &File, request: u32, arg: *mut libc::c_void) -> io::Result<libc::c_int> {
    // SAFETY: the caller guarantees `arg` matches `request`; the fd is open.
    let ret = unsafe { libc::ioctl(file.as_raw_fd(), request as _, arg) };
    if ret < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(ret)
    }
}

/// `HW_REFINE` or `HW_PARAMS`: every parameter is marked for refinement first, since the
/// kernel clears `rmask` and only re-checks the parameters it names.
fn hw_ioctl(file: &File, request: u32, params: &mut HwParams) -> io::Result<()> {
    params.rmask = u32::MAX;
    // SAFETY: `params` is a `struct snd_pcm_hw_params` (size checked above), which is what
    // both requests take.
    unsafe { ioctl(file, request, (params as *mut HwParams).cast()) }?;
    Ok(())
}

fn refine(file: &File, params: &mut HwParams) -> io::Result<()> {
    hw_ioctl(file, IOCTL_HW_REFINE, params)
}

fn open_node(address: &PcmAddress) -> io::Result<File> {
    pchal::devfs::open(address.path(), pchal::devfs::Access::ReadWrite).map_err(io::Error::other)
}

/// Asks the device which formats, channel counts (1, 2) and standard rates it accepts.
///
/// # Errors
///
/// The device node cannot be opened (it may be busy) or rejects every interleaved setup.
pub fn probe(address: &PcmAddress) -> io::Result<Capabilities> {
    let file = open_node(address)?;
    let accepts = |configure: &dyn Fn(&mut HwParams)| {
        let mut params = HwParams::any();
        params.set_interleaved();
        configure(&mut params);
        refine(&file, &mut params).is_ok()
    };
    let mut base = HwParams::any();
    base.set_interleaved();
    refine(&file, &mut base)?;
    let formats: Vec<Format> =
        Format::ALL.into_iter().filter(|f| base.mask_has(PARAM_FORMAT, f.to_kernel())).collect();
    let channels =
        [1, 2].into_iter().filter(|&c| accepts(&|p| p.set_range(PARAM_CHANNELS, c, c))).collect();
    let rates = STANDARD_RATES
        .into_iter()
        .filter(|&r| accepts(&|p| p.set_range(PARAM_RATE, r, r)))
        .collect();
    Ok(Capabilities { formats, channels, rates })
}

/// An open, configured PCM stream.
#[derive(Debug)]
pub struct Pcm {
    file: File,
    address: PcmAddress,
    config: Config,
    buffer_frames: u32,
    /// Set by an xrun or a new setup; the next transfer prepares the stream first.
    needs_prepare: bool,
}

impl Pcm {
    /// Opens the device with `config`: an ALSA buffer of two bursts in periods of half a
    /// burst where the hardware allows it, otherwise the nearest it accepts.
    ///
    /// # Errors
    ///
    /// The node cannot be opened or rejects the format, channel count or rate.
    pub fn open(address: &PcmAddress, config: Config) -> io::Result<Self> {
        let file = open_node(address)?;
        let burst = config.burst_frames.max(64);
        let mut params = HwParams::any();
        params.set_interleaved();
        params.set_mask(PARAM_FORMAT, config.format.to_kernel());
        params.set_range(PARAM_CHANNELS, config.channels, config.channels);
        params.set_range(PARAM_RATE, config.rate, config.rate);
        refine(&file, &mut params)?;
        // Preferred geometry first; if the hardware has other granularity, let the kernel
        // pick the smallest period and the largest buffer within looser limits.
        let preferred =
            [(burst / 2, burst / 2, 2 * burst, 2 * burst), (burst / 4, burst, burst, 4 * burst)];
        let mut chosen = None;
        for (period_min, period_max, buffer_min, buffer_max) in preferred {
            let mut attempt = params;
            attempt.set_range(PARAM_PERIOD_SIZE, period_min, period_max);
            attempt.set_range(PARAM_BUFFER_SIZE, buffer_min, buffer_max);
            if hw_ioctl(&file, IOCTL_HW_PARAMS, &mut attempt).is_ok() {
                chosen = Some(attempt);
                break;
            }
        }
        let params = match chosen {
            Some(params) => params,
            None => {
                let mut attempt = params;
                hw_ioctl(&file, IOCTL_HW_PARAMS, &mut attempt)?;
                attempt
            }
        };
        let buffer = params.interval(PARAM_BUFFER_SIZE);
        let period = params.interval(PARAM_PERIOD_SIZE);
        if buffer.flags & INTERVAL_EMPTY != 0 || buffer.min == 0 {
            return Err(io::Error::other("kernel chose an empty buffer size"));
        }
        let buffer_frames = buffer.min;
        let start = match address.direction {
            // Start once a burst is queued, so the first write does not underrun.
            Direction::Playback => burst.min(buffer_frames),
            Direction::Capture => 1,
        };
        let mut sw = SwParams {
            tstamp_mode: 0,
            period_step: 1,
            sleep_min: 0,
            avail_min: libc::c_ulong::from(period.min.max(1)),
            xfer_align: 1,
            start_threshold: libc::c_ulong::from(start),
            stop_threshold: libc::c_ulong::from(buffer_frames),
            silence_threshold: 0,
            silence_size: 0,
            boundary: 0,
            proto: 0,
            tstamp_type: 0,
            reserved: [0; 56],
        };
        // The kernel fills in `boundary`; the first call only reads it back.
        // SAFETY: `sw` is a `struct snd_pcm_sw_params` (size checked above).
        unsafe { ioctl(&file, IOCTL_SW_PARAMS, (&mut sw as *mut SwParams).cast()) }?;
        Ok(Self { file, address: address.clone(), config, buffer_frames, needs_prepare: true })
    }

    /// Size of the ALSA ring buffer in frames.
    pub fn buffer_frames(&self) -> u32 {
        self.buffer_frames
    }

    fn prepare(&mut self) -> io::Result<()> {
        // SAFETY: PREPARE takes no argument.
        unsafe { ioctl(&self.file, IOCTL_PREPARE, std::ptr::null_mut()) }?;
        self.needs_prepare = false;
        Ok(())
    }

    /// Moves `frames` frames between `buf` and the device; blocks until all are done.
    fn transfer(&mut self, buf: *mut u8, frames: usize) -> io::Result<()> {
        let frame_bytes = self.config.frame_bytes();
        let request = match self.address.direction {
            Direction::Playback => IOCTL_WRITEI_FRAMES,
            Direction::Capture => IOCTL_READI_FRAMES,
        };
        let mut done = 0;
        let mut recoveries = 0;
        while done < frames {
            if self.needs_prepare {
                self.prepare()?;
            }
            let mut xfer = XferI {
                result: 0,
                // `done < frames`, so the offset stays inside the caller's buffer.
                buf: buf.wrapping_add(done * frame_bytes).cast(),
                frames: (frames - done) as libc::c_ulong,
            };
            // SAFETY: `xfer` is a `struct snd_xferi` whose buffer holds `frames - done`
            // frames at `buf + done * frame_bytes`, inside the caller's slice.
            match unsafe { ioctl(&self.file, request, (&mut xfer as *mut XferI).cast()) } {
                Ok(_) => done += xfer.result.max(0) as usize,
                Err(err) if err.raw_os_error() == Some(libc::EINTR) => {}
                Err(err)
                    if matches!(err.raw_os_error(), Some(libc::EPIPE | libc::ESTRPIPE))
                        && recoveries < 3 =>
                {
                    // Underrun or overrun: start over from a prepared stream.
                    recoveries += 1;
                    warn!("{}: xrun ({err}), preparing again", self.address.path().display());
                    self.needs_prepare = true;
                }
                Err(err) => return Err(err),
            }
        }
        Ok(())
    }

    /// Plays `data`, whole frames only; blocks while the ALSA buffer is full.
    ///
    /// # Errors
    ///
    /// The device failed (unplugged, repeated xruns).
    pub fn write(&mut self, data: &[u8]) -> io::Result<()> {
        assert_eq!(self.address.direction, Direction::Playback, "write on a capture device");
        let frames = data.len() / self.config.frame_bytes();
        // The kernel only reads through this pointer for a playback stream.
        self.transfer(data.as_ptr().cast_mut(), frames)
    }

    /// Fills `data` with captured frames (whole frames only); blocks until they arrive.
    ///
    /// # Errors
    ///
    /// The device failed (unplugged, repeated xruns).
    pub fn read(&mut self, data: &mut [u8]) -> io::Result<()> {
        assert_eq!(self.address.direction, Direction::Capture, "read on a playback device");
        let frames = data.len() / self.config.frame_bytes();
        self.transfer(data.as_mut_ptr(), frames)
    }

    /// Frames between the application pointer and what the listener hears (playback) or
    /// what was captured but not read yet (capture).
    pub fn delay_frames(&self) -> Option<u64> {
        let mut delay: libc::c_long = 0;
        // SAFETY: DELAY writes one `snd_pcm_sframes_t`.
        unsafe { ioctl(&self.file, IOCTL_DELAY, (&mut delay as *mut libc::c_long).cast()) }.ok()?;
        u64::try_from(delay).ok()
    }

    /// Lets the queued frames play out (bounded by the buffer size), then stops.
    pub fn drain(&mut self) {
        if !self.needs_prepare && self.address.direction == Direction::Playback {
            // SAFETY: DRAIN takes no argument.
            if let Err(err) = unsafe { ioctl(&self.file, IOCTL_DRAIN, std::ptr::null_mut()) } {
                warn!("{}: drain: {err}", self.address.path().display());
            }
        }
        self.needs_prepare = true;
    }

    /// Stops at once, dropping queued frames.
    pub fn drop_frames(&mut self) {
        // SAFETY: DROP takes no argument.
        if let Err(err) = unsafe { ioctl(&self.file, IOCTL_DROP, std::ptr::null_mut()) } {
            warn!("{}: drop: {err}", self.address.path().display());
        }
        self.needs_prepare = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ioctl_numbers_match_the_kernel() {
        assert_eq!(IOCTL_HW_REFINE, 0xc260_4110);
        assert_eq!(IOCTL_HW_PARAMS, 0xc260_4111);
        assert_eq!(IOCTL_SW_PARAMS, 0xc088_4113);
        assert_eq!(IOCTL_PREPARE, 0x4140);
        assert_eq!(IOCTL_WRITEI_FRAMES, 0x4018_4150);
        assert_eq!(IOCTL_READI_FRAMES, 0x8018_4151);
        assert_eq!(IOCTL_DELAY, 0x8008_4121);
    }

    #[test]
    fn masks_and_paths() {
        let mut params = HwParams::any();
        params.set_mask(PARAM_FORMAT, Format::S24Packed.to_kernel());
        assert!(params.mask_has(PARAM_FORMAT, 32));
        assert!(!params.mask_has(PARAM_FORMAT, 2));
        let address = PcmAddress { card: 1, device: 3, direction: Direction::Playback };
        assert_eq!(address.path(), PathBuf::from("/dev/snd/pcmC1D3p"));
        let config =
            Config { format: Format::S24Packed, channels: 2, rate: 48000, burst_frames: 960 };
        assert_eq!(config.frame_bytes(), 6);
    }
}
