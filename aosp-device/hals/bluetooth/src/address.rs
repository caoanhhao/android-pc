//! The controller's public address on boards whose controller has none of its own
//! (Qualcomm UART chips report the NVM file's default address, and the kernel keeps them
//! unconfigured): the one the vendor stored, else one derived from a stable seed.

use std::fs;
use std::path::Path;

use log::{info, warn};

/// A Bluetooth device address in display order (`1c:2b:3a:4d:5e:df` is
/// `[0x1c, 0x2b, 0x3a, 0x4d, 0x5e, 0xdf]`).
pub type BdAddr = [u8; 6];

/// Parses `xx:xx:xx:xx:xx:xx` (Samsung's efs/bluetooth/bt_addr), ignoring surrounding
/// whitespace.
pub fn parse(text: &str) -> Option<BdAddr> {
    let mut address = [0; 6];
    let mut parts = text.trim().split(':');
    for byte in &mut address {
        let part = parts.next()?;
        if part.len() != 2 {
            return None;
        }
        *byte = u8::from_str_radix(part, 16).ok()?;
    }
    (parts.next().is_none() && address != [0; 6]).then_some(address)
}

/// A stable address from `seed` (the device serial number): FNV-1a over it, with the two
/// top bits cleared so it never looks like a random LE address type.
pub fn derive(seed: &str) -> BdAddr {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in seed.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let bytes = hash.to_be_bytes();
    let mut address = [0; 6];
    address.copy_from_slice(&bytes[..6]);
    address[0] &= 0x3f;
    address
}

/// The address from `file`, else derived from `seed`, else none.
pub fn resolve(file: Option<&Path>, seed: Option<&str>) -> Option<BdAddr> {
    if let Some(file) = file {
        match fs::read_to_string(file).map(|text| parse(&text)) {
            Ok(Some(address)) => {
                info!("public address from {}", file.display());
                return Some(address);
            }
            Ok(None) => warn!("{}: not an address", file.display()),
            Err(err) => warn!("cannot read {}: {err}", file.display()),
        }
    }
    let seed = seed.filter(|seed| !seed.is_empty())?;
    info!("public address derived from the serial number");
    Some(derive(seed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bt_addr() {
        assert_eq!(parse("1C:2B:3A:4D:5E:DF\n"), Some([0x1c, 0x2b, 0x3a, 0x4d, 0x5e, 0xdf]));
        assert_eq!(parse("1C:2B:3A:4D:5E"), None);
        assert_eq!(parse("1C:2B:3A:4D:5E:DF:00"), None);
        assert_eq!(parse("1C:2B:3A:4D:5E:XX"), None);
        assert_eq!(parse("1:CE:94:0D:D5:DF0"), None);
        assert_eq!(parse("00:00:00:00:00:00"), None);
    }

    #[test]
    fn derives_a_stable_address() {
        assert_eq!(derive("R00A000ABCD"), derive("R00A000ABCD"));
        assert_ne!(derive("R00A000ABCD"), derive("R00A000ABCE"));
        assert_eq!(derive("R00A000ABCD")[0] & 0xc0, 0);
    }

    #[test]
    fn falls_back_to_the_seed() {
        let missing = Path::new("/nonexistent/bt_addr");
        assert_eq!(resolve(Some(missing), Some("abc")), Some(derive("abc")));
        assert_eq!(resolve(Some(missing), Some("")), None);
        assert_eq!(resolve(None, None), None);
    }
}
