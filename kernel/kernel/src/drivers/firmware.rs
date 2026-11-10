//! Device firmware files (Linux's request_firmware).
//!
//! Drivers probe before any file system with firmware is mounted, so the
//! files come from the boot media: a Limine module whose path ends in the
//! file name (`tools/mkusb.sh` puts them in `boot/firmware/` on the
//! stick's ESP), else `lib/firmware/<name>` in the initramfs (newc cpio).
//! Files are uncompressed (linux-firmware ships zstd; the tools
//! decompress them).

/// The contents of firmware file `name` (e.g. "iwlwifi-so-a0-gf-a0-89.ucode").
pub fn request(name: &str) -> Option<&'static [u8]> {
    if let Some(m) = crate::boot::module(name) {
        log::info!("firmware: {name} from the boot media ({} KiB)", m.len() / 1024);
        return Some(m);
    }
    let f = crate::boot::initramfs().and_then(|cpio| cpio_find(cpio, name));
    match f {
        Some(d) => log::info!("firmware: {name} from the initramfs ({} KiB)", d.len() / 1024),
        None => log::warn!("firmware: {name} not found"),
    }
    f
}

fn hex(b: &[u8]) -> Option<usize> {
    usize::from_str_radix(core::str::from_utf8(b).ok()?, 16).ok()
}

/// `lib/firmware/<name>` in a newc cpio archive.
fn cpio_find(cpio: &'static [u8], name: &str) -> Option<&'static [u8]> {
    let mut off = 0;
    while off + 110 <= cpio.len() && &cpio[off..off + 6] == b"070701" {
        let h = &cpio[off..off + 110];
        let size = hex(&h[54..62])?;
        let namesize = hex(&h[94..102])?;
        let name_start = off + 110;
        let data_start = (name_start + namesize + 3) & !3;
        let path = cpio.get(name_start..name_start + namesize.saturating_sub(1))?;
        if path == b"TRAILER!!!" {
            break;
        }
        let path = path.strip_prefix(b"./").unwrap_or(path);
        if path.strip_prefix(b"lib/firmware/").is_some_and(|p| p == name.as_bytes()) {
            return cpio.get(data_start..data_start + size);
        }
        off = (data_start + size + 3) & !3;
    }
    None
}
