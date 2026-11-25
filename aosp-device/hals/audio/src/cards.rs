//! Sound card discovery from `/proc/asound/cards` and `/proc/asound/pcm`.
//!
//! The primary module drives the built-in analog codec (HD Audio, AC'97, virtio-snd on
//! Cuttlefish): its first playback and first capture PCM. An HDMI/DisplayPort PCM is picked
//! when the framework connects an HDMI device ([`select_hdmi`]); USB audio cards are listed
//! but not picked, the framework reports USB devices itself.
//!
//! No AIDL types here, so the parsing is unit tested on the host.

use std::fmt;

/// What kind of sink or source a PCM device is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    /// Built-in codec (line out, headphones, speakers, microphones).
    Analog,
    /// HDMI or DisplayPort audio of a graphics card.
    Hdmi,
    /// A USB audio class device (headset, webcam microphone, DAC).
    Usb,
}

/// One PCM device with the direction(s) it supports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PcmInfo {
    pub card: u32,
    pub device: u32,
    /// The card's long name (`HDA Intel PCH`).
    pub card_name: String,
    /// The PCM's name (`ALC897 Analog`, `HDMI 0`).
    pub name: String,
    pub kind: Kind,
    pub playback: bool,
    pub capture: bool,
}

impl fmt::Display for PcmInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "card {} device {} ({}: {}, {:?})",
            self.card, self.device, self.card_name, self.name, self.kind
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Card {
    index: u32,
    driver: String,
    name: String,
}

/// Parses `/proc/asound/cards`: ` 0 [PCH            ]: HDA-Intel - HDA Intel PCH` followed
/// by an indented line with the long name, which is skipped.
fn parse_cards(text: &str) -> Vec<Card> {
    text.lines()
        .filter_map(|line| {
            let (index, rest) = line.trim_start().split_once(" [")?;
            let index = index.trim().parse().ok()?;
            let (_, rest) = rest.split_once("]: ")?;
            let (driver, name) = rest.split_once(" - ").unwrap_or((rest, rest));
            Some(Card { index, driver: driver.trim().to_owned(), name: name.trim().to_owned() })
        })
        .collect()
}

/// Parses `/proc/asound/pcm`: `00-01: ALC897 Alt Analog : ALC897 Alt Analog : playback 1`.
fn parse_pcms(text: &str, cards: &[Card]) -> Vec<PcmInfo> {
    text.lines()
        .filter_map(|line| {
            let (address, rest) = line.split_once(": ")?;
            let (card, device) = address.split_once('-')?;
            let card: u32 = card.parse().ok()?;
            let device = device.parse().ok()?;
            let fields: Vec<&str> = rest.split(" : ").map(str::trim).collect();
            let name = fields.first()?.to_string();
            let playback = fields.iter().any(|f| f.starts_with("playback"));
            let capture = fields.iter().any(|f| f.starts_with("capture"));
            let info = cards.iter().find(|c| c.index == card);
            let driver = info.map_or("", |c| c.driver.as_str());
            let card_name = info.map_or_else(String::new, |c| c.name.clone());
            Some(PcmInfo {
                card,
                device,
                kind: classify(driver, &name),
                card_name,
                name,
                playback,
                capture,
            })
        })
        .collect()
}

fn classify(driver: &str, pcm_name: &str) -> Kind {
    if driver == "USB-Audio" {
        Kind::Usb
    } else if pcm_name.contains("HDMI")
        || pcm_name.contains("DisplayPort")
        || pcm_name.starts_with("DP")
    {
        Kind::Hdmi
    } else {
        Kind::Analog
    }
}

/// Every PCM device of every card, in card and device order.
pub fn list(cards: &str, pcms: &str) -> Vec<PcmInfo> {
    let mut list = parse_pcms(pcms, &parse_cards(cards));
    list.sort_by_key(|p| (p.card, p.device));
    list
}

