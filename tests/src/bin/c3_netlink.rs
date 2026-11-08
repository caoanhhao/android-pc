//! C3: AF_NETLINK. NETLINK_KOBJECT_UEVENT sockets as ueventd opens them
//! (SOCK_DGRAM, group 1, SO_PASSCRED, SO_RCVBUFFORCE, non-blocking):
//! getsockname, EAGAIN when idle, and on the Rust kernel the uevents
//! themselves: writing "change"/"add" to a sysfs `uevent` file (the
//! coldboot mechanism) and a hotplugged loop device, each received with
//! the kernel's sender address (port 0, group 1), SCM_CREDENTIALS of uid 0
//! and the KEY=value payload. NETLINK_ROUTE: a dump request is answered
//! and ends with NLMSG_DONE. Also the sysfs layout ueventd relies on:
//! /sys/class and /sys/block entries are symlinks into /sys/devices,
//! device directories have a `subsystem` link.
#![no_std]
#![no_main]

use rt::*;

rt::test_main!("c3_netlink", main);

const AF_NETLINK: i32 = 16;
const NETLINK_ROUTE: i32 = 0;
const NETLINK_KOBJECT_UEVENT: i32 = 15;
const SO_RCVBUFFORCE: i32 = 33;
const SOCK_RAW: i32 = 3;
const BIND: usize = 49;
const GETSOCKNAME: usize = 51;
const SENDTO: usize = 44;
const NLMSG_ERROR: u16 = 2;
const NLMSG_DONE: u16 = 3;
const RTM_GETLINK: u16 = 18;
const LOOP_CTL_ADD: u32 = 0x4c80;
const LOOP_CTL_REMOVE: u32 = 0x4c81;

fn sock3(domain: i32, ty: i32, proto: i32) -> isize {
    unsafe { syscall!(41, domain, ty, proto) }
}

fn nl_addr(pid: u32, groups: u32) -> [u8; 12] {
    let mut a = [0u8; 12];
    a[..2].copy_from_slice(&(AF_NETLINK as u16).to_le_bytes());
    a[4..8].copy_from_slice(&pid.to_le_bytes());
    a[8..].copy_from_slice(&groups.to_le_bytes());
    a
}

struct Msg {
    data: [u8; 2048],
    len: usize,
    src: [u8; 12],
    uid: Option<u32>,
}

/// One message (non-blocking unless `block`).
fn recv_msg(fd: i32) -> Result<Msg, isize> {
    let mut m = Msg { data: [0; 2048], len: 0, src: [0; 12], uid: None };
    let mut ctl = [0u8; 64];
    let mut iov = Iovec { base: m.data.as_mut_ptr(), len: m.data.len() };
    let mut h = Msghdr {
        name: m.src.as_mut_ptr(),
        namelen: 12,
        iov: &mut iov,
        iovlen: 1,
        control: ctl.as_mut_ptr(),
        controllen: ctl.len(),
        flags: 0,
    };
    let r = recvmsg(fd, &mut h, 0);
    if r < 0 {
        return Err(r);
    }
    m.len = r as usize;
    if h.controllen >= 28 && i32::from_le_bytes(ctl[12..16].try_into().unwrap()) == SCM_CREDENTIALS {
        m.uid = Some(u32::from_le_bytes(ctl[20..24].try_into().unwrap()));
    }
    Ok(m)
}

/// The uevent whose header is `head`, among those queued.
fn find_uevent(fd: i32, head: &[u8]) -> Option<Msg> {
    for _ in 0..200 {
        match recv_msg(fd) {
            Ok(m) if m.data[..m.len].starts_with(head) => return Some(m),
            Ok(_) => continue,
            Err(_) => {
                sleep_ms(5);
            }
        }
    }
    None
}

fn has_var(m: &Msg, var: &[u8]) -> bool {
    m.data[..m.len].split(|&c| c == 0).any(|f| f == var)
}

fn uevent_socket() -> i32 {
    let fd = sock3(AF_NETLINK, SOCK_DGRAM | SOCK_CLOEXEC | SOCK_NONBLOCK, NETLINK_KOBJECT_UEVENT);
    if fd < 0 {
        return fd as i32;
    }
    let fd = fd as i32;
    let big = 1i32 << 20;
    setsockopt(fd, SOL_SOCKET, SO_RCVBUFFORCE, &big as *const i32 as *const u8, 4);
    let on = 1i32;
    setsockopt(fd, SOL_SOCKET, SO_PASSCRED, &on as *const i32 as *const u8, 4);
    let a = nl_addr(0, 0xffff_ffff);
    let r = unsafe { syscall!(BIND, fd, a.as_ptr(), 12) };
    if r < 0 {
        close(fd);
        return r as i32;
    }
    fd
}

