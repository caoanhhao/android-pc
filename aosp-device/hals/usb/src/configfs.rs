//! The configfs gadget (`/config/usb_gadget/g1`): linking functions into its one
//! configuration and binding it to the UDC.
//!
//! The gadget itself (IDs, strings, the FunctionFS instances and their mounts) is created
//! by the device's init script, as on every configfs Android device; this module only
//! rearranges it.

use std::fs;
use std::io;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use crate::functions::{Function, Plan, ON_DEMAND};

/// The gadget directory and the paths derived from it.
#[derive(Debug, Clone)]
pub struct Gadget {
    root: PathBuf,
    ffs_root: PathBuf,
}

impl Gadget {
    /// `root` is the gadget (`/config/usb_gadget/g1`), `ffs_root` the parent of the
    /// FunctionFS mounts (`/dev/usb-ffs`).
    pub fn new(root: impl Into<PathBuf>, ffs_root: impl Into<PathBuf>) -> Self {
        Self { root: root.into(), ffs_root: ffs_root.into() }
    }

    fn config(&self) -> PathBuf {
        self.root.join("configs/b.1")
    }

    /// The UDC the gadget is bound to, if any.
    pub fn udc(&self) -> Option<String> {
        let udc = fs::read_to_string(self.root.join("UDC")).ok()?;
        let udc = udc.trim();
        (!udc.is_empty()).then(|| udc.to_owned())
    }

    /// Binds the gadget to `udc`.
    pub fn bind(&self, udc: &str) -> io::Result<()> {
        write(&self.root.join("UDC"), udc)
    }

    /// Unbinds the gadget; the host sees a disconnect.
    pub fn unbind(&self) -> io::Result<()> {
        if self.udc().is_none() {
            return Ok(());
        }
        write(&self.root.join("UDC"), "")
    }

    /// The names of the functions linked into the configuration (the link targets'
    /// directory names), in no particular order.
    pub fn linked(&self) -> Vec<String> {
        links(&self.config())
            .into_iter()
            .filter_map(|link| fs::read_link(link).ok())
            .filter_map(|target| target.file_name().map(|n| n.to_string_lossy().into_owned()))
            .collect()
    }

    /// Whether the FunctionFS instance `name` has its endpoints, i.e. its daemon wrote the
    /// descriptors and the gadget may bind with it.
    pub fn ffs_ready(&self, name: &str) -> bool {
        self.ffs_root.join(name).join("ep1").exists()
    }

    /// The FunctionFS mounts that exist, for watching their endpoints come and go.
    pub fn ffs_dirs(&self) -> Vec<PathBuf> {
        ["adb", "mtp", "ptp"].iter().map(|n| self.ffs_root.join(n)).filter(|d| d.is_dir()).collect()
    }

    /// Replaces the configuration with `plan`. The gadget must be unbound.
    pub fn apply(&self, plan: &Plan) -> io::Result<()> {
        let config = self.config();
        for link in links(&config) {
            fs::remove_file(&link).map_err(|e| annotate(&link, e))?;
        }
        // Drop on-demand functions the plan does not use; NCM and RNDIS each keep a network
        // interface while they exist.
        for function in ON_DEMAND.iter().filter(|f| !plan.functions.contains(f)) {
            let dir = self.function_dir(function);
            if dir.exists() {
                if let Err(err) = fs::remove_dir(&dir) {
                    log::warn!("cannot remove {}: {err}", dir.display());
                }
            }
        }

        write(&self.root.join("idProduct"), &format!("0x{:04x}", plan.id_product))?;
        let (class, subclass, protocol) = if plan.iad { ("0xEF", "0x02", "0x01") } else { ("0", "0", "0") };
        write(&self.root.join("bDeviceClass"), class)?;
        write(&self.root.join("bDeviceSubClass"), subclass)?;
        write(&self.root.join("bDeviceProtocol"), protocol)?;
        let strings = config.join("strings/0x409");
        if strings.is_dir() {
            write(&strings.join("configuration"), &plan.config_name)?;
        }

        for (index, function) in plan.functions.iter().enumerate() {
            let dir = self.function_dir(function);
            if !dir.exists() {
                fs::create_dir(&dir).map_err(|e| annotate(&dir, e))?;
            }
            let link = config.join(format!("f{}", index + 1));
            symlink(&dir, &link).map_err(|e| annotate(&link, e))?;
        }
        Ok(())
    }

