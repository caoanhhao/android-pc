//! android.hardware.bluetooth HAL for PCs: the kernel's first Bluetooth controller
//! (`hci0`, a USB dongle or the Wi-Fi card's Bluetooth part) through an HCI user channel.
//!
//! `--h4 <tty>` drives an H4 UART instead, which is how it runs against Cuttlefish's
//! virtual controller (/dev/hvc5) for VTS.

mod hci;
mod mgmt;
mod packet;
mod transport;

use std::path::PathBuf;
use std::process::ExitCode;

use android_hardware_bluetooth::aidl::android::hardware::bluetooth::IBluetoothHci::{
    BnBluetoothHci, IBluetoothHci,
};
use binder::BinderFeatures;
use log::{error, info};
use pchal::Service;

use crate::hci::BluetoothHci;
use crate::transport::Transport;

const LOG_TAG: &str = "android.hardware.bluetooth-service.pc";
const USAGE: &str = "usage: android.hardware.bluetooth-service.pc [--h4 <tty>]";

fn parse_args(args: &[String]) -> Option<Transport> {
    match args {
        [] => Some(Transport::UserChannel { controller: 0 }),
        [flag, path] if flag == "--h4" => Some(Transport::Uart { path: PathBuf::from(path) }),
        _ => None,
    }
}

fn main() -> ExitCode {
    pchal::init_logging(LOG_TAG);

    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(transport) = parse_args(&args) else {
        error!("{USAGE}, got {args:?}");
        return ExitCode::FAILURE;
    };
    info!("controller: {transport:?}");

    let binder =
        BnBluetoothHci::new_binder(BluetoothHci::new(transport), BinderFeatures::default());
    // A second thread delivers client-death notices while a call is in progress.
    let service = Service::new(BluetoothHci::get_descriptor(), "default").extra_threads(1);
    let Err(err) = service.serve(binder.as_binder());
    error!("{err}");
    ExitCode::FAILURE
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn parses_transports() {
        assert_eq!(parse_args(&[]), Some(Transport::UserChannel { controller: 0 }));
        assert_eq!(
            parse_args(&args(&["--h4", "/dev/hvc5"])),
            Some(Transport::Uart { path: "/dev/hvc5".into() })
        );
        assert_eq!(parse_args(&args(&["--h4"])), None);
        assert_eq!(parse_args(&args(&["/dev/hvc5"])), None);
    }
}
