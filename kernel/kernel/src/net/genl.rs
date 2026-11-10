//! Generic netlink (NETLINK_GENERIC): the `nlctrl` family, which resolves
//! family names to ids and multicast group names to group numbers
//! (CTRL_CMD_GETFAMILY, what libnl's genl_ctrl_resolve does), and the
//! families the kernel registers (`nl80211`, `net::wireless`).
//!
//! A request is answered synchronously, like Linux does in sendmsg: the
//! family's handler returns nothing, one message or a dump (NLM_F_MULTI
//! messages and NLMSG_DONE); errors become NLMSG_ERROR, and NLM_F_ACK
//! adds the zero NLMSG_ERROR after a successful non-dump request.
//! Multicast group numbers are fixed small numbers (below 32, so
//! NETLINK_ADD_MEMBERSHIP works with the socket's 32-bit group mask).
//!
//! [`Nla`] builds attributes, [`attrs`] / [`find`] parse them.

use alloc::string::String;
use alloc::vec::Vec;

use super::netlink::{self, NLM_F_ACK, NLM_F_DUMP, NLMSG_HDRLEN, nlmsg};
use crate::errno::*;

pub const NETLINK_GENERIC: u32 = 16;
pub const GENL_HDRLEN: usize = 4;
const NLMSG_ERROR: u16 = 2;
const NLMSG_DONE: u16 = 3;
const NLM_F_MULTI: u16 = 2;

const GENL_ID_CTRL: u16 = 16;
const CTRL_CMD_NEWFAMILY: u8 = 1;
const CTRL_CMD_GETFAMILY: u8 = 3;
const CTRL_ATTR_FAMILY_ID: u16 = 1;
const CTRL_ATTR_FAMILY_NAME: u16 = 2;
const CTRL_ATTR_VERSION: u16 = 3;
const CTRL_ATTR_HDRSIZE: u16 = 4;
const CTRL_ATTR_MAXATTR: u16 = 5;
const CTRL_ATTR_OPS: u16 = 6;
const CTRL_ATTR_MCAST_GROUPS: u16 = 7;
const CTRL_ATTR_OP_ID: u16 = 1;
const CTRL_ATTR_OP_FLAGS: u16 = 2;
const CTRL_ATTR_MCAST_GRP_NAME: u16 = 1;
const CTRL_ATTR_MCAST_GRP_ID: u16 = 2;
/// nlctrl's "notify" group.
const GENL_CTRL_NOTIFY_GRP: u32 = 16;

pub const NLA_F_NESTED: u16 = 0x8000;
const NLA_TYPE_MASK: u16 = 0x3fff;

/// A parsed request to a family.
pub struct Req<'a> {
    pub cmd: u8,
    pub flags: u16,
    pub seq: u32,
    /// The sender's port id.
    pub portid: u32,
    /// The attributes after the genl header.
    pub attrs: &'a [u8],
}

impl Req<'_> {
    pub fn dump(&self) -> bool {
        self.flags & NLM_F_DUMP == NLM_F_DUMP
    }

    pub fn get(&self, ty: u16) -> Option<&[u8]> {
        find(self.attrs, ty)
    }

    pub fn u32(&self, ty: u16) -> Option<u32> {
        self.get(ty).and_then(as_u32)
    }

    pub fn has(&self, ty: u16) -> bool {
        self.get(ty).is_some()
    }
}

/// What a family's handler answers: (command, attributes) messages.
pub enum Answer {
    /// Nothing (an ack if the request asked for one).
    None,
    One(u8, Vec<u8>),
    Dump(Vec<(u8, Vec<u8>)>),
}

pub struct Family {
    pub name: &'static str,
    pub id: u16,
    pub version: u32,
    pub maxattr: u32,
    /// (command, CTRL_ATTR_OP_FLAGS) of the supported commands.
    pub ops: &'static [u8],
    /// (name, group number).
    pub groups: &'static [(&'static str, u32)],
    pub handler: fn(&Req) -> KResult<Answer>,
}

const CTRL_OPS: &[u8] = &[CTRL_CMD_GETFAMILY];

const CTRL: Family = Family {
    name: "nlctrl",
    id: GENL_ID_CTRL,
    version: 2,
    maxattr: 10,
    ops: CTRL_OPS,
    groups: &[("notify", GENL_CTRL_NOTIFY_GRP)],
    handler: ctrl_request,
};

