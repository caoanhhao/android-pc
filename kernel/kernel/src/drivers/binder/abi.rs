//! The binder user ABI (`include/uapi/linux/android/binder.h` and
//! `binderfs.h`), 64-bit, protocol version 8. Structures are handled as
//! little-endian byte arrays; the offsets are noted next to each size.

const fn ioc(dir: u32, ty: u8, nr: u32, size: usize) -> u32 {
    (dir << 30) | ((size as u32) << 16) | ((ty as u32) << 8) | nr
}
const W: u32 = 1;
const R: u32 = 2;
const RW: u32 = 3;

/// Argument size encoded in a command or return code.
pub const fn ioc_size(cmd: u32) -> usize {
    ((cmd >> 16) & 0x3fff) as usize
}

pub const PROTOCOL_VERSION: i32 = 8;

/// binder_write_read: write_size 0, write_consumed 8, write_buffer 16,
/// read_size 24, read_consumed 32, read_buffer 40.
pub const BWR_SIZE: usize = 48;
/// binder_transaction_data: target 0 (handle or ptr), cookie 8, code 16,
/// flags 20, sender_pid 24, sender_euid 28, data_size 32, offsets_size 40,
/// buffer 48, offsets 56. The _secctx and _sg variants append a u64.
pub const TR_SIZE: usize = 64;
pub const TR_SECCTX_SIZE: usize = 72;

pub const BINDER_WRITE_READ: u32 = ioc(RW, b'b', 1, BWR_SIZE);
pub const BINDER_SET_IDLE_TIMEOUT: u32 = ioc(W, b'b', 3, 8);
pub const BINDER_SET_MAX_THREADS: u32 = ioc(W, b'b', 5, 4);
pub const BINDER_SET_IDLE_PRIORITY: u32 = ioc(W, b'b', 6, 4);
pub const BINDER_SET_CONTEXT_MGR: u32 = ioc(W, b'b', 7, 4);
pub const BINDER_THREAD_EXIT: u32 = ioc(W, b'b', 8, 4);
pub const BINDER_VERSION: u32 = ioc(RW, b'b', 9, 4);
/// binder_node_debug_info: ptr 0, cookie 8, has_strong_ref 16, has_weak_ref 20.
pub const BINDER_GET_NODE_DEBUG_INFO: u32 = ioc(RW, b'b', 11, 24);
/// binder_node_info_for_ref: handle 0, strong_count 4, weak_count 8, reserved 12..24.
pub const BINDER_GET_NODE_INFO_FOR_REF: u32 = ioc(RW, b'b', 12, 24);
/// flat_binder_object (flags at 4, binder 8, cookie 16).
pub const BINDER_SET_CONTEXT_MGR_EXT: u32 = ioc(W, b'b', 13, 24);
pub const BINDER_FREEZE: u32 = ioc(W, b'b', 14, 12);
/// binder_frozen_status_info: pid 0, sync_recv 4, async_recv 8.
pub const BINDER_GET_FROZEN_INFO: u32 = ioc(RW, b'b', 15, 12);
pub const BINDER_ENABLE_ONEWAY_SPAM_DETECTION: u32 = ioc(W, b'b', 16, 4);
/// binder_extended_error: id 0, command 4, param 8.
pub const BINDER_GET_EXTENDED_ERROR: u32 = ioc(RW, b'b', 17, 12);

/// binderfs_device: name[256], major 256, minor 260.
pub const BINDERFS_DEVICE_SIZE: usize = 264;
pub const BINDER_CTL_ADD: u32 = ioc(RW, b'b', 1, BINDERFS_DEVICE_SIZE);

pub const BR_OK: u32 = ioc(0, b'r', 1, 0);
pub const BR_TRANSACTION_SEC_CTX: u32 = ioc(R, b'r', 2, TR_SECCTX_SIZE);
pub const BR_TRANSACTION: u32 = ioc(R, b'r', 2, TR_SIZE);
pub const BR_REPLY: u32 = ioc(R, b'r', 3, TR_SIZE);
pub const BR_DEAD_REPLY: u32 = ioc(0, b'r', 5, 0);
pub const BR_TRANSACTION_COMPLETE: u32 = ioc(0, b'r', 6, 0);
pub const BR_INCREFS: u32 = ioc(R, b'r', 7, 16);
pub const BR_ACQUIRE: u32 = ioc(R, b'r', 8, 16);
pub const BR_RELEASE: u32 = ioc(R, b'r', 9, 16);
pub const BR_DECREFS: u32 = ioc(R, b'r', 10, 16);
pub const BR_NOOP: u32 = ioc(0, b'r', 12, 0);
pub const BR_SPAWN_LOOPER: u32 = ioc(0, b'r', 13, 0);
pub const BR_DEAD_BINDER: u32 = ioc(R, b'r', 15, 8);
pub const BR_CLEAR_DEATH_NOTIFICATION_DONE: u32 = ioc(R, b'r', 16, 8);
pub const BR_FAILED_REPLY: u32 = ioc(0, b'r', 17, 0);

