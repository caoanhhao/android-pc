//! The kernel's Bluetooth management protocol, only as far as finding a controller and
//! giving an unconfigured one its public address: `Read Controller Index List`,
//! `Read Unconfigured Controller Index List`, `Set Public Address` and the index events
//! (BlueZ doc/mgmt-api.txt).

/// `MGMT_OP_READ_INDEX_LIST` for no controller (`MGMT_INDEX_NONE`), no parameters:
/// opcode, controller index and parameter length, little endian.
pub const READ_INDEX_LIST: [u8; 6] = [0x03, 0x00, 0xff, 0xff, 0x00, 0x00];
/// `MGMT_OP_READ_UNCONF_INDEX_LIST`, the same way. Controllers whose driver found no valid
/// public address (Qualcomm UART chips with the NVM's default one) are only listed here.
pub const READ_UNCONF_INDEX_LIST: [u8; 6] = [0x36, 0x00, 0xff, 0xff, 0x00, 0x00];

const EV_CMD_COMPLETE: u16 = 0x0001;
const EV_INDEX_ADDED: u16 = 0x0004;
const EV_UNCONF_INDEX_ADDED: u16 = 0x001d;
const OP_READ_INDEX_LIST: u16 = 0x0003;
const OP_READ_UNCONF_INDEX_LIST: u16 = 0x0036;
const OP_SET_PUBLIC_ADDRESS: u16 = 0x0039;
const STATUS_SUCCESS: u8 = 0;

/// A controller the management channel announced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Controller {
    pub index: u16,
    /// False until it has a valid public address.
    pub configured: bool,
}

/// The controller to use for `wanted`, if `event` announces one: `wanted` itself, else the
/// lowest index above it (as AOSP's default HAL does when hci0 is gone and the controller
/// came back as hci1).
pub fn controller_from_event(event: &[u8], wanted: u16) -> Option<Controller> {
    let le16 = |at: usize| event.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    let (code, index, len) = (le16(0)?, le16(2)?, usize::from(le16(4)?));
    let params = event.get(6..6 + len)?;
    match code {
        EV_INDEX_ADDED | EV_UNCONF_INDEX_ADDED => (index >= wanted)
            .then_some(Controller { index, configured: code == EV_INDEX_ADDED }),
        EV_CMD_COMPLETE => {
            // Command opcode, status, number of controllers, the indexes.
            let (&[op_lo, op_hi, status, n_lo, n_hi], indexes) = params.split_first_chunk()?;
            let configured = match u16::from_le_bytes([op_lo, op_hi]) {
                OP_READ_INDEX_LIST => true,
                OP_READ_UNCONF_INDEX_LIST => false,
                _ => return None,
            };
            if status != STATUS_SUCCESS {
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
                .map(|index| Controller { index, configured })
        }
        _ => None,
    }
}

/// `MGMT_OP_SET_PUBLIC_ADDRESS` for controller `index`. `address` is in display order
/// (`18:ce:..`), the command carries it little endian. The kernel programs it through the
/// driver's `set_bdaddr` at every power-on, and the controller becomes configured.
pub fn set_public_address(index: u16, address: [u8; 6]) -> [u8; 12] {
    let mut command = [0; 12];
    command[..2].copy_from_slice(&OP_SET_PUBLIC_ADDRESS.to_le_bytes());
    command[2..4].copy_from_slice(&index.to_le_bytes());
    command[4..6].copy_from_slice(&6u16.to_le_bytes());
    for (to, from) in command[6..].iter_mut().zip(address.iter().rev()) {
        *to = *from;
    }
    command
}

/// The status of `Set Public Address` if `event` completes it.
pub fn set_public_address_status(event: &[u8]) -> Option<u8> {
    let code = u16::from_le_bytes(event.get(..2)?.try_into().ok()?);
    let op = u16::from_le_bytes(event.get(6..8)?.try_into().ok()?);
    // Command Complete (0x0001) or Command Status (0x0002).
    (matches!(code, 0x0001 | 0x0002) && op == OP_SET_PUBLIC_ADDRESS).then(|| event.get(8).copied())?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index_list(op: u16, indexes: &[u16]) -> Vec<u8> {
        let mut params = op.to_le_bytes().to_vec();
        params.push(STATUS_SUCCESS);
        params.extend(u16::try_from(indexes.len()).unwrap().to_le_bytes());
        params.extend(indexes.iter().flat_map(|i| i.to_le_bytes()));
        let mut event = vec![0x01, 0x00, 0xff, 0xff];
        event.extend(u16::try_from(params.len()).unwrap().to_le_bytes());
        event.extend(params);
        event
    }

    fn configured(index: u16) -> Option<Controller> {
        Some(Controller { index, configured: true })
    }

    #[test]
    fn picks_the_wanted_or_next_controller() {
        let list = |indexes: &[u16]| index_list(OP_READ_INDEX_LIST, indexes);
        assert_eq!(controller_from_event(&list(&[2, 0]), 0), configured(0));
        assert_eq!(controller_from_event(&list(&[3, 1]), 0), configured(1));
        assert_eq!(controller_from_event(&list(&[0]), 1), None);
        assert_eq!(controller_from_event(&list(&[]), 0), None);
    }

    #[test]
    fn unconfigured_controllers() {
        assert_eq!(
            controller_from_event(&index_list(OP_READ_UNCONF_INDEX_LIST, &[0]), 0),
            Some(Controller { index: 0, configured: false })
        );
        assert_eq!(
            controller_from_event(&[0x1d, 0x00, 0x01, 0x00, 0x00, 0x00], 0),
            Some(Controller { index: 1, configured: false })
        );
    }

    #[test]
    fn index_added_event() {
        assert_eq!(controller_from_event(&[0x04, 0x00, 0x00, 0x00, 0x00, 0x00], 0), configured(0));
        // Index Removed.
        assert_eq!(controller_from_event(&[0x05, 0x00, 0x00, 0x00, 0x00, 0x00], 0), None);
    }

    #[test]
    fn ignores_failures_and_truncated_events() {
        let mut failed = index_list(OP_READ_INDEX_LIST, &[0]);
        failed[8] = 0x01;
        assert_eq!(controller_from_event(&failed, 0), None);
        let whole = index_list(OP_READ_INDEX_LIST, &[0]);
        for len in 0..whole.len() {
            assert_eq!(controller_from_event(&whole[..len], 0), None, "length {len}");
        }
    }

    #[test]
    fn public_address_command() {
        assert_eq!(
            set_public_address(0, [0x18, 0xce, 0x94, 0x0d, 0xd5, 0xdf]),
            [0x39, 0x00, 0x00, 0x00, 0x06, 0x00, 0xdf, 0xd5, 0x0d, 0x94, 0xce, 0x18]
        );
        // Captured on the Galaxy Tab S7 LTE.
        let done = [0x01, 0x00, 0x00, 0x00, 0x07, 0x00, 0x39, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        assert_eq!(set_public_address_status(&done), Some(0));
        assert_eq!(set_public_address_status(&index_list(OP_READ_INDEX_LIST, &[0])), None);
    }
}
