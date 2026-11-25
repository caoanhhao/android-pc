//! Which configfs functions, product ID and device class a set of `GadgetFunction` bits
//! needs. Pure logic, so it is tested on the host.

/// The `GadgetFunction` bits (android.hardware.usb.gadget), repeated here so this module
/// builds without the AIDL crate.
pub const ADB: i64 = 1;
#[allow(dead_code)] // only the tests name it
pub const ACCESSORY: i64 = 1 << 1;
pub const MTP: i64 = 1 << 2;
pub const MIDI: i64 = 1 << 3;
pub const PTP: i64 = 1 << 4;
pub const RNDIS: i64 = 1 << 5;
#[allow(dead_code)] // only the tests name it
pub const AUDIO_SOURCE: i64 = 1 << 6;
#[allow(dead_code)] // only the tests name it
pub const UVC: i64 = 1 << 7;
pub const NCM: i64 = 1 << 10;

/// Every bit this HAL can configure. ACCESSORY and AUDIO_SOURCE need the Android common
/// kernel's f_accessory/f_audio_source, which mainline does not have; UVC needs a
/// userspace streaming daemon, which this product does not have.
const SUPPORTED: i64 = ADB | MTP | MIDI | PTP | RNDIS | NCM;

/// A configfs function instance (`functions/<name>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Function {
    /// The directory name under `functions/`, `<type>.<instance>`.
    pub name: &'static str,
    /// For a FunctionFS function, its instance name: the mount under `/dev/usb-ffs/` that
    /// must have its endpoints (descriptors written by its daemon) before the gadget binds.
    pub ffs: Option<&'static str>,
    /// The name used in the configuration string (`mtp_adb`, ...); `None` for helpers that
    /// are not a framework function.
    pub label: Option<&'static str>,
}

/// RNDIS first: Windows only binds RNDIS when it is the first interface.
pub const F_RNDIS: Function = Function { name: "rndis.gs4", ffs: None, label: Some("rndis") };
pub const F_MTP: Function = Function { name: "ffs.mtp", ffs: Some("mtp"), label: Some("mtp") };
pub const F_PTP: Function = Function { name: "ffs.ptp", ffs: Some("ptp"), label: Some("ptp") };
pub const F_MIDI: Function = Function { name: "midi.gs5", ffs: None, label: Some("midi") };
pub const F_NCM: Function = Function { name: "ncm.gs9", ffs: None, label: Some("ncm") };
pub const F_ADB: Function = Function { name: "ffs.adb", ffs: Some("adb"), label: Some("adb") };
/// The ACM serial console (a debug root shell on this product), kept in every
/// configuration when enabled.
pub const F_ACM: Function = Function { name: "acm.gs8", ffs: None, label: None };

/// Function instances this HAL creates on demand and removes when unused, so an idle
/// network function leaves no `usb0` behind for tethering to pick up.
pub const ON_DEMAND: [Function; 3] = [F_RNDIS, F_NCM, F_MIDI];

/// What the gadget looks like for one set of functions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// The functions to link into the configuration, in interface order.
    pub functions: Vec<Function>,
    /// `idProduct`, from Google's 18d1 table so hosts (libmtp, Windows) know the device.
    pub id_product: u16,
    /// The configuration string, e.g. `mtp_adb`.
    pub config_name: String,
    /// Whether a function needs an interface association descriptor (RNDIS, NCM, ACM,
    /// MIDI is fine either way): the device then reports class EF/02/01.
    pub iad: bool,
}

impl Plan {
    /// FunctionFS instances whose daemons must be ready before binding.
    pub fn ffs(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.functions.iter().filter_map(|f| f.ffs)
    }

    /// Whether nothing is linked, so the gadget stays unbound.
    pub fn is_empty(&self) -> bool {
        self.functions.is_empty()
    }
}

/// Why a set of functions cannot be configured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unsupported(pub i64);

/// The plan for `functions` (GadgetFunction bits), plus the ACM console when `acm`.
pub fn plan(functions: i64, acm: bool) -> Result<Plan, Unsupported> {
    let unsupported = functions & !SUPPORTED;
    if unsupported != 0 {
        return Err(Unsupported(unsupported));
    }
    // MTP and PTP share the FunctionFS port the framework's MTP server drives.
    if functions & MTP != 0 && functions & PTP != 0 {
        return Err(Unsupported(PTP));
    }

    let mut list = Vec::new();
    for (bit, function) in
        [(RNDIS, F_RNDIS), (MTP, F_MTP), (PTP, F_PTP), (MIDI, F_MIDI), (NCM, F_NCM), (ADB, F_ADB)]
    {
        if functions & bit != 0 {
            list.push(function);
        }
    }
    let config_name = {
        let labels: Vec<_> = list.iter().filter_map(|f| f.label).collect();
        if labels.is_empty() { "none".to_owned() } else { labels.join("_") }
    };
    if acm {
        list.push(F_ACM);
    }
    let iad = list.iter().any(|f| matches!(f.name, "rndis.gs4" | "ncm.gs9" | "acm.gs8"));
    Ok(Plan { functions: list, id_product: id_product(functions), config_name, iad })
}