pub const BC_TRANSACTION: u32 = ioc(W, b'c', 0, TR_SIZE);
pub const BC_REPLY: u32 = ioc(W, b'c', 1, TR_SIZE);
pub const BC_ACQUIRE_RESULT: u32 = ioc(W, b'c', 2, 4);
pub const BC_FREE_BUFFER: u32 = ioc(W, b'c', 3, 8);
pub const BC_INCREFS: u32 = ioc(W, b'c', 4, 4);
pub const BC_ACQUIRE: u32 = ioc(W, b'c', 5, 4);
pub const BC_RELEASE: u32 = ioc(W, b'c', 6, 4);
pub const BC_DECREFS: u32 = ioc(W, b'c', 7, 4);
/// binder_ptr_cookie: ptr 0, cookie 8.
pub const BC_INCREFS_DONE: u32 = ioc(W, b'c', 8, 16);
pub const BC_ACQUIRE_DONE: u32 = ioc(W, b'c', 9, 16);
pub const BC_ATTEMPT_ACQUIRE: u32 = ioc(W, b'c', 10, 8);
pub const BC_REGISTER_LOOPER: u32 = ioc(0, b'c', 11, 0);
pub const BC_ENTER_LOOPER: u32 = ioc(0, b'c', 12, 0);
pub const BC_EXIT_LOOPER: u32 = ioc(0, b'c', 13, 0);
/// binder_handle_cookie (packed): handle 0, cookie 4.
pub const BC_REQUEST_DEATH_NOTIFICATION: u32 = ioc(W, b'c', 14, 12);
pub const BC_CLEAR_DEATH_NOTIFICATION: u32 = ioc(W, b'c', 15, 12);
pub const BC_DEAD_BINDER_DONE: u32 = ioc(W, b'c', 16, 8);
pub const BC_TRANSACTION_SG: u32 = ioc(W, b'c', 17, TR_SIZE + 8);
pub const BC_REPLY_SG: u32 = ioc(W, b'c', 18, TR_SIZE + 8);

const fn pack(a: u8, b: u8, c: u8) -> u32 {
    ((a as u32) << 24) | ((b as u32) << 16) | ((c as u32) << 8) | 0x85
}
/// flat_binder_object: type 0, flags 4, binder/handle 8, cookie 16 (24 bytes).
pub const BINDER_TYPE_BINDER: u32 = pack(b's', b'b', b'*');
pub const BINDER_TYPE_WEAK_BINDER: u32 = pack(b'w', b'b', b'*');
pub const BINDER_TYPE_HANDLE: u32 = pack(b's', b'h', b'*');
pub const BINDER_TYPE_WEAK_HANDLE: u32 = pack(b'w', b'h', b'*');
/// binder_fd_object: type 0, pad 4, fd 8, cookie 16 (24 bytes).
pub const BINDER_TYPE_FD: u32 = pack(b'f', b'd', b'*');
/// binder_fd_array_object: type 0, pad 4, num_fds 8, parent 16, parent_offset 24 (32 bytes).
pub const BINDER_TYPE_FDA: u32 = pack(b'f', b'd', b'a');
/// binder_buffer_object: type 0, flags 4, buffer 8, length 16, parent 24,
/// parent_offset 32 (40 bytes).
pub const BINDER_TYPE_PTR: u32 = pack(b'p', b't', b'*');
pub const BINDER_BUFFER_FLAG_HAS_PARENT: u32 = 1;

pub const FLAT_BINDER_FLAG_ACCEPTS_FDS: u32 = 0x100;
pub const FLAT_BINDER_FLAG_TXN_SECURITY_CTX: u32 = 0x1000;

pub const TF_ONE_WAY: u32 = 0x01;
pub const TF_ACCEPT_FDS: u32 = 0x10;

pub fn rd32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

pub fn rd64(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

pub fn wr32(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_le_bytes());
}

pub fn wr64(b: &mut [u8], o: usize, v: u64) {
    b[o..o + 8].copy_from_slice(&v.to_le_bytes());
}
