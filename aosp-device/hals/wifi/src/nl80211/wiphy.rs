//! What a wireless device (`struct wiphy`) supports, from `NL80211_CMD_GET_WIPHY`.

use super::attr::Attrs;

/// `NL80211_ATTR_WIPHY`.
pub(crate) const ATTR_WIPHY: u16 = 1;
/// `NL80211_ATTR_WIPHY_NAME`.
const ATTR_WIPHY_NAME: u16 = 2;
/// `NL80211_ATTR_WIPHY_BANDS`.
const ATTR_WIPHY_BANDS: u16 = 22;
/// `NL80211_ATTR_SUPPORTED_IFTYPES`.
const ATTR_SUPPORTED_IFTYPES: u16 = 32;
/// `NL80211_ATTR_INTERFACE_COMBINATIONS`.
const ATTR_INTERFACE_COMBINATIONS: u16 = 120;

/// `NL80211_BAND_ATTR_FREQS`.
const BAND_ATTR_FREQS: u16 = 1;
/// `NL80211_FREQUENCY_ATTR_FREQ`.
const FREQUENCY_ATTR_FREQ: u16 = 1;
/// `NL80211_FREQUENCY_ATTR_DISABLED`.
const FREQUENCY_ATTR_DISABLED: u16 = 2;
/// `NL80211_FREQUENCY_ATTR_NO_IR`: no transmission before hearing an AP (radar or
/// regulatory rules), so no access point of our own there.
const FREQUENCY_ATTR_NO_IR: u16 = 3;
/// `NL80211_FREQUENCY_ATTR_RADAR`: radar detection (DFS) is required.
const FREQUENCY_ATTR_RADAR: u16 = 5;

/// `NL80211_IFACE_COMB_LIMITS`.
const IFACE_COMB_LIMITS: u16 = 1;
/// `NL80211_IFACE_COMB_MAXNUM`.
const IFACE_COMB_MAXNUM: u16 = 2;
/// `NL80211_IFACE_LIMIT_MAX`.
const IFACE_LIMIT_MAX: u16 = 1;
/// `NL80211_IFACE_LIMIT_TYPES`.
const IFACE_LIMIT_TYPES: u16 = 2;

/// Interface types (`enum nl80211_iftype`) this HAL deals with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum Iftype {
    /// `NL80211_IFTYPE_STATION`: a client (STA) interface.
    Station = 2,
    /// `NL80211_IFTYPE_AP`: an access point (soft AP, hotspot).
    Ap = 3,
    /// `NL80211_IFTYPE_P2P_CLIENT`.
    P2pClient = 8,
    /// `NL80211_IFTYPE_P2P_GO`.
    P2pGo = 9,
    /// `NL80211_IFTYPE_P2P_DEVICE`: the P2P management interface (`p2p-dev-wlan0`),
    /// which wpa_supplicant handles; only test fixtures name it.
    #[cfg(test)]
    P2pDevice = 10,
}

/// A set of `enum nl80211_iftype` values, one bit per type.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Iftypes(u32);

impl Iftypes {
    /// Whether `iftype` is in the set.
    pub fn contains(self, iftype: Iftype) -> bool {
        self.0 & (1 << iftype as u32) != 0
    }

    /// The set with the bits of `bits`, bit N standing for iftype N.
    #[cfg(test)]
    pub fn from_bits(bits: u32) -> Self {
        Self(bits)
    }

    /// The set of the flag attributes in `attrs`, whose types are iftype numbers.
    fn from_flags(attrs: Attrs<'_>) -> Self {
        Self(attrs.filter(|attr| attr.kind < 32).fold(0, |set, attr| set | 1 << attr.kind))
    }
}

/// At most `max` interfaces whose types are in `iftypes` (`struct ieee80211_iface_limit`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limit {
    /// The highest number of such interfaces.
    pub max: u32,
    /// The types this limit counts.
    pub iftypes: Iftypes,
}

/// One set of interfaces the device runs at the same time
/// (`struct ieee80211_iface_combination`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Combination {
    /// Per-type limits; an interface type outside them is not allowed.
    pub limits: Vec<Limit>,
    /// The highest number of interfaces in total.
    pub max_interfaces: u32,
}

