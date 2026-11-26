//! The flash LED of a built-in camera and when it lights.
//!
//! The LED is a Linux LED flash class device (`/sys/class/leds/<color>:flash`, gts7l: the
//! PM8150L flash driver on both rear LED channels). The HAL drives it in torch mode only:
//! the framework's torch, `FLASH_MODE_TORCH`, and as the "flash" of a still capture, where
//! the LED stays lit until the software auto exposure has settled under it. A real strobe
//! would need exposure metered under a pre-flash and scaled up, which the software ISP
//! cannot do in the strobe's time limit.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::metadata::Metadata;
use crate::tags::*;

/// Where Linux lists LED class devices.
pub const LEDS_DIR: &str = "/sys/class/leds";

/// Frames the LED must light before the exposure counts as settled under it: the frames
/// already queued were exposed without it, and auto exposure moves every few frames.
pub const MIN_LIT_FRAMES: u32 = 10;
/// Frames a precapture sequence or a flash capture waits at most for the exposure.
pub const MAX_LIT_FRAMES: u32 = 30;
/// Frames the LED stays lit after a precapture sequence while no still capture comes.
const ARMED_FRAMES: u32 = 90;

/// A flash LED, driven through its torch brightness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlashLed {
    dir: PathBuf,
    /// `max_brightness`: torch levels above off.
    levels: u32,
}

impl FlashLed {
    /// The first LED in `leds_dir` named `*:flash` or `*:torch` with a brightness range.
    pub fn find(leds_dir: &Path) -> Option<Self> {
        let mut dirs: Vec<PathBuf> = fs::read_dir(leds_dir)
            .ok()?
            .filter_map(|entry| Some(entry.ok()?.path()))
            .filter(|path| {
                path.file_name()
                    .and_then(OsStr::to_str)
                    .is_some_and(|name| name.ends_with(":flash") || name.ends_with(":torch"))
            })
            .collect();
        dirs.sort();
        dirs.into_iter().find_map(|dir| {
            Self::open(&dir).inspect_err(|err| log::warn!("{}: {err}", dir.display())).ok()
        })
    }

    /// The LED class device at `dir`.
    ///
    /// # Errors
    ///
    /// Returns an error if `max_brightness` cannot be read or is 0.
    pub fn open(dir: &Path) -> io::Result<Self> {
        let max = fs::read_to_string(dir.join("max_brightness"))?;
        let levels: u32 = max.trim().parse().map_err(io::Error::other)?;
        if levels == 0 {
            return Err(io::Error::other("max_brightness is 0"));
        }
        Ok(Self { dir: dir.to_owned(), levels })
    }

    /// The LED's name (`"white:flash"`).
    pub fn name(&self) -> &str {
        self.dir.file_name().and_then(OsStr::to_str).unwrap_or_default()
    }

    /// The torch brightness levels above off.
    pub fn levels(&self) -> u32 {
        self.levels
    }

    /// Lights the LED at `level` (clamped to [`levels`](Self::levels)); 0 is off.
    ///
    /// # Errors
    ///
    /// Returns the error of writing `brightness`.
    pub fn set(&self, level: u32) -> io::Result<()> {
        fs::write(self.dir.join("brightness"), level.min(self.levels).to_string())
    }
}

/// What auto exposure knows about the frames before a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Metering {
    /// The exposure has settled.
    pub converged: bool,
    /// The scene needs long exposure and high gain: auto flash fires.
    pub dark: bool,
}

/// The flash for one request's frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlashFrame {
    /// The LED lights for the frame.
    pub lit: bool,
    /// The exposure must settle under the LED before the frame is taken.
    pub settle: bool,
    /// `CONTROL_AE_STATE` of the result.
    pub ae_state: u8,
    /// `FLASH_STATE` of the result.
    pub flash_state: u8,
}

/// The flash part of the AE state machine: precapture sequences, auto and forced flash,
/// single flash and torch, request by request.
#[derive(Debug, Clone, Default)]
pub struct FlashControl {
    /// Frames into a precapture sequence lit by the LED.
    precapture: Option<u32>,
    /// Frames since a lit precapture sequence ended; the LED stays lit for the still
    /// capture that follows it.
    armed: Option<u32>,
    /// Whether the LED lit the last frame.
    lit: bool,
    /// Whether the scene was dark when last metered without the LED.
    dark: bool,
}

