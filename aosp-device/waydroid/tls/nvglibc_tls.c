/*
 * Copyright 2026 The Android PC Authors. All rights reserved.
 * Use of this source code is governed by a BSD-style license that can be
 * found in the LICENSE file.
 */

/*
 * libnvglibc_tls.so: static TLS for NVIDIA's initial-exec TLS in Waydroid (docs/waydroid-nvidia.md).
 *
 * Android 13's bionic gives static TLS only to libraries loaded at process start. This library is
 * a DT_NEEDED of Waydroid's /system/lib64/libvulkan.so (patched in the overlay), so every process
 * that links libvulkan (zygote and its apps, SurfaceFlinger, gpuservice) has these bytes in every
 * thread's static TLS. nvglibc (hals/nvglibc/src/tls_surplus.c) places NVIDIA's TLS segments in
 * `area` and writes their initial images into this library's own image, which bionic copies into
 * each new thread.
 *
 * The block is .tdata, not .tbss, so that the image bionic copies covers all of `area`: only the
 * marker past it is non-zero, so threads that existed before nvglibc wrote the image see zeros.
 */

__attribute__((visibility("default"), aligned(64))) __thread struct {
	char area[512]; /* tls_surplus.c's SURPLUS_SIZE */
	char marker;
} nvglibc_tls_surplus = { .marker = 1 };