    fn function_dir(&self, function: &Function) -> PathBuf {
        self.root.join("functions").join(function.name)
    }

    /// The bus speed the host negotiated, from the UDC's `current_speed`.
    pub fn speed(&self, udc: &str, udc_class: &Path) -> Option<String> {
        let speed = fs::read_to_string(udc_class.join(udc).join("current_speed")).ok()?;
        Some(speed.trim().to_owned())
    }
}

/// The symbolic links in a configuration directory.
fn links(config: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(config) else { return Vec::new() };
    entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|t| t.is_symlink()))
        .map(|e| e.path())
        .collect()
}

fn write(path: &Path, value: &str) -> io::Result<()> {
    fs::write(path, value).map_err(|e| annotate(path, e))
}

fn annotate(path: &Path, err: io::Error) -> io::Error {
    io::Error::new(err.kind(), format!("{}: {err}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::functions::{plan, ADB, MTP, NCM};

    /// A fake gadget as init.gts7l.usb.rc leaves it: g1 with the bring-up links.
    fn fake() -> (tempfile::TempDir, Gadget) {
        let dir = tempfile::tempdir().unwrap();
        let g1 = dir.path().join("g1");
        for d in ["configs/b.1/strings/0x409", "functions/ffs.adb", "functions/ffs.mtp", "functions/acm.gs8", "functions/ncm.gs9"] {
            fs::create_dir_all(g1.join(d)).unwrap();
        }
        fs::write(g1.join("UDC"), "a600000.usb\n").unwrap();
        symlink(g1.join("functions/ffs.adb"), g1.join("configs/b.1/f1")).unwrap();
        symlink(g1.join("functions/acm.gs8"), g1.join("configs/b.1/f8")).unwrap();
        symlink(g1.join("functions/ncm.gs9"), g1.join("configs/b.1/f9")).unwrap();
        fs::create_dir_all(dir.path().join("ffs/adb")).unwrap();
        fs::write(dir.path().join("ffs/adb/ep1"), "").unwrap();
        fs::create_dir_all(dir.path().join("ffs/mtp")).unwrap();
        let gadget = Gadget::new(g1, dir.path().join("ffs"));
        (dir, gadget)
    }

    #[test]
    fn reads_the_bring_up_gadget() {
        let (_dir, gadget) = fake();
        assert_eq!(gadget.udc().as_deref(), Some("a600000.usb"));
        let mut linked = gadget.linked();
        linked.sort();
        assert_eq!(linked, ["acm.gs8", "ffs.adb", "ncm.gs9"]);
        assert!(gadget.ffs_ready("adb"));
        assert!(!gadget.ffs_ready("mtp"));
        assert_eq!(gadget.ffs_dirs().len(), 2);
    }

    #[test]
    fn applies_a_plan() {
        let (_dir, gadget) = fake();
        gadget.unbind().unwrap();
        assert_eq!(gadget.udc(), None);
        gadget.apply(&plan(MTP | ADB, true).unwrap()).unwrap();
        let mut linked = gadget.linked();
        linked.sort();
        assert_eq!(linked, ["acm.gs8", "ffs.adb", "ffs.mtp"]);
        // The idle NCM function is gone, with its usb0.
        assert!(!gadget.root.join("functions/ncm.gs9").exists());
        assert_eq!(fs::read_to_string(gadget.root.join("idProduct")).unwrap(), "0x4ee2");
        assert_eq!(fs::read_to_string(gadget.root.join("bDeviceClass")).unwrap(), "0xEF");
        let name = gadget.root.join("configs/b.1/strings/0x409/configuration");
        assert_eq!(fs::read_to_string(name).unwrap(), "mtp_adb");
        assert_eq!(
            fs::read_link(gadget.root.join("configs/b.1/f1")).unwrap(),
            gadget.root.join("functions/ffs.mtp")
        );
    }

    #[test]
    fn creates_on_demand_functions() {
        let (_dir, gadget) = fake();
        gadget.apply(&plan(MTP, false).unwrap()).unwrap();
        gadget.apply(&plan(NCM | ADB, false).unwrap()).unwrap();
        assert!(gadget.root.join("functions/ncm.gs9").is_dir());
        let mut linked = gadget.linked();
        linked.sort();
        assert_eq!(linked, ["ffs.adb", "ncm.gs9"]);
        assert_eq!(fs::read_to_string(gadget.root.join("bDeviceClass")).unwrap(), "0xEF");
    }
}
