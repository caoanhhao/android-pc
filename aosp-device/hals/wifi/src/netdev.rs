//! Network interface ioctls: link state, MAC address, ethtool driver information.
//!
//! These are the ioctls the platform's `hal_wifi` policy allows on a UDP socket
//! (`SIOCSIFFLAGS`, `SIOCSIFHWADDR`, `SIOCETHTOOL`, plus the `SIOCG*` every domain has).

use std::ffi::CString;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

/// A MAC address.
pub type MacAddress = [u8; 6];

/// `ETHTOOL_GDRVINFO`.
const ETHTOOL_GDRVINFO: u32 = 0x3;
/// `ETHTOOL_GPERMADDR`.
const ETHTOOL_GPERMADDR: u32 = 0x20;
/// `ARPHRD_ETHER`: the address family of an Ethernet (and Wi-Fi) hardware address.
const ARPHRD_ETHER: libc::sa_family_t = 1;
/// The size of `ethtool_drvinfo`'s string fields.
const ETHTOOL_STRING: usize = 32;
/// The room `ethtool_perm_addr` gets for the address (MAX_ADDR_LEN).
const MAX_ADDR_LEN: usize = 32;

/// `struct ifreq`: an interface name and a 24-byte request union.
#[repr(C)]
struct IfReq {
    name: [libc::c_char; libc::IFNAMSIZ],
    data: IfReqData,
}

#[repr(C)]
#[derive(Clone, Copy)]
union IfReqData {
    flags: libc::c_short,
    hwaddr: libc::sockaddr,
    pointer: *mut libc::c_void,
    size: [u8; 24],
}

/// `struct ethtool_drvinfo`.
#[repr(C)]
struct EthtoolDrvinfo {
    cmd: u32,
    driver: [u8; ETHTOOL_STRING],
    version: [u8; ETHTOOL_STRING],
    fw_version: [u8; ETHTOOL_STRING],
    bus_info: [u8; ETHTOOL_STRING],
    erom_version: [u8; ETHTOOL_STRING],
    reserved2: [u8; 12],
    n_priv_flags: u32,
    n_stats: u32,
    testinfo_len: u32,
    eedump_len: u32,
    regdump_len: u32,
}

/// `struct ethtool_perm_addr` with room for the address.
#[repr(C)]
struct EthtoolPermAddr {
    cmd: u32,
    size: u32,
    data: [u8; MAX_ADDR_LEN],
}

/// The administrative state of an interface (`IFF_UP`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Link {
    /// Up: the driver runs it.
    Up,
    /// Down.
    Down,
}

/// What ethtool reports about the driver of an interface.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DriverInfo {
    /// The driver's name and version, such as `iwlwifi 6.18.21`.
    pub driver: String,
    /// The firmware version; empty when the driver has no firmware (mac80211_hwsim).
    pub firmware: String,
}

/// The index of interface `name`, if it exists.
pub fn index(name: &str) -> Option<u32> {
    let name = CString::new(name).ok()?;
    // SAFETY: `name` is a valid NUL-terminated string.
    let index = unsafe { libc::if_nametoindex(name.as_ptr()) };
    (index != 0).then_some(index)
}

/// A socket to issue interface ioctls on.
#[derive(Debug)]
pub struct Netdev {
    fd: OwnedFd,
}

