//! Planar YUV 4:2:0 frames and the conversions between camera and output formats.
//!
//! Every captured frame is first turned into a [`Yuv420`] at the capture size. Outputs
//! are then cut from it: the largest centred crop with the output's aspect ratio, scaled
//! with nearest-neighbour sampling, into any YCbCr 4:2:0 layout ([`Layout`]) or into
//! another [`Yuv420`] for the JPEG encoder.

use std::fmt;

/// Width and height in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Size {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

impl Size {
    /// The number of pixels.
    pub fn area(self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }
}

/// A YUV 4:2:0 image with three tightly packed planes (I420).
#[derive(Clone, PartialEq, Eq)]
pub struct Yuv420 {
    size: Size,
    y: Vec<u8>,
    cb: Vec<u8>,
    cr: Vec<u8>,
}

impl fmt::Debug for Yuv420 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Yuv420").field("size", &self.size).finish_non_exhaustive()
    }
}

/// Why a captured frame could not be converted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConvertError(pub String);

impl fmt::Display for ConvertError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ConvertError {}

/// The size of a chroma plane of a 4:2:0 image.
pub fn chroma_size(size: Size) -> Size {
    Size { width: size.width.div_ceil(2), height: size.height.div_ceil(2) }
}

fn area(size: Size) -> usize {
    size.width as usize * size.height as usize
}

/// One plane of a decoded image at its own resolution, rows `stride` bytes apart.
#[derive(Debug, Clone, Copy)]
pub struct PlaneRef<'data> {
    /// The samples.
    pub data: &'data [u8],
    /// Width and height in samples.
    pub size: Size,
    /// Bytes between rows.
    pub stride: usize,
}

impl PlaneRef<'_> {
    fn sample(&self, x: usize, y: usize) -> u8 {
        self.data[y * self.stride + x]
    }
}

impl Yuv420 {
    /// A frame of `size` filled with one colour.
    pub fn filled(size: Size, y: u8, cb: u8, cr: u8) -> Self {
        let chroma = area(chroma_size(size));
        Self { size, y: vec![y; area(size)], cb: vec![cb; chroma], cr: vec![cr; chroma] }
    }

    /// The image size.
    pub fn size(&self) -> Size {
        self.size
    }

    /// The Y, Cb and Cr planes; chroma planes are [`chroma_size`] samples.
    pub fn planes(&self) -> [&[u8]; 3] {
        [&self.y, &self.cb, &self.cr]
    }

    /// Converts packed YUYV (Y0 Cb Y1 Cr) of `size`, averaging chroma of row pairs.
    ///
    /// # Errors
    ///
    /// Returns [`ConvertError`] if `data` holds less than a full frame.
    pub fn from_yuyv(data: &[u8], size: Size) -> Result<Self, ConvertError> {
        let (width, height) = (size.width as usize, size.height as usize);
        let stride = width * 2;
        if data.len() < stride * height || width % 2 != 0 {
            return Err(ConvertError(format!(
                "YUYV frame of {} bytes for {width}x{height}",
                data.len()
            )));
        }
        let mut frame = Self::filled(size, 0, 0, 0);
        for (row, line) in data.chunks_exact(stride).take(height).enumerate() {
            let out = &mut frame.y[row * width..(row + 1) * width];
            for (pixel, pair) in out.iter_mut().zip(line.as_chunks::<2>().0) {
                *pixel = pair[0];
            }
        }
        let chroma = chroma_size(size);
        for cy in 0..chroma.height as usize {
            let top = &data[2 * cy * stride..][..stride];
            let bottom = &data[(2 * cy + 1).min(height - 1) * stride..][..stride];
            for cx in 0..chroma.width as usize {
                let average = |offset: usize| {
                    (u16::from(top[offset]) + u16::from(bottom[offset])).div_ceil(2) as u8
                };
                let index = cy * chroma.width as usize + cx;
                frame.cb[index] = average(4 * cx + 1);
                frame.cr[index] = average(4 * cx + 3);
            }
        }
        Ok(frame)
    }

