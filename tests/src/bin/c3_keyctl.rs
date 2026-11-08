//! C3: kernel keys, as init (session keyring) and fscrypt ("fscrypt"
//! keyring, logon keys) use them: KEYCTL_GET_KEYRING_ID, add_key of user,
//! logon and keyring keys, update by a second add_key, KEYCTL_READ,
//! DESCRIBE, SEARCH (also through a nested keyring), LINK/UNLINK,
//! SETPERM, REVOKE, request_key and the errors (ENOKEY, EKEYREVOKED,
//! EOPNOTSUPP for logon payloads, EINVAL for bad special ids).
#![no_std]
#![no_main]

use rt::*;

rt::test_main!("c3_keyctl", main);

const ADD_KEY: usize = 248;
const REQUEST_KEY: usize = 249;
const KEYCTL: usize = 250;
const KEY_SPEC_SESSION_KEYRING: i32 = -3;
const KEYCTL_GET_KEYRING_ID: usize = 0;
const KEYCTL_JOIN_SESSION_KEYRING: usize = 1;
const KEYCTL_REVOKE: usize = 3;
const KEYCTL_SETPERM: usize = 5;
const KEYCTL_DESCRIBE: usize = 6;
const KEYCTL_LINK: usize = 8;
const KEYCTL_UNLINK: usize = 9;
const KEYCTL_SEARCH: usize = 10;
const KEYCTL_READ: usize = 11;

fn add_key(ty: &core::ffi::CStr, desc: &core::ffi::CStr, payload: &[u8], ring: i32) -> isize {
    unsafe { syscall!(ADD_KEY, ty.as_ptr(), desc.as_ptr(), payload.as_ptr(), payload.len(), ring) }
}

fn keyctl(op: usize, a: usize, b: usize, c: usize, d: usize) -> isize {
    unsafe { syscall!(KEYCTL, op, a, b, c, d) }
}

fn main() {
    // A fresh anonymous session keyring for this test.
    let ses = keyctl(KEYCTL_JOIN_SESSION_KEYRING, 0, 0, 0, 0);
    if ses == -(ENOSYS as isize) || ses == -(EPERM as isize) {
        skip("all", "no key retention service here");
        return;
    }
    check!(ses > 0, "join_session", "{}", errname(ses));
    let id = keyctl(KEYCTL_GET_KEYRING_ID, KEY_SPEC_SESSION_KEYRING as usize, 1, 0, 0);
    check_eq!(id, ses, "get_session_id");
    check_err!(keyctl(KEYCTL_GET_KEYRING_ID, -100i32 as usize, 0, 0, 0), EINVAL, "bad_special_id");

    let k = add_key(c"user", c"c3:test", b"payload-1", KEY_SPEC_SESSION_KEYRING);
    check!(k > 0, "add_user_key", "{}", errname(k));
    let mut buf = [0u8; 128];
    let n = keyctl(KEYCTL_READ, k as usize, buf.as_mut_ptr() as usize, buf.len(), 0);
    check!(n == 9 && &buf[..9] == b"payload-1", "read_payload", "{}", errname(n));
    let n = keyctl(KEYCTL_READ, k as usize, 0, 0, 0);
    check_eq!(n, 9, "read_size_only");
    let n = keyctl(KEYCTL_DESCRIBE, k as usize, buf.as_mut_ptr() as usize, buf.len(), 0);
    check!(n > 0 && buf.starts_with(b"user;") && trim(&buf[..n as usize]).ends_with(b";c3:test"), "describe",
        "{}", Bytes(&buf[..n.max(0) as usize]));

    // Same type and description: updated in place, same serial.
    let k2 = add_key(c"user", c"c3:test", b"payload-two", KEY_SPEC_SESSION_KEYRING);
    check_eq!(k2, k, "add_again_same_id");
    let n = keyctl(KEYCTL_READ, k as usize, buf.as_mut_ptr() as usize, buf.len(), 0);
    check!(n == 11 && &buf[..11] == b"payload-two", "add_again_updates");

    // The session keyring lists the key.
    let n = keyctl(KEYCTL_READ, ses as usize, buf.as_mut_ptr() as usize, buf.len(), 0);
    let listed = buf[..n.max(0) as usize].chunks(4).any(|c| i32::from_le_bytes(c.try_into().unwrap()) as isize == k);
    check!(listed, "keyring_lists_key");

    // fscrypt-style: a keyring in the session keyring with a logon key.
    let ring = add_key(c"keyring", c"fscrypt", b"", KEY_SPEC_SESSION_KEYRING);
    check!(ring > 0, "add_keyring", "{}", errname(ring));
    check_eq!(keyctl(KEYCTL_SETPERM, ring as usize, 0x3f3f_0000, 0, 0), 0, "setperm");
    let lk = add_key(c"logon", c"fscrypt:0011223344556677", &[7u8; 64], ring as i32);
    check!(lk > 0, "add_logon_key", "{}", errname(lk));
    check_err!(keyctl(KEYCTL_READ, lk as usize, buf.as_mut_ptr() as usize, buf.len(), 0), EOPNOTSUPP, "logon_unreadable");
    let found = keyctl(KEYCTL_SEARCH, ses as usize, c"keyring".as_ptr() as usize, c"fscrypt".as_ptr() as usize, 0);
    check_eq!(found, ring, "search_keyring");
    let found = keyctl(KEYCTL_SEARCH, ses as usize, c"logon".as_ptr() as usize, c"fscrypt:0011223344556677".as_ptr() as usize, 0);
    check_eq!(found, lk, "search_nested");
    check_err!(
        keyctl(KEYCTL_SEARCH, ses as usize, c"user".as_ptr() as usize, c"c3:none".as_ptr() as usize, 0),
        ENOKEY,
        "search_missing"
    );

    // Link the user key into the fscrypt keyring too, then unlink it.
    check_eq!(keyctl(KEYCTL_LINK, k as usize, ring as usize, 0, 0), 0, "link");
    let found = keyctl(KEYCTL_SEARCH, ring as usize, c"user".as_ptr() as usize, c"c3:test".as_ptr() as usize, 0);
    check_eq!(found, k, "search_linked");
    check_eq!(keyctl(KEYCTL_UNLINK, k as usize, ring as usize, 0, 0), 0, "unlink");
    check_err!(
        keyctl(KEYCTL_SEARCH, ring as usize, c"user".as_ptr() as usize, c"c3:test".as_ptr() as usize, 0),
        ENOKEY,
        "search_after_unlink"
    );

    // request_key finds it in the session keyring; a missing one is ENOKEY.
    let r = unsafe { syscall!(REQUEST_KEY, c"user".as_ptr(), c"c3:test".as_ptr(), 0, 0) };
    check_eq!(r, k, "request_key");
    let r = unsafe { syscall!(REQUEST_KEY, c"user".as_ptr(), c"c3:missing".as_ptr(), 0, 0) };
    check!(r < 0, "request_key_missing", "{}", errname(r));

    // Revoked keys cannot be read.
    check_eq!(keyctl(KEYCTL_REVOKE, k as usize, 0, 0, 0), 0, "revoke");
    check_err!(keyctl(KEYCTL_READ, k as usize, buf.as_mut_ptr() as usize, buf.len(), 0), EKEYREVOKED, "read_revoked");
    check_err!(add_key(c"user", c"x", b"", -100), EINVAL, "add_bad_ring");
}
