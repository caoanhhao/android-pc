//! Video capture through the V4L2 ioctl ABI (`linux/videodev2.h`), as used by `uvcvideo`.
//!
//! Only what a webcam needs: query a node, list its discrete formats, sizes and frame
//! rates, and stream memory-mapped buffers.

use std::fmt;
use std::fs::File;
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::path::{Path, PathBuf};
use std::ptr::{self, NonNull};
use std::time::Duration;

use crate::image::Size;

/// A V4L2 pixel format code (`V4L2_PIX_FMT_*`).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FourCc(pub u32);

impl FourCc {
    /// Motion JPEG, what webcams send at high resolutions.
    pub const MJPEG: Self = Self::new(*b"MJPG");
    /// Packed YUV 4:2:2 (Y0 U Y1 V).
    pub const YUYV: Self = Self::new(*b"YUYV");

    const fn new(code: [u8; 4]) -> Self {
        Self(u32::from_le_bytes(code))
    }
}

impl fmt::Debug for FourCc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let bytes = self.0.to_le_bytes();
        if bytes.iter().all(u8::is_ascii_graphic) {
            write!(f, "{}", bytes.iter().map(|byte| char::from(*byte)).collect::<String>())
        } else {
            write!(f, "{:#010x}", self.0)
        }
    }
}

/// A frame interval in seconds, `numerator / denominator`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Interval {
    /// Numerator.
    pub numerator: u32,
    /// Denominator.
    pub denominator: u32,
}

impl Interval {
    /// Frames per second at this interval.
    pub fn fps(self) -> f64 {
        f64::from(self.denominator) / f64::from(self.numerator)
    }

    /// The interval as a duration in nanoseconds.
    pub fn nanos(self) -> i64 {
        1_000_000_000 * i64::from(self.numerator) / i64::from(self.denominator)
    }
}

/// One capture mode: a pixel format at one size with the frame rates it supports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mode {
    /// The pixel format the device sends.
    pub fourcc: FourCc,
    /// The frame size.
    pub size: Size,
    /// Supported frame intervals, shortest (fastest) first.
    pub intervals: Vec<Interval>,
}

const CAP_VIDEO_CAPTURE: u32 = 0x0000_0001;
const CAP_STREAMING: u32 = 0x0400_0000;
const BUF_TYPE_VIDEO_CAPTURE: u32 = 1;
const MEMORY_MMAP: u32 = 1;
const FMT_FLAG_EMULATED: u32 = 0x0002;
const FRMSIZE_TYPE_DISCRETE: u32 = 1;
const FRMIVAL_TYPE_DISCRETE: u32 = 1;
const FIELD_ANY: u32 = 0;

#[repr(C)]
#[derive(Default)]
struct Capability {
    driver: [u8; 16],
    card: [u8; 32],
    bus_info: [u8; 32],
    version: u32,
    capabilities: u32,
    device_caps: u32,
    reserved: [u32; 3],
}

#[repr(C)]
#[derive(Default)]
struct FmtDesc {
    index: u32,
    kind: u32,
    flags: u32,
    description: [u8; 32],
    pixelformat: u32,
    mbus_code: u32,
    reserved: [u32; 3],
}

#[repr(C)]
#[derive(Default)]
struct FrmSizeEnum {
    index: u32,
    pixel_format: u32,
    kind: u32,
    /// `discrete { width, height }` or `stepwise { 6 x u32 }`.
    size: [u32; 6],
    reserved: [u32; 2],
}

#[repr(C)]
#[derive(Default)]
struct FrmIvalEnum {
    index: u32,
    pixel_format: u32,
    width: u32,
    height: u32,
    kind: u32,
    /// `discrete { numerator, denominator }` or `stepwise { min, max, step }`.
    interval: [u32; 6],
    reserved: [u32; 2],
}