    /// Builds a 4:2:0 frame from decoded planes of any chroma subsampling (a JPEG's 4:2:2,
    /// 4:4:4, ...), sampling chroma at the nearest position. Without chroma planes
    /// (greyscale) chroma is neutral.
    pub fn from_planes(luma: PlaneRef<'_>, chroma: Option<[PlaneRef<'_>; 2]>) -> Self {
        let size = luma.size;
        let mut frame = Self::filled(size, 0, 128, 128);
        let width = size.width as usize;
        for (row, out) in frame.y.chunks_exact_mut(width).enumerate() {
            out.copy_from_slice(&luma.data[row * luma.stride..][..width]);
        }
        if let Some([cb, cr]) = chroma {
            let out_size = chroma_size(size);
            let (out_width, out_height) = (out_size.width as usize, out_size.height as usize);
            for cy in 0..out_height {
                let sy = cy * cb.size.height as usize / out_height;
                for cx in 0..out_width {
                    let sx = cx * cb.size.width as usize / out_width;
                    frame.cb[cy * out_width + cx] = cb.sample(sx, sy);
                    frame.cr[cy * out_width + cx] = cr.sample(sx, sy);
                }
            }
        }
        frame
    }

    /// The centred crop of `self` with `target`'s aspect ratio, scaled to `target`.
    pub fn scaled(&self, target: Size) -> Self {
        if target == self.size {
            return self.clone();
        }
        let mut out = Self::filled(target, 0, 0, 0);
        let packed = |width: u32| Plane { offset: 0, pixel_stride: 1, row_stride: width as usize };
        let chroma = packed(chroma_size(target).width);
        let sampler = Sampler::new(self, target);
        sampler.fill(&mut out.y, packed(target.width), Component::Luma);
        sampler.fill(&mut out.cb, chroma, Component::Cb);
        sampler.fill(&mut out.cr, chroma, Component::Cr);
        out
    }

    /// Writes the centred crop of `self`, scaled to `target`, into `buffer` laid out as
    /// `layout` (planar, semi-planar or any other 4:2:0 arrangement).
    ///
    /// # Errors
    ///
    /// Returns [`ConvertError`] if the layout reaches beyond `buffer`.
    pub fn write_to(
        &self,
        buffer: &mut [u8],
        layout: &Layout,
        target: Size,
    ) -> Result<(), ConvertError> {
        let needed = layout.required_len(target);
        if buffer.len() < needed {
            return Err(ConvertError(format!(
                "buffer of {} bytes, layout needs {needed} for {}x{}",
                buffer.len(),
                target.width,
                target.height
            )));
        }
        let sampler = Sampler::new(self, target);
        sampler.fill(buffer, layout.y, Component::Luma);
        sampler.fill(buffer, layout.cb, Component::Cb);
        sampler.fill(buffer, layout.cr, Component::Cr);
        Ok(())
    }
}

/// Where one component's samples are in a buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Plane {
    /// Byte offset of the first sample.
    pub offset: usize,
    /// Bytes from one sample to the next in a row (2 for interleaved Cb/Cr).
    pub pixel_stride: usize,
    /// Bytes from one row to the next.
    pub row_stride: usize,
}

/// A YCbCr 4:2:0 buffer layout (the chroma planes are subsampled by two both ways).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    /// Luma.
    pub y: Plane,
    /// Blue-difference chroma.
    pub cb: Plane,
    /// Red-difference chroma.
    pub cr: Plane,
}

impl Layout {
    /// Bytes a buffer of `size` needs for this layout.
    pub fn required_len(&self, size: Size) -> usize {
        let end = |plane: Plane, size: Size| {
            plane.offset
                + (size.height as usize - 1) * plane.row_stride
                + (size.width as usize - 1) * plane.pixel_stride
                + 1
        };
        let chroma = chroma_size(size);
        end(self.y, size).max(end(self.cb, chroma)).max(end(self.cr, chroma))
    }

