# Wi-Fi (E6)

How AOSP's Wi-Fi stack runs on the kernel: what the unmodified
Cuttlefish vendor image expects, the kernel's nl80211/cfg80211 layer, the
simulated radio used in QEMU, and the Intel AX211 driver for the real PC.

## 1. What the Cuttlefish image expects

Read from `device/google/cuttlefish` and `system/connectivity/wificond`
(android-16.0.0_r4, `aosp_cf_x86_64_only_phone`). No AOSP change is needed.

- **Interface**: `wifi.interface=wlan0`, `wifi.direct.interface=p2p-dev-wlan0`.
  Cuttlefish normally makes `wlan0` itself: `init.vendor.rc` runs
  `setup_wifi` (a `virt_wifi` link over `eth2`) only when
  `ro.boot.wifi_impl=virt_wifi`, and `init.wifi` (`mac80211_create_radios`,
  hwsim) only when `ro.boot.wifi_impl=mac80211_hwsim_virtio`. Our boot
  arguments set neither, so a `wlan0` the kernel registers is used as is.
  `rename_eth0` still runs and fails harmlessly (eth0 is up).
- **Wi-Fi HAL** (`com.android.hardware.wifi` APEX, `vendor.wifi_hal_legacy`):
  `BOARD_WLAN_DEVICE := emulator`, so the legacy HAL is goldfish's
  `libwifi-hal-emu`, which only uses rtnetlink (RTM_GETLINK on `wlan0`) and
  no nl80211 vendor commands. Driver loading is a no-op
  (`WIFI_DRIVER_FW_PATH_* = /dev/null`). With
  `config_wifi_connected_mac_randomization_supported=true` (the Cuttlefish
  overlay) the HAL sets a random MAC: link down, `SIOCSIFHWADDR`, link up.
- **wificond**: nl80211 GET_PROTOCOL_FEATURES, split GET_WIPHY dumps (it
  merges the messages per wiphy and requires FEATURE_FLAGS,
  MAX_NUM_SCAN_SSIDS, MAX_NUM_SCHED_SCAN_SSIDS, MAX_MATCH_SETS and
  WIPHY_BANDS), GET_INTERFACE dump, TRIGGER_SCAN / GET_SCAN (BSS needs
  BSSID, FREQUENCY, INFORMATION_ELEMENTS with an SSID element,
  LAST_SEEN_BOOTTIME or TSF, SIGNAL_MBM, CAPABILITY), GET_STATION
  (TX_PACKETS, TX_FAILED, SIGNAL), multicast groups "scan", "mlme",
  "regulatory". Scheduled scans (PNO) may fail.
- **wpa_supplicant** (`com.google.cf.wpa_supplicant` APEX, nl80211 driver,
  `lib_driver_cmd_simulated_cf_bp` for private commands): with
  AUTHENTICATE in SUPPORTED_COMMANDS it uses its own SME (AUTHENTICATE,
  ASSOCIATE and their events, SAE when FEATURE_SAE is set), keys with
  NEW_KEY, the port with SET_STATION(AUTHORIZED), EAPOL through an
  AF_PACKET socket on `wlan0`, REGISTER_FRAME / FRAME for action frames,
  `/dev/rfkill` if present.
- NetworkStack's IpClient needs `/proc/sys/net/ipv{4,6}/conf/wlan0/*`;
  wpa_supplicant's l2_packet needs the interface ioctls on its packet
  socket and a classic BPF filter reading `pkt_type`.
- hostapd (soft AP) is not supported (no AP iftype is advertised).

## 2. Kernel side

- `net/genl.rs`: generic netlink (`nlctrl` GETFAMILY, families with fixed
  group numbers below 32).
- `net/wireless/`: one wiphy per radio (`phyN`, station link `wlanN`, a
  link of the IP stack through `EthDevice`), BSS table, the station MLME
  (authentication with three 200 ms tries, (re)association request built
  from the AP's rates plus wpa_supplicant's IEs, deauth/disassoc either
  way), Ethernet ↔ 802.11 data frames, the controlled port (only EAPOL
  until authorized), keys passed to the driver. `nl80211.rs` has the
  commands and events listed in §1. Drivers implement `WifiDriver` and are
  polled from a soft interrupt every timer tick while running.