fn families() -> [&'static Family; 2] {
    [&CTRL, &crate::net::wireless::nl80211::FAMILY]
}

/// A request sent to the kernel on a NETLINK_GENERIC socket (one
/// nlmsghdr and its payload); the reply messages for the sender.
pub fn request(msg: &[u8], portid: u32) -> Vec<Vec<u8>> {
    let ty = u16::from_le_bytes([msg[4], msg[5]]);
    let flags = u16::from_le_bytes([msg[6], msg[7]]);
    let seq = u32::from_le_bytes(msg[8..12].try_into().unwrap());
    let err = |e: i32| {
        let mut p = e.to_le_bytes().to_vec();
        p.extend_from_slice(&msg[..NLMSG_HDRLEN]);
        alloc::vec![nlmsg(NLMSG_ERROR, 0, seq, portid, &p)]
    };
    if msg.len() < NLMSG_HDRLEN + GENL_HDRLEN {
        return err(-(EINVAL.0 as i32));
    }
    let Some(fam) = families().into_iter().find(|f| f.id == ty) else {
        return err(-(ENOENT.0 as i32));
    };
    let req = Req { cmd: msg[NLMSG_HDRLEN], flags, seq, portid, attrs: &msg[NLMSG_HDRLEN + GENL_HDRLEN..] };
    let ver = fam.version as u8;
    match (fam.handler)(&req) {
        Err(e) => err(-(e.0 as i32)),
        Ok(Answer::Dump(list)) => {
            let mut out: Vec<Vec<u8>> =
                list.into_iter().map(|(cmd, a)| nlmsg(fam.id, NLM_F_MULTI, seq, portid, &genl_payload(cmd, ver, &a))).collect();
            out.push(nlmsg(NLMSG_DONE, NLM_F_MULTI, seq, portid, &0i32.to_le_bytes()));
            out
        }
        Ok(a) => {
            let mut out = Vec::new();
            if let Answer::One(cmd, a) = a {
                out.push(nlmsg(fam.id, 0, seq, portid, &genl_payload(cmd, ver, &a)));
            }
            if flags & NLM_F_ACK != 0 {
                out.extend(err(0));
            }
            out
        }
    }
}

fn genl_payload(cmd: u8, version: u8, attrs: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(GENL_HDRLEN + attrs.len());
    p.extend_from_slice(&[cmd, version, 0, 0]);
    p.extend_from_slice(attrs);
    p
}

/// A kernel event of `family` to the sockets in multicast group `group`.
pub fn multicast(family: &Family, group: u32, cmd: u8, attrs: &[u8]) {
    let m = nlmsg(family.id, 0, 0, 0, &genl_payload(cmd, family.version as u8, attrs));
    netlink::multicast(NETLINK_GENERIC, 1 << (group - 1), &m);
}

/// A kernel event of `family` to one socket.
pub fn unicast(family: &Family, portid: u32, cmd: u8, attrs: &[u8]) -> bool {
    let m = nlmsg(family.id, 0, 0, portid, &genl_payload(cmd, family.version as u8, attrs));
    netlink::unicast(NETLINK_GENERIC, portid, &m)
}

fn family_attrs(f: &Family) -> Vec<u8> {
    let mut a = Nla::new();
    a.str(CTRL_ATTR_FAMILY_NAME, f.name);
    a.u16(CTRL_ATTR_FAMILY_ID, f.id);
    a.u32(CTRL_ATTR_VERSION, f.version);
    a.u32(CTRL_ATTR_HDRSIZE, 0);
    a.u32(CTRL_ATTR_MAXATTR, f.maxattr);
    let ops = a.nest(CTRL_ATTR_OPS);
    for (i, &op) in f.ops.iter().enumerate() {
        let o = a.nest(i as u16 + 1);
        a.u32(CTRL_ATTR_OP_ID, op as u32);
        // GENL_CMD_CAP_DO | GENL_CMD_CAP_DUMP | GENL_CMD_CAP_HASPOL
        a.u32(CTRL_ATTR_OP_FLAGS, 0x2 | 0x4 | 0x8);
        a.end(o);
    }
    a.end(ops);
    if !f.groups.is_empty() {
        let g = a.nest(CTRL_ATTR_MCAST_GROUPS);
        for (i, &(name, id)) in f.groups.iter().enumerate() {
            let n = a.nest(i as u16 + 1);
            a.u32(CTRL_ATTR_MCAST_GRP_ID, id);
            a.str(CTRL_ATTR_MCAST_GRP_NAME, name);
            a.end(n);
        }
        a.end(g);
    }
    a.0
}