/// Reads [`list`] from `/proc/asound`; no sound cards (or no `/proc/asound`) is an empty
/// list.
pub fn discover() -> Vec<PcmInfo> {
    let read = |path| std::fs::read_to_string(path).unwrap_or_default();
    list(&read("/proc/asound/cards"), &read("/proc/asound/pcm"))
}

/// The PCMs the primary module drives: the first analog playback and capture devices.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Primary {
    pub playback: Option<PcmInfo>,
    pub capture: Option<PcmInfo>,
}

/// PCM device numbers a board asks for, on the first card with analog PCMs (a SoC card
/// whose microphone is on another front end than its speakers). `None`: the first one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Preferred {
    pub playback: Option<u32>,
    pub capture: Option<u32>,
}

impl Primary {
    pub fn select(pcms: &[PcmInfo]) -> Self {
        Self::select_preferred(pcms, Preferred::default())
    }

    /// [`Primary::select`], with the devices `preferred` names where the card has them.
    pub fn select_preferred(pcms: &[PcmInfo], preferred: Preferred) -> Self {
        let card = pcms.iter().find(|p| p.kind == Kind::Analog).map(|p| p.card);
        let pick = |wanted: fn(&PcmInfo) -> bool, device: Option<u32>| {
            let analog = || pcms.iter().filter(|p| p.kind == Kind::Analog && wanted(p));
            device
                .and_then(|device| analog().find(|p| Some(p.card) == card && p.device == device))
                .or_else(|| analog().next())
                .cloned()
        };
        Self {
            playback: pick(|p| p.playback, preferred.playback),
            capture: pick(|p| p.capture, preferred.capture),
        }
    }
}

/// The PCM device an HD Audio HDMI/DP jack control reports on:
/// `HDMI/DP,pcm=3 Jack` -> 3.
pub fn hdmi_jack_device(control_name: &str) -> Option<u32> {
    let (_, device) = control_name.strip_suffix(" Jack")?.rsplit_once(",pcm=")?;
    device.parse().ok()
}

/// What `persist.vendor.audio.hdmi` asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HdmiChoice {
    /// Unset or `auto`: the PCM whose jack reports a monitor with audio.
    Auto,
    /// `off`: never play to HDMI; the framework's HDMI connection is refused, so audio
    /// stays on the built-in codec.
    Off,
    /// `<device>` or `<card>,<device>`: this playback PCM (on the first card with HDMI
    /// PCMs when no card is given).
    Device { card: Option<u32>, device: u32 },
}

impl HdmiChoice {
    /// Parses the property value; unknown values mean [`HdmiChoice::Auto`].
    pub fn parse(value: Option<&str>) -> Self {
        let value = value.unwrap_or("").trim();
        if value.eq_ignore_ascii_case("off") {
            return Self::Off;
        }
        let parsed = match value.split_once(',') {
            Some((card, device)) => card
                .trim()
                .parse()
                .ok()
                .zip(device.trim().parse().ok())
                .map(|(card, device)| Self::Device { card: Some(card), device }),
            None => value.parse().ok().map(|device| Self::Device { card: None, device }),
        };
        parsed.unwrap_or(Self::Auto)
    }
}

/// The PCM to play to when the framework connects an HDMI device. `connected` are the
/// `(card, device)` pairs whose HDMI jack is on. With [`HdmiChoice::Auto`], the first
/// connected HDMI PCM, or the first HDMI PCM if no jack is on (the jack needs a valid ELD,
/// which some GPUs only provide with their own driver).
pub fn select_hdmi(
    pcms: &[PcmInfo],
    connected: &[(u32, u32)],
    choice: HdmiChoice,
) -> Option<PcmInfo> {
    let hdmi = || pcms.iter().filter(|p| p.kind == Kind::Hdmi && p.playback);
    match choice {
        HdmiChoice::Off => None,
        HdmiChoice::Auto => hdmi()
            .find(|p| connected.contains(&(p.card, p.device)))
            .or_else(|| hdmi().next())
            .cloned(),
        HdmiChoice::Device { card, device } => {
            let card = card.or_else(|| hdmi().next().map(|p| p.card))?;
            pcms.iter().find(|p| p.card == card && p.device == device && p.playback).cloned()
        }
    }
}