impl Netdev {
    /// Opens the socket.
    ///
    /// # Errors
    ///
    /// Returns the OS error of `socket`.
    pub fn open() -> io::Result<Self> {
        // SAFETY: plain syscall; the result is checked before use.
        let raw = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0) };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `raw` is a socket this function just opened and nothing else owns.
        Ok(Self { fd: unsafe { OwnedFd::from_raw_fd(raw) } })
    }

    /// The administrative state of interface `name`.
    ///
    /// # Errors
    ///
    /// Fails with `ENODEV` if there is no such interface.
    pub fn link(&self, name: &str) -> io::Result<Link> {
        let mut req = request(name)?;
        self.ioctl(libc::SIOCGIFFLAGS, &mut req)?;
        // SAFETY: SIOCGIFFLAGS filled in the flags member.
        let flags = unsafe { req.data.flags };
        Ok(if libc::c_int::from(flags) & libc::IFF_UP != 0 { Link::Up } else { Link::Down })
    }

    /// Brings interface `name` up or down.
    ///
    /// # Errors
    ///
    /// Returns the OS error, such as `ERFKILL` when the radio is blocked.
    pub fn set_link(&self, name: &str, link: Link) -> io::Result<()> {
        let mut req = request(name)?;
        self.ioctl(libc::SIOCGIFFLAGS, &mut req)?;
        // SAFETY: SIOCGIFFLAGS filled in the flags member.
        let flags = libc::c_int::from(unsafe { req.data.flags });
        let flags = match link {
            Link::Up => flags | libc::IFF_UP,
            Link::Down => flags & !libc::IFF_UP,
        };
        req.data.flags = libc::c_short::try_from(flags).expect("interface flags fit in a short");
        self.ioctl(libc::SIOCSIFFLAGS, &mut req)
    }

    /// Sets the MAC address of interface `name`. Drivers only accept this while the
    /// interface is down, so it is taken down and brought back up if it was up.
    ///
    /// # Errors
    ///
    /// Returns the OS error, such as `EADDRNOTAVAIL` for a multicast address.
    pub fn set_mac(&self, name: &str, mac: MacAddress) -> io::Result<()> {
        let link = self.link(name)?;
        if link == Link::Up {
            self.set_link(name, Link::Down)?;
        }
        let mut req = request(name)?;
        // SAFETY: sockaddr is plain data; all zeroes is a valid value.
        let mut hwaddr: libc::sockaddr = unsafe { std::mem::zeroed() };
        hwaddr.sa_family = ARPHRD_ETHER;
        for (dst, src) in hwaddr.sa_data.iter_mut().zip(mac) {
            *dst = src as libc::c_char;
        }
        req.data.hwaddr = hwaddr;
        let result = self.ioctl(libc::SIOCSIFHWADDR, &mut req);
        if link == Link::Up {
            self.set_link(name, Link::Up)?;
        }
        result
    }

    /// The permanent (factory) MAC address of interface `name`, or its current one if the
    /// driver reports none.
    ///
    /// # Errors
    ///
    /// Fails if the interface does not exist.
    pub fn permanent_mac(&self, name: &str) -> io::Result<MacAddress> {
        let mut perm = EthtoolPermAddr {
            cmd: ETHTOOL_GPERMADDR,
            size: MAX_ADDR_LEN as u32,
            data: [0; MAX_ADDR_LEN],
        };
        self.ethtool(name, std::ptr::from_mut(&mut perm).cast())?;
        let mac: MacAddress = perm.data[..6].try_into().expect("6 bytes");
        if perm.size == 6 && mac != [0; 6] {
            return Ok(mac);
        }
        self.current_mac(name)
    }

    /// The driver and firmware of interface `name`.
    ///
    /// # Errors
    ///
    /// Fails if the interface does not exist or its driver has no ethtool support.
    pub fn driver_info(&self, name: &str) -> io::Result<DriverInfo> {
        // SAFETY: ethtool_drvinfo is plain data; all zeroes is a valid value.
        let mut info: EthtoolDrvinfo = unsafe { std::mem::zeroed() };
        info.cmd = ETHTOOL_GDRVINFO;
        self.ethtool(name, std::ptr::from_mut(&mut info).cast())?;
        let driver = [c_string(&info.driver), c_string(&info.version)]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        Ok(DriverInfo { driver, firmware: c_string(&info.fw_version) })
    }

    fn current_mac(&self, name: &str) -> io::Result<MacAddress> {
        let mut req = request(name)?;
        self.ioctl(libc::SIOCGIFHWADDR, &mut req)?;
        // SAFETY: SIOCGIFHWADDR filled in the hwaddr member.
        let hwaddr = unsafe { req.data.hwaddr };
        let mut mac = [0; 6];
        for (dst, src) in mac.iter_mut().zip(hwaddr.sa_data) {
            // c_char is i8 on x86_64 and u8 on arm64: a plain cast is lint-clean on only one.
            *dst = u8::from_ne_bytes(src.to_ne_bytes());
        }
        Ok(mac)
    }

    fn ethtool(&self, name: &str, data: *mut libc::c_void) -> io::Result<()> {
        let mut req = request(name)?;
        req.data.pointer = data;
        self.ioctl(libc::SIOCETHTOOL, &mut req)
    }

    fn ioctl(&self, request: libc::c_ulong, req: &mut IfReq) -> io::Result<()> {
        // SAFETY: `req` is a valid ifreq for the duration of the call; for SIOCETHTOOL its
        // pointer refers to a live ethtool struct of the size the command expects.
        let ret =
            unsafe { libc::ioctl(self.fd.as_raw_fd(), request as _, std::ptr::from_mut(req)) };
        if ret < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

/// An ifreq naming interface `name`, its union zeroed.
fn request(name: &str) -> io::Result<IfReq> {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() >= libc::IFNAMSIZ || bytes.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("bad interface name {name:?}"),
        ));
    }
    let mut req = IfReq { name: [0; libc::IFNAMSIZ], data: IfReqData { size: [0; 24] } };
    for (dst, &src) in req.name.iter_mut().zip(bytes) {
        *dst = src as libc::c_char;
    }
    Ok(req)
}

/// The NUL-terminated string at the start of `bytes`.
fn c_string(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

/// `aa:bb:cc:dd:ee:ff`, for logs.
pub fn format_mac(mac: MacAddress) -> String {
    mac.map(|byte| format!("{byte:02x}")).join(":")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn struct_sizes_match_the_kernel() {
        assert_eq!(std::mem::size_of::<IfReq>(), 40);
        assert_eq!(std::mem::size_of::<EthtoolDrvinfo>(), 196);
        assert_eq!(std::mem::size_of::<EthtoolPermAddr>(), 40);
    }

    #[test]
    fn rejects_bad_names() {
        assert!(request("").is_err());
        assert!(request("a-name-that-is-too-long").is_err());
        assert!(request("wl\0an").is_err());
        assert!(request("wlan0").is_ok());
    }

    #[test]
    fn strings_and_macs() {
        assert_eq!(c_string(b"iwlwifi\0\0junk"), "iwlwifi");
        assert_eq!(c_string(b"full"), "full");
        assert_eq!(format_mac([0x02, 0, 0xab, 1, 2, 0xff]), "02:00:ab:01:02:ff");
    }

    #[test]
    fn loopback_is_not_wireless() {
        // Every Linux host and device has lo; it has no hardware address to speak of.
        assert!(index("lo").is_some());
        assert_eq!(index("no-such-if0"), None);
    }
}
