//! android.hardware.usb (IUsb) and android.hardware.usb.gadget (IUsbGadget) HAL for
//! android-pc devices with a USB device port: Type-C port status from the typec class,
//! USB functions on the configfs gadget.

mod configfs;
mod functions;
mod gadget;
mod port;
mod props;
mod typec;

use std::process::ExitCode;
use std::thread;

use android_hardware_usb::aidl::android::hardware::usb::IUsb::{BnUsb, IUsb};
use android_hardware_usb_gadget::aidl::android::hardware::usb::gadget::IUsbGadget::{
    BnUsbGadget, IUsbGadget,
};
use binder::BinderFeatures;
use log::{error, warn};
use pchal::uevent::UeventSocket;
use pchal::Service;

use crate::configfs::Gadget;
use crate::gadget::UsbGadget;
use crate::port::Usb;

const LOG_TAG: &str = "android.hardware.usb-service.pc";
const GADGET_DIR: &str = "/config/usb_gadget/g1";
const FFS_DIR: &str = "/dev/usb-ffs";
const TYPEC_DIR: &str = "/sys/class/typec";

fn main() -> ExitCode {
    pchal::init_logging(LOG_TAG);

    let gadget = UsbGadget::new(Gadget::new(GADGET_DIR, FFS_DIR));
    gadget.spawn_monitor();
    let gadget_binder = BnUsbGadget::new_binder(gadget, BinderFeatures::default());
    let gadget_name = format!("{}/default", UsbGadget::get_descriptor());
    if let Err(status) = binder::add_service(&gadget_name, gadget_binder.as_binder()) {
        error!("cannot register {gadget_name}: {status:?}");
        return ExitCode::FAILURE;
    }

    let usb = Usb::new(TYPEC_DIR);
    spawn_uevent_listener(usb.clone());
    let binder = BnUsb::new_binder(usb, BinderFeatures::default());
    // A gadget request waits up to 2 s for adbd; IUsb calls must not queue behind it.
    let service = Service::new(Usb::get_descriptor(), "default").extra_threads(2);
    let Err(err) = service.serve(binder.as_binder());
    error!("{err}");
    ExitCode::FAILURE
}

/// Pushes the ports' status to the framework on every typec uevent (a cable plugged in,
/// a role swap, a partner's PD negotiation).
fn spawn_uevent_listener(usb: Usb) {
    let socket = match UeventSocket::open() {
        Ok(socket) => socket,
        Err(err) => {
            warn!("no uevents, port status only on request: {err}");
            return;
        }
    };
    let spawned = thread::Builder::new().name("uevent".into()).spawn(move || loop {
        match socket.recv() {
            Ok(event) if event.subsystem() == Some("typec") => usb.notify(),
            Ok(_) => {}
            Err(err) => {
                error!("{err}");
                return;
            }
        }
    });
    if let Err(err) = spawned {
        error!("cannot start the uevent thread: {err}");
    }
}
