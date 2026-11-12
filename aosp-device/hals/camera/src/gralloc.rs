//! Writing into gralloc buffers through the stable C mapper API (`AIMapper`, IMapper 5).
//!
//! The mapper is the vendor's `/vendor/lib64/hw/mapper.<suffix>.so` (minigbm on this
//! device). Buffers arrive as AIDL `NativeHandle`s; they are imported once and cached by
//! the session, then locked for CPU writes per capture.

use std::ffi::{c_int, c_void, CString};
use std::fmt;
use std::fs;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::ptr::NonNull;
use std::sync::OnceLock;
use std::time::Duration;

use android_hardware_common::aidl::android::hardware::common::NativeHandle::NativeHandle;
use log::info;
use pccamera::image::{Layout, Plane};

/// Where the vendor mapper libraries live.
const MAPPER_DIR: &str = "/vendor/lib64/hw";

/// `BufferUsage::CPU_WRITE_OFTEN`.
const CPU_WRITE_OFTEN: u64 = 0x30;

/// `StandardMetadataType::PLANE_LAYOUTS`.
const PLANE_LAYOUTS: i64 = 15;

/// How long a buffer's acquire fence may take to signal.
const FENCE_TIMEOUT: Duration = Duration::from_secs(1);

/// `PlaneLayoutComponentType` values.
const COMPONENT_Y: i64 = 1;
const COMPONENT_CB: i64 = 2;
const COMPONENT_CR: i64 = 4;

type BufferHandle = *const c_void;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

/// A function pointer this HAL never calls.
type Slot = Option<unsafe extern "C" fn()>;

/// The first members of `AIMapperV5`, in declaration order.
#[repr(C)]
struct MapperV5 {
    import_buffer: unsafe extern "C" fn(*const c_void, *mut BufferHandle) -> i32,
    free_buffer: unsafe extern "C" fn(BufferHandle) -> i32,
    _get_transport_size: Slot,
    lock: unsafe extern "C" fn(BufferHandle, u64, Rect, c_int, *mut *mut c_void) -> i32,
    unlock: unsafe extern "C" fn(BufferHandle, *mut c_int) -> i32,
    _flush_locked_buffer: Slot,
    _reread_locked_buffer: Slot,
    _get_metadata: Slot,
    get_standard_metadata: unsafe extern "C" fn(BufferHandle, i64, *mut c_void, usize) -> i32,
}

/// `AIMapper`: the version is aligned like `max_align_t`.
#[repr(C, align(16))]
struct AIMapper {
    version: u32,
    v5: MapperV5,
}

/// A gralloc failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrallocError(String);

impl fmt::Display for GrallocError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "gralloc: {}", self.0)
    }
}

impl std::error::Error for GrallocError {}

fn check(what: &str, status: i32) -> Result<(), GrallocError> {
    if status == 0 {
        Ok(())
    } else {
        Err(GrallocError(format!("{what} failed: AIMapper_Error {status}")))
    }
}

/// The loaded vendor mapper.
pub struct Mapper(&'static MapperV5);

impl fmt::Debug for Mapper {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Mapper")
    }
}

