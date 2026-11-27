//! Per-user enrollment storage, laid out like the stock HAL's (`BAuthInfoStorage`):
//! `<root>/fp/User_<user>/<user>_<slot>tmpl.dat` holds the TA's encrypted template of slot
//! 1..=[`MAX_SLOTS`]; next to them `sid` (the gatekeeper secure user id the finger was
//! enrolled under) and `authid` (the authenticator id), both little-endian u64.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Finger slots the TA supports per user (`bio_idx` 1..4).
pub const MAX_SLOTS: u32 = 4;

/// One user's enrollments.
pub struct Store {
    dir: PathBuf,
    user: i32,
}

impl Store {
    /// The store of `user` under `root` (`/data/vendor/biometrics`).
    pub fn new(root: &Path, user: i32) -> Store {
        Store { dir: root.join("fp").join(format!("User_{user}")), user }
    }

    /// The user name the TA binds templates to.
    pub fn ta_user(&self) -> String {
        self.user.to_string()
    }

    fn template_path(&self, slot: u32) -> PathBuf {
        self.dir.join(format!("{}_{slot}tmpl.dat", self.user))
    }

    /// Enrolled slots, ascending.
    pub fn slots(&self) -> Vec<u32> {
        (1..=MAX_SLOTS).filter(|&s| self.template_path(s).is_file()).collect()
    }

    /// The first slot without a template.
    pub fn free_slot(&self) -> Option<u32> {
        (1..=MAX_SLOTS).find(|&s| !self.template_path(s).exists())
    }

    /// All of the user's templates, concatenated in slot order as `Identify_Init` takes them.
    pub fn templates(&self) -> io::Result<Vec<u8>> {
        let mut all = Vec::new();
        for slot in self.slots() {
            all.extend(fs::read(self.template_path(slot))?);
        }
        Ok(all)
    }

    /// Writes slot `slot`'s template.
    pub fn save_template(&self, slot: u32, template: &[u8]) -> io::Result<()> {
        self.write_atomic(&self.template_path(slot), template)
    }

    /// Deletes slot `slot`'s template; a missing one is not an error.
    pub fn remove(&self, slot: u32) -> io::Result<()> {
        match fs::remove_file(self.template_path(slot)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    fn read_u64(&self, name: &str) -> u64 {
        fs::read(self.dir.join(name))
            .ok()
            .and_then(|b| b.try_into().ok())
            .map_or(0, u64::from_le_bytes)
    }

    fn write_u64(&self, name: &str, v: u64) -> io::Result<()> {
        self.write_atomic(&self.dir.join(name), &v.to_le_bytes())
    }

    /// The gatekeeper secure user id the fingers were enrolled under (0: none).
    pub fn sid(&self) -> u64 {
        self.read_u64("sid")
    }

    /// Stores the gatekeeper secure user id.
    pub fn set_sid(&self, sid: u64) -> io::Result<()> {
        self.write_u64("sid", sid)
    }

    /// The authenticator id (0: none).
    pub fn authenticator_id(&self) -> u64 {
        self.read_u64("authid")
    }

    /// Stores the authenticator id.
    pub fn set_authenticator_id(&self, id: u64) -> io::Result<()> {
        self.write_u64("authid", id)
    }

    fn write_atomic(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        let tmp = path.with_extension("tmp");
        fs::write(&tmp, data)?;
        fs::rename(&tmp, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_and_ids() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path(), 10);
        assert_eq!(store.free_slot(), Some(1));
        assert_eq!(store.authenticator_id(), 0);
        store.save_template(1, b"one").unwrap();
        store.save_template(3, b"three").unwrap();
        assert_eq!(store.slots(), vec![1, 3]);
        assert_eq!(store.free_slot(), Some(2));
        assert_eq!(store.templates().unwrap(), b"onethree");
        assert!(root.path().join("fp/User_10/10_3tmpl.dat").is_file());
        store.set_authenticator_id(0x1122).unwrap();
        assert_eq!(store.authenticator_id(), 0x1122);
        store.remove(1).unwrap();
        store.remove(1).unwrap();
        assert_eq!(store.slots(), vec![3]);
    }
}