#[repr(C)]
#[derive(Default)]
struct PixFormat {
    width: u32,
    height: u32,
    pixelformat: u32,
    field: u32,
    bytesperline: u32,
    sizeimage: u32,
    colorspace: u32,
    private: u32,
    flags: u32,
    ycbcr_enc: u32,
    quantization: u32,
    xfer_func: u32,
}

/// `struct v4l2_format`: the union is 8-aligned on 64-bit because of `v4l2_window`.
#[repr(C)]
struct Format {
    kind: u32,
    padding: u32,
    pix: PixFormat,
    /// The rest of the 200-byte union.
    raw: [u64; 19],
}

#[repr(C)]
#[derive(Default)]
struct CaptureParm {
    capability: u32,
    capturemode: u32,
    timeperframe: [u32; 2],
    extendedmode: u32,
    readbuffers: u32,
    reserved: [u32; 4],
}

#[repr(C)]
struct StreamParm {
    kind: u32,
    capture: CaptureParm,
    raw: [u8; 160],
}

#[repr(C)]
#[derive(Default)]
struct RequestBuffers {
    count: u32,
    kind: u32,
    memory: u32,
    capabilities: u32,
    flags: u8,
    reserved: [u8; 3],
}

#[repr(C)]
#[derive(Default)]
struct Buffer {
    index: u32,
    kind: u32,
    bytesused: u32,
    flags: u32,
    field: u32,
    timestamp: [i64; 2],
    timecode: [u32; 4],
    sequence: u32,
    memory: u32,
    /// `m.offset` for MMAP buffers (a union with a pointer, hence 64 bits).
    offset: u64,
    length: u32,
    reserved2: u32,
    request_fd: u32,
}

const fn ioc(dir: u32, nr: u32, size: usize) -> u32 {
    (dir << 30) | ((size as u32) << 16) | ((b'V' as u32) << 8) | nr
}
const READ: u32 = 2;
const WRITE: u32 = 1;
const VIDIOC_QUERYCAP: u32 = ioc(READ, 0, size_of::<Capability>());
const VIDIOC_ENUM_FMT: u32 = ioc(READ | WRITE, 2, size_of::<FmtDesc>());
const VIDIOC_S_FMT: u32 = ioc(READ | WRITE, 5, size_of::<Format>());
const VIDIOC_REQBUFS: u32 = ioc(READ | WRITE, 8, size_of::<RequestBuffers>());
const VIDIOC_QUERYBUF: u32 = ioc(READ | WRITE, 9, size_of::<Buffer>());
const VIDIOC_QBUF: u32 = ioc(READ | WRITE, 15, size_of::<Buffer>());
const VIDIOC_DQBUF: u32 = ioc(READ | WRITE, 17, size_of::<Buffer>());
const VIDIOC_STREAMON: u32 = ioc(WRITE, 18, size_of::<i32>());
const VIDIOC_STREAMOFF: u32 = ioc(WRITE, 19, size_of::<i32>());
const VIDIOC_S_PARM: u32 = ioc(READ | WRITE, 22, size_of::<StreamParm>());
const VIDIOC_ENUM_FRAMESIZES: u32 = ioc(READ | WRITE, 74, size_of::<FrmSizeEnum>());
const VIDIOC_ENUM_FRAMEINTERVALS: u32 = ioc(READ | WRITE, 75, size_of::<FrmIvalEnum>());