/// Monitor state of one HDMI pin, from `/proc/asound/card<N>/eld#<codec>.<pin>`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Eld {
    pub monitor_present: bool,
    pub eld_valid: bool,
    pub monitor_name: String,
}

pub fn parse_eld(text: &str) -> Eld {
    let mut eld = Eld::default();
    for line in text.lines() {
        let Some((key, value)) = line.split_once(char::is_whitespace) else { continue };
        let value = value.trim();
        match key {
            "monitor_present" => eld.monitor_present = value == "1",
            "eld_valid" => eld.eld_valid = value == "1",
            "monitor_name" => eld.monitor_name = value.to_owned(),
            _ => {}
        }
    }
    eld
}

#[cfg(test)]
mod tests {
    use super::*;

    // The Z790 PC (docs/usb-boot-aosp17.md, section 8).
    const PC_CARDS: &str = " 0 [PCH            ]: HDA-Intel - HDA Intel PCH
                      HDA Intel PCH at 0x6201120000 irq 199
 1 [NVidia         ]: HDA-Intel - HDA NVidia
                      HDA NVidia at 0x87080000 irq 17
 2 [Webcam         ]: USB-Audio - USB Webcam
                      Generic USB Webcam at usb-0000:00:14.0-3, high speed
";
    const PC_PCMS: &str = "00-00: ALC897 Analog : ALC897 Analog : playback 1 : capture 1
00-01: ALC897 Alt Analog : ALC897 Alt Analog : playback 1
01-03: HDMI 0 : HDMI 0 : playback 1
01-07: HDMI 1 : HDMI 1 : playback 1
01-08: HDMI 2 : HDMI 2 : playback 1
01-09: HDMI 3 : HDMI 3 : playback 1
02-00: USB Audio : USB Audio : capture 1
";

    #[test]
    fn parses_cards() {
        let cards = parse_cards(PC_CARDS);
        assert_eq!(cards.len(), 3);
        assert_eq!(
            cards[1],
            Card { index: 1, driver: "HDA-Intel".into(), name: "HDA NVidia".into() }
        );
        assert_eq!(cards[2].driver, "USB-Audio");
    }

    #[test]
    fn classifies_pc_devices() {
        let pcms = list(PC_CARDS, PC_PCMS);
        let kinds: Vec<(u32, u32, Kind)> =
            pcms.iter().map(|p| (p.card, p.device, p.kind)).collect();
        assert_eq!(kinds[0], (0, 0, Kind::Analog));
        assert_eq!(kinds[2], (1, 3, Kind::Hdmi));
        assert_eq!(kinds[6], (2, 0, Kind::Usb));
        assert!(pcms[0].playback && pcms[0].capture);
        assert!(pcms[1].playback && !pcms[1].capture);
        assert!(!pcms[6].playback && pcms[6].capture);
    }

    #[test]
    fn selects_the_analog_codec() {
        let primary = Primary::select(&list(PC_CARDS, PC_PCMS));
        let playback = primary.playback.expect("analog playback");
        assert_eq!((playback.card, playback.device), (0, 0));
        assert_eq!(playback.name, "ALC897 Analog");
        let capture = primary.capture.expect("analog capture");
        assert_eq!((capture.card, capture.device), (0, 0));
    }

    const SOC_CARDS: &str =
        " 0 [S7             ]: sm8250 - Galaxy Tab S7+\n                      Galaxy Tab S7+\n";
    const SOC_PCMS: &str = "00-00: MultiMedia1 (*) :  : playback 1 : capture 1\n00-01: MultiMedia2 (*) :  : capture 1\n00-02: MultiMedia3 (*) :  : playback 1\n";

