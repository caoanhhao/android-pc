//! hyprtopd: the process table for HyprTop (docs/hypr-desktop.md, "Apps (E8)").
//!
//! SELinux keeps every app domain, system_app included, out of other domains' /proc files, so
//! HyprTop cannot list processes itself. init starts this daemon with the readproc group and its
//! own domain; HyprTop connects to the `hyprtopd` socket and writes one byte per refresh, and the
//! daemon answers with one line per process, ended by a line with a single `.`:
//!
//! `pid uid ticks threads rss_pages state name`
//!
//! `ticks` is utime + stime in clock ticks; `name` (the rest of the line) is the first word of the
//! command line without its directory, or the kernel's command name in brackets-free form.

use std::fmt::Write as _;
use std::fs;
use std::io::{Read, Write};
use std::os::fd::FromRawFd;
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::thread;

fn process_line(pid: u32, out: &mut String) -> Option<()> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let open = stat.find('(')?;
    let close = stat.rfind(')')?;
    let comm = &stat[open + 1..close];
    // Fields after the command name start at field 3 (state).
    let f: Vec<&str> = stat.get(close + 2..)?.split(' ').collect();
    if f.len() < 22 {
        return None;
    }
    let ticks = f[11].parse::<u64>().ok()? + f[12].parse::<u64>().ok()?;
    let uid = fs::metadata(format!("/proc/{pid}")).ok()?.uid();
    let cmdline = fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
    let first = cmdline.split(|&b| b == 0).next().unwrap_or_default();
    let first = String::from_utf8_lossy(first);
    let name = match first.trim() {
        "" => comm.to_string(),
        arg => arg.rsplit('/').next().unwrap_or(arg).to_string(),
    };
    let name = name.replace(['\n', '\r'], " ");
    writeln!(out, "{pid} {uid} {ticks} {} {} {} {name}", f[17], f[21], f[0]).ok()
}

fn table() -> String {
    let mut out = String::with_capacity(64 * 1024);
    if let Ok(dir) = fs::read_dir("/proc") {
        for entry in dir.flatten() {
            if let Some(pid) = entry.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) {
                process_line(pid, &mut out);
            }
        }
    }
    out.push_str(".\n");
    out
}

fn serve(mut client: UnixStream) {
    let mut request = [0u8; 1];
    while matches!(client.read(&mut request), Ok(1)) {
        if client.write_all(table().as_bytes()).is_err() {
            break;
        }
    }
}

fn main() {
    // init passes the socket from hyprtopd.rc's `socket` line in ANDROID_SOCKET_hyprtopd.
    let fd: i32 = std::env::var("ANDROID_SOCKET_hyprtopd")
        .ok()
        .and_then(|v| v.parse().ok())
        .expect("hyprtopd: no init socket");
    // SAFETY: init hands this process the socket's descriptor, which nothing else owns.
    let listener = unsafe {
        if libc::listen(fd, 4) != 0 {
            panic!("hyprtopd: listen failed");
        }
        UnixListener::from_raw_fd(fd)
    };
    for client in listener.incoming().flatten() {
        thread::spawn(move || serve(client));
    }
}
