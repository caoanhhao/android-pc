// Phase K4 (docs/nvidia-proprietary.md): display setup for the "NVIDIA (proprietary, Venus)"
// boot entry (androidboot.android_pc.gpu=nvidia), run by init.nvdisplay.rc at post-fs-data,
// before the composer and the allocator start (class hal, at boot).
//
// 1. Finds the nvidia-drm card (modeset=1) and points drm_hwcomposer at it
//    (vendor.hwc.drm.device), whatever its minor: Cuttlefish loads vkms in early-init, so the
//    NVIDIA card is not always card0 (Phase I1 had the same problem with nouveau).
// 2. Reads IN_FORMATS of every primary plane and publishes, per format, the modifiers that all
//    of them can scan out (vendor.gralloc.vtest.modifiers.<fourcc hex>.<n>, hex values). The
//    minigbm-vtest gralloc (K3) passes that list to the vtest server (K2), which allocates
//    NVIDIA block-linear images only with one of these modifiers. SurfaceFlinger's client
//    target therefore always gets a modifier nvidia-drm can put on the primary plane, and
//    drm_hwcomposer's drmModeAddFB2WithModifiers cannot fail on it. Overlay planes stay off,
//    so every layer is composed by SurfaceFlinger (ANGLE on Venus) into that buffer.
//
// Without an NVIDIA GPU (QEMU) or when nvidia-drm never shows up, it changes nothing: the
// composer keeps Cuttlefish's /dev/dri/card0 and gralloc gets no list (any modifier).
// vendor.pc.nvdisplay says what happened; the same line goes to the kernel log (pclog).

#define LOG_TAG "pcnvdisplay"

#include <android-base/properties.h>
#include <android-base/stringprintf.h>
#include <dirent.h>
#include <drm_fourcc.h>
#include <fcntl.h>
#include <inttypes.h>
#include <log/log.h>
#include <stdarg.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <xf86drm.h>
#include <xf86drmMode.h>

#include <algorithm>
#include <iterator>
#include <map>
#include <set>
#include <string>
#include <vector>

using android::base::SetProperty;
using android::base::StringPrintf;

