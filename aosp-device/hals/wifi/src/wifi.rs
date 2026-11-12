//! IWifi: starts and stops the HAL and hands out its one chip.

use std::sync::{Arc, Mutex};

use android_hardware_wifi::aidl::android::hardware::wifi::{
    IWifi::IWifi,
    IWifiChip::{BnWifiChip, IWifiChip},
    IWifiEventCallback::IWifiEventCallback,
    WifiStatusCode::WifiStatusCode,
};
use binder::{BinderFeatures, Interface, Strong};
use log::{error, info};
use pchal::Callbacks;

use crate::chip::{add_callback, Chip, ChipState, CHIP_ID};
use crate::status::{error_with, invalid_args};

/// The chip of a started HAL.
struct Started {
    chip: Strong<dyn IWifiChip>,
    state: Arc<Mutex<ChipState>>,
}

/// The IWifi binder object.
#[derive(Default)]
pub struct Wifi {
    started: Mutex<Option<Started>>,
    callbacks: Callbacks<dyn IWifiEventCallback>,
}

impl Interface for Wifi {}

impl IWifi for Wifi {
    fn registerEventCallback(
        &self,
        callback: &Strong<dyn IWifiEventCallback>,
    ) -> binder::Result<()> {
        add_callback(&self.callbacks, callback)
    }

    fn isStarted(&self) -> binder::Result<bool> {
        Ok(self.started.lock().expect("wifi lock poisoned").is_some())
    }

    fn start(&self) -> binder::Result<()> {
        {
            let mut started = self.started.lock().expect("wifi lock poisoned");
            if started.is_some() {
                return Ok(());
            }
            match ChipState::probe() {
                Ok(state) => {
                    let state = Arc::new(Mutex::new(state));
                    let chip = BnWifiChip::new_binder(
                        Chip::new(Arc::clone(&state)),
                        BinderFeatures::default(),
                    );
                    *started = Some(Started { chip, state });
                }
                Err(err) => {
                    drop(started);
                    error!("start: {err}");
                    self.callbacks.notify(
                        |()| true,
                        |callback| callback.onFailure(WifiStatusCode::ERROR_UNKNOWN),
                    );
                    return Err(error_with(WifiStatusCode::ERROR_UNKNOWN, err));
                }
            }
        }
        info!("started");
        self.callbacks.notify(|()| true, |callback| callback.onStart());
        Ok(())
    }

    fn stop(&self) -> binder::Result<()> {
        let Some(started) = self.started.lock().expect("wifi lock poisoned").take() else {
            return Ok(());
        };
        started.state.lock().expect("chip lock poisoned").teardown();
        info!("stopped");
        self.callbacks.notify(|()| true, |callback| callback.onStop());
        Ok(())
    }

    fn getChipIds(&self) -> binder::Result<Vec<i32>> {
        let started = self.started.lock().expect("wifi lock poisoned");
        Ok(started.iter().map(|_| CHIP_ID).collect())
    }

    fn getChip(&self, chip_id: i32) -> binder::Result<Strong<dyn IWifiChip>> {
        match &*self.started.lock().expect("wifi lock poisoned") {
            Some(started) if chip_id == CHIP_ID => Ok(started.chip.clone()),
            _ => invalid_args(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stopped_hal_has_no_chip() {
        let wifi = Wifi::default();
        assert!(!wifi.isStarted().expect("isStarted"));
        assert_eq!(wifi.getChipIds().expect("getChipIds"), Vec::<i32>::new());
        let err = wifi.getChip(CHIP_ID).expect_err("not started");
        assert_eq!(err.service_specific_error(), WifiStatusCode::ERROR_INVALID_ARGS.0);
        wifi.stop().expect("stop is idempotent");
    }
}