/// Runs one ioctl, retrying on `EINTR`.
///
/// # Safety
///
/// `request` must be an ioctl whose argument is a `T` (its size is encoded in `request`).
unsafe fn ioctl<T>(fd: BorrowedFd<'_>, request: u32, arg: &mut T) -> io::Result<()> {
    loop {
        // SAFETY: the caller guarantees `arg` has the type `request` expects; it is a live,
        // exclusive reference for the whole call. The request number is passed through
        // unchanged (`as _` adapts to bionic's `int` and glibc's `unsigned long`).
        let ret = unsafe { libc::ioctl(fd.as_raw_fd(), request as _, ptr::from_mut(arg)) };
        if ret != -1 {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

/// An open V4L2 device node.
#[derive(Debug)]
pub struct Device {
    file: File,
    path: PathBuf,
}

impl Device {
    /// Opens `path` (`/dev/videoN`) for reading and writing.
    ///
    /// # Errors
    ///
    /// Returns the open error (missing node, permission or SELinux denial).
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref();
        let file =
            pchal::devfs::open(path, pchal::devfs::Access::ReadWrite).map_err(io::Error::other)?;
        Ok(Self { file, path: path.to_owned() })
    }

    /// The node's path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The device name (`card`), if this node captures video with streaming I/O. UVC
    /// cameras also create metadata nodes, which return `None`.
    ///
    /// # Errors
    ///
    /// Returns the error of `VIDIOC_QUERYCAP`.
    pub fn capture_name(&self) -> io::Result<Option<String>> {
        let mut cap = Capability::default();
        // SAFETY: VIDIOC_QUERYCAP takes a `struct v4l2_capability`.
        unsafe { ioctl(self.file.as_fd(), VIDIOC_QUERYCAP, &mut cap)? };
        let wanted = CAP_VIDEO_CAPTURE | CAP_STREAMING;
        if cap.device_caps & wanted != wanted {
            return Ok(None);
        }
        let len = cap.card.iter().position(|byte| *byte == 0).unwrap_or(cap.card.len());
        Ok(Some(String::from_utf8_lossy(&cap.card[..len]).into_owned()))
    }

    /// Every discrete size of every native (not emulated) format in `fourccs`, with
    /// its discrete frame intervals. Sizes without any interval are left out.
    ///
    /// # Errors
    ///
    /// Returns an ioctl error other than the `EINVAL` that ends each enumeration.
    pub fn modes(&self, fourccs: &[FourCc]) -> io::Result<Vec<Mode>> {
        let fd = self.file.as_fd();
        let mut modes = Vec::new();
        for index in 0.. {
            let mut desc = FmtDesc { index, kind: BUF_TYPE_VIDEO_CAPTURE, ..Default::default() };
            // SAFETY: VIDIOC_ENUM_FMT takes a `struct v4l2_fmtdesc`.
            if !enumerated(unsafe { ioctl(fd, VIDIOC_ENUM_FMT, &mut desc) })? {
                break;
            }
            let fourcc = FourCc(desc.pixelformat);
            if desc.flags & FMT_FLAG_EMULATED != 0 || !fourccs.contains(&fourcc) {
                continue;
            }
            for size in self.sizes(fourcc)? {
                let intervals = self.intervals(fourcc, size)?;
                if !intervals.is_empty() {
                    modes.push(Mode { fourcc, size, intervals });
                }
            }
        }
        Ok(modes)
    }

    fn sizes(&self, fourcc: FourCc) -> io::Result<Vec<Size>> {
        let mut sizes = Vec::new();
        for index in 0.. {
            let mut size = FrmSizeEnum { index, pixel_format: fourcc.0, ..Default::default() };
            // SAFETY: VIDIOC_ENUM_FRAMESIZES takes a `struct v4l2_frmsizeenum`.
            if !enumerated(unsafe { ioctl(self.file.as_fd(), VIDIOC_ENUM_FRAMESIZES, &mut size) })?
                || size.kind != FRMSIZE_TYPE_DISCRETE
            {
                break;
            }
            sizes.push(Size { width: size.size[0], height: size.size[1] });
        }
        Ok(sizes)
    }

    fn intervals(&self, fourcc: FourCc, size: Size) -> io::Result<Vec<Interval>> {
        let mut intervals = Vec::new();
        for index in 0.. {
            let mut ival = FrmIvalEnum {
                index,
                pixel_format: fourcc.0,
                width: size.width,
                height: size.height,
                ..Default::default()
            };
            // SAFETY: VIDIOC_ENUM_FRAMEINTERVALS takes a `struct v4l2_frmivalenum`.
            let found = enumerated(unsafe {
                ioctl(self.file.as_fd(), VIDIOC_ENUM_FRAMEINTERVALS, &mut ival)
            })?;
            if !found || ival.kind != FRMIVAL_TYPE_DISCRETE {
                break;
            }
            let [numerator, denominator, ..] = ival.interval;
            if numerator != 0 && denominator != 0 {
                intervals.push(Interval { numerator, denominator });
            }
        }
        intervals.sort_by(|a, b| b.fps().total_cmp(&a.fps()));
        Ok(intervals)
    }
}

/// Maps the `EINVAL` that ends a V4L2 enumeration to `Ok(false)`.
fn enumerated(result: io::Result<()>) -> io::Result<bool> {
    match result {
        Ok(()) => Ok(true),
        Err(err) if err.raw_os_error() == Some(libc::EINVAL) => Ok(false),
        Err(err) => Err(err),
    }
}

/// One memory-mapped capture buffer.
#[derive(Debug)]
struct Mapping {
    address: NonNull<u8>,
    length: usize,
}

/// A running capture: buffers are mapped and queued, the device is streaming.
///
/// Dropping it stops streaming and frees the buffers.
#[derive(Debug)]
pub struct Stream {
    file: File,
    mode: Mode,
    interval: Interval,
    buffers: Vec<Mapping>,
}

// SAFETY: the mappings are plain shared memory owned by the stream; nothing else aliases
// them while the stream exists, and frames borrow the stream mutably.
unsafe impl Send for Stream {}

impl Stream {
    /// Sets `mode` at the frame `interval` (one of `mode.intervals`), maps `count`
    /// buffers and starts streaming.
    ///
    /// # Errors
    ///
    /// Returns the failing ioctl's or `mmap`'s error; the device is left stopped.
    pub fn start(device: &Device, mode: &Mode, interval: Interval, count: u32) -> io::Result<Self> {
        let file = device.file.try_clone()?;
        let fd = file.as_fd();
        let mut format = Format {
            kind: BUF_TYPE_VIDEO_CAPTURE,
            padding: 0,
            pix: PixFormat {
                width: mode.size.width,
                height: mode.size.height,
                pixelformat: mode.fourcc.0,
                field: FIELD_ANY,
                ..Default::default()
            },
            raw: [0; 19],
        };
        // SAFETY: VIDIOC_S_FMT takes a `struct v4l2_format`.
        unsafe { ioctl(fd, VIDIOC_S_FMT, &mut format)? };
        if format.pix.width != mode.size.width
            || format.pix.height != mode.size.height
            || format.pix.pixelformat != mode.fourcc.0
        {
            return Err(io::Error::other(format!(
                "{:?} {}x{} not accepted (got {:?} {}x{})",
                mode.fourcc,
                mode.size.width,
                mode.size.height,
                FourCc(format.pix.pixelformat),
                format.pix.width,
                format.pix.height
            )));
        }

        let mut parm = StreamParm {
            kind: BUF_TYPE_VIDEO_CAPTURE,
            capture: CaptureParm {
                timeperframe: [interval.numerator, interval.denominator],
                ..Default::default()
            },
            raw: [0; 160],
        };
        // SAFETY: VIDIOC_S_PARM takes a `struct v4l2_streamparm`.
        unsafe { ioctl(fd, VIDIOC_S_PARM, &mut parm)? };

        let mut request = RequestBuffers {
            count,
            kind: BUF_TYPE_VIDEO_CAPTURE,
            memory: MEMORY_MMAP,
            ..Default::default()
        };
        // SAFETY: VIDIOC_REQBUFS takes a `struct v4l2_requestbuffers`.
        unsafe { ioctl(fd, VIDIOC_REQBUFS, &mut request)? };

        let mut stream = Self {
            file,
            mode: mode.clone(),
            interval,
            buffers: Vec::with_capacity(request.count as usize),
        };
        for index in 0..request.count {
            let mut buffer = new_buffer(index);
            // SAFETY: VIDIOC_QUERYBUF takes a `struct v4l2_buffer`.
            unsafe { ioctl(stream.file.as_fd(), VIDIOC_QUERYBUF, &mut buffer)? };
            let length = buffer.length as usize;
            let offset = libc::off_t::try_from(buffer.offset).map_err(io::Error::other)?;
            // SAFETY: maps `length` bytes of the device at the offset the driver chose; the
            // result is checked before use and unmapped in `Drop`.
            let address = unsafe {
                libc::mmap(
                    ptr::null_mut(),
                    length,
                    libc::PROT_READ,
                    libc::MAP_SHARED,
                    stream.file.as_raw_fd(),
                    offset,
                )
            };
            if address == libc::MAP_FAILED {
                return Err(io::Error::last_os_error());
            }
            let address = NonNull::new(address.cast()).expect("mmap never returns NULL on success");
            stream.buffers.push(Mapping { address, length });
            stream.queue(index)?;
        }

        let mut kind = BUF_TYPE_VIDEO_CAPTURE as i32;
        // SAFETY: VIDIOC_STREAMON takes an `int` buffer type.
        unsafe { ioctl(stream.file.as_fd(), VIDIOC_STREAMON, &mut kind)? };
        Ok(stream)
    }

    /// The mode being captured.
    pub fn mode(&self) -> &Mode {
        &self.mode
    }

    /// The frame interval asked for.
    pub fn interval(&self) -> Interval {
        self.interval
    }

    fn queue(&self, index: u32) -> io::Result<()> {
        let mut buffer = new_buffer(index);
        // SAFETY: VIDIOC_QBUF takes a `struct v4l2_buffer`.
        unsafe { ioctl(self.file.as_fd(), VIDIOC_QBUF, &mut buffer) }
    }

    /// Waits up to `timeout` for the next frame.
    ///
    /// Returns `Ok(None)` on timeout. The frame's buffer goes back to the driver when the
    /// [`Frame`] is dropped.
    ///
    /// # Errors
    ///
    /// Returns the error of `poll` or `VIDIOC_DQBUF` (`ENODEV` once the camera is unplugged).
    pub fn next_frame(&mut self, timeout: Duration) -> io::Result<Option<Frame<'_>>> {
        if !pchal::poll::readable(self.file.as_fd(), timeout)? {
            return Ok(None);
        }
        let mut buffer = new_buffer(0);
        // SAFETY: VIDIOC_DQBUF takes a `struct v4l2_buffer`.
        unsafe { ioctl(self.file.as_fd(), VIDIOC_DQBUF, &mut buffer)? };
        let mapping = self.buffers.get(buffer.index as usize).ok_or_else(|| {
            io::Error::other(format!("driver returned unknown buffer {}", buffer.index))
        })?;
        let len = (buffer.bytesused as usize).min(mapping.length);
        // SAFETY: the mapping is `length` readable bytes; the driver does not write the
        // buffer again until it is queued, which only `Frame::drop` does.
        let data = unsafe { std::slice::from_raw_parts(mapping.address.as_ptr(), len) };
        Ok(Some(Frame { stream: self, index: buffer.index, data }))
    }
}

fn new_buffer(index: u32) -> Buffer {
    Buffer { index, kind: BUF_TYPE_VIDEO_CAPTURE, memory: MEMORY_MMAP, ..Default::default() }
}

impl Drop for Stream {
    fn drop(&mut self) {
        let mut kind = BUF_TYPE_VIDEO_CAPTURE as i32;
        // SAFETY: VIDIOC_STREAMOFF takes an `int` buffer type.
        if let Err(err) = unsafe { ioctl(self.file.as_fd(), VIDIOC_STREAMOFF, &mut kind) } {
            log::warn!("VIDIOC_STREAMOFF: {err}");
        }
        for mapping in &self.buffers {
            // SAFETY: each mapping came from `mmap` with this length and no frame borrows
            // it any more (frames borrow the stream).
            unsafe { libc::munmap(mapping.address.as_ptr().cast(), mapping.length) };
        }
        let mut request = RequestBuffers {
            count: 0,
            kind: BUF_TYPE_VIDEO_CAPTURE,
            memory: MEMORY_MMAP,
            ..Default::default()
        };
        // SAFETY: VIDIOC_REQBUFS takes a `struct v4l2_requestbuffers`.
        if let Err(err) = unsafe { ioctl(self.file.as_fd(), VIDIOC_REQBUFS, &mut request) } {
            log::warn!("VIDIOC_REQBUFS 0: {err}");
        }
    }
}

/// A captured frame, borrowed from its [`Stream`].
#[derive(Debug)]
pub struct Frame<'stream> {
    stream: &'stream Stream,
    index: u32,
    data: &'stream [u8],
}