namespace {

constexpr int kWaitSeconds = 30;
constexpr size_t kMaxModifiers = 64;  // minigbm-vtest's MAX_MODIFIERS
constexpr char kModifierProp[] = "vendor.gralloc.vtest.modifiers";

// The formats the vtest server allocates (waydroid-nvidia vrend-0002, vtest_vk_alloc.c).
constexpr uint32_t kFormats[] = {
        DRM_FORMAT_ABGR8888,    DRM_FORMAT_XBGR8888,    DRM_FORMAT_ARGB8888,
        DRM_FORMAT_XRGB8888,    DRM_FORMAT_RGB565,      DRM_FORMAT_ABGR2101010,
        DRM_FORMAT_XBGR2101010, DRM_FORMAT_ABGR16161616F, DRM_FORMAT_XBGR16161616F,
};

void Log(const char* fmt, ...) {
    char msg[512];
    va_list ap;
    va_start(ap, fmt);
    vsnprintf(msg, sizeof(msg), fmt, ap);
    va_end(ap);
    ALOGI("%s", msg);
    int kmsg = open("/dev/kmsg", O_WRONLY | O_CLOEXEC);
    if (kmsg >= 0) {
        dprintf(kmsg, "<6>pcnvdisplay: %s\n", msg);
        close(kmsg);
    }
}

std::string ReadFile(const std::string& path) {
    char buf[64] = {};
    int fd = open(path.c_str(), O_RDONLY | O_CLOEXEC);
    if (fd < 0) return {};
    ssize_t n = read(fd, buf, sizeof(buf) - 1);
    close(fd);
    if (n <= 0) return {};
    std::string s(buf, n);
    while (!s.empty() && (s.back() == '\n' || s.back() == ' ')) s.pop_back();
    return s;
}

// True when the PCI bus has an NVIDIA display controller (vendor 0x10de, class 0x03xxxx).
bool HasNvidiaGpu() {
    DIR* dir = opendir("/sys/bus/pci/devices");
    if (!dir) return false;
    bool found = false;
    while (dirent* e = readdir(dir)) {
        if (e->d_name[0] == '.') continue;
        std::string dev = std::string("/sys/bus/pci/devices/") + e->d_name;
        if (ReadFile(dev + "/vendor") == "0x10de" && ReadFile(dev + "/class").rfind("0x03", 0) == 0) {
            found = true;
            break;
        }
    }
    closedir(dir);
    return found;
}

// Opens /dev/dri/cardN of the nvidia-drm driver; returns the fd and sets `path`.
int OpenNvidiaCard(std::string* path) {
    for (int i = 0; i < 16; ++i) {
        std::string p = StringPrintf("/dev/dri/card%d", i);
        int fd = open(p.c_str(), O_RDWR | O_CLOEXEC);
        if (fd < 0) continue;
        // The first opener of a primary node becomes DRM master; drm_hwcomposer needs it.
        drmDropMaster(fd);
        drmVersionPtr v = drmGetVersion(fd);
        bool nvidia = v && v->name && strcmp(v->name, "nvidia-drm") == 0;
        drmFreeVersion(v);
        if (nvidia) {
            *path = p;
            return fd;
        }
        close(fd);
    }
    return -1;
}

uint64_t PropValue(int fd, drmModeObjectProperties* props, const char* name, bool* found) {
    *found = false;
    for (uint32_t i = 0; i < props->count_props; ++i) {
        drmModePropertyPtr p = drmModeGetProperty(fd, props->props[i]);
        if (!p) continue;
        bool match = strcmp(p->name, name) == 0;
        drmModeFreeProperty(p);
        if (match) {
            *found = true;
            return props->prop_values[i];
        }
    }
    return 0;
}

// Modifiers per format that every primary plane accepts (IN_FORMATS).
std::map<uint32_t, std::set<uint64_t>> PrimaryModifiers(int fd, int* primaries) {
    std::map<uint32_t, std::set<uint64_t>> common;
    *primaries = 0;
    drmModePlaneResPtr res = drmModeGetPlaneResources(fd);
    if (!res) return common;
    for (uint32_t i = 0; i < res->count_planes; ++i) {
        uint32_t id = res->planes[i];
        drmModeObjectProperties* props = drmModeObjectGetProperties(fd, id, DRM_MODE_OBJECT_PLANE);
        if (!props) continue;
        bool has_type = false, has_formats = false;
        uint64_t type = PropValue(fd, props, "type", &has_type);
        uint64_t blob_id = PropValue(fd, props, "IN_FORMATS", &has_formats);
        drmModeFreeObjectProperties(props);
        if (!has_type || type != DRM_PLANE_TYPE_PRIMARY) continue;

        std::map<uint32_t, std::set<uint64_t>> mine;
        drmModePropertyBlobPtr blob = has_formats ? drmModeGetPropertyBlob(fd, blob_id) : nullptr;
        if (blob) {
            drmModeFormatModifierIterator iter = {};
            while (drmModeFormatModifierBlobIterNext(blob, &iter)) mine[iter.fmt].insert(iter.mod);
            drmModeFreePropertyBlob(blob);
        } else {
            // No IN_FORMATS: the plane takes its formats with an implicit layout only.
            drmModePlanePtr plane = drmModeGetPlane(fd, id);
            if (plane) {
                for (uint32_t f = 0; f < plane->count_formats; ++f)
                    mine[plane->formats[f]].insert(DRM_FORMAT_MOD_LINEAR);
                drmModeFreePlane(plane);
            }
        }
        if (*primaries == 0) {
            common = std::move(mine);
        } else {
            for (auto it = common.begin(); it != common.end();) {
                auto other = mine.find(it->first);
                if (other == mine.end()) {
                    it = common.erase(it);
                    continue;
                }
                std::set<uint64_t> both;
                std::set_intersection(it->second.begin(), it->second.end(),
                                      other->second.begin(), other->second.end(),
                                      std::inserter(both, both.begin()));
                it->second = std::move(both);
                ++it;
            }
        }
        ++*primaries;
    }
    drmModeFreePlaneResources(res);
    return common;
}

// "DP-1 connected 2560x1080@144 (12 modes)" for each connector, for the log.
std::string Connectors(int fd) {
    std::string out;
    drmModeResPtr res = drmModeGetResources(fd);
    if (!res) return "no KMS resources";
    for (int i = 0; i < res->count_connectors; ++i) {
        drmModeConnectorPtr c = drmModeGetConnector(fd, res->connectors[i]);
        if (!c) continue;
        const char* type = drmModeGetConnectorTypeName(c->connector_type);
        out += StringPrintf("%s%s-%u %s", out.empty() ? "" : ", ", type ? type : "?",
                            c->connector_type_id,
                            c->connection == DRM_MODE_CONNECTED ? "connected" : "disconnected");
        for (int m = 0; m < c->count_modes; ++m) {
            if (c->modes[m].type & DRM_MODE_TYPE_PREFERRED) {
                out += StringPrintf(" %ux%u@%u", c->modes[m].hdisplay, c->modes[m].vdisplay,
                                    c->modes[m].vrefresh);
                break;
            }
        }
        drmModeFreeConnector(c);
    }
    drmModeFreeResources(res);
    return out;
}

}  // namespace

