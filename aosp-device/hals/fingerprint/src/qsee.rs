//! Client of one QSEE (TrustZone) app through `/dev/qseecom`.
//!
//! The ABI is ours (kernel patch 0301, `include/uapi/misc/qcom_qseecom.h`): one open file is
//! one client of one app. `APP_LOAD` binds the file to an app and starts it from its split
//! image (`<name>.mdt` followed by `<name>.b00`.. `.bNN`) when the secure world does not run
//! it yet; `APP_SEND` calls it with a request and a response buffer; closing the file drops
//! the client, and the driver shuts the app down with its last client.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::path::Path;

const QSEECOM_DEV: &str = "/dev/qseecom";
const APP_NAME_SIZE: usize = 64;
/// Largest request or response the driver takes.
pub const MAX_BUF_SIZE: usize = 1 << 20;

#[repr(C)]
struct AppLoad {
    name: [u8; APP_NAME_SIZE],
    image: u64,
    image_size: u32,
    mdt_size: u32,
    app_id: u32,
    flags: u32,
}

#[repr(C)]
struct AppSend {
    req: u64,
    rsp: u64,
    req_size: u32,
    rsp_size: u32,
}

const fn ioc(dir: u32, nr: u32, size: usize) -> u32 {
    (dir << 30) | ((size as u32) << 16) | (0xF7 << 8) | nr
}
const IOC_APP_LOAD: u32 = ioc(3, 1, std::mem::size_of::<AppLoad>());
const IOC_APP_SEND: u32 = ioc(1, 2, std::mem::size_of::<AppSend>());

/// Calls `ioctl(2)` and turns -1 into the errno.
pub(crate) fn ioctl<T>(file: &File, request: u32, arg: *mut T) -> io::Result<()> {
    // SAFETY: the request numbers and argument types match the kernel drivers' ABI; `arg`
    // points to a live value of the type the request expects (or is null for `_IO` ones).
    let ret = unsafe { libc::ioctl(file.as_raw_fd(), request as _, arg) };
    if ret < 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
}

/// Reads `<dir>/<name>.mdt` and appends the segment files its program headers list.
fn read_image(dir: &Path, name: &str) -> io::Result<(Vec<u8>, usize)> {
    let mut image = fs::read(dir.join(format!("{name}.mdt")))?;
    let mdt_size = image.len();
    let phnum = match image.get(4) {
        Some(2) if image.len() >= 0x3A => u16::from_le_bytes([image[0x38], image[0x39]]),
        Some(1) if image.len() >= 0x2E => u16::from_le_bytes([image[0x2C], image[0x2D]]),
        _ => return Err(io::Error::new(io::ErrorKind::InvalidData, "not an ELF .mdt")),
    };
    for i in 0..phnum {
        // Segments without file data (the ELF header's own) have no file.
        match fs::read(dir.join(format!("{name}.b{i:02}"))) {
            Ok(seg) => image.extend_from_slice(&seg),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok((image, mdt_size))
}

/// A bound client of one app.
pub struct App {
    file: File,
    app_id: u32,
}

impl App {
    /// Binds to app `name`, starting it from the image in `dir` if it does not run yet.
    pub fn load(dir: &Path, name: &str) -> io::Result<App> {
        let (image, mdt_size) = read_image(dir, name)?;
        let file = OpenOptions::new().read(true).write(true).open(QSEECOM_DEV)?;
        let mut arg = AppLoad {
            name: [0; APP_NAME_SIZE],
            image: image.as_ptr() as u64,
            image_size: image.len() as u32,
            mdt_size: mdt_size as u32,
            app_id: 0,
            flags: 0,
        };
        arg.name[..name.len()].copy_from_slice(name.as_bytes());
        ioctl(&file, IOC_APP_LOAD, &mut arg)?;
        Ok(App { file, app_id: arg.app_id })
    }

    /// The secure world's id of the app.
    pub fn id(&self) -> u32 {
        self.app_id
    }

    /// Calls the app. `rsp` goes in as it is (some commands read preset fields) and comes back
    /// filled by the app.
    pub fn send(&self, req: &[u8], rsp: &mut [u8]) -> io::Result<()> {
        let mut arg = AppSend {
            req: req.as_ptr() as u64,
            rsp: rsp.as_mut_ptr() as u64,
            req_size: req.len() as u32,
            rsp_size: rsp.len() as u32,
        };
        ioctl(&self.file, IOC_APP_SEND, &mut arg)
    }
}