impl Mapper {
    /// The process-wide mapper, loaded on first use.
    ///
    /// # Errors
    ///
    /// Returns [`GrallocError`] if no `mapper.*.so` loads or it is older than IMapper 5.
    pub fn get() -> Result<&'static Self, GrallocError> {
        static MAPPER: OnceLock<Result<Mapper, GrallocError>> = OnceLock::new();
        MAPPER.get_or_init(Self::load).as_ref().map_err(Clone::clone)
    }

    fn load() -> Result<Self, GrallocError> {
        let mut libraries: Vec<_> = fs::read_dir(MAPPER_DIR)
            .map_err(|err| GrallocError(format!("{MAPPER_DIR}: {err}")))?
            .filter_map(|entry| entry.ok().map(|entry| entry.file_name()))
            .filter_map(|name| name.into_string().ok())
            .filter(|name| name.starts_with("mapper.") && name.ends_with(".so"))
            .collect();
        libraries.sort();
        let name = libraries
            .first()
            .ok_or_else(|| GrallocError(format!("no mapper.*.so in {MAPPER_DIR}")))?;
        let path = CString::new(format!("{MAPPER_DIR}/{name}")).expect("no NUL in a file name");
        // SAFETY: dlopen with a valid C string; the library stays loaded for the process.
        let library = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
        if library.is_null() {
            return Err(GrallocError(format!("cannot load {name}")));
        }
        // SAFETY: dlsym on a live handle with a NUL-terminated name.
        let symbol = unsafe { libc::dlsym(library, c"AIMapper_loadIMapper".as_ptr()) };
        if symbol.is_null() {
            return Err(GrallocError(format!("{name} has no AIMapper_loadIMapper")));
        }
        // SAFETY: the stable C API declares
        // `AIMapper_Error AIMapper_loadIMapper(AIMapper** outImplementation)`.
        let load: unsafe extern "C" fn(*mut *mut AIMapper) -> i32 =
            unsafe { std::mem::transmute(symbol) };
        let mut mapper: *mut AIMapper = std::ptr::null_mut();
        // SAFETY: `mapper` is a valid out-pointer.
        check("AIMapper_loadIMapper", unsafe { load(&mut mapper) })?;
        // SAFETY: on success the mapper is a static table owned by the library.
        let mapper: &'static AIMapper = unsafe { mapper.as_ref() }
            .ok_or_else(|| GrallocError("AIMapper_loadIMapper returned null".into()))?;
        if mapper.version < 5 {
            return Err(GrallocError(format!("{name} is IMapper {}", mapper.version)));
        }
        info!("loaded {name} (IMapper {})", mapper.version);
        Ok(Self(&mapper.v5))
    }
}

/// One plane as `PlaneLayout` describes it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PlaneLayout {
    /// `(type, offsetInBits, sizeInBits)` of each component in the plane.
    pub components: Vec<(i64, i64, i64)>,
    /// Byte offset of the plane from the locked address.
    pub offset: i64,
    /// Bits from one sample to the next.
    pub sample_increment_bits: i64,
    /// Bytes from one row to the next.
    pub stride: i64,
    /// Bytes the plane occupies.
    pub total_size: i64,
}

/// Decodes gralloc's encoded `PLANE_LAYOUTS` metadata (see `IMapperMetadataTypes.h`):
/// a header (type name, value), then the planes with their components.
pub fn decode_plane_layouts(bytes: &[u8]) -> Option<Vec<PlaneLayout>> {
    struct Reader<'bytes>(&'bytes [u8]);
    impl Reader<'_> {
        fn int(&mut self) -> Option<i64> {
            let (value, rest) = self.0.split_first_chunk::<8>()?;
            self.0 = rest;
            Some(i64::from_ne_bytes(*value))
        }
        fn skip_string(&mut self) -> Option<()> {
            let len = usize::try_from(self.int()?).ok()?;
            self.0 = self.0.get(len..)?;
            Some(())
        }
    }
    let mut reader = Reader(bytes);
    reader.skip_string()?;
    if reader.int()? != PLANE_LAYOUTS {
        return None;
    }
    let planes = reader.int()?;
    let mut layouts = Vec::new();
    for _ in 0..planes {
        let mut plane = PlaneLayout::default();
        for _ in 0..reader.int()? {
            reader.skip_string()?;
            plane.components.push((reader.int()?, reader.int()?, reader.int()?));
        }
        plane.offset = reader.int()?;
        plane.sample_increment_bits = reader.int()?;
        plane.stride = reader.int()?;
        let _width = reader.int()?;
        let _height = reader.int()?;
        plane.total_size = reader.int()?;
        let _subsampling = (reader.int()?, reader.int()?);
        layouts.push(plane);
    }
    Some(layouts)
}

/// The YCbCr layout of a buffer from its planes, if it has 8-bit Y, Cb and Cr.
pub fn ycbcr_layout(planes: &[PlaneLayout]) -> Option<Layout> {
    let find = |kind: i64| {
        planes.iter().find_map(|plane| {
            let (_, offset_bits, size_bits) =
                *plane.components.iter().find(|component| component.0 == kind)?;
            if size_bits != 8 || offset_bits % 8 != 0 {
                return None;
            }
            Some(Plane {
                offset: usize::try_from(plane.offset + offset_bits / 8).ok()?,
                pixel_stride: usize::try_from(plane.sample_increment_bits / 8).ok()?,
                row_stride: usize::try_from(plane.stride).ok()?,
            })
        })
    };
    Some(Layout { y: find(COMPONENT_Y)?, cb: find(COMPONENT_CB)?, cr: find(COMPONENT_CR)? })
}

