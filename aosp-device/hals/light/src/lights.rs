//! The ILights implementation over sysfs.

use std::path::{Path, PathBuf};

use android_hardware_light::aidl::android::hardware::light::{
    FlashMode::FlashMode, HwLight::HwLight, HwLightEffect::HwLightEffect,
    HwLightState::HwLightState, ILights::ILights, LightType::LightType,
};
use binder::{ExceptionCode, Interface, Status};
use log::{debug, info, warn};

/// One controllable light: an LED class device or a display backlight.
#[derive(Debug)]
struct Light {
    hw: HwLight,
    brightness: PathBuf,
    max_brightness: u32,
}

impl Light {
    /// Reads `max_brightness` from the sysfs device directory `dir`.
    fn probe(dir: &Path, id: i32, kind: LightType, ordinal: i32) -> pchal::Result<Self> {
        let max_brightness = pchal::sysfs::read(dir.join("max_brightness"))?;
        Ok(Self {
            // minUpdatePeriodMillis 0: no setLightEffects animations.
            hw: HwLight { id, ordinal, r#type: kind, minUpdatePeriodMillis: 0 },
            brightness: dir.join("brightness"),
            max_brightness,
        })
    }
}

/// The lights found at startup. sysfs is the only state, so no locking is needed.
#[derive(Debug, Default)]
pub struct Lights {
    lights: Vec<Light>,
}

impl Interface for Lights {}

impl Lights {
    /// Collects the LEDs in `leds_dir` that Android has a light type for, then every
    /// backlight in `backlight_dir`. Devices that cannot be probed are logged and skipped.
    pub fn discover(leds_dir: &Path, backlight_dir: &Path) -> Self {
        let leds = list(leds_dir)
            .into_iter()
            .filter_map(|dir| Some((led_type(file_name(&dir))?, dir)));
        let backlights = list(backlight_dir).into_iter().map(|dir| (LightType::BACKLIGHT, dir));

        let mut lights: Vec<Light> = Vec::new();
        for (kind, dir) in leds.chain(backlights) {
            let id = i32::try_from(lights.len()).expect("fewer than 2^31 lights");
            let ordinal = lights.iter().filter(|light| light.hw.r#type == kind).count();
            let ordinal = i32::try_from(ordinal).expect("fewer than 2^31 lights");
            match Light::probe(&dir, id, kind, ordinal) {
                Ok(light) => {
                    debug!("light {id}: {kind:?} at {}", dir.display());
                    lights.push(light);
                }
                Err(err) => warn!("skipping {}: {err}", dir.display()),
            }
        }
        info!("found {} lights", lights.len());
        Self { lights }
    }

    fn light(&self, id: i32) -> binder::Result<&Light> {
        self.lights
            .iter()
            .find(|light| light.hw.id == id)
            .ok_or_else(|| Status::new_exception(ExceptionCode::UNSUPPORTED_OPERATION, None))
    }
}

impl ILights for Lights {
    fn setLightState(&self, id: i32, state: &HwLightState) -> binder::Result<()> {
        let light = self.light(id)?;
        if state.flashMode != FlashMode::NONE {
            // LED triggers could blink in hardware; steady light is what every PC LED does.
            debug!("light {id}: flash mode {:?} shown as steady", state.flashMode);
        }
        let value = scale(luminance(state.color), light.max_brightness);
        pchal::sysfs::write(&light.brightness, value)?;
        Ok(())
    }

    fn setLightEffects(&self, _effects: &[HwLightEffect]) -> binder::Result<()> {
        // Every light reports minUpdatePeriodMillis 0, which means no effects.
        Err(Status::new_exception(ExceptionCode::UNSUPPORTED_OPERATION, None))
    }

