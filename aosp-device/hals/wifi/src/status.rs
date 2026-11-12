//! `WifiStatusCode` errors, which the HAL returns as service-specific binder errors.

use std::fmt::Display;

use android_hardware_wifi::aidl::android::hardware::wifi::WifiStatusCode::WifiStatusCode;
use binder::Status;

/// The binder status for `code`.
pub fn error(code: WifiStatusCode) -> Status {
    Status::new_service_specific_error(code.0, None)
}

/// The binder status for `code`, with `message` for the client's log.
pub fn error_with(code: WifiStatusCode, message: impl Display) -> Status {
    Status::new_service_specific_error_str(code.0, Some(message.to_string()))
}

/// `ERROR_NOT_SUPPORTED`, for the optional features this HAL does not have.
pub fn not_supported<T>() -> binder::Result<T> {
    Err(error(WifiStatusCode::ERROR_NOT_SUPPORTED))
}

/// `ERROR_INVALID_ARGS`, for example for an interface name the chip does not have.
pub fn invalid_args<T>() -> binder::Result<T> {
    Err(error(WifiStatusCode::ERROR_INVALID_ARGS))
}

/// `ERROR_UNKNOWN` carrying the failure, for kernel calls that went wrong.
pub fn unknown(err: impl Display) -> Status {
    error_with(WifiStatusCode::ERROR_UNKNOWN, err)
}