/// A frequency band (`enum nl80211_band`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Band {
    /// 2.4 GHz.
    Ghz2,
    /// 5 GHz.
    Ghz5,
    /// 60 GHz (802.11ad).
    Ghz60,
    /// 6 GHz.
    Ghz6,
}

impl Band {
    fn from_index(index: u16) -> Option<Self> {
        match index {
            0 => Some(Self::Ghz2),
            1 => Some(Self::Ghz5),
            2 => Some(Self::Ghz60),
            3 => Some(Self::Ghz6),
            _ => None,
        }
    }
}

/// A 20 MHz channel the device can use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Channel {
    /// Its band.
    pub band: Band,
    /// Its centre frequency.
    pub mhz: u32,
    /// Whether the device may start transmitting there (an access point needs that).
    pub initiate_radiation: bool,
    /// Whether it is a DFS channel (radar detection required).
    pub dfs: bool,
}

/// What one wireless device supports.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Wiphy {
    /// Its index (`phy<N>` by default).
    pub index: u32,
    /// Its name, such as `phy0`.
    pub name: String,
    /// The interface types it can create.
    pub iftypes: Iftypes,
    /// The interface combinations it runs; empty means one interface at a time.
    pub combinations: Vec<Combination>,
    /// The channels regulatory rules allow, in the order the device lists them.
    pub channels: Vec<Channel>,
}

impl Wiphy {
    /// Adds the attributes of one message of a split wiphy dump; each message carries
    /// some of the information (the bands may span several).
    pub fn merge(&mut self, attrs: Attrs<'_>) {
        for attr in attrs {
            match attr.kind {
                ATTR_WIPHY => self.index = attr.u32().unwrap_or(self.index),
                ATTR_WIPHY_NAME => {
                    if let Some(name) = attr.string() {
                        self.name = name.to_owned();
                    }
                }
                ATTR_SUPPORTED_IFTYPES => self.iftypes = Iftypes::from_flags(attr.nested()),
                ATTR_INTERFACE_COMBINATIONS => {
                    self.combinations =
                        attr.nested().map(|c| parse_combination(c.nested())).collect();
                }
                ATTR_WIPHY_BANDS => {
                    for band in attr.nested() {
                        let Some(kind) = Band::from_index(band.kind) else { continue };
                        let Some(freqs) = band.nested().get(BAND_ATTR_FREQS) else { continue };
                        self.channels
                            .extend(freqs.nested().filter_map(|f| parse_channel(kind, f.nested())));
                    }
                }
                _ => {}
            }
        }
    }
}

