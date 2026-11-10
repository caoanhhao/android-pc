//! /dev/net/tun (misc 10:200): TUNSETIFF creates a point-to-point link
//! (ARPHRD_NONE) that exists while the file is open, so interface ioctls,
//! rtnetlink and netd see it (system_server's Thread network service
//! creates "thread-wpan" this way). No packets flow: the link is not part
//! of the IP stack's routing, what is written to the file is counted and
//! dropped, and reads find nothing (EAGAIN, or block).

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use core::any::Any;
use spin::Mutex;

use super::inet::{IFF_MULTICAST, Link, with_net_nopoll};
use crate::errno::*;
use crate::fs::file::{FileOps, OpenFile};
use crate::fs::poll::{POLLOUT, POLLWRNORM, WaitQueue};
use crate::process::current;
use crate::sched::{self, WaitMode};

pub const TUN_MINOR: u32 = 200;

const TUNSETIFF: u32 = 0x4004_54ca;
const TUNGETIFF: u32 = 0x8004_54d2;
const TUNSETPERSIST: u32 = 0x4004_54cb;
const TUNSETOWNER: u32 = 0x4004_54cc;
const TUNSETGROUP: u32 = 0x4004_54ce;
const TUNGETFEATURES: u32 = 0x8004_54cf;

const IFF_TUN: u16 = 0x0001;
const IFF_TAP: u16 = 0x0002;
const IFF_NO_PI: u16 = 0x1000;
const IFF_TUN_EXCL: u16 = 0x8000;
const IFF_POINTOPOINT: u32 = 0x10;
const IFF_NOARP: u32 = 0x80;
const ARPHRD_NONE: u16 = 0xfffe;

struct Tun {
    /// (link index, name, flags) once attached.
    link: Mutex<Option<(u32, String, u16)>>,
    wq: Arc<WaitQueue>,
}

impl Tun {
    fn attach(&self, arg: u64) -> KResult<u64> {
        let mm = current().mm();
        let mut ifr = [0u8; 40];
        mm.copy_in(arg, &mut ifr)?;
        let flags = u16::from_le_bytes([ifr[16], ifr[17]]);
        if flags & (IFF_TUN | IFF_TAP) != IFF_TUN {
            return Err(EINVAL); // only layer-3 tun devices
        }
        let mut me = self.link.lock();
        if me.is_some() {
            return Err(EBUSY);
        }
        let raw = &ifr[..16];
        let n = raw.iter().position(|&b| b == 0).unwrap_or(16);
        let requested = String::from_utf8_lossy(&raw[..n]).into_owned();
        let (index, name) = with_net_nopoll(|net| {
            let name = if requested.is_empty() || requested.contains('%') {
                let pat = if requested.is_empty() { "tun%d" } else { requested.as_str() };
                (0..)
                    .map(|i| pat.replacen("%d", &format!("{i}"), 1))
                    .find(|c| net.link_by_name(c).is_none())
                    .unwrap()
            } else {
                if net.link_by_name(&requested).is_some() {
                    // An existing link of that name (TUN_EXCL or not: there
                    // are no persistent tun devices to re-attach to).
                    return Err(EBUSY);
                }
                requested.clone()
            };
            if name.len() > 15 {
                return Err(EINVAL);
            }
            let index = net.links.iter().map(|l| l.index).max().unwrap_or(1) + 1;
            net.links.push(Link {
                index,
                name: name.clone(),
                mac: [0; 6],
                mtu: 1500,
                flags: IFF_POINTOPOINT | IFF_NOARP | IFF_MULTICAST,
                arptype: ARPHRD_NONE,
                addrs: alloc::vec::Vec::new(),
                txqlen: 500,
                carrier: true,
            });
            Ok((index, name))
        })?;
        *me = Some((index, name.clone(), flags & !IFF_TUN_EXCL));
        drop(me);
        let dir = format!("devices/virtual/net/{name}");
        crate::fs::sysfs::add_device(&dir, "net", "class/net", format!("INTERFACE={name}\nIFINDEX={index}\n"), true);
        crate::fs::sysfs::set(&dir, "ifindex", &format!("{index}\n"));
        crate::fs::sysfs::set(&dir, "mtu", "1500\n");
        crate::fs::sysfs::set(&dir, "type", &format!("{ARPHRD_NONE}\n"));
        crate::fs::sysfs::set(&dir, "tun_flags", &format!("{:#x}\n", flags & !IFF_TUN_EXCL));
        super::rtnetlink::notify_links();
        ifr[..16].fill(0);
        ifr[..name.len()].copy_from_slice(name.as_bytes());
        mm.copy_out(arg, &ifr[..16])?;
        Ok(0)
    }
}

impl Drop for Tun {
    fn drop(&mut self) {
        if let Some((index, name, _)) = self.link.get_mut().take() {
            with_net_nopoll(|net| {
                net.links.retain(|l| l.index != index);
                net.routes.retain(|r| r.oif != index);
                net.sync();
            });
            crate::fs::sysfs::remove_device(&format!("devices/virtual/net/{name}"));
            super::rtnetlink::notify_links();
        }
    }
}

impl FileOps for Tun {
    fn read(&self, f: &OpenFile, _: &mut u64, _buf: &mut [u8]) -> KResult<usize> {
        if self.link.lock().is_none() {
            return Err(EBADFD);
        }
        if f.nonblock() {
            return Err(EAGAIN);
        }
        sched::wait_until(self.wq.key(), None, WaitMode::Interruptible, || false);
        Err(ERESTARTSYS)
    }

    fn write(&self, _: &OpenFile, _: &mut u64, data: &[u8]) -> KResult<usize> {
        if self.link.lock().is_none() {
            return Err(EBADFD);
        }
        Ok(data.len())
    }

    fn ioctl(&self, _: &OpenFile, cmd: u32, arg: u64) -> KResult<u64> {
        let mm = current().mm();
        match cmd {
            TUNSETIFF => self.attach(arg),
            TUNGETIFF => {
                let me = self.link.lock();
                let (_, name, flags) = me.as_ref().ok_or(EBADFD)?;
                let mut ifr = [0u8; 18];
                ifr[..name.len()].copy_from_slice(name.as_bytes());
                ifr[16..18].copy_from_slice(&flags.to_le_bytes());
                mm.copy_out(arg, &ifr)?;
                Ok(0)
            }
            TUNGETFEATURES => {
                mm.copy_out(arg, &((IFF_TUN | IFF_NO_PI | IFF_TUN_EXCL) as u32).to_le_bytes())?;
                Ok(0)
            }
            TUNSETPERSIST | TUNSETOWNER | TUNSETGROUP => Ok(0),
            _ => Err(EINVAL),
        }
    }

    fn poll(&self, _: &OpenFile) -> u32 {
        POLLOUT | POLLWRNORM
    }

    fn wait_queue(&self, _: &OpenFile) -> Option<Arc<WaitQueue>> {
        Some(self.wq.clone())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub fn open() -> KResult<Box<dyn FileOps>> {
    Ok(Box::new(Tun { link: Mutex::new(None), wq: WaitQueue::new() }))
}
