//! IUsb: the Type-C ports' roles and status from the typec class, pushed to the framework
//! (UsbPortManager) on every typec uevent.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use android_hardware_usb::aidl::android::hardware::usb::{
    ConnectorType::ConnectorType, ContaminantDetectionStatus::ContaminantDetectionStatus,
    ContaminantProtectionStatus::ContaminantProtectionStatus, IUsb::IUsb,
    IUsbCallback::IUsbCallback, PlugOrientation::PlugOrientation, PortDataRole::PortDataRole,
    PortMode::PortMode, PortPowerRole::PortPowerRole, PortRole::PortRole,
    PortStatus::PortStatus, PowerBrickStatus::PowerBrickStatus,
    StaticPortInformation::StaticPortInformation, Status::Status, UsbDataStatus::UsbDataStatus,
};
use binder::{Interface, Strong};
use log::{info, warn};

use crate::typec::{self, DataRole, PowerRole};

/// The IUsb service. Cheap to clone; clones share the callback.
#[derive(Clone)]
pub struct Usb {
    class: PathBuf,
    callback: Arc<Mutex<Option<Strong<dyn IUsbCallback>>>>,
}

impl Usb {
    /// Serves the ports under `class` (`/sys/class/typec`).
    pub fn new(class: impl Into<PathBuf>) -> Self {
        Self { class: class.into(), callback: Arc::default() }
    }

    fn callback(&self) -> Option<Strong<dyn IUsbCallback>> {
        self.callback.lock().unwrap().clone()
    }

    fn statuses(&self) -> Vec<PortStatus> {
        typec::ports(&self.class).iter().map(port_status).collect()
    }

    fn has_port(&self, name: &str) -> bool {
        typec::ports(&self.class).iter().any(|p| p.name == name)
    }

    /// Sends the current status of every port to the framework (a typec uevent arrived).
    pub fn notify(&self) {
        if let Some(callback) = self.callback() {
            let statuses = self.statuses();
            if let Err(err) = callback.notifyPortStatusChange(&statuses, Status::SUCCESS) {
                warn!("notifyPortStatusChange: {err}");
            }
        }
    }
}

/// The framework's view of one port.
fn port_status(port: &typec::Port) -> PortStatus {
    let data = match port.data_role {
        Some(DataRole::Host) => PortDataRole::HOST,
        Some(DataRole::Device) => PortDataRole::DEVICE,
        None => PortDataRole::NONE,
    };
    let power = match port.power_role {
        Some(PowerRole::Source) => PortPowerRole::SOURCE,
        Some(PowerRole::Sink) => PortPowerRole::SINK,
        None => PortPowerRole::NONE,
    };
    let mode = match (port.partner, port.accessory.as_deref(), port.data_role) {
        (false, _, _) => PortMode::NONE,
        (true, Some("audio"), _) => PortMode::AUDIO_ACCESSORY,
        (true, Some("debug"), _) => PortMode::DEBUG_ACCESSORY,
        (true, _, Some(DataRole::Host)) => PortMode::DFP,
        (true, _, _) => PortMode::UFP,
    };
    let supported_modes = if port.dual_data || port.dual_power {
        vec![PortMode::DRP]
    } else {
        vec![PortMode::UFP]
    };
    let orientation = match (port.partner, port.orientation.as_deref()) {
        (false, _) => PlugOrientation::UNPLUGGED,
        (true, Some("normal")) => PlugOrientation::PLUGGED_NORMAL,
        (true, Some("reverse")) => PlugOrientation::PLUGGED_FLIPPED,
        (true, _) => PlugOrientation::PLUGGED_UNKNOWN,
    };
    // Role swaps need PD with the partner and a driver that accepts them.
    let swappable = port.partner && port.partner_pd;
    PortStatus {
        portName: port.name.clone(),
        currentDataRole: data,
        currentPowerRole: power,
        currentMode: mode,
        canChangeMode: false,
        canChangeDataRole: swappable && port.dual_data && port.data_role_writable,
        canChangePowerRole: swappable && port.dual_power && port.power_role_writable,
        supportedModes: supported_modes,
        supportedContaminantProtectionModes: vec![],
        supportsEnableContaminantPresenceProtection: false,
        contaminantProtectionStatus: ContaminantProtectionStatus::NONE,
        supportsEnableContaminantPresenceDetection: false,
        contaminantDetectionStatus: ContaminantDetectionStatus::NOT_SUPPORTED,
        usbDataStatus: vec![UsbDataStatus::ENABLED],
        powerTransferLimited: false,
        powerBrickStatus: PowerBrickStatus::UNKNOWN,
        plugOrientation: orientation,
        ..Default::default()
    }
}

impl Interface for Usb {
    fn dump(
        &self,
        writer: &mut dyn std::io::Write,
        _args: &[&std::ffi::CStr],
    ) -> Result<(), binder::StatusCode> {
        for port in typec::ports(&self.class) {
            writeln!(writer, "{port:?}").map_err(|_| binder::StatusCode::UNKNOWN_ERROR)?;
        }
        Ok(())
    }
}