- `drivers/rfkill.rs`: `/dev/rfkill` and `/sys/class/rfkill` (generic,
  usable for Bluetooth too).
- `net/wireless/hwsim.rs`: a simulated radio with simulated access points
  (`wifisim=SSID[:PASSPHRASE][@CHANNEL],...`), which answer scans,
  authentication and association, run the WPA2-PSK 4-way handshake
  (`crypto.rs`: SHA-1, HMAC, PBKDF2, PRF, AES key wrap) and bridge data to
  a virtio-net card (`wifisim.uplink=BB:DD.F`).

`tools/run-qemu.sh --aosp-wifi-test` boots AOSP with an open and a
WPA2-Personal simulated AP and checks, through `cmd wifi`: Wi-Fi on,
scan results, a refused wrong passphrase, WPA2 connection, DHCP on
`wlan0` (QEMU's user network 10.0.3.0/24 behind the uplink) and a ping of
the gateway over `wlan0`.

## 3. Intel AX211 (`drivers/iwlwifi/`)

The PC's Wi-Fi is the CNVi AX211 (PCI 00:14.3, 8086:7a70, "so" MAC with
the "GF" RF). The host's Linux loads `iwlwifi-so-a0-gf-a0-89.ucode` (API
89, which uses the MLD command set) and `iwlwifi-so-a0-gf-a0.pnvm`;
`tools/mkusb.sh` decompresses both from `/lib/firmware/intel/iwlwifi`
into `boot/firmware/` on the stick, Limine loads them as modules and
`drivers/firmware.rs` finds them by name.

Done (following Linux's iwlwifi pcie/ and mvm/, not tested on hardware
yet):

- transport: ownership, reset, APM, context info v2 with the firmware
  sections, IML, RX ring of 4 KiB buffers, command queue, polled
  (CSR_INT causes, closed-RB index);
- firmware boot: ALIVE (status, SKU id, error table addresses), PNVM for
  the SKU, INIT_EXTENDED_CFG, NVM_ACCESS_COMPLETE, INIT_COMPLETE,
  NVM_GET_INFO (bands, chains; MAC from the CSR straps);
- runtime config: SoC latency, TX antennas, BT coexistence, MCC "ZZ"
  (LAR), scan config; MAC_CONFIG and LINK_CONFIG at interface up;
- UMAC scans (v17) with the probe request template; beacons and probe
  responses from RX MPDU notifications feed the BSS table, so Settings
  lists networks;
- firmware errors: the LMAC/UMAC error tables are dumped to the log.

- association and data, written from Linux's MLD mvm path but not yet
  run on the hardware: PHY context + RLC config, link activated,
  STA_CONFIG for the AP, one TX queue (TID 8) for management and non-QoS
  data, session protection while authenticating, TX_CMD v10 without a
  fixed rate (the firmware picks), after association MAC/LINK/STA
  updates and TLC with the AP's legacy rates, keys through SEC_KEY_CMD
  (the firmware encrypts and decrypts). Legacy rates only: no HT/VHT/HE,
  QoS or aggregation yet, so throughput is at most 54 Mb/s.

Log lines to look for on the boot screen (photograph them):
`iwlwifi: hw_rev … rf_id …`, `firmware: … from the boot media`,
`iwlwifi: firmware 89.…`, `iwlwifi: IML loaded`, `iwlwifi: ALIVE status
0xcafe …`, `iwlwifi: PNVM loaded`, `iwlwifi: NVM: …`, `iwlwifi: regulatory
…`, `iwlwifi: ready, address …`, `wifi: phy0 (iwlwifi) is wlan0`, and
after turning Wi-Fi on in Settings `wifi: wlan0 scan done (N BSS)` /
`iwlwifi: scan done (status …, N frames)`; when connecting
`iwlwifi: joining …`, `iwlwifi: joined: queue N, session protection …`,
`wifi: wlan0 auth reply … status 0`, `… assoc reply … status 0 aid N`,
`iwlwifi: associated`, `iwlwifi: pairwise key 0 installed`,
`wifi: wlan0 connected to …, port authorized`. On failure: `iwlwifi: no ALIVE
…` (with secure-boot CPU status), `FIRMWARE ERROR` and the two error
table lines.
