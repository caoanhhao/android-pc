//! USB Type-C ports from the kernel's typec class (`/sys/class/typec`). Pure sysfs
//! parsing, so it is tested on the host.

use std::fs;
use std::path::{Path, PathBuf};

/// A data or power role pair as the typec class names them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataRole {
    Host,
    Device,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerRole {
    Source,
    Sink,
}

/// One `portN` directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Port {
    /// `port0`, the name the framework shows.
    pub name: String,
    pub sysfs: PathBuf,
    /// The current data role, `None` when the file is missing or nothing is selected.
    pub data_role: Option<DataRole>,
    pub power_role: Option<PowerRole>,
    /// Whether the port can take either role (the files list both).
    pub dual_data: bool,
    pub dual_power: bool,
    /// Whether userspace may write the role files (the driver implements role swaps).
    pub data_role_writable: bool,
    pub power_role_writable: bool,
    /// Whether something is plugged in (`portN-partner` exists).
    pub partner: bool,
    /// The partner's PD support, from `portN-partner/supports_usb_power_delivery`.
    pub partner_pd: bool,
    /// `audio` or `debug` when an accessory is attached.
    pub accessory: Option<String>,
    /// `orientation` (`normal`, `reverse`, `unknown`), when the driver exposes it.
    pub orientation: Option<String>,
}

/// The ports under `class` (`/sys/class/typec`), sorted by name. Partners, cables and
/// alternate modes, which live in the same directory, are skipped.
pub fn ports(class: &Path) -> Vec<Port> {
    let Ok(entries) = fs::read_dir(class) else { return Vec::new() };
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| is_port_name(name))
        .collect();
    names.sort();
    names.into_iter().map(|name| read_port(class, &name)).collect()
}

/// `port<N>`, not `port0-partner`, `port0-cable` or `port0.1` (an alternate mode).
fn is_port_name(name: &str) -> bool {
    name.strip_prefix("port").is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// Reads `class/name`.
pub fn read_port(class: &Path, name: &str) -> Port {
    let dir = class.join(name);
    let partner_dir = class.join(format!("{name}-partner"));
    let data = read(&dir.join("data_role"));
    let power = read(&dir.join("power_role"));
    let partner = partner_dir.is_dir();
    Port {
        name: name.to_owned(),
        sysfs: fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone()),
        data_role: data.as_deref().and_then(selected).and_then(|r| match r {
            "host" => Some(DataRole::Host),
            "device" => Some(DataRole::Device),
            _ => None,
        }),
        power_role: power.as_deref().and_then(selected).and_then(|r| match r {
            "source" => Some(PowerRole::Source),
            "sink" => Some(PowerRole::Sink),
            _ => None,
        }),
        dual_data: data.as_deref().is_some_and(|d| d.contains("host") && d.contains("device")),
        dual_power: power.as_deref().is_some_and(|p| p.contains("source") && p.contains("sink")),
        data_role_writable: writable(&dir.join("data_role")),
        power_role_writable: writable(&dir.join("power_role")),
        partner,
        partner_pd: partner && read(&partner_dir.join("supports_usb_power_delivery")).as_deref() == Some("yes"),
        accessory: partner
            .then(|| read(&partner_dir.join("accessory_mode")))
            .flatten()
            .filter(|m| m != "none"),
        orientation: read(&dir.join("orientation")),
    }
}

/// The bracketed choice of a typec role file: `host [device]` → `device`. A file with a
/// single word (a fixed role) is that word.
pub fn selected(value: &str) -> Option<&str> {
    if let (Some(start), Some(end)) = (value.find('['), value.find(']')) {
        return (start < end).then(|| &value[start + 1..end]);
    }
    let words: Vec<&str> = value.split_whitespace().collect();
    (words.len() == 1).then(|| words[0])
}

fn read(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_owned())
}

fn writable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o200 != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_role() {
        assert_eq!(selected("host [device]"), Some("device"));
        assert_eq!(selected("[source] sink"), Some("source"));
        assert_eq!(selected("sink"), Some("sink"));
        assert_eq!(selected("host device"), None);
        assert_eq!(selected(""), None);
    }

    fn file(path: &Path, value: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, value).unwrap();
    }

    /// The tablet's MAX77705 port with a PC on it, as read on 2026-11-25.
    #[test]
    fn max77705_port_with_a_host() {
        let dir = tempfile::tempdir().unwrap();
        let class = dir.path();
        file(&class.join("port0/data_role"), "host [device]\n");
        file(&class.join("port0/power_role"), "source [sink]\n");
        file(&class.join("port0-partner/supports_usb_power_delivery"), "no\n");
        file(&class.join("port0-partner/accessory_mode"), "none\n");
        use std::os::unix::fs::PermissionsExt;
        for f in ["data_role", "power_role"] {
            fs::set_permissions(class.join("port0").join(f), fs::Permissions::from_mode(0o444)).unwrap();
        }

        let ports = ports(class);
        assert_eq!(ports.len(), 1);
        let port = &ports[0];
        assert_eq!(port.name, "port0");
        assert_eq!(port.data_role, Some(DataRole::Device));
        assert_eq!(port.power_role, Some(PowerRole::Sink));
        assert!(port.dual_data && port.dual_power);
        assert!(!port.data_role_writable && !port.power_role_writable);
        assert!(port.partner && !port.partner_pd);
        assert_eq!(port.accessory, None);
        assert_eq!(port.orientation, None);
    }

    #[test]
    fn unplugged_port_and_other_entries() {
        let dir = tempfile::tempdir().unwrap();
        let class = dir.path();
        file(&class.join("port1/data_role"), "[host] device\n");
        file(&class.join("port1/power_role"), "[source] sink\n");
        file(&class.join("port1/orientation"), "unknown\n");
        fs::create_dir_all(class.join("port1-cable")).unwrap();
        fs::create_dir_all(class.join("port1.0")).unwrap();
        let ports = ports(class);
        assert_eq!(ports.len(), 1);
        assert_eq!(ports[0].data_role, Some(DataRole::Host));
        assert!(!ports[0].partner);
        assert_eq!(ports[0].orientation.as_deref(), Some("unknown"));
    }
}
