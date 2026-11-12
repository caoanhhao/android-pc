//! Camera metadata (`camera_metadata_t`, `system/media/camera`) in Rust.
//!
//! AIDL carries metadata as the raw bytes of a C `camera_metadata_t`: a 48-byte header, an
//! array of 16-byte entries and a data area. [`Metadata`] keeps the entries in a sorted map
//! and converts from and to that layout. Values are typed through [`Tag`], so a tag can
//! only be set with the element type the framework expects for it (the framework rejects
//! the whole buffer when a type does not match).

use std::collections::BTreeMap;
use std::fmt;
use std::marker::PhantomData;

/// `camera_metadata_rational_t`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Rational {
    /// Numerator.
    pub numerator: i32,
    /// Denominator.
    pub denominator: i32,
}

/// An element type a metadata entry can hold (`TYPE_BYTE` ... `TYPE_RATIONAL`).
pub trait Value: Copy + private::Sealed {
    /// The `camera_metadata_type` number.
    const TYPE: u8;
    /// Size of one element in bytes.
    const SIZE: usize;
    /// Appends the element in native byte order.
    fn write(self, out: &mut Vec<u8>);
    /// Reads one element from exactly [`Self::SIZE`] bytes.
    fn read(bytes: &[u8]) -> Self;
}

mod private {
    pub trait Sealed {}
}

macro_rules! impl_value {
    ($type:ty, $id:expr) => {
        impl private::Sealed for $type {}
        impl Value for $type {
            const TYPE: u8 = $id;
            const SIZE: usize = std::mem::size_of::<$type>();
            fn write(self, out: &mut Vec<u8>) {
                out.extend_from_slice(&self.to_ne_bytes());
            }
            fn read(bytes: &[u8]) -> Self {
                <$type>::from_ne_bytes(bytes.try_into().expect("caller passes SIZE bytes"))
            }
        }
    };
}

impl_value!(u8, 0);
impl_value!(i32, 1);
impl_value!(f32, 2);
impl_value!(i64, 3);
impl_value!(f64, 4);

impl private::Sealed for Rational {}
impl Value for Rational {
    const TYPE: u8 = 5;
    const SIZE: usize = 8;
    fn write(self, out: &mut Vec<u8>) {
        self.numerator.write(out);
        self.denominator.write(out);
    }
    fn read(bytes: &[u8]) -> Self {
        Self { numerator: i32::read(&bytes[..4]), denominator: i32::read(&bytes[4..]) }
    }
}

/// Element sizes indexed by type number.
const TYPE_SIZES: [usize; 6] = [1, 4, 4, 8, 8, 8];

/// A metadata tag whose entries hold elements of type `T`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Tag<T> {
    id: u32,
    element: PhantomData<fn() -> T>,
}

impl<T: Value> Tag<T> {
    /// The tag with number `id` (`section << 16 | index`).
    pub const fn new(id: u32) -> Self {
        Self { id, element: PhantomData }
    }

    /// The tag number.
    pub const fn id(self) -> u32 {
        self.id
    }
}

/// One entry: its element type and the elements as native-endian bytes.
#[derive(Debug, Clone, PartialEq)]
struct Entry {
    kind: u8,
    data: Vec<u8>,
}

impl Entry {
    fn count(&self) -> usize {
        self.data.len() / TYPE_SIZES[usize::from(self.kind)]
    }
}

/// A set of metadata entries, at most one per tag.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Metadata {
    entries: BTreeMap<u32, Entry>,
}

/// Why bytes are not a valid `camera_metadata_t`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseMetadataError(&'static str);

impl fmt::Display for ParseMetadataError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid camera metadata: {}", self.0)
    }
}

impl std::error::Error for ParseMetadataError {}

/// `sizeof(camera_metadata_t)`.
const HEADER_SIZE: usize = 48;
/// `sizeof(camera_metadata_buffer_entry_t)`.
const ENTRY_SIZE: usize = 16;
/// `DATA_ALIGNMENT`: out-of-line values start on 8-byte boundaries.
const DATA_ALIGNMENT: usize = 8;
/// `CURRENT_METADATA_VERSION`.
const VERSION: u32 = 1;
/// `FLAG_SORTED`: entries are sorted by tag, so lookups can bisect.
const FLAG_SORTED: u32 = 1;
/// `CAMERA_METADATA_INVALID_VENDOR_ID`.
const NO_VENDOR_ID: u64 = u64::MAX;
/// Values of at most this many bytes are stored inside the entry.
const INLINE_SIZE: usize = 4;

const fn align(value: usize) -> usize {
    value.next_multiple_of(DATA_ALIGNMENT)
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_ne_bytes(bytes[offset..offset + 4].try_into().expect("4 bytes"))
}

