//! JPEG decoding (MJPEG frames) and encoding (still captures) through `jpeg/pcjpeg.c`, a
//! small C layer over libjpeg's raw data interface. AOSP's `libjpeg` lacks the TurboJPEG
//! API, and libjpeg's longjmp error handling has to stay in C.

use std::ffi::{c_char, c_int, c_void, CStr};
use std::fmt;
use std::ptr;

use crate::image::{PlaneRef, Size, Yuv420};

/// `struct pcjpeg_info`.
#[repr(C)]
#[derive(Debug, Default)]
struct Info {
    width: c_int,
    height: c_int,
    components: c_int,
    plane_width: [c_int; 3],
    plane_height: [c_int; 3],
    plane_stride: [c_int; 3],
    plane_rows: [c_int; 3],
}

// Linked through Soong's `static_libs: ["libpcjpeg"]`.
extern "C" {
    fn pcjpeg_decode(
        jpeg: *const u8,
        jpeg_len: usize,
        info: *mut Info,
        planes: *const *mut u8,
        message: *mut c_char,
        message_len: usize,
    ) -> c_int;
    fn pcjpeg_encode_i420(
        y: *const u8,
        cb: *const u8,
        cr: *const u8,
        width: c_int,
        height: c_int,
        quality: c_int,
        jpeg: *mut *mut u8,
        jpeg_len: *mut usize,
        message: *mut c_char,
        message_len: usize,
    ) -> c_int;
}

/// A libjpeg failure, with its message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JpegError(String);

impl fmt::Display for JpegError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "jpeg: {}", self.0)
    }
}

impl std::error::Error for JpegError {}

/// Room for libjpeg's messages (`JMSG_LENGTH_MAX` is 200).
type Message = [c_char; 256];

fn check(ret: c_int, message: &Message) -> Result<(), JpegError> {
    if ret == 0 {
        return Ok(());
    }
    // SAFETY: the C side always NUL-terminates `message` (snprintf), and it starts zeroed.
    let text = unsafe { CStr::from_ptr(message.as_ptr()) };
    Err(JpegError(text.to_string_lossy().into_owned()))
}

fn to_usize(value: c_int) -> usize {
    usize::try_from(value).expect("libjpeg reports non-negative dimensions")
}

/// Decodes `jpeg` (one MJPEG frame), which must be `expected` in size.
///
/// # Errors
///
/// Returns [`JpegError`] for corrupt data (webcams send a damaged first frame now and
/// then), an unsupported colour space, or a frame of another size.
pub fn decode(jpeg: &[u8], expected: Size) -> Result<Yuv420, JpegError> {
    let mut info = Info::default();
    let mut message: Message = [0; 256];
    // SAFETY: `jpeg` is readable for its length; null planes only read the header.
    let ret = unsafe {
        pcjpeg_decode(
            jpeg.as_ptr(),
            jpeg.len(),
            &mut info,
            ptr::null(),
            message.as_mut_ptr(),
            message.len(),
        )
    };
    check(ret, &message)?;
    let size = Size {
        width: u32::try_from(info.width).unwrap_or(0),
        height: u32::try_from(info.height).unwrap_or(0),
    };
    if size != expected {
        return Err(JpegError(format!(
            "frame is {}x{}, expected {}x{}",
            info.width, info.height, expected.width, expected.height
        )));
    }

    let components = to_usize(info.components);
    let mut planes: [Vec<u8>; 3] = Default::default();
    for (index, plane) in planes.iter_mut().enumerate().take(components) {
        *plane = vec![0; to_usize(info.plane_stride[index]) * to_usize(info.plane_rows[index])];
    }
    let pointers = planes.each_mut().map(|plane| plane.as_mut_ptr());
    // SAFETY: each used plane is `plane_stride * plane_rows` bytes, as the header pass of
    // the same data said; greyscale leaves the unused (dangling, empty) planes alone.
    let ret = unsafe {
        pcjpeg_decode(
            jpeg.as_ptr(),
            jpeg.len(),
            &mut info,
            pointers.as_ptr(),
            message.as_mut_ptr(),
            message.len(),
        )
    };
    check(ret, &message)?;

    let plane = |index: usize| PlaneRef {
        data: &planes[index],
        size: Size {
            width: info.plane_width[index].unsigned_abs(),
            height: info.plane_height[index].unsigned_abs(),
        },
        stride: to_usize(info.plane_stride[index]),
    };
    let chroma = (components == 3).then(|| [plane(1), plane(2)]);
    Ok(Yuv420::from_planes(plane(0), chroma))
}

/// Encodes `image` as baseline 4:2:0 JPEG at `quality` (1-100).
///
/// # Errors
///
/// Returns [`JpegError`] if libjpeg fails or the image is too large.
pub fn encode(image: &Yuv420, quality: u8) -> Result<Vec<u8>, JpegError> {
    let size = image.size();
    let too_large = || JpegError("image too large".into());
    let width = c_int::try_from(size.width).map_err(|_| too_large())?;
    let height = c_int::try_from(size.height).map_err(|_| too_large())?;
    let [y, cb, cr] = image.planes();
    let mut jpeg: *mut u8 = ptr::null_mut();
    let mut len = 0;
    let mut message: Message = [0; 256];
    // SAFETY: the planes are tightly packed I420 planes of `size`; on success the C side
    // hands over a malloc'ed buffer of `len` bytes, freed below.
    let ret = unsafe {
        pcjpeg_encode_i420(
            y.as_ptr(),
            cb.as_ptr(),
            cr.as_ptr(),
            width,
            height,
            c_int::from(quality.clamp(1, 100)),
            &mut jpeg,
            &mut len,
            message.as_mut_ptr(),
            message.len(),
        )
    };
    check(ret, &message)?;
    // SAFETY: on success `jpeg` points to `len` bytes of encoded data.
    let encoded = unsafe { std::slice::from_raw_parts(jpeg, len) }.to_vec();
    // SAFETY: allocated with malloc by the C side for this call and not used afterwards.
    unsafe { libc::free(jpeg.cast::<c_void>()) };
    Ok(encoded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_a_flat_frame() {
        let size = Size { width: 64, height: 48 };
        let frame = Yuv420::filled(size, 120, 90, 160);
        let jpeg = encode(&frame, 95).unwrap();
        assert_eq!(&jpeg[..2], &[0xff, 0xd8], "SOI marker");
        let decoded = decode(&jpeg, size).unwrap();
        assert_eq!(decoded.size(), size);
        for (plane, want) in decoded.planes().into_iter().zip([120, 90, 160]) {
            assert!(plane.iter().all(|value| value.abs_diff(want) <= 2), "plane near {want}");
        }
    }

    #[test]
    fn round_trips_odd_sizes() {
        let size = Size { width: 37, height: 21 };
        let jpeg = encode(&Yuv420::filled(size, 200, 60, 70), 90).unwrap();
        let decoded = decode(&jpeg, size).unwrap();
        assert!(decoded.planes()[0].iter().all(|value| value.abs_diff(200) <= 2));
    }

    #[test]
    fn rejects_garbage_and_wrong_sizes() {
        let size = Size { width: 16, height: 16 };
        let err = decode(&[0, 1, 2, 3], size).unwrap_err();
        assert!(!err.0.is_empty(), "libjpeg's message is kept");
        let jpeg = encode(&Yuv420::filled(size, 0, 128, 128), 50).unwrap();
        assert!(decode(&jpeg, Size { width: 32, height: 16 }).is_err());
    }
}