/// Google's product IDs for the standard Android function sets.
fn id_product(functions: i64) -> u16 {
    let adb = functions & ADB != 0;
    match (functions & !ADB, adb) {
        (MTP, false) => 0x4ee1,
        (MTP, true) => 0x4ee2,
        (RNDIS, false) => 0x4ee3,
        (RNDIS, true) => 0x4ee4,
        (PTP, false) => 0x4ee5,
        (PTP, true) => 0x4ee6,
        (MIDI, false) => 0x4ee8,
        (MIDI, true) => 0x4ee9,
        (NCM, false) => 0x4eeb,
        (NCM, true) => 0x4eec,
        _ => 0x4ee7,
    }
}

/// The GadgetFunction bits of the functions linked into a configuration, from the names
/// of their `functions/` directories (the targets of the configuration's links).
pub fn from_linked<'a>(names: impl IntoIterator<Item = &'a str>) -> i64 {
    names
        .into_iter()
        .map(|name| match name {
            "ffs.adb" => ADB,
            "ffs.mtp" => MTP,
            "ffs.ptp" => PTP,
            "midi.gs5" => MIDI,
            "rndis.gs4" => RNDIS,
            "ncm.gs9" => NCM,
            _ => 0,
        })
        .fold(0, |acc, bit| acc | bit)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(plan: &Plan) -> Vec<&'static str> {
        plan.functions.iter().map(|f| f.name).collect()
    }

    #[test]
    fn adb_alone() {
        let plan = plan(ADB, false).unwrap();
        assert_eq!(names(&plan), ["ffs.adb"]);
        assert_eq!(plan.id_product, 0x4ee7);
        assert_eq!(plan.config_name, "adb");
        assert!(!plan.iad);
        assert_eq!(plan.ffs().collect::<Vec<_>>(), ["adb"]);
    }

    #[test]
    fn mtp_adb_with_console() {
        let plan = plan(MTP | ADB, true).unwrap();
        assert_eq!(names(&plan), ["ffs.mtp", "ffs.adb", "acm.gs8"]);
        assert_eq!(plan.id_product, 0x4ee2);
        assert_eq!(plan.config_name, "mtp_adb");
        assert!(plan.iad);
        assert_eq!(plan.ffs().collect::<Vec<_>>(), ["mtp", "adb"]);
    }

    #[test]
    fn rndis_comes_first() {
        let plan = plan(ADB | RNDIS, false).unwrap();
        assert_eq!(names(&plan), ["rndis.gs4", "ffs.adb"]);
        assert_eq!(plan.id_product, 0x4ee4);
        assert!(plan.iad);
    }

    #[test]
    fn product_ids() {
        assert_eq!(plan(MTP, false).unwrap().id_product, 0x4ee1);
        assert_eq!(plan(PTP | ADB, false).unwrap().id_product, 0x4ee6);
        assert_eq!(plan(MIDI, false).unwrap().id_product, 0x4ee8);
        assert_eq!(plan(NCM | ADB, false).unwrap().id_product, 0x4eec);
        assert_eq!(plan(RNDIS | MTP, false).unwrap().id_product, 0x4ee7);
    }

    #[test]
    fn charging_only() {
        let plan = plan(0, false).unwrap();
        assert!(plan.is_empty());
        assert_eq!(plan.config_name, "none");
        let console = super::plan(0, true).unwrap();
        assert_eq!(names(&console), ["acm.gs8"]);
    }

    #[test]
    fn unsupported_functions() {
        assert_eq!(plan(ACCESSORY | ADB, false), Err(Unsupported(ACCESSORY)));
        assert_eq!(plan(AUDIO_SOURCE, false), Err(Unsupported(AUDIO_SOURCE)));
        assert_eq!(plan(UVC, false), Err(Unsupported(UVC)));
        assert_eq!(plan(MTP | PTP, false), Err(Unsupported(PTP)));
    }

    #[test]
    fn linked_functions() {
        assert_eq!(from_linked(["ffs.mtp", "ffs.adb", "acm.gs8"]), MTP | ADB);
        assert_eq!(from_linked(["acm.gs8", "ncm.gs9"]), NCM);
        assert_eq!(from_linked([]), 0);
    }
}