int main() {
    if (!HasNvidiaGpu()) {
        SetProperty("vendor.pc.nvdisplay", "none: no NVIDIA GPU");
        Log("no NVIDIA GPU, composer stays on %s",
            android::base::GetProperty("vendor.hwc.drm.device", "?").c_str());
        return 0;
    }
    std::string card;
    int fd = -1;
    for (int t = 0; t < kWaitSeconds * 2 && fd < 0; ++t) {
        fd = OpenNvidiaCard(&card);
        if (fd < 0) usleep(500 * 1000);
    }
    if (fd < 0) {
        SetProperty("vendor.pc.nvdisplay", "none: no nvidia-drm card");
        Log("no nvidia-drm card after %d s, composer stays on %s", kWaitSeconds,
            android::base::GetProperty("vendor.hwc.drm.device", "?").c_str());
        return 0;
    }
    drmSetClientCap(fd, DRM_CLIENT_CAP_UNIVERSAL_PLANES, 1);
    drmSetClientCap(fd, DRM_CLIENT_CAP_ATOMIC, 1);
    uint64_t has_modifiers = 0;
    drmGetCap(fd, DRM_CAP_ADDFB2_MODIFIERS, &has_modifiers);

    SetProperty("vendor.hwc.drm.device", card);
    Log("%s is nvidia-drm, drm_hwcomposer uses it; ADDFB2_MODIFIERS %d; %s", card.c_str(),
        static_cast<int>(has_modifiers), Connectors(fd).c_str());

    int primaries = 0;
    auto modifiers = PrimaryModifiers(fd, &primaries);
    std::string summary;
    for (uint32_t format : kFormats) {
        auto it = modifiers.find(format);
        std::vector<uint64_t> mods;
        if (it != modifiers.end()) mods.assign(it->second.begin(), it->second.end());
        if (mods.size() > kMaxModifiers) mods.resize(kMaxModifiers);
        std::string list;
        for (size_t i = 0; i < mods.size(); ++i) {
            SetProperty(StringPrintf("%s.%x.%zu", kModifierProp, format, i),
                        StringPrintf("%" PRIx64, mods[i]));
            list += StringPrintf("%s%" PRIx64, list.empty() ? "" : " ", mods[i]);
        }
        char fourcc[5] = {static_cast<char>(format), static_cast<char>(format >> 8),
                          static_cast<char>(format >> 16), static_cast<char>(format >> 24), 0};
        Log("%s: %zu scanout modifiers on %d primary planes: %s", fourcc, mods.size(), primaries,
            list.c_str());
        summary += StringPrintf("%s%s:%zu", summary.empty() ? "" : " ", fourcc, mods.size());
    }
    SetProperty("vendor.pc.nvdisplay", card + " " + summary);
    close(fd);
    return 0;
}