impl Metadata {
    /// An empty set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether there are no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Sets `tag` to `values`, replacing any previous entry.
    pub fn set<T: Value>(&mut self, tag: Tag<T>, values: &[T]) {
        let mut data = Vec::with_capacity(values.len() * T::SIZE);
        for value in values {
            value.write(&mut data);
        }
        self.entries.insert(tag.id, Entry { kind: T::TYPE, data });
    }

    /// The elements of `tag`, or `None` if it is absent or holds another element type.
    pub fn get<T: Value>(&self, tag: Tag<T>) -> Option<Vec<T>> {
        let entry = self.entries.get(&tag.id).filter(|entry| entry.kind == T::TYPE)?;
        Some(entry.data.chunks_exact(T::SIZE).map(T::read).collect())
    }

    /// The first element of `tag`, if any.
    pub fn first<T: Value>(&self, tag: Tag<T>) -> Option<T> {
        let entry = self.entries.get(&tag.id).filter(|entry| entry.kind == T::TYPE)?;
        entry.data.get(..T::SIZE).map(T::read)
    }

    /// Whether an entry for tag number `id` exists.
    pub fn contains(&self, id: u32) -> bool {
        self.entries.contains_key(&id)
    }

    /// The tag numbers present, in ascending order.
    pub fn tags(&self) -> impl Iterator<Item = u32> + '_ {
        self.entries.keys().copied()
    }

    /// Copies every entry of `other` into `self`, replacing entries with the same tag.
    pub fn update(&mut self, other: &Metadata) {
        self.entries.extend(other.entries.iter().map(|(id, entry)| (*id, entry.clone())));
    }

    /// Encodes the entries as a packed, sorted `camera_metadata_t`.
    pub fn to_bytes(&self) -> Vec<u8> {
        let data_start = align(HEADER_SIZE + ENTRY_SIZE * self.entries.len());
        let mut entries = Vec::with_capacity(ENTRY_SIZE * self.entries.len());
        let mut data = Vec::new();
        for (id, entry) in &self.entries {
            entries.extend_from_slice(&id.to_ne_bytes());
            let count = u32::try_from(entry.count()).expect("entry count fits u32");
            entries.extend_from_slice(&count.to_ne_bytes());
            if entry.data.len() > INLINE_SIZE {
                let offset = u32::try_from(data.len()).expect("data offset fits u32");
                entries.extend_from_slice(&offset.to_ne_bytes());
                data.extend_from_slice(&entry.data);
                data.resize(align(data.len()), 0);
            } else {
                let mut inline = [0; INLINE_SIZE];
                inline[..entry.data.len()].copy_from_slice(&entry.data);
                entries.extend_from_slice(&inline);
            }
            entries.extend_from_slice(&[entry.kind, 0, 0, 0]);
        }

        let size = data_start + data.len();
        let as_u32 = |value: usize| u32::try_from(value).expect("metadata size fits u32");
        let mut out = Vec::with_capacity(size);
        for field in [
            as_u32(size),
            VERSION,
            FLAG_SORTED,
            as_u32(self.entries.len()), // entry_count
            as_u32(self.entries.len()), // entry_capacity
            as_u32(HEADER_SIZE),        // entries_start
            as_u32(data.len()),         // data_count
            as_u32(data.len()),         // data_capacity
            as_u32(data_start),
            0, // padding
        ] {
            out.extend_from_slice(&field.to_ne_bytes());
        }
        out.extend_from_slice(&NO_VENDOR_ID.to_ne_bytes());
        out.extend_from_slice(&entries);
        out.resize(data_start, 0);
        out.extend_from_slice(&data);
        out
    }

    /// Decodes a `camera_metadata_t`, checking every offset against the buffer.
    ///
    /// # Errors
    ///
    /// Returns [`ParseMetadataError`] if the header, an entry or a value lies outside
    /// `bytes` or an entry has an unknown element type.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ParseMetadataError> {
        if bytes.len() < HEADER_SIZE {
            return Err(ParseMetadataError("shorter than the header"));
        }
        let field = |index: usize| u32_at(bytes, index * 4) as usize;
        let (size, entry_count, entries_start, data_capacity, data_start) =
            (field(0), field(3), field(5), field(7), field(8));
        if size > bytes.len() {
            return Err(ParseMetadataError("size beyond the buffer"));
        }
        let entries_end = entry_count
            .checked_mul(ENTRY_SIZE)
            .and_then(|len| len.checked_add(entries_start))
            .ok_or(ParseMetadataError("entry table overflows"))?;
        let data_end = data_start
            .checked_add(data_capacity)
            .ok_or(ParseMetadataError("data area overflows"))?;
        if entries_end > size || data_end > size {
            return Err(ParseMetadataError("entries or data beyond the size"));
        }

        let mut metadata = Self::new();
        for raw in bytes[entries_start..entries_end].as_chunks::<ENTRY_SIZE>().0 {
            let id = u32_at(raw, 0);
            let count = u32_at(raw, 4) as usize;
            let kind = raw[12];
            let element = *TYPE_SIZES
                .get(usize::from(kind))
                .ok_or(ParseMetadataError("unknown element type"))?;
            let len =
                count.checked_mul(element).ok_or(ParseMetadataError("entry size overflows"))?;
            let data = if len > INLINE_SIZE {
                let start = data_start + u32_at(raw, 8) as usize;
                bytes
                    .get(start..)
                    .and_then(|rest| rest.get(..len))
                    .filter(|_| start + len <= data_end)
                    .ok_or(ParseMetadataError("value beyond the data area"))?
            } else {
                &raw[8..8 + len]
            };
            metadata.entries.insert(id, Entry { kind, data: data.to_vec() });
        }
        Ok(metadata)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BYTE_TAG: Tag<u8> = Tag::new(0x0001_0000);
    const INT_TAG: Tag<i32> = Tag::new(0x0001_0001);
    const LONG_TAG: Tag<i64> = Tag::new(0x000d_0002);
    const FLOAT_TAG: Tag<f32> = Tag::new(0x000d_0003);
    const RATIONAL_TAG: Tag<Rational> = Tag::new(0x0001_0002);

    fn sample() -> Metadata {
        let mut metadata = Metadata::new();
        metadata.set(INT_TAG, &[640, 480, 1280, 720]);
        metadata.set(BYTE_TAG, &[4]);
        metadata.set(LONG_TAG, &[33_333_333]);
        metadata.set(FLOAT_TAG, &[1.0]);
        metadata.set(RATIONAL_TAG, &[Rational { numerator: 1, denominator: 3 }]);
        metadata
    }

    #[test]
    fn round_trips() {
        let metadata = sample();
        let bytes = metadata.to_bytes();
        assert_eq!(Metadata::from_bytes(&bytes).unwrap(), metadata);
        assert_eq!(bytes.len() % DATA_ALIGNMENT, 0);
    }

    #[test]
    fn layout_matches_camera_metadata_t() {
        let bytes = sample().to_bytes();
        assert_eq!(u32_at(&bytes, 0) as usize, bytes.len(), "size");
        assert_eq!(u32_at(&bytes, 4), VERSION);
        assert_eq!(u32_at(&bytes, 12), 5, "entry count");
        assert_eq!(u32_at(&bytes, 20), 48, "entries start");
        let data_start = u32_at(&bytes, 32) as usize;
        assert_eq!(data_start, align(48 + 5 * 16));
        // Entries are sorted: the byte tag 0x10000 comes first and is stored inline.
        assert_eq!(u32_at(&bytes, 48), 0x0001_0000);
        assert_eq!(bytes[48 + 8], 4);
        assert_eq!(bytes[48 + 12], u8::TYPE);
        // The four ints (16 bytes) are the first out-of-line value, at data offset 0.
        assert_eq!(u32_at(&bytes, 64), 0x0001_0001);
        assert_eq!(u32_at(&bytes, 64 + 8), 0);
        assert_eq!(u32_at(&bytes, data_start + 4), 480);
    }

    #[test]
    fn typed_access() {
        let metadata = sample();
        assert_eq!(metadata.first(BYTE_TAG), Some(4));
        assert_eq!(metadata.get(INT_TAG), Some(vec![640, 480, 1280, 720]));
        // The same tag number read as another type is absent.
        assert_eq!(metadata.first(Tag::<i32>::new(BYTE_TAG.id())), None);
        assert!(metadata.contains(LONG_TAG.id()));
    }

    #[test]
    fn update_replaces_entries() {
        let mut base = sample();
        let mut newer = Metadata::new();
        newer.set(BYTE_TAG, &[1]);
        base.update(&newer);
        assert_eq!(base.first(BYTE_TAG), Some(1));
        assert_eq!(base.len(), 5);
    }

    #[test]
    fn empty_round_trips() {
        let bytes = Metadata::new().to_bytes();
        assert_eq!(bytes.len(), HEADER_SIZE);
        assert!(Metadata::from_bytes(&bytes).unwrap().is_empty());
    }

    #[test]
    fn rejects_truncated_buffers() {
        let bytes = sample().to_bytes();
        assert!(Metadata::from_bytes(&bytes[..40]).is_err());
        assert!(Metadata::from_bytes(&bytes[..bytes.len() - 8]).is_err());
    }
}