impl Frame<'_> {
    /// The frame's bytes in the stream's pixel format.
    pub fn data(&self) -> &[u8] {
        self.data
    }
}

impl Drop for Frame<'_> {
    fn drop(&mut self) {
        if let Err(err) = self.stream.queue(self.index) {
            log::warn!("VIDIOC_QBUF {}: {err}", self.index);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn struct_layouts_match_videodev2() {
        // Sizes from linux/videodev2.h on x86_64.
        assert_eq!(size_of::<Capability>(), 104);
        assert_eq!(size_of::<FmtDesc>(), 64);
        assert_eq!(size_of::<FrmSizeEnum>(), 44);
        assert_eq!(size_of::<FrmIvalEnum>(), 52);
        assert_eq!(size_of::<Format>(), 208);
        assert_eq!(size_of::<StreamParm>(), 204);
        assert_eq!(size_of::<RequestBuffers>(), 20);
        assert_eq!(size_of::<Buffer>(), 88);
        assert_eq!(std::mem::offset_of!(Buffer, timestamp), 24);
        assert_eq!(std::mem::offset_of!(Buffer, sequence), 56);
        assert_eq!(std::mem::offset_of!(Buffer, offset), 64);
        assert_eq!(std::mem::offset_of!(Buffer, length), 72);
        assert_eq!(std::mem::offset_of!(Format, pix), 8);
    }

    #[test]
    fn ioctl_numbers_match_videodev2() {
        assert_eq!(VIDIOC_QUERYCAP, 0x8068_5600);
        assert_eq!(VIDIOC_ENUM_FMT, 0xc040_5602);
        assert_eq!(VIDIOC_ENUM_FRAMESIZES, 0xc02c_564a);
        assert_eq!(VIDIOC_ENUM_FRAMEINTERVALS, 0xc034_564b);
        assert_eq!(VIDIOC_S_FMT, 0xc0d0_5605);
        assert_eq!(VIDIOC_S_PARM, 0xc0cc_5616);
        assert_eq!(VIDIOC_REQBUFS, 0xc014_5608);
        assert_eq!(VIDIOC_QUERYBUF, 0xc058_5609);
        assert_eq!(VIDIOC_QBUF, 0xc058_560f);
        assert_eq!(VIDIOC_DQBUF, 0xc058_5611);
        assert_eq!(VIDIOC_STREAMON, 0x4004_5612);
        assert_eq!(VIDIOC_STREAMOFF, 0x4004_5613);
    }

    #[test]
    fn fourcc_names() {
        assert_eq!(format!("{:?}", FourCc::MJPEG), "MJPG");
        assert_eq!(FourCc::YUYV.0, 0x5659_5559);
    }

    #[test]
    fn interval_rates() {
        let interval = Interval { numerator: 1, denominator: 30 };
        assert!((interval.fps() - 30.0).abs() < 1e-9);
        assert_eq!(interval.nanos(), 33_333_333);
    }
}
