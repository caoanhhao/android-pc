//! Netlink and generic netlink message encoding (`linux/netlink.h`, `linux/genetlink.h`).

/// `struct nlmsghdr`: length, type, flags, sequence number, port id.
const NLMSG_HDRLEN: usize = 16;
/// `struct genlmsghdr`: command, version, reserved.
pub(crate) const GENL_HDRLEN: usize = 4;
/// `struct nlattr`: length, type.
const NLA_HDRLEN: usize = 4;
/// Attribute type bits without `NLA_F_NESTED` and `NLA_F_NET_BYTEORDER`.
const NLA_TYPE_MASK: u16 = 0x3fff;

pub(crate) const NLMSG_ERROR: u16 = 2;
pub(crate) const NLMSG_DONE: u16 = 3;
pub(crate) const NLM_F_REQUEST: u16 = 0x1;
pub(crate) const NLM_F_ACK: u16 = 0x4;
/// `NLM_F_ROOT | NLM_F_MATCH`.
pub(crate) const NLM_F_DUMP: u16 = 0x300;

/// Generic netlink version sent in requests; nl80211 and the controller ignore it.
const GENL_VERSION: u8 = 1;

fn align(len: usize) -> usize {
    (len + 3) & !3
}

/// A generic netlink request being built: header, command, then attributes.
#[derive(Debug, Clone)]
pub struct Request {
    buf: Vec<u8>,
}

impl Request {
    /// Starts a request for `cmd` of the generic netlink `family`.
    pub fn new(family: u16, flags: u16, cmd: u8) -> Self {
        let mut buf = vec![0; NLMSG_HDRLEN + GENL_HDRLEN];
        buf[4..6].copy_from_slice(&family.to_ne_bytes());
        buf[6..8].copy_from_slice(&(flags | NLM_F_REQUEST).to_ne_bytes());
        buf[NLMSG_HDRLEN] = cmd;
        buf[NLMSG_HDRLEN + 1] = GENL_VERSION;
        Self { buf }
    }

    /// Whether the request asks for a dump (a multi-part reply ended by `NLMSG_DONE`).
    pub fn is_dump(&self) -> bool {
        u16::from_ne_bytes([self.buf[6], self.buf[7]]) & NLM_F_DUMP == NLM_F_DUMP
    }

    /// Appends an attribute holding `payload`.
    pub fn bytes(mut self, kind: u16, payload: &[u8]) -> Self {
        let len = u16::try_from(NLA_HDRLEN + payload.len()).expect("netlink attribute too long");
        self.buf.extend_from_slice(&len.to_ne_bytes());
        self.buf.extend_from_slice(&kind.to_ne_bytes());
        self.buf.extend_from_slice(payload);
        self.buf.resize(align(self.buf.len()), 0);
        self
    }

    /// Appends a `u32` attribute.
    pub fn u32(self, kind: u16, value: u32) -> Self {
        self.bytes(kind, &value.to_ne_bytes())
    }

    /// Appends a NUL-terminated string attribute.
    pub fn string(self, kind: u16, value: &str) -> Self {
        let mut payload = Vec::with_capacity(value.len() + 1);
        payload.extend_from_slice(value.as_bytes());
        payload.push(0);
        self.bytes(kind, &payload)
    }

    /// Appends a flag attribute (no payload).
    pub fn flag(self, kind: u16) -> Self {
        self.bytes(kind, &[])
    }

    /// The finished message with its length and sequence number filled in.
    pub fn finish(mut self, seq: u32) -> Vec<u8> {
        let len = u32::try_from(self.buf.len()).expect("netlink message too long");
        self.buf[0..4].copy_from_slice(&len.to_ne_bytes());
        self.buf[8..12].copy_from_slice(&seq.to_ne_bytes());
        self.buf
    }
}

/// One netlink message out of a datagram the kernel sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame<'buf> {
    /// `nlmsg_type`: the generic netlink family, `NLMSG_ERROR` or `NLMSG_DONE`.
    pub kind: u16,
    /// `nlmsg_seq` of the request this answers.
    pub seq: u32,
    /// Everything after the netlink header.
    pub payload: &'buf [u8],
}

