/*
 * Copyright 2026 The Android PC Authors. All rights reserved.
 * Use of this source code is governed by a BSD-style license that can be
 * found in the LICENSE file.
 */

/*
 * Waydroid's composer with libnativewindow.so and libEGL.so preloaded (docs/waydroid-nvidia.md).
 *
 * Installed over /vendor/bin/hw/android.hardware.graphics.composer@2.1-service, with the
 * vendor image's binary beside it as ...-service.real (an init "override" of the service is not
 * applied in Waydroid). It execs the real composer with LD_PRELOAD set; preloaded libraries count
 * as children of the executable and are loaded at process start:
 *   - libnativewindow.so: Android 13's ANGLE finds AHardwareBuffer_* with
 *     dlsym(dlopen(NULL)), which searches only the executable's tree; the composer gets
 *     libnativewindow only later, with hwcomposer.waydroid.so (from ~/projects/waydroid-nvidia,
 *     venus/notes-cursor.md).
 *   - libEGL.so: it needs libnvglibc_tls.so (tools/waydroid/nvidia-build.sh), whose static TLS
 *     nvglibc places NVIDIA's initial-exec TLS in; loaded later with the HAL module it would be
 *     dynamic TLS, NVIDIA's driver would not load, and the composer's EGL, which copies every
 *     mouse cursor image into a wl_shm buffer (egl_render_to_pixels), would fail: no cursor.
 * Both are LL-NDK, so a vendor process may preload them.
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#define REAL "/vendor/bin/hw/android.hardware.graphics.composer@2.1-service.real"
#define PRELOAD "libnativewindow.so:libEGL.so"

int main(int argc, char **argv)
{
	const char *old = getenv("LD_PRELOAD");

	(void)argc;
	if (old && *old) {
		char *both = malloc(strlen(PRELOAD) + 1 + strlen(old) + 1);

		if (both) {
			sprintf(both, "%s:%s", PRELOAD, old);
			setenv("LD_PRELOAD", both, 1);
		}
	} else {
		setenv("LD_PRELOAD", PRELOAD, 1);
	}

	/* argv[0] stays the service path, so pidof and init still match it */
	execv(REAL, argv);
	perror("composer-preload-wrapper: execv " REAL);
	return 127;
}
