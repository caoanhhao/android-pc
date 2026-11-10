//! USB hubs (class 9), set up once at boot.
//!
//! The hub's slot context is marked as a hub (port count, and the TT
//! think time of a high-speed hub) with Configure Endpoint; a SuperSpeed
//! hub is told its depth (SET_HUB_DEPTH). Every port is powered, and
//! after the power-on delay plus 100 ms of debounce each connected one
//! is reset (USB2; a SuperSpeed port enables itself once its link
//! trained, and is reset only if it does not) and the device behind it
//! enumerated with the route string extended by the port number. Low and
//! full-speed devices behind a high-speed hub go through that hub's
//! transaction translator. The status-change endpoint is not used (no
//! hot-plug).

use alloc::format;
use alloc::vec::Vec;

use super::{SPEED_FULL, SPEED_HIGH, SPEED_LOW, SPEED_SUPER, TRB_CONFIGURE_EP, Topo, UsbDev, Xhci, settle, wait_until};

// Hub class requests and features.
const GET_STATUS: u8 = 0;
const CLEAR_FEATURE: u8 = 1;
const SET_FEATURE: u8 = 3;
const GET_DESCRIPTOR: u8 = 6;
const SET_HUB_DEPTH: u8 = 12;
const PORT_RESET: u16 = 4;
const PORT_POWER: u16 = 8;
const C_PORT_CONNECTION: u16 = 16;
const C_PORT_ENABLE: u16 = 17;
const C_PORT_RESET: u16 = 20;
const C_PORT_LINK_STATE: u16 = 25;
const C_BH_PORT_RESET: u16 = 29;

const ST_CONNECTION: u16 = 1 << 0;
const ST_ENABLE: u16 = 1 << 1;
const ST_LOW_SPEED: u16 = 1 << 9;
const ST_HIGH_SPEED: u16 = 1 << 10;
const CH_RESET: u16 = 1 << 4;

/// Tiers of hubs below a root port (the USB limit).
const MAX_DEPTH: u32 = 5;

impl Xhci {
    fn port_status(&mut self, d: &mut UsbDev, port: u32) -> Option<(u16, u16)> {
        if !self.control(d, 0xa3, GET_STATUS, 0, port as u16, 4) {
            return None;
        }
        let b = unsafe { core::slice::from_raw_parts(self.data.1, 4) };
        Some((u16::from_le_bytes([b[0], b[1]]), u16::from_le_bytes([b[2], b[3]])))
    }

    fn port_feature(&mut self, d: &mut UsbDev, set: bool, feature: u16, port: u32) -> bool {
        self.control_quiet(d, 0x23, if set { SET_FEATURE } else { CLEAR_FEATURE }, feature, port as u16, 0, !set)
    }

    /// Reset `port` and wait for the reset to complete; the port status.
    fn port_reset(&mut self, d: &mut UsbDev, port: u32) -> Option<u16> {
        if !self.port_feature(d, true, PORT_RESET, port) {
            return None;
        }
        let mut st = None;
        wait_until(500, || {
            wait_until(10, || false);
            match self.port_status(d, port) {
                Some((s, c)) if c & CH_RESET != 0 => {
                    st = Some(s);
                    true
                }
                _ => false,
            }
        });
        self.port_feature(d, false, C_PORT_RESET, port);
        // Reset recovery (TRSTRCY).
        wait_until(10, || false);
        st
    }