impl<'buf> Frame<'buf> {
    /// For an `NLMSG_ERROR` message, the error: 0 for an acknowledgement, else a
    /// negative errno.
    pub fn error(&self) -> Option<i32> {
        if self.kind != NLMSG_ERROR {
            return None;
        }
        let code = self.payload.get(..4)?;
        Some(i32::from_ne_bytes(code.try_into().expect("slice of 4 bytes")))
    }
}

/// Splits a datagram into its netlink messages; stops at the first malformed one.
pub fn frames(datagram: &[u8]) -> impl Iterator<Item = Frame<'_>> {
    let mut rest = datagram;
    std::iter::from_fn(move || {
        let header = rest.get(..NLMSG_HDRLEN)?;
        let len = u32::from_ne_bytes(header[0..4].try_into().expect("4 bytes")) as usize;
        if len < NLMSG_HDRLEN || len > rest.len() {
            rest = &[];
            return None;
        }
        let frame = Frame {
            kind: u16::from_ne_bytes([header[4], header[5]]),
            seq: u32::from_ne_bytes(header[8..12].try_into().expect("4 bytes")),
            payload: &rest[NLMSG_HDRLEN..len],
        };
        rest = rest.get(align(len)..).unwrap_or_default();
        Some(frame)
    })
}

/// One attribute: its type (without flag bits) and payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Attr<'buf> {
    /// The attribute type, such as `NL80211_ATTR_WIPHY`.
    pub kind: u16,
    /// The payload, without padding.
    pub payload: &'buf [u8],
}

impl<'buf> Attr<'buf> {
    /// The payload as a `u16`, if it has that size.
    pub fn u16(&self) -> Option<u16> {
        Some(u16::from_ne_bytes(self.payload.try_into().ok()?))
    }

    /// The payload as a `u32`, if it has that size.
    pub fn u32(&self) -> Option<u32> {
        Some(u32::from_ne_bytes(self.payload.try_into().ok()?))
    }

    /// The payload as a string, without its NUL terminator.
    pub fn string(&self) -> Option<&'buf str> {
        let bytes = self.payload.split(|&b| b == 0).next().unwrap_or_default();
        std::str::from_utf8(bytes).ok()
    }

    /// The attributes nested in the payload.
    pub fn nested(&self) -> Attrs<'buf> {
        Attrs(self.payload)
    }
}

/// A sequence of attributes; iteration stops at the first malformed one.
#[derive(Debug, Clone, Copy, Default)]
pub struct Attrs<'buf>(&'buf [u8]);

impl<'buf> Attrs<'buf> {
    /// The attributes encoded in `buf`.
    pub fn new(buf: &'buf [u8]) -> Self {
        Self(buf)
    }

    /// The first attribute of type `kind`.
    pub fn get(mut self, kind: u16) -> Option<Attr<'buf>> {
        self.find(|attr| attr.kind == kind)
    }
}

impl<'buf> Iterator for Attrs<'buf> {
    type Item = Attr<'buf>;

    fn next(&mut self) -> Option<Attr<'buf>> {
        let header = self.0.get(..NLA_HDRLEN)?;
        let len = usize::from(u16::from_ne_bytes([header[0], header[1]]));
        if len < NLA_HDRLEN || len > self.0.len() {
            self.0 = &[];
            return None;
        }
        let attr = Attr {
            kind: u16::from_ne_bytes([header[2], header[3]]) & NLA_TYPE_MASK,
            payload: &self.0[NLA_HDRLEN..len],
        };
        self.0 = self.0.get(align(len)..).unwrap_or_default();
        Some(attr)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Encodes attributes the way the kernel does (`nla_put`), for parser tests.
    pub(crate) fn encode(attrs: &[(u16, Vec<u8>)]) -> Vec<u8> {
        let mut buf = Vec::new();
        for (kind, payload) in attrs {
            let len = u16::try_from(NLA_HDRLEN + payload.len()).expect("short attribute");
            buf.extend_from_slice(&len.to_ne_bytes());
            buf.extend_from_slice(&kind.to_ne_bytes());
            buf.extend_from_slice(payload);
            buf.resize(align(buf.len()), 0);
        }
        buf
    }