impl FlashControl {
    /// Decides the flash for a request with `settings`, given the metering of the frames
    /// before it, and advances the state machine.
    pub fn plan(&mut self, settings: &Metadata, metering: Metering) -> FlashFrame {
        if !self.lit {
            self.dark = metering.dark;
        }
        // `Some`: auto exposure decides the flash; `None`: FLASH_MODE does.
        let auto = match settings.first(CONTROL_AE_MODE).unwrap_or(CONTROL_AE_MODE_ON) {
            CONTROL_AE_MODE_ON_ALWAYS_FLASH => Some(true),
            CONTROL_AE_MODE_ON_AUTO_FLASH => Some(self.dark),
            _ => None,
        };
        let wants_flash = auto == Some(true);
        let flash_mode = settings.first(FLASH_MODE).unwrap_or(0);
        let still = settings.first(CONTROL_CAPTURE_INTENT) == Some(CONTROL_CAPTURE_INTENT_STILL_CAPTURE);
        let trigger = settings.first(CONTROL_AE_PRECAPTURE_TRIGGER).unwrap_or(0);

        let mut ae_state = CONTROL_AE_STATE_CONVERGED;
        match trigger {
            CONTROL_AE_PRECAPTURE_TRIGGER_START => {
                self.armed = None;
                self.precapture = wants_flash.then_some(0);
                ae_state = CONTROL_AE_STATE_PRECAPTURE;
            }
            CONTROL_AE_PRECAPTURE_TRIGGER_CANCEL => {
                self.armed = None;
                self.precapture = None;
            }
            _ => {}
        }
        if let Some(frames) = self.precapture {
            let settled = frames >= MIN_LIT_FRAMES && metering.converged;
            if settled || frames >= MAX_LIT_FRAMES {
                self.precapture = None;
                self.armed = Some(0);
            } else {
                self.precapture = Some(frames + 1);
                ae_state = CONTROL_AE_STATE_PRECAPTURE;
            }
        }
        if let Some(frames) = self.armed {
            self.armed = (frames < ARMED_FRAMES && auto.is_some()).then_some(frames + 1);
        }

        let armed = self.armed.is_some();
        let fire = (still && wants_flash) || (flash_mode == FLASH_MODE_SINGLE && auto.is_none());
        let torch = flash_mode == FLASH_MODE_TORCH && auto.is_none();
        let lit = fire || torch || armed || self.precapture.is_some();
        let settle = fire && !armed;
        if fire {
            // The still capture uses up the precapture sequence.
            self.armed = None;
        }
        if ae_state == CONTROL_AE_STATE_CONVERGED && wants_flash && !armed && !fire {
            ae_state = CONTROL_AE_STATE_FLASH_REQUIRED;
        }
        self.lit = lit;
        FlashFrame {
            lit,
            settle,
            ae_state,
            flash_state: if lit { FLASH_STATE_FIRED } else { FLASH_STATE_READY },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(ae_mode: u8, flash_mode: u8, intent: u8, trigger: u8) -> Metadata {
        let mut md = Metadata::new();
        md.set(CONTROL_AE_MODE, &[ae_mode]);
        md.set(FLASH_MODE, &[flash_mode]);
        md.set(CONTROL_CAPTURE_INTENT, &[intent]);
        md.set(CONTROL_AE_PRECAPTURE_TRIGGER, &[trigger]);
        md
    }

    const PREVIEW: u8 = CONTROL_CAPTURE_INTENT_PREVIEW;
    const STILL: u8 = CONTROL_CAPTURE_INTENT_STILL_CAPTURE;
    const BRIGHT: Metering = Metering { converged: true, dark: false };
    const DARK: Metering = Metering { converged: true, dark: true };

    #[test]
    fn plain_auto_exposure_never_lights() {
        let mut flash = FlashControl::default();
        for intent in [PREVIEW, STILL] {
            let frame = flash.plan(&settings(CONTROL_AE_MODE_ON, 0, intent, 0), DARK);
            assert!(!frame.lit && !frame.settle);
            assert_eq!(frame.ae_state, CONTROL_AE_STATE_CONVERGED);
            assert_eq!(frame.flash_state, FLASH_STATE_READY);
        }
    }

    #[test]
    fn torch_follows_flash_mode() {
        let mut flash = FlashControl::default();
        let frame = flash.plan(&settings(CONTROL_AE_MODE_ON, FLASH_MODE_TORCH, PREVIEW, 0), BRIGHT);
        assert!(frame.lit && !frame.settle);
        assert_eq!(frame.flash_state, FLASH_STATE_FIRED);
        assert!(!flash.plan(&settings(CONTROL_AE_MODE_ON, 0, PREVIEW, 0), BRIGHT).lit);
        // Auto exposure flash modes override FLASH_MODE.
        let frame =
            flash.plan(&settings(CONTROL_AE_MODE_ON_AUTO_FLASH, FLASH_MODE_TORCH, PREVIEW, 0), BRIGHT);
        assert!(!frame.lit);
    }

    #[test]
    fn auto_flash_precapture_then_still() {
        let mut flash = FlashControl::default();
        let preview = settings(CONTROL_AE_MODE_ON_AUTO_FLASH, 0, PREVIEW, 0);
        assert_eq!(flash.plan(&preview, DARK).ae_state, CONTROL_AE_STATE_FLASH_REQUIRED);

        let trigger = settings(CONTROL_AE_MODE_ON_AUTO_FLASH, 0, PREVIEW, CONTROL_AE_PRECAPTURE_TRIGGER_START);
        let frame = flash.plan(&trigger, DARK);
        assert!(frame.lit);
        assert_eq!(frame.ae_state, CONTROL_AE_STATE_PRECAPTURE);
        // Under the LED the scene is bright: the decision made before it holds.
        let mut frames = 1;
        while flash.plan(&preview, BRIGHT).ae_state == CONTROL_AE_STATE_PRECAPTURE {
            frames += 1;
            assert!(frames <= MAX_LIT_FRAMES + 1);
        }
        assert!(frames >= MIN_LIT_FRAMES);
        let frame = flash.plan(&preview, BRIGHT);
        assert!(frame.lit, "the LED stays lit until the still capture");
        assert_eq!(frame.ae_state, CONTROL_AE_STATE_CONVERGED);

        let frame = flash.plan(&settings(CONTROL_AE_MODE_ON_AUTO_FLASH, 0, STILL, 0), BRIGHT);
        assert!(frame.lit && !frame.settle);
        assert_eq!(frame.flash_state, FLASH_STATE_FIRED);
        assert!(!flash.plan(&preview, BRIGHT).lit);
    }

    #[test]
    fn auto_flash_stays_off_in_bright_scenes() {
        let mut flash = FlashControl::default();
        let trigger = settings(CONTROL_AE_MODE_ON_AUTO_FLASH, 0, PREVIEW, CONTROL_AE_PRECAPTURE_TRIGGER_START);
        let frame = flash.plan(&trigger, BRIGHT);
        assert!(!frame.lit);
        assert_eq!(frame.ae_state, CONTROL_AE_STATE_PRECAPTURE);
        let still = settings(CONTROL_AE_MODE_ON_AUTO_FLASH, 0, STILL, 0);
        let frame = flash.plan(&still, BRIGHT);
        assert!(!frame.lit);
        assert_eq!(frame.ae_state, CONTROL_AE_STATE_CONVERGED);
    }

    #[test]
    fn forced_flash_without_precapture_settles() {
        let mut flash = FlashControl::default();
        let frame = flash.plan(&settings(CONTROL_AE_MODE_ON_ALWAYS_FLASH, 0, STILL, 0), BRIGHT);
        assert!(frame.lit && frame.settle);
        let frame = flash.plan(&settings(CONTROL_AE_MODE_ON, FLASH_MODE_SINGLE, STILL, 0), BRIGHT);
        assert!(frame.lit && frame.settle);
    }

    #[test]
    fn cancel_and_timeout_turn_the_led_off() {
        let mut flash = FlashControl::default();
        let preview = settings(CONTROL_AE_MODE_ON_ALWAYS_FLASH, 0, PREVIEW, 0);
        let start = settings(CONTROL_AE_MODE_ON_ALWAYS_FLASH, 0, PREVIEW, CONTROL_AE_PRECAPTURE_TRIGGER_START);
        let cancel = settings(CONTROL_AE_MODE_ON_ALWAYS_FLASH, 0, PREVIEW, CONTROL_AE_PRECAPTURE_TRIGGER_CANCEL);
        assert!(flash.plan(&start, BRIGHT).lit);
        assert!(!flash.plan(&cancel, BRIGHT).lit);

        assert!(flash.plan(&start, BRIGHT).lit);
        let lit = (0..MAX_LIT_FRAMES + ARMED_FRAMES + 5).filter(|_| flash.plan(&preview, BRIGHT).lit).count();
        assert!(lit as u32 <= MAX_LIT_FRAMES + ARMED_FRAMES + 1);
        assert!(!flash.plan(&preview, BRIGHT).lit);
    }

    #[test]
    fn finds_the_flash_led() {
        let root = std::env::temp_dir().join(format!("pccamera-leds-{}", std::process::id()));
        let flash = root.join("white:flash");
        fs::create_dir_all(&flash).unwrap();
        fs::create_dir_all(root.join("input3::capslock")).unwrap();
        fs::write(flash.join("max_brightness"), "10\n").unwrap();
        let led = FlashLed::find(&root).unwrap();
        assert_eq!((led.name(), led.levels()), ("white:flash", 10));
        led.set(25).unwrap();
        assert_eq!(fs::read_to_string(flash.join("brightness")).unwrap(), "10");
        fs::remove_dir_all(&root).unwrap();
        assert!(FlashLed::find(&root).is_none());
    }
}
