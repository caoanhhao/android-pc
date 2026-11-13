//! Short paths for the AIDL types, and copies/comparisons of AIDL parcelables.
//!
//! The Rust backend derives only `Debug` for these parcelables (they have no
//! `@RustDerive`), so [`dup`] and [`same`] go through a parcel: a value is copied by
//! writing it and reading it back, and two values are equal when their parcels are
//! byte-identical. Only plain data goes this way (no binders or file descriptors).

pub use android_hardware_audio_common::aidl::android::hardware::audio::common::{
    SinkMetadata::SinkMetadata, SourceMetadata::SourceMetadata,
};
pub use android_hardware_audio_core::aidl::android::hardware::audio::core::{
    AudioPatch::AudioPatch,
    AudioRoute::AudioRoute,
    IModule::{
        BnModule, IModule, OpenInputStreamArguments::OpenInputStreamArguments,
        OpenInputStreamReturn::OpenInputStreamReturn,
        OpenOutputStreamArguments::OpenOutputStreamArguments,
        OpenOutputStreamReturn::OpenOutputStreamReturn, ScreenRotation::ScreenRotation,
        SupportedPlaybackRateFactors::SupportedPlaybackRateFactors,
    },
    ModuleDebug::ModuleDebug,
    StreamDescriptor::StreamDescriptor,
    VendorParameter::VendorParameter,
};
pub use android_media_audio_common_types::aidl::android::media::audio::common::{
    AudioChannelLayout::{self as channel_layout, AudioChannelLayout},
    AudioDevice::AudioDevice,
    AudioDeviceAddress::AudioDeviceAddress,
    AudioDeviceDescription::{self as device_description, AudioDeviceDescription},
    AudioDeviceType::AudioDeviceType,
    AudioFormatDescription::AudioFormatDescription,
    AudioFormatType::AudioFormatType,
    AudioIoFlags::AudioIoFlags,
    AudioMMapPolicy::AudioMMapPolicy,
    AudioMMapPolicyInfo::AudioMMapPolicyInfo,
    AudioMMapPolicyType::AudioMMapPolicyType,
    AudioMode::AudioMode,
    AudioOutputFlags::AudioOutputFlags,
    AudioPort::AudioPort,
    AudioPortConfig::AudioPortConfig,
    AudioPortDeviceExt::{self as device_ext, AudioPortDeviceExt},
    AudioPortExt::AudioPortExt,
    AudioPortMixExt::AudioPortMixExt,
    AudioProfile::AudioProfile,
    FlushFromFrameSupport::FlushFromFrameSupport,
    Int::Int,
    MicrophoneInfo::MicrophoneInfo,
    PcmType::PcmType,
};
use binder::binder_impl::{Deserialize, Parcel, Serialize};
use binder::{ExceptionCode, Status};

/// Copies a plain-data parcelable.
pub fn dup<T: Serialize + Deserialize>(value: &T) -> T {
    let mut parcel = Parcel::new();
    parcel.write(value).expect("plain data parcelables serialize");
    // SAFETY: position 0 is the start of the data just written.
    unsafe { parcel.set_data_position(0) }.expect("position 0 is valid");
    parcel.read().expect("a parcelable reads back what it wrote")
}

/// Whether two plain-data parcelables are equal.
pub fn same<T: Serialize>(a: &T, b: &T) -> bool {
    let bytes = |value: &T| {
        let mut parcel = Parcel::new();
        parcel.write(value).expect("plain data parcelables serialize");
        parcel.marshal()
    };
    bytes(a) == bytes(b)
}

/// A service-specific failure as the framework expects it: an exception code, logged.
pub fn error(code: ExceptionCode, message: &str) -> Status {
    log::error!("{message}");
    Status::new_exception_str(code, Some(message))
}

pub fn illegal_argument(message: &str) -> Status {
    error(ExceptionCode::ILLEGAL_ARGUMENT, message)
}

pub fn illegal_state(message: &str) -> Status {
    error(ExceptionCode::ILLEGAL_STATE, message)
}

pub fn unsupported() -> Status {
    Status::new_exception(ExceptionCode::UNSUPPORTED_OPERATION, None)
}

/// A PCM format description (`AudioFormatDescription{type: PCM, pcm}`).
pub fn pcm_format(pcm: PcmType) -> AudioFormatDescription {
    AudioFormatDescription { r#type: AudioFormatType::PCM, pcm, encoding: String::new() }
}

/// Bytes per sample of a PCM format, `None` for non-PCM or unknown formats.
pub fn pcm_sample_bytes(format: &AudioFormatDescription) -> Option<usize> {
    if format.r#type != AudioFormatType::PCM || !format.encoding.is_empty() {
        return None;
    }
    match format.pcm {
        PcmType::UINT_8_BIT => Some(1),
        PcmType::INT_16_BIT => Some(2),
        PcmType::INT_24_BIT => Some(3),
        PcmType::INT_32_BIT | PcmType::FIXED_Q_8_24 | PcmType::FLOAT_32_BIT => Some(4),
        _ => None,
    }
}

/// Channel count of a layout or index mask, `None` for other channel masks.
pub fn channel_count(layout: &AudioChannelLayout) -> Option<usize> {
    match layout {
        AudioChannelLayout::LayoutMask(mask) | AudioChannelLayout::IndexMask(mask) => {
            Some(mask.count_ones() as usize).filter(|&n| n > 0)
        }
        _ => None,
    }
}

/// Bytes per frame of a PCM configuration, `None` if it is not a PCM layout/index mask.
pub fn frame_bytes(format: &AudioFormatDescription, layout: &AudioChannelLayout) -> Option<usize> {
    Some(pcm_sample_bytes(format)? * channel_count(layout)?)
}

/// Whether bit `flag` is set in an `AudioIoFlags` bit field.
#[cfg(test)]
pub fn has_flag(flags: i32, flag: i32) -> bool {
    flags & (1 << flag) != 0
}

/// `RecordTrackMetadata`/`PlaybackTrackMetadata` tags must be vendor extensions:
/// `VX_[A-Z0-9]{3,}_[_A-Z0-9]+`.
pub fn is_vendor_extension(tag: &str) -> bool {
    let Some(rest) = tag.strip_prefix("VX_") else { return false };
    let Some((vendor, name)) = rest.split_once('_') else { return false };
    let upper_digit = |c: char| c.is_ascii_uppercase() || c.is_ascii_digit();
    vendor.len() >= 3
        && vendor.chars().all(upper_digit)
        && !name.is_empty()
        && name.chars().all(|c| upper_digit(c) || c == '_')
}

/// `PlaybackTrackMetadata.codecProvenance` must be empty or an audio MIME type.
pub fn is_valid_codec_provenance(mime: Option<&str>) -> bool {
    match mime {
        None | Some("") => true,
        Some(mime) => mime.strip_prefix("audio/").is_some_and(|subtype| !subtype.is_empty()),
    }
}