#[allow(non_snake_case)]
impl IUsb for Usb {
    fn enableContaminantPresenceDetection(
        &self,
        portName: &str,
        enable: bool,
        transactionId: i64,
    ) -> binder::Result<()> {
        if let Some(callback) = self.callback() {
            callback.notifyContaminantEnabledStatus(
                portName,
                false,
                if enable { Status::NOT_SUPPORTED } else { Status::SUCCESS },
                transactionId,
            )?;
        }
        Ok(())
    }

    fn enableUsbData(&self, portName: &str, enable: bool, transactionId: i64) -> binder::Result<()> {
        // USB data is always on; turning it off (USB data signalling restriction) needs a
        // way to cut the MAX77705's data lines that the driver does not offer yet.
        let status = if !self.has_port(portName) {
            Status::INVALID_ARGUMENT
        } else if enable {
            Status::SUCCESS
        } else {
            Status::NOT_SUPPORTED
        };
        if let Some(callback) = self.callback() {
            callback.notifyEnableUsbDataStatus(portName, enable, status, transactionId)?;
        }
        Ok(())
    }

    fn enableUsbDataWhileDocked(&self, portName: &str, transactionId: i64) -> binder::Result<()> {
        if let Some(callback) = self.callback() {
            callback.notifyEnableUsbDataWhileDockedStatus(portName, Status::NOT_SUPPORTED, transactionId)?;
        }
        Ok(())
    }

    fn queryPortStatus(&self, transactionId: i64) -> binder::Result<()> {
        if let Some(callback) = self.callback() {
            let statuses = self.statuses();
            callback.notifyPortStatusChange(&statuses, Status::SUCCESS)?;
            for status in &statuses {
                callback.notifyQueryPortStatus(&status.portName, Status::SUCCESS, transactionId)?;
            }
        }
        Ok(())
    }

    fn setCallback(&self, callback: &Strong<dyn IUsbCallback>) -> binder::Result<()> {
        info!("callback set");
        *self.callback.lock().unwrap() = Some(callback.clone());
        self.notify();
        Ok(())
    }

    fn switchRole(&self, portName: &str, role: &PortRole, transactionId: i64) -> binder::Result<()> {
        let port = typec::ports(&self.class).into_iter().find(|p| p.name == portName);
        let status = match (&port, role) {
            (None, _) => Status::INVALID_ARGUMENT,
            (Some(port), PortRole::DataRole(data)) if port.data_role_writable => {
                let value = if *data == PortDataRole::HOST { "host" } else { "device" };
                write_role(&port.sysfs.join("data_role"), value)
            }
            (Some(port), PortRole::PowerRole(power)) if port.power_role_writable => {
                let value = if *power == PortPowerRole::SOURCE { "source" } else { "sink" };
                write_role(&port.sysfs.join("power_role"), value)
            }
            _ => Status::NOT_SUPPORTED,
        };
        if let Some(callback) = self.callback() {
            callback.notifyRoleSwitchStatus(portName, role, status, transactionId)?;
        }
        Ok(())
    }

    fn limitPowerTransfer(&self, portName: &str, limit: bool, transactionId: i64) -> binder::Result<()> {
        if let Some(callback) = self.callback() {
            let status = if limit { Status::NOT_SUPPORTED } else { Status::SUCCESS };
            callback.notifyLimitPowerTransferStatus(portName, false, status, transactionId)?;
        }
        Ok(())
    }

    fn resetUsbPort(&self, portName: &str, transactionId: i64) -> binder::Result<()> {
        if let Some(callback) = self.callback() {
            callback.notifyResetUsbPortStatus(portName, Status::NOT_SUPPORTED, transactionId)?;
        }
        Ok(())
    }

    fn queryStaticPortInformation(&self, portName: &str, transactionId: i64) -> binder::Result<()> {
        let port = typec::ports(&self.class).into_iter().find(|p| p.name == portName);
        let (info, status) = match port {
            Some(port) => {
                let mut power = vec![PortPowerRole::SINK];
                if port.dual_power {
                    power.push(PortPowerRole::SOURCE);
                }
                let mut data = vec![PortDataRole::DEVICE];
                if port.dual_data {
                    data.push(PortDataRole::HOST);
                }
                let info = StaticPortInformation {
                    portName: port.name.clone(),
                    sysfsPath: port.sysfs.to_string_lossy().into_owned(),
                    connectorType: ConnectorType::C,
                    powerRolesSupported: power,
                    dataRolesSupported: data,
                    ..Default::default()
                };
                (info, Status::SUCCESS)
            }
            None => (StaticPortInformation::default(), Status::INVALID_ARGUMENT),
        };
        if let Some(callback) = self.callback() {
            callback.notifyQueryStaticPortInformation(portName, &info, status, transactionId)?;
        }
        Ok(())
    }
}

fn write_role(path: &std::path::Path, value: &str) -> Status {
    match std::fs::write(path, value) {
        Ok(()) => Status::SUCCESS,
        Err(err) => {
            warn!("{}: {value}: {err}", path.display());
            Status::ERROR
        }
    }
}