/// The bytes the planes span from the locked address.
fn mapped_len(planes: &[PlaneLayout]) -> usize {
    planes
        .iter()
        .filter_map(|plane| usize::try_from(plane.offset + plane.total_size).ok())
        .max()
        .unwrap_or(0)
}

/// An imported gralloc buffer, freed on drop.
pub struct Buffer {
    mapper: &'static Mapper,
    handle: NonNull<c_void>,
}

impl fmt::Debug for Buffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Buffer").field(&self.handle).finish()
    }
}

// SAFETY: an imported handle may be used from any thread (IMapper is thread-safe); the
// session serialises locking per buffer.
unsafe impl Send for Buffer {}
// SAFETY: as above; `&Buffer` methods only call thread-safe mapper functions.
unsafe impl Sync for Buffer {}

impl Buffer {
    /// Imports `handle` (the mapper clones its file descriptors).
    ///
    /// # Errors
    ///
    /// Returns [`GrallocError`] if the mapper cannot load or rejects the handle.
    pub fn import(handle: &NativeHandle) -> Result<Self, GrallocError> {
        let mapper = Mapper::get()?;
        // native_handle_t: version (= its header size), numFds, numInts, fds, ints.
        let header = [3 * 4, handle.fds.len() as c_int, handle.ints.len() as c_int];
        let raw: Vec<c_int> = header
            .into_iter()
            .chain(handle.fds.iter().map(AsRawFd::as_raw_fd))
            .chain(handle.ints.iter().copied())
            .collect();
        let mut imported: BufferHandle = std::ptr::null();
        // SAFETY: `raw` is a well-formed native_handle_t whose fds stay open for the call
        // (they belong to `handle`); the mapper clones what it keeps.
        check("importBuffer", unsafe {
            (mapper.0.import_buffer)(raw.as_ptr().cast(), &mut imported)
        })?;
        let handle = NonNull::new(imported.cast_mut())
            .ok_or_else(|| GrallocError("importBuffer returned null".into()))?;
        Ok(Self { mapper, handle })
    }

    fn plane_layouts(&self) -> Result<Vec<PlaneLayout>, GrallocError> {
        let mut bytes = vec![0u8; 1024];
        loop {
            // SAFETY: `bytes` is writable for its length; the mapper writes at most that.
            let needed = unsafe {
                (self.mapper.0.get_standard_metadata)(
                    self.handle.as_ptr(),
                    PLANE_LAYOUTS,
                    bytes.as_mut_ptr().cast(),
                    bytes.len(),
                )
            };
            let needed = usize::try_from(needed)
                .map_err(|_| GrallocError(format!("PLANE_LAYOUTS: error {needed}")))?;
            if needed <= bytes.len() {
                bytes.truncate(needed);
                break;
            }
            bytes.resize(needed, 0);
        }
        decode_plane_layouts(&bytes)
            .ok_or_else(|| GrallocError("cannot decode PLANE_LAYOUTS".into()))
    }

    /// Waits for `acquire_fence` and locks the whole buffer of `width` x `height` for
    /// CPU writes.
    ///
    /// # Errors
    ///
    /// Returns [`GrallocError`] if the fence does not signal in time or the lock fails.
    pub fn lock(
        &self,
        width: u32,
        height: u32,
        acquire_fence: Option<BorrowedFd<'_>>,
    ) -> Result<Locked<'_>, GrallocError> {
        if let Some(fence) = acquire_fence {
            let signalled = pchal::poll::readable(fence, FENCE_TIMEOUT)
                .map_err(|err| GrallocError(format!("acquire fence: {err}")))?;
            if !signalled {
                return Err(GrallocError("acquire fence timed out".into()));
            }
        }
        let planes = self.plane_layouts()?;
        let len = mapped_len(&planes);
        let rect = Rect {
            left: 0,
            top: 0,
            right: i32::try_from(width).unwrap_or(i32::MAX),
            bottom: i32::try_from(height).unwrap_or(i32::MAX),
        };
        let mut address: *mut c_void = std::ptr::null_mut();
        // SAFETY: the handle is imported; `address` is a valid out-pointer; -1 = no fence.
        check("lock", unsafe {
            (self.mapper.0.lock)(self.handle.as_ptr(), CPU_WRITE_OFTEN, rect, -1, &mut address)
        })?;
        let address = NonNull::new(address.cast::<u8>())
            .ok_or_else(|| GrallocError("lock returned null".into()))?;
        Ok(Locked { buffer: self, address, len, planes })
    }
}

