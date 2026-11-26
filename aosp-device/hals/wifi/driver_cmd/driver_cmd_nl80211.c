// SPDX-License-Identifier: BSD-3-Clause
/*
 * Private driver command library for wpa_supplicant (BOARD_WPA_SUPPLICANT_PRIVATE_LIB).
 *
 * Without one, wpa_supplicant is built with ANDROID_LIB_STUB and has no driver_cmd at all,
 * so ISupplicantStaIface.setCountryCode ("COUNTRY xx") fails with FAILURE_UNKNOWN and the
 * framework's Wi-Fi country code stays null (cmd wifi get-country-code). Upstream drivers
 * (ath11k, iwlwifi, mt76) need no vendor commands: COUNTRY becomes the regular nl80211
 * regulatory hint, MACADDR reads the interface address. Everything else is reported as
 * unsupported, as it was with the stub.
 */

#include <linux/if_ether.h>

#include "includes.h"

#include "common.h"
#include "driver.h"
#include "driver_nl80211.h"
#include "linux_ioctl.h"

int wpa_driver_nl80211_driver_cmd(void *priv, char *cmd, char *buf, size_t buf_len)
{
	struct i802_bss *bss = priv;
	struct wpa_driver_nl80211_data *drv = bss->drv;

	if (os_strncasecmp(cmd, "COUNTRY ", 8) == 0) {
		const char *alpha2 = cmd + 8;

		if (os_strlen(alpha2) != 2)
			return -1;
		/* NL80211_CMD_REQ_SET_REG, the same request "country=" in the config makes */
		return wpa_driver_nl80211_ops.set_country(priv, alpha2) ? -1 : 0;
	}

	if (os_strcasecmp(cmd, "MACADDR") == 0) {
		u8 addr[ETH_ALEN];

		if (linux_get_ifhwaddr(drv->global->ioctl_sock, bss->ifname, addr))
			return -1;
		return os_snprintf(buf, buf_len, "Macaddr = " MACSTR "\n", MAC2STR(addr));
	}

	wpa_printf(MSG_DEBUG, "nl80211: unsupported driver command '%s'", cmd);
	return -1;
}

/* The P2P hooks the stub library provided (driver_nl80211_android.c, ANDROID_LIB_STUB). */

int wpa_driver_set_p2p_noa(void *priv, u8 count, int start, int duration)
{
	return 0;
}

int wpa_driver_get_p2p_noa(void *priv, u8 *buf, size_t len)
{
	return 0;
}

int wpa_driver_set_p2p_ps(void *priv, int legacy_ps, int opp_ps, int ctwindow)
{
	return -1;
}

int wpa_driver_set_ap_wps_p2p_ie(void *priv, const struct wpabuf *beacon,
				 const struct wpabuf *proberesp,
				 const struct wpabuf *assocresp)
{
	return 0;
}