    /// A configured hub: mark its slot, then enumerate what its ports have.
    pub(super) fn hub(&mut self, mut d: UsbDev, usbdev: &str) {
        let name = format!("usb{}-{}", self.bus, d.topo.path);
        if d.topo.depth >= MAX_DEPTH {
            log::warn!("xhci: {name}: hub too deep, ignored");
            return;
        }
        let ss = d.speed >= SPEED_SUPER;
        let ty: u16 = if ss { 0x2a } else { 0x29 };
        if !self.control(&mut d, 0xa0, GET_DESCRIPTOR, ty << 8, 0, 12) {
            log::warn!("xhci: {name}: no hub descriptor");
            return;
        }
        let h = unsafe { core::slice::from_raw_parts(self.data.1, 12) };
        let ports = h[2] as u32;
        let chars = u16::from_le_bytes([h[3], h[4]]);
        let power_ms = (h[5] as u64 * 2).max(20);
        let ttt = if d.speed == SPEED_HIGH { ((chars >> 5) & 3) as u32 } else { 0 };
        d.hub = Some((ports, ttt));
        self.input_ctx(&d, 1);
        self.slot_ctx(&d, 1);
        let input = d.input.0;
        if let Err(code) = self.command_code([input as u32, (input >> 32) as u32, 0, TRB_CONFIGURE_EP << 10 | d.slot << 24]) {
            log::warn!("xhci: {name}: cannot mark the slot as a hub (completion code {code}); low/full-speed devices behind it may fail");
        }
        let depth = d.topo.depth as u16;
        if ss && !self.control(&mut d, 0x20, SET_HUB_DEPTH, depth, 0, 0) {
            log::warn!("xhci: {name}: SET_HUB_DEPTH failed");
        }
        log::info!(
            "xhci: {name}: {} hub, {ports} ports, TT think time {ttt}, power-on {power_ms} ms",
            super::speed_name(d.speed)
        );
        crate::fs::sysfs::set(usbdev, "maxchild", &format!("{ports}\n"));
        for p in 1..=ports {
            self.port_feature(&mut d, true, PORT_POWER, p);
        }
        // Devices connect some time after power: wait until no port's
        // connection moved for 300 ms.
        let settled = settle(power_ms + 100, power_ms + 2000, 300, || {
            (1..=ports).map(|p| self.port_status(&mut d, p).map_or(0, |s| s.0 & (ST_CONNECTION | ST_ENABLE))).collect::<Vec<u16>>()
        });
        if !settled {
            log::warn!("xhci: {name}: ports still changing after {} ms", power_ms + 2000);
        }
        for p in 1..=ports {
            let Some((st0, ch)) = self.port_status(&mut d, p) else {
                log::warn!("xhci: {name}: port {p}: no status");
                continue;
            };
            if st0 & ST_CONNECTION == 0 {
                continue;
            }
            log::info!("xhci: {name}: port {p}: connected, status {st0:#06x} change {ch:#06x}");
            if p > 15 {
                log::warn!("xhci: {name}: port {p}: beyond the route string's reach, skipped");
                continue;
            }
            for attempt in 0..3 {
                let Some(st) = self.hub_port_enable(&mut d, p, ss, attempt > 0) else {
                    continue;
                };
                let speed = if ss {
                    d.speed
                } else if st & ST_LOW_SPEED != 0 {
                    SPEED_LOW
                } else if st & ST_HIGH_SPEED != 0 {
                    SPEED_HIGH
                } else {
                    SPEED_FULL
                };
                let topo = Topo {
                    port: d.topo.port,
                    route: d.topo.route | p << (4 * d.topo.depth),
                    depth: d.topo.depth + 1,
                    tt: child_tt(d.speed, d.slot, d.topo.tt, p, speed),
                    path: format!("{}.{p}", d.topo.path),
                };
                if self.enumerate(topo, speed) {
                    break;
                }
                log::warn!("xhci: {name}: port {p}: enumeration failed (try {}), resetting the port", attempt + 1);
            }
        }
    }

    /// Enable a connected hub port (reset it; a SuperSpeed port only if
    /// its link does not enable itself, unless `force`): its status.
    fn hub_port_enable(&mut self, d: &mut UsbDev, p: u32, ss: bool, force: bool) -> Option<u16> {
        let name = format!("usb{}-{}", self.bus, d.topo.path);
        let mut st = self.port_status(d, p)?.0;
        if ss {
            if st & ST_ENABLE == 0 && !force {
                let mut now = st;
                wait_until(500, || {
                    wait_until(10, || false);
                    now = self.port_status(d, p).map_or(now, |s| s.0);
                    now & ST_ENABLE != 0
                });
                st = now;
            }
            if st & ST_ENABLE == 0 || force {
                st = self.port_reset(d, p).unwrap_or(0);
            }
            for f in [C_PORT_CONNECTION, C_PORT_LINK_STATE, C_BH_PORT_RESET] {
                self.port_feature(d, false, f, p);
            }
        } else {
            st = self.port_reset(d, p).unwrap_or(0);
            for f in [C_PORT_CONNECTION, C_PORT_ENABLE] {
                self.port_feature(d, false, f, p);
            }
        }
        if st & ST_ENABLE == 0 {
            log::warn!("xhci: {name}: port {p}: not enabled after reset ({st:#x})");
            return None;
        }
        Some(st)
    }
}

/// The transaction translator for a device of `speed` on port `port` of
/// a hub (`hub_speed`, slot `hub_slot`, itself behind `hub_tt`): a
/// low/full-speed device behind a high-speed hub uses that hub's TT; one
/// behind a full-speed hub uses the TT that hub sits behind.
pub(super) fn child_tt(hub_speed: u32, hub_slot: u32, hub_tt: Option<(u32, u32)>, port: u32, speed: u32) -> Option<(u32, u32)> {
    match speed {
        SPEED_LOW | SPEED_FULL if hub_speed == SPEED_HIGH => Some((hub_slot, port)),
        SPEED_LOW | SPEED_FULL => hub_tt,
        _ => None,
    }
}