impl Drop for Buffer {
    fn drop(&mut self) {
        // SAFETY: the handle was imported by this mapper and is freed once.
        unsafe { (self.mapper.0.free_buffer)(self.handle.as_ptr()) };
    }
}

/// A buffer locked for CPU writes, unlocked on drop.
pub struct Locked<'buffer> {
    buffer: &'buffer Buffer,
    address: NonNull<u8>,
    len: usize,
    planes: Vec<PlaneLayout>,
}

impl Locked<'_> {
    /// The buffer's bytes as its planes span them.
    pub fn bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: the mapper mapped the buffer at `address`, and the plane layouts it
        // reported span `len` bytes from there; the lock is exclusive while `self` lives.
        unsafe { std::slice::from_raw_parts_mut(self.address.as_ptr(), self.len) }
    }

    /// The YCbCr layout, if the buffer is a YUV buffer.
    pub fn ycbcr_layout(&self) -> Option<Layout> {
        ycbcr_layout(&self.planes)
    }
}

impl Drop for Locked<'_> {
    fn drop(&mut self) {
        let mut release_fence: c_int = -1;
        // SAFETY: the buffer is locked by `self`; `release_fence` is a valid out-pointer.
        let status = unsafe {
            (self.buffer.mapper.0.unlock)(self.buffer.handle.as_ptr(), &mut release_fence)
        };
        if status != 0 {
            log::warn!("unlock failed: AIMapper_Error {status}");
        }
        if release_fence >= 0 {
            // SAFETY: the mapper handed us this fence fd; we own and close it after the
            // CPU writes it guards have completed (they are synchronous).
            let fence = unsafe { OwnedFd::from_raw_fd(release_fence) };
            if let Err(err) = pchal::poll::readable(fence.as_fd(), FENCE_TIMEOUT) {
                log::warn!("release fence: {err}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode(planes: &[(i64, Vec<(i64, i64)>, i64, i64, i64)]) -> Vec<u8> {
        let mut out = Vec::new();
        let int = |out: &mut Vec<u8>, value: i64| out.extend_from_slice(&value.to_ne_bytes());
        let name = b"android.hardware.graphics.common.StandardMetadataType";
        int(&mut out, name.len() as i64);
        out.extend_from_slice(name);
        int(&mut out, PLANE_LAYOUTS);
        int(&mut out, planes.len() as i64);
        for (offset, components, increment, stride, total) in planes {
            int(&mut out, components.len() as i64);
            for (kind, offset_bits) in components {
                let name = b"android.hardware.graphics.common.PlaneLayoutComponentType";
                int(&mut out, name.len() as i64);
                out.extend_from_slice(name);
                for value in [*kind, *offset_bits, 8] {
                    int(&mut out, value);
                }
            }
            for value in [*offset, *increment, *stride, 0, 0, *total, 1, 1] {
                int(&mut out, value);
            }
        }
        out
    }

    #[test]
    fn decodes_nv12() {
        // 64x32 NV12 with a 64-byte stride: Y plane, then interleaved CbCr.
        let bytes = encode(&[
            (0, vec![(COMPONENT_Y, 0)], 8, 64, 2048),
            (2048, vec![(COMPONENT_CB, 0), (COMPONENT_CR, 8)], 16, 64, 1024),
        ]);
        let planes = decode_plane_layouts(&bytes).unwrap();
        assert_eq!(mapped_len(&planes), 3072);
        let layout = ycbcr_layout(&planes).unwrap();
        assert_eq!(layout.y, Plane { offset: 0, pixel_stride: 1, row_stride: 64 });
        assert_eq!(layout.cb, Plane { offset: 2048, pixel_stride: 2, row_stride: 64 });
        assert_eq!(layout.cr, Plane { offset: 2049, pixel_stride: 2, row_stride: 64 });
    }

    #[test]
    fn rejects_truncated_metadata_and_blobs() {
        let bytes = encode(&[(0, vec![(COMPONENT_Y, 0)], 8, 64, 2048)]);
        assert!(decode_plane_layouts(&bytes[..bytes.len() - 1]).is_none());
        let planes = decode_plane_layouts(&bytes).unwrap();
        assert!(ycbcr_layout(&planes).is_none(), "no chroma");
    }
}