fn ctrl_request(r: &Req) -> KResult<Answer> {
    if r.cmd != CTRL_CMD_GETFAMILY {
        return Err(EOPNOTSUPP);
    }
    if r.dump() {
        return Ok(Answer::Dump(families().iter().map(|f| (CTRL_CMD_NEWFAMILY, family_attrs(f))).collect()));
    }
    let by_name: Option<String> = r.get(CTRL_ATTR_FAMILY_NAME).map(|n| String::from_utf8_lossy(n).trim_end_matches('\0').into());
    let by_id = r.get(CTRL_ATTR_FAMILY_ID).and_then(as_u16);
    let f = families()
        .into_iter()
        .find(|f| by_name.as_deref() == Some(f.name) || by_id == Some(f.id))
        .ok_or(ENOENT)?;
    Ok(Answer::One(CTRL_CMD_NEWFAMILY, family_attrs(f)))
}

/// An attribute list being built.
#[derive(Default)]
pub struct Nla(pub Vec<u8>);

impl Nla {
    pub fn new() -> Nla {
        Nla(Vec::new())
    }

    pub fn put(&mut self, ty: u16, data: &[u8]) {
        let len = 4 + data.len();
        self.0.extend_from_slice(&(len as u16).to_le_bytes());
        self.0.extend_from_slice(&ty.to_le_bytes());
        self.0.extend_from_slice(data);
        while self.0.len() % 4 != 0 {
            self.0.push(0);
        }
    }

    pub fn flag(&mut self, ty: u16) {
        self.put(ty, &[]);
    }

    pub fn u8(&mut self, ty: u16, v: u8) {
        self.put(ty, &[v]);
    }

    pub fn u16(&mut self, ty: u16, v: u16) {
        self.put(ty, &v.to_le_bytes());
    }

    pub fn u32(&mut self, ty: u16, v: u32) {
        self.put(ty, &v.to_le_bytes());
    }

    pub fn u64(&mut self, ty: u16, v: u64) {
        self.put(ty, &v.to_le_bytes());
    }

    /// A NUL-terminated string.
    pub fn str(&mut self, ty: u16, s: &str) {
        let mut b = Vec::with_capacity(s.len() + 1);
        b.extend_from_slice(s.as_bytes());
        b.push(0);
        self.put(ty, &b);
    }

    /// Start a nested attribute; close it with [`Nla::end`].
    pub fn nest(&mut self, ty: u16) -> usize {
        let at = self.0.len();
        self.0.extend_from_slice(&[0, 0]);
        self.0.extend_from_slice(&(ty | NLA_F_NESTED).to_le_bytes());
        at
    }

    pub fn end(&mut self, at: usize) {
        let len = (self.0.len() - at) as u16;
        self.0[at..at + 2].copy_from_slice(&len.to_le_bytes());
    }
}

/// The attributes in `b`: (type without the NESTED/BYTEORDER flags, data).
pub fn attrs(b: &[u8]) -> Vec<(u16, &[u8])> {
    let mut out = Vec::new();
    let mut off = 0;
    while off + 4 <= b.len() {
        let len = u16::from_le_bytes([b[off], b[off + 1]]) as usize;
        let ty = u16::from_le_bytes([b[off + 2], b[off + 3]]) & NLA_TYPE_MASK;
        if len < 4 || off + len > b.len() {
            break;
        }
        out.push((ty, &b[off + 4..off + len]));
        off += (len + 3) & !3;
    }
    out
}

/// The first attribute of type `ty` in `b`.
pub fn find(b: &[u8], ty: u16) -> Option<&[u8]> {
    attrs(b).into_iter().find(|&(t, _)| t == ty).map(|(_, d)| d)
}

pub fn as_u32(d: &[u8]) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(..4)?.try_into().ok()?))
}

pub fn as_u16(d: &[u8]) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(..2)?.try_into().ok()?))
}

pub fn as_u64(d: &[u8]) -> Option<u64> {
    Some(u64::from_le_bytes(d.get(..8)?.try_into().ok()?))
}
