//! The nl80211 generic netlink family: what a wireless device supports, creating and
//! deleting its interfaces, and the regulatory domain.

mod attr;
mod socket;
mod wiphy;

use std::io;

use attr::{Request, NLM_F_ACK, NLM_F_DUMP};
use socket::GenlSocket;
use wiphy::ATTR_WIPHY;

pub use wiphy::{Band, Channel, Iftype, Iftypes, Wiphy};
#[cfg(test)]
pub use wiphy::{Combination, Limit};

/// `GENL_ID_CTRL`: the generic netlink controller, which resolves family names.
const GENL_ID_CTRL: u16 = 0x10;
/// `CTRL_CMD_GETFAMILY`.
const CTRL_CMD_GETFAMILY: u8 = 3;
/// `CTRL_ATTR_FAMILY_ID`.
const CTRL_ATTR_FAMILY_ID: u16 = 1;
/// `CTRL_ATTR_FAMILY_NAME`.
const CTRL_ATTR_FAMILY_NAME: u16 = 2;

/// `NL80211_CMD_GET_WIPHY`.
const CMD_GET_WIPHY: u8 = 1;
/// `NL80211_CMD_GET_INTERFACE`.
const CMD_GET_INTERFACE: u8 = 5;
/// `NL80211_CMD_NEW_INTERFACE`.
const CMD_NEW_INTERFACE: u8 = 7;
/// `NL80211_CMD_DEL_INTERFACE`.
const CMD_DEL_INTERFACE: u8 = 8;
/// `NL80211_CMD_REQ_SET_REG`.
const CMD_REQ_SET_REG: u8 = 27;

/// `NL80211_ATTR_IFINDEX`.
const ATTR_IFINDEX: u16 = 3;
/// `NL80211_ATTR_IFNAME`.
const ATTR_IFNAME: u16 = 4;
/// `NL80211_ATTR_IFTYPE`.
const ATTR_IFTYPE: u16 = 5;
/// `NL80211_ATTR_REG_ALPHA2`.
const ATTR_REG_ALPHA2: u16 = 33;
/// `NL80211_ATTR_SPLIT_WIPHY_DUMP`: the wiphy information of modern drivers (many
/// bands and channels) only fits when split over several messages.
const ATTR_SPLIT_WIPHY_DUMP: u16 = 174;

/// An nl80211 connection.
#[derive(Debug)]
pub struct Nl80211 {
    socket: GenlSocket,
    family: u16,
}

impl Nl80211 {
    /// Opens a generic netlink socket and looks up the nl80211 family.
    ///
    /// # Errors
    ///
    /// Fails if the socket cannot be opened or cfg80211 is not loaded (`ENOENT`).
    pub fn open() -> io::Result<Self> {
        let mut socket = GenlSocket::open()?;
        let request = Request::new(GENL_ID_CTRL, NLM_F_ACK, CTRL_CMD_GETFAMILY)
            .string(CTRL_ATTR_FAMILY_NAME, "nl80211");
        let family = socket
            .transact(request)?
            .iter()
            .find_map(|reply| reply.attrs().get(CTRL_ATTR_FAMILY_ID)?.u16())
            .ok_or_else(|| io::Error::other("no nl80211 family id in the controller's reply"))?;
        Ok(Self { socket, family })
    }

    /// The wiphy index of the network interface `ifindex`.
    ///
    /// # Errors
    ///
    /// Fails with `ENODEV` if `ifindex` is not a wireless interface.
    pub fn wiphy_of(&mut self, ifindex: u32) -> io::Result<u32> {
        let request =
            Request::new(self.family, NLM_F_ACK, CMD_GET_INTERFACE).u32(ATTR_IFINDEX, ifindex);
        self.socket
            .transact(request)?
            .iter()
            .find_map(|reply| reply.attrs().get(ATTR_WIPHY)?.u32())
            .ok_or_else(|| io::Error::other(format!("no wiphy for interface {ifindex}")))
    }

    /// What wiphy `index` supports.
    ///
    /// # Errors
    ///
    /// Fails if the dump fails or does not name the wiphy.
    pub fn wiphy(&mut self, index: u32) -> io::Result<Wiphy> {
        let request = Request::new(self.family, NLM_F_DUMP, CMD_GET_WIPHY)
            .u32(ATTR_WIPHY, index)
            .flag(ATTR_SPLIT_WIPHY_DUMP);
        let mut wiphy = Wiphy { index, ..Wiphy::default() };
        // The filter is a hint: older kernels dump every wiphy.
        for reply in self.socket.transact(request)? {
            if reply.attrs().get(ATTR_WIPHY).and_then(|attr| attr.u32()) == Some(index) {
                wiphy.merge(reply.attrs());
            }
        }
        // Every wiphy has a name, sent in the first message of its dump.
        if wiphy.name.is_empty() {
            return Err(io::Error::other(format!("wiphy {index} not found")));
        }
        Ok(wiphy)
    }

    /// Creates the network interface `name` of `iftype` on wiphy `index`.
    ///
    /// # Errors
    ///
    /// Returns the driver's error, such as `EBUSY` when its interface combinations do not
    /// allow another interface of that type.
    pub fn new_interface(&mut self, wiphy: u32, name: &str, iftype: Iftype) -> io::Result<()> {
        let request = Request::new(self.family, NLM_F_ACK, CMD_NEW_INTERFACE)
            .u32(ATTR_WIPHY, wiphy)
            .string(ATTR_IFNAME, name)
            .u32(ATTR_IFTYPE, iftype as u32);
        self.socket.transact(request).map(drop)
    }

    /// Deletes the network interface `ifindex`.
    ///
    /// # Errors
    ///
    /// Returns the kernel's error.
    pub fn del_interface(&mut self, ifindex: u32) -> io::Result<()> {
        let request =
            Request::new(self.family, NLM_F_ACK, CMD_DEL_INTERFACE).u32(ATTR_IFINDEX, ifindex);
        self.socket.transact(request).map(drop)
    }

    /// Asks the kernel to use the regulatory rules of country `alpha2` (ISO 3166-1, or
    /// `00` for the world domain). The kernel applies them asynchronously.
    ///
    /// # Errors
    ///
    /// Returns the kernel's error, such as `EINVAL` for a malformed code.
    pub fn set_regulatory_domain(&mut self, alpha2: [u8; 2]) -> io::Result<()> {
        let code = std::str::from_utf8(&alpha2)
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        let request =
            Request::new(self.family, NLM_F_ACK, CMD_REQ_SET_REG).string(ATTR_REG_ALPHA2, code);
        self.socket.transact(request).map(drop)
    }
}
