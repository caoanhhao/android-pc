//! Which interfaces the chip runs at the same time, in the AIDL HAL's terms, derived from
//! the wiphy's nl80211 interface combinations.

use std::iter;

use crate::nl80211::{Iftype, Iftypes, Wiphy};

/// Interface limits are capped at this many: AOSP's framework expands every combination
/// into all its interface sets, and mac80211_hwsim allows 2048 of each.
const MAX_PER_LIMIT: u32 = 2;

/// An interface kind this HAL creates (`IfaceConcurrencyType` minus bridged AP and NAN,
/// which it does not offer).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A Wi-Fi client.
    Sta,
    /// A soft AP (hotspot).
    Ap,
    /// A Wi-Fi Direct group (client or owner).
    P2p,
}

impl Kind {
    const ALL: [Kind; 3] = [Kind::Sta, Kind::Ap, Kind::P2p];
}

/// At most `max` interfaces whose kinds are in `kinds`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Limit {
    /// The kinds this limit counts.
    pub kinds: Vec<Kind>,
    /// How many of them may exist.
    pub max: u32,
}

/// One set of interfaces that can exist together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Combination {
    /// Each interface counts against one limit that lists its kind.
    pub limits: Vec<Limit>,
}

/// How many interfaces of each kind exist.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    sta: u32,
    ap: u32,
    p2p: u32,
}

impl Counts {
    /// The number of `kind` interfaces.
    pub fn get(self, kind: Kind) -> u32 {
        match kind {
            Kind::Sta => self.sta,
            Kind::Ap => self.ap,
            Kind::P2p => self.p2p,
        }
    }

    /// These counts plus one `kind` interface.
    pub fn with(mut self, kind: Kind) -> Self {
        match kind {
            Kind::Sta => self.sta += 1,
            Kind::Ap => self.ap += 1,
            Kind::P2p => self.p2p += 1,
        }
        self
    }
}

impl Combination {
    /// Whether all interfaces in `counts` fit into the limits at once.
    pub fn allows(&self, counts: Counts) -> bool {
        let interfaces: Vec<Kind> = Kind::ALL
            .into_iter()
            .flat_map(|kind| iter::repeat_n(kind, counts.get(kind) as usize))
            .collect();
        let mut free: Vec<u32> = self.limits.iter().map(|limit| limit.max).collect();
        self.assign(&mut free, &interfaces)
    }

    /// Backtracking search for a limit with room for each interface; the counts are a
    /// handful, so this stays tiny.
    fn assign(&self, free: &mut [u32], interfaces: &[Kind]) -> bool {
        let Some((&kind, rest)) = interfaces.split_first() else { return true };
        for (index, limit) in self.limits.iter().enumerate() {
            if free[index] == 0 || !limit.kinds.contains(&kind) {
                continue;
            }
            free[index] -= 1;
            let fits = self.assign(free, rest);
            free[index] += 1;
            if fits {
                return true;
            }
        }
        false
    }
}

/// Whether any of `combinations` allows `counts`.
pub fn any_allows(combinations: &[Combination], counts: Counts) -> bool {
    combinations.iter().any(|combination| combination.allows(counts))
}

/// The combinations of `wiphy`. A device without interface combinations runs one
/// interface at a time, of any type it supports.
///
/// Each combination offers at most one STA: a second one (multi-STA) needs vendor support
/// this HAL does not have (`setMultiStaPrimaryConnection` and friends). The first limit
/// that counts STAs keeps one of them, the rest of its room goes to its other kinds, and
/// later limits drop STA.
pub fn from_wiphy(wiphy: &Wiphy) -> Vec<Combination> {
    let mut combinations: Vec<Combination> = Vec::new();
    for nl_combination in &wiphy.combinations {
        let mut limits: Vec<Limit> = Vec::new();
        for nl_limit in &nl_combination.limits {
            let max = nl_limit.max.min(nl_combination.max_interfaces).min(MAX_PER_LIMIT);
            let mut kinds = kinds_of(nl_limit.iftypes);
            if kinds.contains(&Kind::Sta) {
                kinds.retain(|&kind| kind != Kind::Sta);
                let has_sta = limits.iter().any(|limit| limit.kinds.contains(&Kind::Sta));
                if !has_sta && max > 0 {
                    limits.push(Limit { kinds: vec![Kind::Sta], max: 1 });
                    push_limit(&mut limits, kinds, max - 1);
                    continue;
                }
            }
            push_limit(&mut limits, kinds, max);
        }
        let combination = Combination { limits };
        if !combination.limits.is_empty() && !combinations.contains(&combination) {
            combinations.push(combination);
        }
    }
    if combinations.is_empty() {
        let kinds = kinds_of(wiphy.iftypes);
        if !kinds.is_empty() {
            combinations.push(Combination { limits: vec![Limit { kinds, max: 1 }] });
        }
    }
    combinations
}

fn push_limit(limits: &mut Vec<Limit>, kinds: Vec<Kind>, max: u32) {
    if max > 0 && !kinds.is_empty() {
        limits.push(Limit { kinds, max });
    }
}