fn main() {
    let fd = uevent_socket();
    if fd < 0 {
        skip("all", "no NETLINK_KOBJECT_UEVENT");
        return;
    }
    check!(fd >= 0, "uevent_socket");
    let mut a = [0u8; 12];
    let mut alen = 12u32;
    check_ok!(unsafe { syscall!(GETSOCKNAME, fd, a.as_mut_ptr(), &mut alen as *mut u32) }, "getsockname");
    check!(u16::from_le_bytes([a[0], a[1]]) == AF_NETLINK as u16 && u32::from_le_bytes(a[4..8].try_into().unwrap()) != 0,
        "bound_port_id");
    while recv_msg(fd).is_ok() {}
    check!(matches!(recv_msg(fd), Err(e) if e == -(EAGAIN as isize)), "idle_eagain");
    check_err!(sock3(AF_NETLINK, SOCK_DGRAM, 32), EPROTONOSUPPORT, "bad_protocol");
    check_err!(sock3(AF_NETLINK, SOCK_STREAM, 0), ESOCKTNOSUPPORT_OR_PROTO, "stream_refused");

    // NETLINK_ROUTE: RTM_GETLINK dump ends with NLMSG_DONE.
    let rt = sock3(AF_NETLINK, SOCK_RAW | SOCK_CLOEXEC, NETLINK_ROUTE);
    check!(rt >= 0, "route_socket", "{}", errname(rt));
    let mut req = [0u8; 32];
    req[..4].copy_from_slice(&32u32.to_le_bytes());
    req[4..6].copy_from_slice(&RTM_GETLINK.to_le_bytes());
    req[6..8].copy_from_slice(&(1u16 | 0x300).to_le_bytes()); // REQUEST | DUMP
    req[8..12].copy_from_slice(&7u32.to_le_bytes());
    let kernel = nl_addr(0, 0);
    let r = unsafe { syscall!(SENDTO, rt, req.as_ptr(), req.len(), 0, kernel.as_ptr(), 12) };
    check_eq!(r, 32, "route_send");
    let mut done = false;
    let mut buf = [0u8; 16384];
    for _ in 0..64 {
        let n = recv(rt as i32, &mut buf, 0);
        if n <= 0 {
            break;
        }
        let mut off = 0;
        while off + 16 <= n as usize {
            let len = u32::from_le_bytes(buf[off..off + 4].try_into().unwrap()) as usize;
            let ty = u16::from_le_bytes(buf[off + 4..off + 6].try_into().unwrap());
            let seq = u32::from_le_bytes(buf[off + 8..off + 12].try_into().unwrap());
            if (ty == NLMSG_DONE || ty == NLMSG_ERROR) && seq == 7 {
                done = true;
            }
            if len < 16 {
                break;
            }
            off += (len + 3) & !3;
        }
        if done {
            break;
        }
    }
    check!(done, "route_dump_done");
    close(rt as i32);

    if !is_rust_kernel() {
        skip("uevents", "triggering uevents needs root on Linux");
        return;
    }

    // sysfs layout.
    let mut l = [0u8; 256];
    let n = readlink(c"/sys/class/mem/null".as_ptr() as *const u8, &mut l);
    check!(n > 0 && l[..n as usize].ends_with(b"devices/virtual/mem/null"), "class_link_into_devices");
    let n = readlink(c"/sys/devices/virtual/mem/null/subsystem".as_ptr() as *const u8, &mut l);
    check!(n > 0 && l[..n as usize].ends_with(b"class/mem"), "subsystem_link");
    let n = readlink(c"/sys/block/loop0".as_ptr() as *const u8, &mut l);
    check!(n > 0 && l[..n as usize].ends_with(b"devices/virtual/block/loop0"), "block_link");
    let n = read_file(c"/sys/devices/virtual/mem/null/uevent".as_ptr() as *const u8, &mut l);
    check!(n > 0 && &l[..n as usize] == b"MAJOR=1\nMINOR=3\nDEVNAME=null\n", "uevent_file_contents");

    // Coldboot: write an action to a uevent file.
    check_eq!(write_file(c"/sys/devices/virtual/mem/null/uevent".as_ptr() as *const u8, b"change\n"), 7, "write_uevent");
    match find_uevent(fd, b"change@/devices/virtual/mem/null\0") {
        Some(m) => {
            check!(has_var(&m, b"ACTION=change") && has_var(&m, b"DEVPATH=/devices/virtual/mem/null"), "uevent_action_devpath");
            check!(has_var(&m, b"SUBSYSTEM=mem") && has_var(&m, b"MAJOR=1") && has_var(&m, b"DEVNAME=null"), "uevent_vars");
            check!(m.data[..m.len].split(|&c| c == 0).any(|f| f.starts_with(b"SEQNUM=")), "uevent_seqnum");
            check!(u32::from_le_bytes(m.src[4..8].try_into().unwrap()) == 0
                && u32::from_le_bytes(m.src[8..12].try_into().unwrap()) == 1, "uevent_from_kernel_group1");
            check_eq!(m.uid, Some(0), "uevent_creds_uid0");
        }
        None => {
            check!(false, "uevent_received");
        }
    }
    check_err!(write_file(c"/sys/devices/virtual/mem/null/uevent".as_ptr() as *const u8, b"bogus"), EINVAL, "bad_action");

    // Hotplug: a new loop device announces itself, and goes away.
    let ctl = open(c"/dev/loop-control".as_ptr() as *const u8, O_RDWR, 0) as i32;
    let n = ioctl(ctl, LOOP_CTL_ADD, 200);
    check_eq!(n, 200, "loop_add");
    let add = find_uevent(fd, b"add@/devices/virtual/block/loop200\0");
    check!(add.as_ref().is_some_and(|m| has_var(m, b"SUBSYSTEM=block") && has_var(m, b"DEVNAME=loop200")
        && has_var(m, b"DEVTYPE=disk")), "hotplug_add");
    check_eq!(ioctl(ctl, LOOP_CTL_REMOVE, 200), 200, "loop_remove");
    check!(find_uevent(fd, b"remove@/devices/virtual/block/loop200\0").is_some(), "hotplug_remove");
    close(ctl);

    // A second listener sees the same broadcast.
    let fd2 = uevent_socket();
    write_file(c"/sys/class/mem/zero/uevent".as_ptr() as *const u8, b"add");
    check!(find_uevent(fd2, b"add@/devices/virtual/mem/zero\0").is_some(), "second_listener");
    close(fd2);
    close(fd);
}

/// Linux refuses SOCK_STREAM netlink sockets with ESOCKTNOSUPPORT.
const ESOCKTNOSUPPORT_OR_PROTO: i32 = 94;
