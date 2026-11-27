//! fpctl: run the fingerprint flows from a root shell, for bring-up without the HAL.
//!
//!   fpctl enroll USER SLOT OUT   enroll a finger into SLOT (1..4), write the template to OUT
//!   fpctl identify USER FILE...  match a finger against template FILEs until one matches

use std::process::ExitCode;
use std::time::Duration;
use std::{env, fs, thread};

use std::sync::Arc;

use gts7l_fingerprint::engine::{CancelHandle, Device, Event};
use gts7l_fingerprint::securefp::SecureFp;

/// Cancels the operation on SIGINT/SIGTERM or after FPCTL_TIMEOUT seconds (default 90), so
/// the TA always gets its Cancel before it is shut down.
fn arm_cancel(cancel: CancelHandle) {
    let secs = env::var("FPCTL_TIMEOUT").ok().and_then(|s| s.parse().ok()).unwrap_or(90);
    let c = cancel.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_secs(secs));
        eprintln!("fpctl: timeout");
        c.cancel();
    });
    thread::spawn(move || {
        let mut sig = 0;
        // SAFETY: waits on the set main() blocked before any thread started.
        unsafe {
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            libc::sigaddset(&mut set, libc::SIGINT);
            libc::sigaddset(&mut set, libc::SIGTERM);
            libc::sigaddset(&mut set, libc::SIGHUP);
            libc::sigwait(&set, &mut sig);
        }
        eprintln!("fpctl: signal {sig}");
        cancel.cancel();
    });
}

fn report(e: Event) {
    println!("event: {e:x?}");
}

fn run(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    match args {
        [cmd, user, slot, out] if cmd == "enroll" => {
            let (mut dev, cancel) = Device::open(Arc::new(SecureFp::start()?), user)?;
            arm_cancel(cancel);
            let template = dev.enroll(user, slot.parse()?, &mut report)?;
            fs::write(out, &template)?;
            println!("template: {} bytes -> {out}", template.len());
        }
        [cmd, user, files @ ..] if cmd == "identify" && !files.is_empty() => {
            let mut templates = Vec::new();
            for f in files {
                templates.extend(fs::read(f)?);
            }
            let (mut dev, cancel) = Device::open(Arc::new(SecureFp::start()?), user)?;
            arm_cancel(cancel);
            let (m, updated) = dev.identify(user, &templates, 0x1234_5678_9abc_def0, &mut report, &|| false)?;
            println!("match: {m:x?}, updated template {} bytes", updated.len());
        }
        _ => return Err("usage: fpctl enroll USER SLOT OUT | fpctl identify USER FILE...".into()),
    }
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    // SAFETY: blocks the termination signals in this (only) thread; arm_cancel's thread
    // inherits the mask and takes them with sigwait.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGINT);
        libc::sigaddset(&mut set, libc::SIGTERM);
        libc::sigaddset(&mut set, libc::SIGHUP);
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
    }
    if env::var_os("FPCTL_DEBUG").is_some() {
        log::set_max_level(log::LevelFilter::Debug);
        log::set_logger(&StderrLogger).ok();
    }
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("fpctl: {e}");
            ExitCode::FAILURE
        }
    }
}

struct StderrLogger;

impl log::Log for StderrLogger {
    fn enabled(&self, _: &log::Metadata) -> bool {
        true
    }
    fn log(&self, r: &log::Record) {
        eprintln!("{} {}", r.level(), r.args());
    }
    fn flush(&self) {}
}