    fn getLights(&self) -> binder::Result<Vec<HwLight>> {
        Ok(self.lights.iter().map(|light| light.hw).collect())
    }
}

/// Lists a sysfs class directory, treating an unreadable one as empty.
fn list(dir: &Path) -> Vec<PathBuf> {
    pchal::sysfs::entries(dir).unwrap_or_else(|err| {
        warn!("{err}");
        Vec::new()
    })
}

fn file_name(path: &Path) -> &str {
    path.file_name().and_then(|name| name.to_str()).unwrap_or_default()
}

/// Maps an LED class name (`devicename:color:function`) to the Android light type for its
/// function. Lock-key LEDs belong to the input subsystem and get no type.
fn led_type(name: &str) -> Option<LightType> {
    let function = name.rsplit(':').next().unwrap_or(name);
    let kind = match function {
        "kbd_backlight" => LightType::KEYBOARD,
        "micmute" => LightType::MICROPHONE,
        "wlan" | "wifi" => LightType::WIFI,
        "bluetooth" => LightType::BLUETOOTH,
        "camera" => LightType::CAMERA,
        "charging" | "battery" => LightType::BATTERY,
        "indicator" | "status" => LightType::NOTIFICATIONS,
        _ => return None,
    };
    Some(kind)
}

/// Perceived brightness (0..=255) of an ARGB colour, with the integer weights the
/// framework's lights code uses. Alpha is ignored.
fn luminance(argb: i32) -> u32 {
    let [_, r, g, b] = argb.to_be_bytes().map(u32::from);
    (77 * r + 150 * g + 29 * b) >> 8
}

/// Scales a 0..=255 brightness to a device's 0..=max range, rounding to nearest.
fn scale(brightness: u32, max: u32) -> u32 {
    let scaled = (u64::from(brightness) * u64::from(max) + 127) / 255;
    u32::try_from(scaled).expect("scaled brightness is at most max")
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;

    #[test]
    fn led_functions_map_to_types() {
        assert_eq!(led_type("tpacpi::kbd_backlight"), Some(LightType::KEYBOARD));
        assert_eq!(led_type("platform::micmute"), Some(LightType::MICROPHONE));
        assert_eq!(led_type("phy0-led"), None);
        assert_eq!(led_type("input3::capslock"), None);
    }

    #[test]
    fn luminance_spans_full_range() {
        assert_eq!(luminance(0xff00_0000_u32 as i32), 0);
        assert_eq!(luminance(0xffff_ffff_u32 as i32), 255);
        assert!(luminance(0xff00_ff00_u32 as i32) > luminance(0xff00_00ff_u32 as i32));
    }

    #[test]
    fn scale_hits_both_ends() {
        assert_eq!(scale(0, 3), 0);
        assert_eq!(scale(255, 3), 3);
        assert_eq!(scale(255, 120_000), 120_000);
    }

    #[test]
    fn discover_and_set_state() -> binder::Result<()> {
        let root = tempfile::tempdir().expect("create temp dir");
        let leds = root.path().join("leds");
        let backlight = root.path().join("backlight");
        for (dir, max) in [
            (leds.join("tpacpi::kbd_backlight"), "2"),
            (leds.join("input3::capslock"), "1"),
            (backlight.join("intel_backlight"), "1000"),
        ] {
            fs::create_dir_all(&dir).expect("create fixture dir");
            fs::write(dir.join("max_brightness"), format!("{max}\n")).expect("write fixture");
        }

        let lights = Lights::discover(&leds, &backlight);
        let types: Vec<_> = lights.getLights()?.iter().map(|light| light.r#type).collect();
        assert_eq!(types, [LightType::KEYBOARD, LightType::BACKLIGHT]);

        let white = HwLightState { color: 0xffff_ffff_u32 as i32, ..Default::default() };
        lights.setLightState(1, &white)?;
        let written = fs::read_to_string(backlight.join("intel_backlight/brightness"))
            .expect("brightness written");
        assert_eq!(written, "1000");
        assert!(lights.setLightState(7, &white).is_err());
        Ok(())
    }
}