    #[test]
    fn selects_the_preferred_soc_devices() {
        let pcms = list(SOC_CARDS, SOC_PCMS);
        let device = |p: &Option<PcmInfo>| p.as_ref().map(|p| (p.card, p.device));
        let first = Primary::select(&pcms);
        assert_eq!((device(&first.playback), device(&first.capture)), (Some((0, 0)), Some((0, 0))));
        let preferred = Preferred { playback: None, capture: Some(1) };
        let chosen = Primary::select_preferred(&pcms, preferred);
        assert_eq!(
            (device(&chosen.playback), device(&chosen.capture)),
            (Some((0, 0)), Some((0, 1)))
        );
        // A device without that direction falls back to the first one.
        let wrong = Preferred { playback: Some(1), capture: Some(2) };
        let chosen = Primary::select_preferred(&pcms, wrong);
        assert_eq!(
            (device(&chosen.playback), device(&chosen.capture)),
            (Some((0, 0)), Some((0, 0)))
        );
    }

    #[test]
    fn hdmi_only_has_no_primary() {
        let cards = " 0 [NVidia         ]: HDA-Intel - HDA NVidia\n";
        let pcms = "00-03: HDMI 0 : HDMI 0 : playback 1\n";
        assert_eq!(Primary::select(&list(cards, pcms)), Primary::default());
    }

    #[test]
    fn hdmi_jack_names() {
        assert_eq!(hdmi_jack_device("HDMI/DP,pcm=3 Jack"), Some(3));
        assert_eq!(hdmi_jack_device("HDMI/DP,pcm=10 Jack"), Some(10));
        assert_eq!(hdmi_jack_device("Line Out Jack"), None);
        assert_eq!(hdmi_jack_device("HDMI/DP,pcm=3"), None);
    }

    #[test]
    fn hdmi_choice_values() {
        assert_eq!(HdmiChoice::parse(None), HdmiChoice::Auto);
        assert_eq!(HdmiChoice::parse(Some("auto")), HdmiChoice::Auto);
        assert_eq!(HdmiChoice::parse(Some("OFF")), HdmiChoice::Off);
        assert_eq!(HdmiChoice::parse(Some("7")), HdmiChoice::Device { card: None, device: 7 });
        assert_eq!(HdmiChoice::parse(Some("1,8")), HdmiChoice::Device { card: Some(1), device: 8 });
        assert_eq!(HdmiChoice::parse(Some("x,8")), HdmiChoice::Auto);
    }

    #[test]
    fn selects_the_connected_hdmi_pcm() {
        let pcms = list(PC_CARDS, PC_PCMS);
        let device = |connected: &[(u32, u32)], choice| {
            select_hdmi(&pcms, connected, choice).map(|p| (p.card, p.device))
        };
        assert_eq!(device(&[(1, 8)], HdmiChoice::Auto), Some((1, 8)));
        // No jack on: the first HDMI PCM.
        assert_eq!(device(&[], HdmiChoice::Auto), Some((1, 3)));
        assert_eq!(device(&[(1, 8)], HdmiChoice::Off), None);
        assert_eq!(device(&[(1, 8)], HdmiChoice::Device { card: None, device: 9 }), Some((1, 9)));
        assert_eq!(
            device(&[], HdmiChoice::Device { card: Some(0), device: 1 }),
            Some((0, 1)),
            "any playback PCM can be named"
        );
        assert_eq!(device(&[], HdmiChoice::Device { card: None, device: 4 }), None);
        let analog_only = list(PC_CARDS, "00-00: ALC897 Analog : ALC897 Analog : playback 1\n");
        assert_eq!(select_hdmi(&analog_only, &[], HdmiChoice::Auto), None);
    }

    #[test]
    fn parses_eld() {
        let eld = parse_eld(
            "monitor_present\t\t1\neld_valid\t\t1\nmonitor_name\t\tLG ULTRAWIDE\n\
             connection_type\t\tHDMI\n",
        );
        assert_eq!(
            eld,
            Eld { monitor_present: true, eld_valid: true, monitor_name: "LG ULTRAWIDE".into() }
        );
        assert_eq!(parse_eld("monitor_present\t\t0\neld_valid\t\t0\n"), Eld::default());
    }

    #[test]
    fn empty_proc_is_no_cards() {
        assert!(list("--- no soundcards ---\n", "").is_empty());
    }
}