/// The kinds that interfaces of `iftypes` can serve. P2P groups need a P2P client or GO
/// interface; the P2P device interface itself is wpa_supplicant's business.
fn kinds_of(iftypes: Iftypes) -> Vec<Kind> {
    let mut kinds = Vec::new();
    if iftypes.contains(Iftype::Station) {
        kinds.push(Kind::Sta);
    }
    if iftypes.contains(Iftype::Ap) {
        kinds.push(Kind::Ap);
    }
    if iftypes.contains(Iftype::P2pClient) || iftypes.contains(Iftype::P2pGo) {
        kinds.push(Kind::P2p);
    }
    kinds
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nl80211;

    fn iftypes(types: &[Iftype]) -> Iftypes {
        Iftypes::from_bits(types.iter().fold(0, |set, &iftype| set | 1 << iftype as u32))
    }

    fn nl_limit(max: u32, types: &[Iftype]) -> nl80211::Limit {
        nl80211::Limit { max, iftypes: iftypes(types) }
    }

    fn counts(sta: u32, ap: u32, p2p: u32) -> Counts {
        Counts { sta, ap, p2p }
    }

    /// iwlmvm: one STA, one AP or P2P group, one P2P device.
    fn iwlmvm() -> Wiphy {
        Wiphy {
            iftypes: iftypes(&[
                Iftype::Station,
                Iftype::Ap,
                Iftype::P2pClient,
                Iftype::P2pGo,
                Iftype::P2pDevice,
            ]),
            combinations: vec![nl80211::Combination {
                limits: vec![
                    nl_limit(1, &[Iftype::Station]),
                    nl_limit(1, &[Iftype::Ap, Iftype::P2pClient, Iftype::P2pGo]),
                    nl_limit(1, &[Iftype::P2pDevice]),
                ],
                max_interfaces: 3,
            }],
            ..Wiphy::default()
        }
    }

    #[test]
    fn iwlmvm_runs_sta_with_ap_or_p2p() {
        let combinations = from_wiphy(&iwlmvm());
        assert_eq!(
            combinations,
            [Combination {
                limits: vec![
                    Limit { kinds: vec![Kind::Sta], max: 1 },
                    Limit { kinds: vec![Kind::Ap, Kind::P2p], max: 1 },
                ]
            }]
        );
        assert!(any_allows(&combinations, counts(1, 1, 0)));
        assert!(any_allows(&combinations, counts(1, 0, 1)));
        assert!(!any_allows(&combinations, counts(1, 1, 1)));
        assert!(!any_allows(&combinations, counts(2, 0, 0)));
        assert!(any_allows(&combinations, Counts::default()));
    }

    #[test]
    fn hwsim_limits_are_capped_to_one_sta() {
        let wiphy = Wiphy {
            combinations: vec![nl80211::Combination {
                limits: vec![
                    nl_limit(
                        2048,
                        &[Iftype::Station, Iftype::Ap, Iftype::P2pClient, Iftype::P2pGo],
                    ),
                    nl_limit(1, &[Iftype::P2pDevice]),
                ],
                max_interfaces: 2048,
            }],
            ..Wiphy::default()
        };
        let combinations = from_wiphy(&wiphy);
        assert_eq!(
            combinations[0].limits,
            [
                Limit { kinds: vec![Kind::Sta], max: 1 },
                Limit { kinds: vec![Kind::Ap, Kind::P2p], max: 1 },
            ]
        );
        assert!(any_allows(&combinations, counts(1, 1, 0)));
        assert!(!any_allows(&combinations, counts(2, 0, 0)), "one STA at most");
        assert!(!any_allows(&combinations, counts(1, 1, 1)));
    }

    #[test]
    fn later_limits_drop_sta() {
        let wiphy = Wiphy {
            combinations: vec![nl80211::Combination {
                limits: vec![
                    nl_limit(2, &[Iftype::Station]),
                    nl_limit(1, &[Iftype::Station, Iftype::Ap]),
                ],
                max_interfaces: 3,
            }],
            ..Wiphy::default()
        };
        assert_eq!(
            from_wiphy(&wiphy)[0].limits,
            [Limit { kinds: vec![Kind::Sta], max: 1 }, Limit { kinds: vec![Kind::Ap], max: 1 }]
        );
    }

    #[test]
    fn single_interface_device() {
        let wiphy = Wiphy { iftypes: iftypes(&[Iftype::Station]), ..Wiphy::default() };
        let combinations = from_wiphy(&wiphy);
        assert_eq!(
            combinations,
            [Combination { limits: vec![Limit { kinds: vec![Kind::Sta], max: 1 }] }]
        );
        assert!(!any_allows(&combinations, counts(0, 1, 0)));
    }

    #[test]
    fn backtracks_over_shared_limits() {
        // The STA must take the STA-only limit for the AP to fit.
        let combination = Combination {
            limits: vec![
                Limit { kinds: vec![Kind::Sta, Kind::Ap], max: 1 },
                Limit { kinds: vec![Kind::Sta], max: 1 },
            ],
        };
        assert!(combination.allows(counts(1, 1, 0)));
        assert!(!combination.allows(counts(1, 0, 1)));
    }

    #[test]
    fn counts_add_up() {
        let counts = Counts::default().with(Kind::Sta).with(Kind::Sta).with(Kind::P2p);
        assert_eq!((counts.get(Kind::Sta), counts.get(Kind::Ap), counts.get(Kind::P2p)), (2, 0, 1));
    }
}
