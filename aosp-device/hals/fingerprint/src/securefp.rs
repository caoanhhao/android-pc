//! Commands of Samsung's `securefp` trusted app (Goodix GW36T1), as the stock HAL's
//! `libbauthtzcommon.so` frames them (docs/gts7l-fingerprint-tz.md §3-§6).
//!
//! Every command is a request and a response in one shared buffer: `u32 cmd` at `req+0`, and
//! in the response the **fpop** (what the HAL must do next) at `rsp+0` and the TA status at
//! `rsp+4`. Sizes are fixed per command; the TA refuses others with status 0x33.

use std::io;
use std::path::Path;

use crate::qsee::App;

/// The TA serving the GW36T1 (not `fingerpr`).
pub const TA_NAME: &str = "securefp";
/// Where the stock firmware keeps the TA images (modem partition `image/`).
pub const TA_DIR: &str = "/mnt/vendor/apnhlos/image";

const CMD_PREPARE: u32 = 0x01;
const CMD_ENROLL_INIT: u32 = 0x02;
const CMD_ENROLL_DO: u32 = 0x03;
const CMD_ENROLL_FINAL: u32 = 0x04;
const CMD_IDENTIFY_INIT: u32 = 0x05;
const CMD_IDENTIFY_DO: u32 = 0x06;
const CMD_IDENTIFY_FINAL: u32 = 0x07;
const CMD_CANCEL: u32 = 0x0A;
const CMD_CONTROL: u32 = 0x0C;
const CMD_TYPE_CHECK: u32 = 0x10;

/// Control op: the 100-byte template directory, part of the template key derivation.
pub const OP_SEND_STOREPATH: u32 = 0x30;

/// `Enroll_Init` type the stock HAL passes.
const ENROLL_TYPE: u32 = 7;
/// GW36T1's sensor id in the stock HAL's table; `Type_Check` answers its TA type, 6.
const SENSOR_ID_GW36T1: u32 = 18;
const TA_TYPE_GW36: u32 = 6;

/// TA status: the capture was rejected (too little finger on the sensor); `rsp+0xC` tells why.
pub const STATUS_BAD_IMAGE: u32 = 0x27;
/// Largest template chunk `Identify_Init` takes per call.
pub const TEMPLATE_CHUNK: usize = 0x3D400;
const HAT_SIZE: usize = 69;

/// The fpop codes the HAL acts on (§6).
pub mod fpop {
    /// Nothing more to do: the command finished.
    pub const DONE: u32 = 0;
    /// Wait for the finger interrupt (finger lifted), then call again.
    pub const WAIT_FINGER_UP: u32 = 3;
    /// Wait for the finger interrupt, then call again.
    pub const WAIT_INTERRUPT: u32 = 4;
    /// A finger arrived; call again.
    pub const FINGER_DOWN: u32 = 5;
    /// The capture goes on; call again.
    pub const FINGER_DOWN_2: u32 = 6;
    /// Reset the sensor, then call again.
    pub const RESET: u32 = 7;
    /// Reset the sensor, then call again.
    pub const RESET_2: u32 = 8;
}

