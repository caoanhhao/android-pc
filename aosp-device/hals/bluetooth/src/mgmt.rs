//! The kernel's Bluetooth management protocol, only as far as finding a controller:
//! `Read Controller Index List` and the `Index Added` event (BlueZ doc/mgmt-api.txt).

/// `MGMT_OP_READ_INDEX_LIST` for no controller (`MGMT_INDEX_NONE`), no parameters:
/// opcode, controller index and parameter length, little endian.
pub const READ_INDEX_LIST: [u8; 6] = [0x03, 0x00, 0xff, 0xff, 0x00, 0x00];

const EV_CMD_COMPLETE: u16 = 0x0001;
const EV_INDEX_ADDED: u16 = 0x0004;
const OP_READ_INDEX_LIST: u16 = 0x0003;
const STATUS_SUCCESS: u8 = 0;

/// The controller index to use for `wanted`, if `event` announces one: `wanted` itself,
/// else the lowest index above it (as AOSP's default HAL does when hci0 is gone and the
/// controller came back as hci1).
pub fn controller_from_event(event: &[u8], wanted: u16) -> Option<u16> {
    let le16 = |at: usize| event.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    let (code, index, len) = (le16(0)?, le16(2)?, usize::from(le16(4)?));
    let params = event.get(6..6 + len)?;
    match code {
        EV_INDEX_ADDED => (index >= wanted).then_some(index),
        EV_CMD_COMPLETE => {
            // Command opcode, status, number of controllers, the indexes.
            let (&[op_lo, op_hi, status, n_lo, n_hi], indexes) = params.split_first_chunk()?;
            if u16::from_le_bytes([op_lo, op_hi]) != OP_READ_INDEX_LIST || status != STATUS_SUCCESS
            {
                return None;
            }
            let count = usize::from(u16::from_le_bytes([n_lo, n_hi]));
            indexes
                .as_chunks::<2>()
                .0
                .iter()
                .take(count)
                .map(|&b| u16::from_le_bytes(b))
                .filter(|&index| index >= wanted)
                .min()
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index_list(indexes: &[u16]) -> Vec<u8> {
        let mut params = vec![0x03, 0x00, STATUS_SUCCESS];
        params.extend(u16::try_from(indexes.len()).unwrap().to_le_bytes());
        params.extend(indexes.iter().flat_map(|i| i.to_le_bytes()));
        let mut event = vec![0x01, 0x00, 0xff, 0xff];
        event.extend(u16::try_from(params.len()).unwrap().to_le_bytes());
        event.extend(params);
        event
    }

    #[test]
    fn picks_the_wanted_or_next_controller() {
        assert_eq!(controller_from_event(&index_list(&[2, 0]), 0), Some(0));
        assert_eq!(controller_from_event(&index_list(&[3, 1]), 0), Some(1));
        assert_eq!(controller_from_event(&index_list(&[0]), 1), None);
        assert_eq!(controller_from_event(&index_list(&[]), 0), None);
    }

    #[test]
    fn index_added_event() {
        assert_eq!(controller_from_event(&[0x04, 0x00, 0x00, 0x00, 0x00, 0x00], 0), Some(0));
        // Index Removed.
        assert_eq!(controller_from_event(&[0x05, 0x00, 0x00, 0x00, 0x00, 0x00], 0), None);
    }

    #[test]
    fn ignores_failures_and_truncated_events() {
        let mut failed = index_list(&[0]);
        failed[8] = 0x01;
        assert_eq!(controller_from_event(&failed, 0), None);
        let whole = index_list(&[0]);
        for len in 0..whole.len() {
            assert_eq!(controller_from_event(&whole[..len], 0), None, "length {len}");
        }
    }
}
