//! System properties. Host builds have no property service: reads see nothing and
//! writes are dropped.

use std::time::Duration;

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

/// Property `name` as a boolean (`1`/`true`/`y`/`yes`/`on` and their opposites).
pub fn read_bool(name: &str, default: bool) -> bool {
    match read(name).as_deref() {
        Some("1" | "true" | "y" | "yes" | "on") => true,
        Some("0" | "false" | "n" | "no" | "off") => false,
        _ => default,
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

/// Waits up to `timeout` for property `name` to become `value`; whether it did.
pub fn wait_for(name: &str, value: &str, timeout: Duration) -> bool {
    #[cfg(target_os = "android")]
    {
        use rustutils::android::system_properties::PropertyWatcher;
        PropertyWatcher::new(name)
            .and_then(|mut watcher| watcher.wait_for_value(value, Some(timeout)))
            .is_ok()
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = (name, value, timeout);
        false
    }
}
