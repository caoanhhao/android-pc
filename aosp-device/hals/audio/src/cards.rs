//! Sound card discovery from `/proc/asound/cards` and `/proc/asound/pcm`.
//!
//! The primary module drives the built-in analog codec (HD Audio, AC'97, virtio-snd on
//! Cuttlefish): its first playback and first capture PCM. HDMI/DisplayPort PCMs and USB
//! audio cards are listed but not picked; the framework reports USB devices itself.
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

impl Primary {
    pub fn select(pcms: &[PcmInfo]) -> Self {
        let first = |wanted: fn(&PcmInfo) -> bool| {
            pcms.iter().find(|p| p.kind == Kind::Analog && wanted(p)).cloned()
        };
        Self { playback: first(|p| p.playback), capture: first(|p| p.capture) }
    }
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

    #[test]
    fn hdmi_only_has_no_primary() {
        let cards = " 0 [NVidia         ]: HDA-Intel - HDA NVidia\n";
        let pcms = "00-03: HDMI 0 : HDMI 0 : playback 1\n";
        assert_eq!(Primary::select(&list(cards, pcms)), Primary::default());
    }

    #[test]
    fn empty_proc_is_no_cards() {
        assert!(list("--- no soundcards ---\n", "").is_empty());
    }
}
