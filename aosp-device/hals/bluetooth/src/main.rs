//! android.hardware.bluetooth HAL for PCs: the kernel's first Bluetooth controller
//! (`hci0`, a USB dongle or the Wi-Fi card's Bluetooth part) through an HCI user channel.
//!
//! `--h4 <tty>` drives an H4 UART instead, which is how it runs against Cuttlefish's
//! virtual controller (/dev/hvc5) for VTS.
//!
//! `--bdaddr-file <file>` and `--bdaddr-seed <serial>` give a controller the kernel holds
//! unconfigured (no valid address of its own, like the Galaxy Tab S7's QCA6390) its public
//! address: the one in `file`, else one derived from the serial number.

mod address;
mod hci;
mod mgmt;
mod packet;
mod transport;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use android_hardware_bluetooth::aidl::android::hardware::bluetooth::IBluetoothHci::{
    BnBluetoothHci, IBluetoothHci,
};
use binder::BinderFeatures;
use log::{error, info, warn};
use pchal::Service;

use crate::hci::BluetoothHci;
use crate::transport::Transport;

const LOG_TAG: &str = "android.hardware.bluetooth-service.pc";
const USAGE: &str = "usage: android.hardware.bluetooth-service.pc [--h4 <tty>] \
                     [--bdaddr-file <file>] [--bdaddr-seed <serial>]";
/// How long the address setup at startup waits for the controller (its driver loads
/// with the other kernel modules, before the HALs start).
const CONTROLLER_TIMEOUT: Duration = Duration::from_secs(10);

/// The command line.
#[derive(Debug, Default, PartialEq, Eq)]
struct Args {
    h4: Option<PathBuf>,
    bdaddr_file: Option<PathBuf>,
    bdaddr_seed: Option<String>,
}

impl Args {
    fn transport(&self) -> Transport {
        match &self.h4 {
            Some(path) => Transport::Uart { path: path.clone() },
            None => Transport::UserChannel { controller: 0 },
        }
    }
}

fn parse_args(args: &[String]) -> Option<Args> {
    let mut parsed = Args::default();
    let mut args = args.iter();
    while let Some(flag) = args.next() {
        let value = args.next()?;
        let slot = match flag.as_str() {
            "--h4" => &mut parsed.h4,
            "--bdaddr-file" => &mut parsed.bdaddr_file,
            "--bdaddr-seed" => {
                parsed.bdaddr_seed = Some(value.clone());
                continue;
            }
            _ => return None,
        };
        *slot = Some(PathBuf::from(value));
    }
    Some(parsed)
}

fn main() -> ExitCode {
    pchal::init_logging(LOG_TAG);

    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(args) = parse_args(&args) else {
        error!("{USAGE}, got {args:?}");
        return ExitCode::FAILURE;
    };
    let transport = args.transport();
    info!("controller: {transport:?}");
    if let Transport::UserChannel { controller } = transport {
        let address =
            address::resolve(args.bdaddr_file.as_deref(), args.bdaddr_seed.as_deref());
        if let Some(address) = address {
            // Without it the controller still works, with the chip's default address.
            if let Err(err) =
                crate::transport::set_public_address(controller, address, CONTROLLER_TIMEOUT)
            {
                warn!("cannot set the public address: {err}");
            }
        }
    }

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
        let transport = |list: &[&str]| parse_args(&args(list)).map(|args| args.transport());
        assert_eq!(transport(&[]), Some(Transport::UserChannel { controller: 0 }));
        assert_eq!(
            transport(&["--h4", "/dev/hvc5"]),
            Some(Transport::Uart { path: "/dev/hvc5".into() })
        );
        assert_eq!(transport(&["--h4"]), None);
        assert_eq!(transport(&["/dev/hvc5"]), None);
    }

    #[test]
    fn parses_the_address_source() {
        assert_eq!(
            parse_args(&args(&["--bdaddr-file", "/efs/bt_addr", "--bdaddr-seed", "R52R"])),
            Some(Args {
                h4: None,
                bdaddr_file: Some("/efs/bt_addr".into()),
                bdaddr_seed: Some("R52R".into()),
            })
        );
        assert_eq!(parse_args(&args(&["--bdaddr-file"])), None);
    }
}
