//! System properties that name the Wi-Fi interfaces (the same ones AOSP's default HAL
//! reads). Host builds have no property service and see none of them.

/// The value of property `name`, if it is set and not empty.
pub fn read(name: &str) -> Option<String> {
    #[cfg(target_os = "android")]
    {
        use rustutils::android::system_properties;
        system_properties::read(name).ok().flatten().filter(|value| !value.is_empty())
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = name;
        None
    }
}

/// Sets property `name`; failures are logged, since no caller can act on them.
pub fn write(name: &str, value: &str) {
    #[cfg(target_os = "android")]
    {
        use rustutils::android::system_properties;
        if let Err(err) = system_properties::write(name, value) {
            log::warn!("cannot set {name}={value}: {err}");
        }
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = (name, value);
    }
}