    /// Wraps attributes into a generic netlink reply of `family` with `seq`.
    pub(crate) fn reply(family: u16, seq: u32, attrs: &[u8]) -> Vec<u8> {
        let len = NLMSG_HDRLEN + GENL_HDRLEN + attrs.len();
        let mut buf = Vec::with_capacity(len);
        buf.extend_from_slice(&u32::try_from(len).expect("short reply").to_ne_bytes());
        buf.extend_from_slice(&family.to_ne_bytes());
        buf.extend_from_slice(&0u16.to_ne_bytes());
        buf.extend_from_slice(&seq.to_ne_bytes());
        buf.extend_from_slice(&0u32.to_ne_bytes());
        buf.extend_from_slice(&[0; GENL_HDRLEN]);
        buf.extend_from_slice(attrs);
        buf
    }

    #[test]
    fn request_layout() {
        let msg = Request::new(0x1c, NLM_F_ACK, 7).u32(3, 5).string(4, "wlan1").flag(9).finish(42);
        assert_eq!(msg.len(), 16 + 4 + 8 + 12 + 4);
        assert_eq!(u32::from_ne_bytes(msg[0..4].try_into().unwrap()) as usize, msg.len());
        assert_eq!(u16::from_ne_bytes([msg[4], msg[5]]), 0x1c);
        assert_eq!(u16::from_ne_bytes([msg[6], msg[7]]), NLM_F_REQUEST | NLM_F_ACK);
        assert_eq!(u32::from_ne_bytes(msg[8..12].try_into().unwrap()), 42);
        assert_eq!(msg[16], 7);
        // "wlan1\0" is 6 bytes, padded to 8.
        assert_eq!(&msg[28..30], &10u16.to_ne_bytes());
        assert_eq!(&msg[32..38], b"wlan1\0");
        assert_eq!(&msg[40..44], &[4, 0, 9, 0]);
    }

    #[test]
    fn dump_flag() {
        assert!(Request::new(1, NLM_F_DUMP, 1).is_dump());
        assert!(!Request::new(1, NLM_F_ACK, 1).is_dump());
    }

    #[test]
    fn parses_frames_and_attributes() {
        let attrs = encode(&[(1, 7u32.to_ne_bytes().to_vec()), (0x8000 | 2, b"phy0\0".to_vec())]);
        let mut datagram = reply(0x1c, 9, &attrs);
        datagram.extend_from_slice(&reply(NLMSG_DONE, 9, &[]));

        let frames: Vec<Frame<'_>> = frames(&datagram).collect();
        assert_eq!(frames.len(), 2);
        assert_eq!((frames[0].kind, frames[0].seq), (0x1c, 9));
        assert_eq!(frames[1].kind, NLMSG_DONE);

        let genl_attrs = Attrs(&frames[0].payload[GENL_HDRLEN..]);
        let parsed: Vec<Attr<'_>> = genl_attrs.collect();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].u32(), Some(7));
        assert_eq!(parsed[1].kind, 2, "flag bits are masked off");
        assert_eq!(parsed[1].string(), Some("phy0"));
        assert_eq!(genl_attrs.get(2).and_then(|a| a.string()), Some("phy0"));
    }

    #[test]
    fn error_frames() {
        let mut payload = (-19i32).to_ne_bytes().to_vec();
        payload.extend_from_slice(&[0; 16]);
        let mut datagram = reply(NLMSG_ERROR, 3, &[]);
        datagram.truncate(16);
        datagram.extend_from_slice(&payload);
        let len = u32::try_from(datagram.len()).unwrap();
        datagram[0..4].copy_from_slice(&len.to_ne_bytes());
        let frame = frames(&datagram).next().expect("one frame");
        assert_eq!(frame.error(), Some(-19));
    }

    #[test]
    fn malformed_input_ends_iteration() {
        assert_eq!(frames(&[1, 2, 3]).count(), 0);
        let mut bad = encode(&[(1, vec![1, 2, 3, 4])]);
        bad[0] = 200; // length beyond the buffer
        assert_eq!(Attrs(&bad).count(), 0);
    }
}
