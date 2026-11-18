/*
 * Copyright (C) 2012 The Android Open Source Project
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *      http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

// The sw_sync_* functions of system/core/libsync/sync.c for vendor modules: libsync is an
// LLNDK library whose stubs export only the sync_* API, so hwcomposer.waydroid links these
// statically instead of the PC's system libsync exporting more symbols.

#include <fcntl.h>
#include <string.h>
#include <sys/ioctl.h>

#include <linux/types.h>

struct sw_sync_create_fence_data {
    __u32 value;
    char name[32];
    __s32 fence;
};

#define SW_SYNC_IOC_MAGIC 'W'
#define SW_SYNC_IOC_CREATE_FENCE _IOWR(SW_SYNC_IOC_MAGIC, 0, struct sw_sync_create_fence_data)
#define SW_SYNC_IOC_INC _IOW(SW_SYNC_IOC_MAGIC, 1, __u32)

int sw_sync_timeline_create(void) {
    int ret = open("/sys/kernel/debug/sync/sw_sync", O_RDWR);
    if (ret < 0) ret = open("/dev/sw_sync", O_RDWR);
    return ret;
}

int sw_sync_timeline_inc(int fd, unsigned count) {
    __u32 arg = count;
    return ioctl(fd, SW_SYNC_IOC_INC, &arg);
}

int sw_sync_fence_create(int fd, const char* name, unsigned value) {
    struct sw_sync_create_fence_data data;
    data.value = value;
    strlcpy(data.name, name, sizeof(data.name));
    int err = ioctl(fd, SW_SYNC_IOC_CREATE_FENCE, &data);
    if (err < 0) return err;
    return data.fence;
}
