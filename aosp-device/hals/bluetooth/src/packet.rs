//! HCI packets and their H4 framing: a one-byte packet type followed by the packet.
//!
//! The HCI user channel carries one H4 frame per datagram; a UART carries a byte stream
//! that [`H4Decoder`] splits back into frames.

use std::fmt;

/// The H4 packet type byte (Bluetooth Core, Vol 4, Part A, 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PacketType {
    Command = 1,
    Acl = 2,
    Sco = 3,
    Event = 4,
    Iso = 5,
}

impl PacketType {
    /// Header length after the type byte, and how to read the payload length from it.
    fn header(self) -> (usize, fn(&[u8]) -> usize) {
        match self {
            Self::Command => (3, |h| h[2].into()),
            Self::Acl => (4, |h| u16::from_le_bytes([h[2], h[3]]).into()),
            Self::Sco => (3, |h| h[2].into()),
            Self::Event => (2, |h| h[1].into()),
            // The top two bits of the ISO length field are reserved.
            Self::Iso => (4, |h| (u16::from_le_bytes([h[2], h[3]]) & 0x3fff).into()),
        }
    }
}

impl TryFrom<u8> for PacketType {
    type Error = InvalidPacketType;

    fn try_from(byte: u8) -> Result<Self, Self::Error> {
        match byte {
            1 => Ok(Self::Command),
            2 => Ok(Self::Acl),
            3 => Ok(Self::Sco),
            4 => Ok(Self::Event),
            5 => Ok(Self::Iso),
            _ => Err(InvalidPacketType(byte)),
        }
    }
}

/// A frame started with a byte that is not an H4 packet type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidPacketType(pub u8);

impl fmt::Display for InvalidPacketType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid H4 packet type 0x{:02x}", self.0)
    }
}

impl std::error::Error for InvalidPacketType {}

/// One HCI packet, without the H4 type byte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet {
    pub kind: PacketType,
    pub data: Vec<u8>,
}

/// HCI_Reset (opcode 0x0c03), no parameters.
pub const HCI_RESET: [u8; 3] = [0x03, 0x0c, 0x00];

impl Packet {
    /// Parses one H4 frame, as read from an HCI user channel datagram.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidPacketType`] for an empty frame (type 0) or an unknown type byte.
    pub fn from_frame(frame: &[u8]) -> Result<Self, InvalidPacketType> {
        let (&kind, data) = frame.split_first().ok_or(InvalidPacketType(0))?;
        Ok(Self { kind: kind.try_into()?, data: data.to_vec() })
    }

    /// The H4 frame for `data` sent as `kind`.
    pub fn frame(kind: PacketType, data: &[u8]) -> Vec<u8> {
        let mut frame = Vec::with_capacity(1 + data.len());
        frame.push(kind as u8);
        frame.extend_from_slice(data);
        frame
    }

    /// Whether this is the Command Complete event for [`HCI_RESET`] with status success.
    pub fn is_reset_complete(&self) -> bool {
        // Event code, length, number of allowed command packets (ignored), opcode, status.
        self.kind == PacketType::Event
            && matches!(self.data.as_slice(), [0x0e, 0x04, _, 0x03, 0x0c, 0x00])
    }
}

/// Splits an H4 byte stream into packets.
#[derive(Debug, Default)]
pub struct H4Decoder {
    buf: Vec<u8>,
}

impl H4Decoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends bytes read from the stream.
    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Takes the next complete packet off the front of the stream, if there is one.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidPacketType`] if the stream is out of sync. The decoder then
    /// drops everything it holds, since there is no way to find the next frame.
    pub fn next_packet(&mut self) -> Result<Option<Packet>, InvalidPacketType> {
        let Some(&type_byte) = self.buf.first() else {
            return Ok(None);
        };
        let kind = PacketType::try_from(type_byte).inspect_err(|_| self.buf.clear())?;
        let (header_len, payload_len) = kind.header();
        let Some(header) = self.buf.get(1..1 + header_len) else {
            return Ok(None);
        };
        let end = 1 + header_len + payload_len(header);
        if self.buf.len() < end {
            return Ok(None);
        }
        let data = self.buf[1..end].to_vec();
        self.buf.drain(..end);
        Ok(Some(Packet { kind, data }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RESET_COMPLETE: [u8; 7] = [4, 0x0e, 0x04, 0x01, 0x03, 0x0c, 0x00];

    #[test]
    fn frame_round_trips() {
        let frame = Packet::frame(PacketType::Command, &HCI_RESET);
        assert_eq!(frame, [1, 0x03, 0x0c, 0x00]);
        let packet = Packet::from_frame(&frame).unwrap();
        assert_eq!(packet, Packet { kind: PacketType::Command, data: HCI_RESET.to_vec() });
    }

    #[test]
    fn rejects_unknown_and_empty_frames() {
        assert_eq!(Packet::from_frame(&[9, 1]), Err(InvalidPacketType(9)));
        assert_eq!(Packet::from_frame(&[]), Err(InvalidPacketType(0)));
    }

    #[test]
    fn recognises_reset_complete() {
        assert!(Packet::from_frame(&RESET_COMPLETE).unwrap().is_reset_complete());
        let mut failed = RESET_COMPLETE;
        failed[6] = 0x01;
        assert!(!Packet::from_frame(&failed).unwrap().is_reset_complete());
    }

    #[test]
    fn decoder_waits_for_whole_packets() {
        let mut decoder = H4Decoder::new();
        decoder.push(&RESET_COMPLETE[..4]);
        assert_eq!(decoder.next_packet(), Ok(None));
        decoder.push(&RESET_COMPLETE[4..]);
        assert!(decoder.next_packet().unwrap().unwrap().is_reset_complete());
        assert_eq!(decoder.next_packet(), Ok(None));
    }

    #[test]
    fn decoder_splits_back_to_back_packets() {
        // ACL handle 0x0001, 3 bytes; ISO length with the reserved bits set; SCO 1 byte.
        let mut stream = vec![2, 0x01, 0x00, 0x03, 0x00, 0xa, 0xb, 0xc];
        stream.extend([5, 0x01, 0x00, 0x01, 0xc0, 0xd]);
        stream.extend([3, 0x01, 0x00, 0x01, 0xe]);
        let mut decoder = H4Decoder::new();
        decoder.push(&stream);
        let kinds: Vec<(PacketType, usize)> = std::iter::from_fn(|| decoder.next_packet().unwrap())
            .map(|p| (p.kind, p.data.len()))
            .collect();
        assert_eq!(kinds, [(PacketType::Acl, 7), (PacketType::Iso, 5), (PacketType::Sco, 4)]);
    }

    #[test]
    fn decoder_resyncs_after_garbage() {
        let mut decoder = H4Decoder::new();
        decoder.push(&[0xff, 4, 0x0e]);
        assert_eq!(decoder.next_packet(), Err(InvalidPacketType(0xff)));
        decoder.push(&RESET_COMPLETE);
        assert!(decoder.next_packet().unwrap().unwrap().is_reset_complete());
    }
}