    /// The tightly packed planar I420 layout of `size`.
    pub fn i420(size: Size) -> Self {
        let chroma = chroma_size(size);
        let luma = area(size);
        Self {
            y: Plane { offset: 0, pixel_stride: 1, row_stride: size.width as usize },
            cb: Plane { offset: luma, pixel_stride: 1, row_stride: chroma.width as usize },
            cr: Plane {
                offset: luma + area(chroma),
                pixel_stride: 1,
                row_stride: chroma.width as usize,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Component {
    Luma,
    Cb,
    Cr,
}

/// Maps output coordinates to source coordinates of a centred crop.
struct Sampler<'src> {
    source: &'src Yuv420,
    target: Size,
    /// Crop rectangle in luma samples: x, y, width, height.
    crop: (usize, usize, usize, usize),
}

impl<'src> Sampler<'src> {
    fn new(source: &'src Yuv420, target: Size) -> Self {
        let (sw, sh) = (u64::from(source.size.width), u64::from(source.size.height));
        let (tw, th) = (u64::from(target.width), u64::from(target.height));
        // Keep the whole width if the target is wider than the source, else the height.
        let (cw, ch) = if sw * th >= tw * sh { (sh * tw / th, sh) } else { (sw, sw * th / tw) };
        let crop = (((sw - cw) / 2) as usize, ((sh - ch) / 2) as usize, cw as usize, ch as usize);
        Self { source, target, crop }
    }

    fn fill(&self, out: &mut [u8], plane: Plane, component: Component) {
        let (source, subsampling, size) = match component {
            Component::Luma => (&self.source.y, 1, self.target),
            Component::Cb => (&self.source.cb, 2, chroma_size(self.target)),
            Component::Cr => (&self.source.cr, 2, chroma_size(self.target)),
        };
        let source_width = (self.source.size.width as usize).div_ceil(subsampling);
        let source_height = (self.source.size.height as usize).div_ceil(subsampling);
        let (x0, y0, cw, ch) = self.crop;
        let (width, height) = (size.width as usize, size.height as usize);
        let columns: Vec<usize> = (0..width)
            .map(|x| ((x0 + x * cw / width) / subsampling).min(source_width - 1))
            .collect();
        for y in 0..height {
            let sy = ((y0 + y * ch / height) / subsampling).min(source_height - 1);
            let row = &source[sy * source_width..][..source_width];
            let start = plane.offset + y * plane.row_stride;
            for (x, sx) in columns.iter().enumerate() {
                out[start + x * plane.pixel_stride] = row[*sx];
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn size(width: u32, height: u32) -> Size {
        Size { width, height }
    }

    /// A frame whose luma is the column index and whose chroma is the chroma row index.
    fn gradient(frame_size: Size) -> Yuv420 {
        let mut frame = Yuv420::filled(frame_size, 0, 0, 0);
        let width = frame_size.width as usize;
        for (i, pixel) in frame.y.iter_mut().enumerate() {
            *pixel = (i % width) as u8;
        }
        let chroma_width = chroma_size(frame_size).width as usize;
        for (i, pixel) in frame.cb.iter_mut().enumerate() {
            *pixel = (i / chroma_width) as u8;
        }
        frame
    }

    #[test]
    fn yuyv_unpacks_and_averages_chroma() {
        // 2x2: row 0 = Y 10,11 Cb 100 Cr 200; row 1 = Y 12,13 Cb 102 Cr 202.
        let data = [10, 100, 11, 200, 12, 102, 13, 202];
        let frame = Yuv420::from_yuyv(&data, size(2, 2)).unwrap();
        assert_eq!(frame.planes(), [&[10, 11, 12, 13][..], &[101][..], &[201][..]]);
        assert!(Yuv420::from_yuyv(&data[..6], size(2, 2)).is_err());
    }

    #[test]
    fn planes_resample_422_chroma() {
        // 4x2 luma, 4:2:2 chroma (2x2).
        let luma = [1, 2, 3, 4, 5, 6, 7, 8];
        let cb: &'static [u8] = &[10, 11, 12, 13];
        let cr: &'static [u8] = &[20, 21, 22, 23];
        let plane = |data: &'static [u8], width, height| PlaneRef {
            data,
            size: size(width, height),
            stride: width as usize,
        };
        let frame = Yuv420::from_planes(
            PlaneRef { data: &luma, size: size(4, 2), stride: 4 },
            Some([plane(cb, 2, 2), plane(cr, 2, 2)]),
        );
        assert_eq!(frame.planes(), [&luma[..], &[10, 11][..], &[20, 21][..]]);
    }

    #[test]
    fn greyscale_has_neutral_chroma() {
        let luma = [7; 4];
        let frame =
            Yuv420::from_planes(PlaneRef { data: &luma, size: size(2, 2), stride: 2 }, None);
        assert_eq!(frame.planes()[1], &[128]);
    }

    #[test]
    fn same_size_is_a_copy() {
        let frame = gradient(size(8, 4));
        assert_eq!(frame.scaled(size(8, 4)), frame);
    }

    #[test]
    fn narrower_target_crops_the_centre() {
        // 8x4 to 4x4: the centre columns 2..6.
        let scaled = gradient(size(8, 4)).scaled(size(4, 4));
        assert_eq!(&scaled.planes()[0][..4], &[2, 3, 4, 5]);
    }

    #[test]
    fn downscale_samples_evenly() {
        let scaled = gradient(size(8, 4)).scaled(size(4, 2));
        assert_eq!(&scaled.planes()[0][..4], &[0, 2, 4, 6]);
        assert_eq!(scaled.planes()[1], &[0, 0]);
    }

    #[test]
    fn writes_semi_planar_layouts() {
        let frame = Yuv420::filled(size(4, 2), 16, 1, 2);
        // NV21-like: Y plane, then interleaved Cr/Cb.
        let layout = Layout {
            y: Plane { offset: 0, pixel_stride: 1, row_stride: 4 },
            cr: Plane { offset: 8, pixel_stride: 2, row_stride: 4 },
            cb: Plane { offset: 9, pixel_stride: 2, row_stride: 4 },
        };
        let mut buffer = vec![0; 12];
        frame.write_to(&mut buffer, &layout, size(4, 2)).unwrap();
        assert_eq!(buffer, [16, 16, 16, 16, 16, 16, 16, 16, 2, 1, 2, 1]);
        assert!(frame.write_to(&mut buffer[..11], &layout, size(4, 2)).is_err());
    }

    #[test]
    fn i420_layout_size() {
        assert_eq!(Layout::i420(size(4, 2)).required_len(size(4, 2)), 12);
        assert_eq!(Layout::i420(size(5, 3)).required_len(size(5, 3)), 15 + 2 * 6);
    }
}