#[derive(Debug, thiserror::Error)]
/// A failed TA call.
pub enum Error {
    #[error("qseecom: {0}")]
    /// `/dev/qseecom` failed.
    Io(#[from] io::Error),
    #[error("securefp command {cmd:#x}: status {status:#x}")]
    /// The TA answered command `cmd` with a nonzero status.
    Status {
        /// The command.
        cmd: u32,
        /// Its status.
        status: u32,
    },
    #[error("securefp: {0}")]
    /// The TA answered something the protocol does not allow.
    Protocol(&'static str),
}

/// Result of a TA call.
pub type Result<T> = std::result::Result<T, Error>;

/// One command's request and response buffers.
struct Buf {
    cmd: u32,
    req: Vec<u8>,
    rsp: Vec<u8>,
}

impl Buf {
    fn new(cmd: u32, req_len: usize, rsp_len: usize) -> Buf {
        let mut req = vec![0; req_len];
        req[..4].copy_from_slice(&cmd.to_le_bytes());
        Buf { cmd, req, rsp: vec![0; rsp_len] }
    }
    fn put(&mut self, off: usize, v: u32) -> &mut Self {
        self.req[off..off + 4].copy_from_slice(&v.to_le_bytes());
        self
    }
    fn put_bytes(&mut self, off: usize, b: &[u8]) -> &mut Self {
        self.req[off..off + b.len()].copy_from_slice(b);
        self
    }
    fn get(&self, off: usize) -> u32 {
        u32::from_le_bytes(self.rsp[off..off + 4].try_into().unwrap())
    }
}

/// What one capture step of `Enroll_Do`/`Identify_Do` answered.
#[derive(Debug, Clone, Copy)]
pub struct Step {
    /// What the HAL must do next (see [`fpop`]).
    pub fpop: u32,
    /// The TA status; 0 is success.
    pub status: u32,
    /// The raw words at `rsp+8..+0x1C`.
    pub words: [u32; 5],
}

impl Step {
    fn from(b: &Buf) -> Step {
        let mut words = [0; 5];
        for (i, w) in words.iter_mut().enumerate() {
            *w = b.get(8 + 4 * i);
        }
        Step { fpop: b.get(0), status: b.get(4), words }
    }
}

/// `Enroll_Do` progress (rsp: +0xC capture result, +0x10 percentage, +0x14 enrolled count,
/// +0x18 total count; from the TA's own copy into the response).
#[derive(Debug, Clone, Copy)]
pub struct EnrollProgress {
    /// Why a capture was rejected (with [`STATUS_BAD_IMAGE`]).
    pub result: u32,
    /// Progress, 0..=100.
    pub percentage: u32,
    /// Captures accepted so far.
    pub count: u32,
    /// Captures the TA counts in total so far.
    pub total: u32,
}

impl From<&Step> for EnrollProgress {
    fn from(s: &Step) -> Self {
        EnrollProgress { result: s.words[1], percentage: s.words[2], count: s.words[3], total: s.words[4] }
    }
}

/// `Identify_Do` result: rsp+0x10 matched score, +0x14 matched bio index (0xFF = none).
#[derive(Debug, Clone)]
pub struct Match {
    /// The matcher score.
    pub score: u32,
    /// The matched slot (1..4).
    pub bio_idx: u32,
    /// The TA's 69-byte auth token, when it made one.
    pub hat: Option<[u8; HAT_SIZE]>,
}

/// A started `securefp` app.
pub struct SecureFp {
    app: App,
}

fn put_str(b: &mut Buf, off: usize, max: usize, s: &str) {
    let n = s.len().min(max - 1);
    b.put_bytes(off, &s.as_bytes()[..n]);
}

impl SecureFp {
    /// Binds to `securefp`, starting it from its image when it does not run yet.
    pub fn start() -> Result<SecureFp> {
        let app = App::load(Path::new(TA_DIR), TA_NAME)?;
        Ok(SecureFp { app })
    }

    /// The secure world's id of the app.
    pub fn app_id(&self) -> u32 {
        self.app.id()
    }

    fn call(&self, b: &mut Buf) -> Result<()> {
        self.app.send(&b.req, &mut b.rsp)?;
        Ok(())
    }

    fn call_ok(&self, b: &mut Buf) -> Result<()> {
        self.call(b)?;
        match b.get(4) {
            0 => Ok(()),
            status => Err(Error::Status { cmd: b.cmd, status }),
        }
    }

    /// `Type_Check`: the only command with the status at `rsp+0`. Confirms the TA serves GW36.
    pub fn type_check(&self) -> Result<()> {
        let mut b = Buf::new(CMD_TYPE_CHECK, 0x40, 0x40);
        b.put(4, SENSOR_ID_GW36T1);
        self.call(&mut b)?;
        match (b.get(0), b.get(4)) {
            (0, TA_TYPE_GW36) => Ok(()),
            (0, _) => Err(Error::Protocol("Type_Check: not a GW36 sensor")),
            (status, _) => Err(Error::Status { cmd: CMD_TYPE_CHECK, status }),
        }
    }

    /// `Prepare` without metadata, before every enroll/identify (the first call of the stock
    /// `common_prepare`).
    pub fn prepare(&self) -> Result<u32> {
        let mut b = Buf::new(CMD_PREPARE, 0xA040, 0xA040);
        b.put(8, 1);
        self.call_ok(&mut b)?;
        Ok(b.get(0))
    }

    fn control_buf(op: u32, user: &str, input: &[u8]) -> Buf {
        let mut b = Buf::new(CMD_CONTROL, 0xFA140, 0xFA040);
        b.put(8, op);
        put_str(&mut b, 0xC, 0x100, user);
        b.put_bytes(0x10C, input).put(0xFA10C, input.len() as u32);
        b
    }

    /// Control op with string or blob input and no output we use.
    pub fn control(&self, op: u32, user: &str, input: &[u8]) -> Result<()> {
        self.call_ok(&mut Self::control_buf(op, user, input))
    }

    /// Control op 0x30: the template directory the TA mixes into its template keys.
    pub fn send_storepath(&self, user: &str, path: &str) -> Result<()> {
        let mut p = [0u8; 100];
        let n = path.len().min(99);
        p[..n].copy_from_slice(&path.as_bytes()[..n]);
        self.control(OP_SEND_STOREPATH, user, &p)
    }

    /// `Enroll_Init`: starts or continues (after `Prepare`) the enrollment of slot `bio_idx`.
    pub fn enroll_init(&self, user: &str, bio_idx: u32, storepath: &str) -> Result<()> {
        let mut b = Buf::new(CMD_ENROLL_INIT, 0x180, 0x40);
        b.put(8, 0);
        put_str(&mut b, 0xC, 0x100, user);
        b.put(0x10C, bio_idx).put(0x110, ENROLL_TYPE);
        put_str(&mut b, 0x114, 100, storepath);
        self.call_ok(&mut b)
    }

    /// One `Enroll_Do`; the caller acts on the fpop. A nonzero status comes back in the step.
    /// The step that reaches 100% carries the encrypted template the HAL stores (`rsp+0x1C`,
    /// length at `rsp+0x3D41C`); the others carry no data.
    pub fn enroll_do(&self) -> Result<(Step, Vec<u8>)> {
        let mut b = Buf::new(CMD_ENROLL_DO, 0x40, 0x47440);
        b.put(8, 1);
        self.call(&mut b)?;
        let len = (b.get(0x3D41C) as usize).min(TEMPLATE_CHUNK);
        Ok((Step::from(&b), b.rsp[0x1C..0x1C + len].to_vec()))
    }

    /// `Enroll_Final`: ends the enrollment (the TA returns no data here).
    pub fn enroll_final(&self) -> Result<()> {
        self.final_data(CMD_ENROLL_FINAL).map(drop)
    }

    fn final_data(&self, cmd: u32) -> Result<Vec<u8>> {
        let mut b = Buf::new(cmd, 0x40, 0xA040);
        self.call_ok(&mut b)?;
        let len = (b.get(0xA014) as usize).min(0xA000);
        Ok(b.rsp[0x14..0x14 + len].to_vec())
    }

    /// `Identify_Init` with the user's templates (concatenated, sent in chunks) and the
    /// challenge for the auth token the TA makes on a match.
    pub fn identify_init(&self, user: &str, templates: &[u8], challenge: u64) -> Result<()> {
        let chunks: Vec<&[u8]> =
            if templates.is_empty() { vec![&[]] } else { templates.chunks(TEMPLATE_CHUNK).collect() };
        for (i, chunk) in chunks.iter().enumerate() {
            let mut b = Buf::new(CMD_IDENTIFY_INIT, 0x3D9C0, 0x1C0);
            let index = if templates.is_empty() { u32::MAX } else { i as u32 };
            b.put(4, index).put(8, 0);
            put_str(&mut b, 0xC, 0x100, user);
            b.put_bytes(0x10C, chunk).put(0x3D50C, chunk.len() as u32);
            // HAT template: version 0, challenge, authenticator type fingerprint (big-endian).
            let mut hat = [0u8; HAT_SIZE];
            hat[1..9].copy_from_slice(&challenge.to_le_bytes());
            hat[25..29].copy_from_slice(&2u32.to_be_bytes());
            b.put_bytes(0x3D914, &hat);
            self.call_ok(&mut b)?;
        }
        Ok(())
    }

    /// One `Identify_Do`, and the auth token the TA made on a match.
    pub fn identify_do(&self) -> Result<(Step, Option<[u8; HAT_SIZE]>)> {
        let mut b = Buf::new(CMD_IDENTIFY_DO, 0x40, 0x474C0);
        b.put(8, 1);
        self.call(&mut b)?;
        let hat = (b.get(0x47485) == 1).then(|| b.rsp[0x47440..0x47440 + HAT_SIZE].try_into().unwrap());
        Ok((Step::from(&b), hat))
    }

    /// `Identify_Final`: an updated template for the matched finger, if the TA refreshed it.
    pub fn identify_final(&self) -> Result<Vec<u8>> {
        self.final_data(CMD_IDENTIFY_FINAL)
    }

    /// Ends a parked capture. Must come before the app is shut down while `Enroll_Do` or
    /// `Identify_Do` waits for the finger, or the next app start can hang the SoC.
    pub fn cancel(&self) -> Result<()> {
        let mut b = Buf::new(CMD_CANCEL, 0x40, 0x40);
        self.call_ok(&mut b)
    }
}