fn parse_combination(attrs: Attrs<'_>) -> Combination {
    let limits = attrs
        .get(IFACE_COMB_LIMITS)
        .map(|limits| {
            limits
                .nested()
                .map(|limit| {
                    let limit = limit.nested();
                    Limit {
                        max: limit.get(IFACE_LIMIT_MAX).and_then(|a| a.u32()).unwrap_or(0),
                        iftypes: limit
                            .get(IFACE_LIMIT_TYPES)
                            .map(|types| Iftypes::from_flags(types.nested()))
                            .unwrap_or_default(),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    let max_interfaces = attrs.get(IFACE_COMB_MAXNUM).and_then(|a| a.u32()).unwrap_or(1);
    Combination { limits, max_interfaces }
}

/// The channel described by `attrs`, or `None` if it is disabled.
fn parse_channel(band: Band, attrs: Attrs<'_>) -> Option<Channel> {
    let mhz = attrs.get(FREQUENCY_ATTR_FREQ)?.u32()?;
    if attrs.get(FREQUENCY_ATTR_DISABLED).is_some() {
        return None;
    }
    Some(Channel {
        band,
        mhz,
        initiate_radiation: attrs.get(FREQUENCY_ATTR_NO_IR).is_none(),
        dfs: attrs.get(FREQUENCY_ATTR_RADAR).is_some(),
    })
}

#[cfg(test)]
mod tests {
    use super::super::attr::tests::encode;
    use super::super::attr::Attrs;
    use super::*;

    const NESTED: u16 = 0x8000;

    fn flags(types: &[u16]) -> Vec<u8> {
        encode(&types.iter().map(|&t| (t, Vec::new())).collect::<Vec<_>>())
    }

    fn u32_attr(kind: u16, value: u32) -> (u16, Vec<u8>) {
        (kind, value.to_ne_bytes().to_vec())
    }

    /// iwlmvm's combination: one STA, one AP or P2P, one P2P device, three in total.
    fn iwlmvm_combination() -> Vec<u8> {
        let limit = |max, types: &[u16]| {
            encode(&[u32_attr(IFACE_LIMIT_MAX, max), (NESTED | IFACE_LIMIT_TYPES, flags(types))])
        };
        let limits = encode(&[
            (NESTED | 1, limit(1, &[2])),
            (NESTED | 2, limit(1, &[3, 8, 9])),
            (NESTED | 3, limit(1, &[10])),
        ]);
        encode(&[(
            NESTED | 1,
            encode(&[(NESTED | IFACE_COMB_LIMITS, limits), u32_attr(IFACE_COMB_MAXNUM, 3)]),
        )])
    }

    fn freq(mhz: u32, extra: &[u16]) -> Vec<u8> {
        let mut attrs = vec![u32_attr(FREQUENCY_ATTR_FREQ, mhz)];
        attrs.extend(extra.iter().map(|&kind| (kind, Vec::new())));
        encode(&attrs)
    }

    #[test]
    fn merges_a_split_dump() {
        let first = encode(&[
            u32_attr(ATTR_WIPHY, 0),
            (ATTR_WIPHY_NAME, b"phy0\0".to_vec()),
            (NESTED | ATTR_SUPPORTED_IFTYPES, flags(&[1, 2, 3, 8, 9, 10])),
            (NESTED | ATTR_INTERFACE_COMBINATIONS, iwlmvm_combination()),
        ]);
        let band = |index: u16, freqs: Vec<Vec<u8>>| {
            let freqs = encode(
                &freqs
                    .into_iter()
                    .enumerate()
                    .map(|(i, f)| (NESTED | i as u16, f))
                    .collect::<Vec<_>>(),
            );
            encode(&[(
                NESTED | ATTR_WIPHY_BANDS,
                encode(&[(NESTED | index, encode(&[(NESTED | BAND_ATTR_FREQS, freqs)]))]),
            )])
        };
        let second = band(0, vec![freq(2412, &[]), freq(2484, &[FREQUENCY_ATTR_DISABLED])]);
        let third = band(
            1,
            vec![freq(5180, &[]), freq(5260, &[FREQUENCY_ATTR_NO_IR, FREQUENCY_ATTR_RADAR])],
        );

        let mut wiphy = Wiphy::default();
        for msg in [&first, &second, &third] {
            wiphy.merge(Attrs::new(msg));
        }

        assert_eq!(wiphy.name, "phy0");
        assert!(wiphy.iftypes.contains(Iftype::Station));
        assert!(wiphy.iftypes.contains(Iftype::P2pDevice));
        assert_eq!(wiphy.combinations.len(), 1);
        let combination = &wiphy.combinations[0];
        assert_eq!(combination.max_interfaces, 3);
        assert_eq!(combination.limits.len(), 3);
        assert_eq!(combination.limits[1].max, 1);
        assert!(combination.limits[1].iftypes.contains(Iftype::Ap));
        assert!(combination.limits[1].iftypes.contains(Iftype::P2pGo));
        assert!(!combination.limits[1].iftypes.contains(Iftype::Station));
        assert_eq!(
            wiphy.channels,
            [
                Channel { band: Band::Ghz2, mhz: 2412, initiate_radiation: true, dfs: false },
                Channel { band: Band::Ghz5, mhz: 5180, initiate_radiation: true, dfs: false },
                Channel { band: Band::Ghz5, mhz: 5260, initiate_radiation: false, dfs: true },
            ]
        );
    }

    #[test]
    fn iftype_set() {
        let set = Iftypes(1 << 2 | 1 << 3);
        assert!(set.contains(Iftype::Ap));
        assert!(!set.contains(Iftype::P2pGo));
    }
}
