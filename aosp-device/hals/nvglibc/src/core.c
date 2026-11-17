// Phase L2 (docs/nvidia-native.md): libnvglibc.so, the core of nvglibc.
//
// NVIDIA's userspace driver is built for glibc. nvglibc loads it with bionic's own linker:
//  - glibc's sonames are small trampoline libraries (libc.so.6 = libnvglibc_c.so, ...) in
//    <dir of libnvglibc.so>/nvglibc/, loaded once; a glibc library's DT_NEEDED and verneed
//    entries match them by soname, and their unversioned symbols satisfy GLIBC_2.x imports.
//  - Most glibc functions have bionic's ABI on x86_64 and jump straight to bionic; the rest
//    (pthread mutexes/condition variables, sigaction, sysconf, stat, locales, dl*) are
//    translated here (pthread.c, libc.c).
//  - A driver library is loaded by path after the glibc libraries it needs, leaves first, so
//    bionic finds every DT_NEEDED already loaded by soname: no linker namespace is needed, which
//    keeps this usable from vendor code (libdl_android is platform-only).
//  - Its initial-exec TLS (libnvidia-tls, DF_STATIC_TLS) lives in bionic's static TLS surplus
//    (tools/patches/aosp17-bionic-static-tls-surplus.patch).

#define _GNU_SOURCE
#include <android/log.h>
#include <dlfcn.h>
#include <elf.h>
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <link.h>
#include <pthread.h>
#include <stdarg.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include "nvglibc.h"
#include "nvglibc_private.h"

#define MAX_DEPTH 16
#define RTLD_DEEPBIND 0x8  // glibc only; dropped

static void* g_libc;
static void* g_libm;
static void* g_libdl;
static void* g_main;       // dlopen(NULL)
static void* g_stub_libc;  // libc.so.6: glibc's names, for glibc's dlsym(RTLD_DEFAULT, ...)
static char g_stub_dir[PATH_MAX];
static char g_driver_path[4096];
static bool g_debug;

static pthread_mutex_t g_lock = PTHREAD_RECURSIVE_MUTEX_INITIALIZER_NP;
static pthread_once_t g_once = PTHREAD_ONCE_INIT;
static bool g_ready;

// Everything loaded through nvglibc, by soname.
struct loaded {
    char* soname;
    char* path;
    void* handle;
};
static struct loaded* g_loaded;
static size_t g_n_loaded;

static __thread char* t_error;        // nvglibc_dlerror()
static __thread char* t_glibc_error;  // glibc's dlerror(): returned once, then cleared
static __thread char t_glibc_error_buf[512];

void nvg_log(int prio, const char* fmt, ...) {
    if (prio < ANDROID_LOG_WARN && !g_debug) return;
    char msg[1024];
    va_list ap;
    va_start(ap, fmt);
    vsnprintf(msg, sizeof(msg), fmt, ap);
    va_end(ap);
    __android_log_write(prio, "nvglibc", msg);
    if (prio >= ANDROID_LOG_WARN || g_debug) fprintf(stderr, "nvglibc: %s\n", msg);
}

static void set_error(const char* fmt, ...) {
    char msg[1024];
    va_list ap;
    va_start(ap, fmt);
    vsnprintf(msg, sizeof(msg), fmt, ap);
    va_end(ap);
    free(t_error);
    t_error = strdup(msg);
    free(t_glibc_error);
    t_glibc_error = strdup(msg);
    nvg_log(ANDROID_LOG_DEBUG, "%s", msg);
}

__attribute__((constructor(101))) static void core_init(void) {
    const char* d = getenv("NVGLIBC_DEBUG");
    g_debug = d && *d && strcmp(d, "0") != 0;
    g_libc = dlopen("libc.so", RTLD_NOW | RTLD_NOLOAD);
    g_libm = dlopen("libm.so", RTLD_NOW | RTLD_NOLOAD);
    g_libdl = dlopen("libdl.so", RTLD_NOW | RTLD_NOLOAD);
    g_main = dlopen(NULL, RTLD_NOW);
    if (!g_libm) g_libm = dlopen("libm.so", RTLD_NOW);

    Dl_info info;
    if (dladdr((void*)core_init, &info) && info.dli_fname) {
        snprintf(g_stub_dir, sizeof(g_stub_dir), "%s", info.dli_fname);
        char* slash = strrchr(g_stub_dir, '/');
        if (slash) *slash = '\0';
        strlcat(g_stub_dir, "/nvglibc", sizeof(g_stub_dir));
    }
    const char* p = getenv("NVGLIBC_DRIVER_PATH");
    snprintf(g_driver_path, sizeof(g_driver_path), "%s", p && *p ? p : NVGLIBC_DEFAULT_DRIVER_PATH);
    nvg_log(ANDROID_LOG_DEBUG, "core at %s, stubs in %s, drivers in %s", info.dli_fname,
            g_stub_dir, g_driver_path);

    // Without an X display NVIDIA's Vulkan driver starts through EGL: GLVND's libEGL.so.1 reads
    // the vendor JSON (library_path libEGL_nvidia.so.0) in the driver directory (the device's
    // /vendor/lib64/nvidia) or, in a glibc root, under the directory beside it.
    if (!getenv("__EGL_VENDOR_LIBRARY_FILENAMES") && !getenv("__EGL_VENDOR_LIBRARY_DIRS")) {
        static const char* const kJsons[] = {"/10_nvidia.json",
                                             "/../share/glvnd/egl_vendor.d/10_nvidia.json"};
        for (const char* p = g_driver_path; *p;) {
            const char* e = strchr(p, ':');
            size_t len = e ? (size_t)(e - p) : strlen(p);
            char json[PATH_MAX];
            bool found = false;
            for (size_t i = 0; i < sizeof(kJsons) / sizeof(kJsons[0]) && !found; ++i) {
                snprintf(json, sizeof(json), "%.*s%s", (int)len, p, kJsons[i]);
                found = access(json, R_OK) == 0;
            }
            if (found) {
                setenv("__EGL_VENDOR_LIBRARY_FILENAMES", json, 0);
                break;
            }
            p += len + (e ? 1 : 0);
        }
    }
}

// ---- ELF dynamic section of a file on disk (DT_SONAME, DT_NEEDED) ----

struct dyninfo {
    char* soname;
    char* needed[64];
    size_t n_needed;
};

static void dyninfo_free(struct dyninfo* di) {
    free(di->soname);
    for (size_t i = 0; i < di->n_needed; i++) free(di->needed[i]);
}

static bool read_dyninfo(const char* path, struct dyninfo* di) {
    memset(di, 0, sizeof(*di));
    int fd = open(path, O_RDONLY | O_CLOEXEC);
    if (fd < 0) {
        set_error("%s: %s", path, strerror(errno));
        return false;
    }
    bool ok = false;
    Elf64_Phdr* ph = NULL;
    Elf64_Dyn* dyn = NULL;
    char* strtab = NULL;
    Elf64_Ehdr eh;
    if (pread(fd, &eh, sizeof(eh), 0) != sizeof(eh) || memcmp(eh.e_ident, ELFMAG, SELFMAG) != 0 ||
        eh.e_ident[EI_CLASS] != ELFCLASS64 || eh.e_machine != EM_X86_64 ||
        eh.e_phentsize != sizeof(Elf64_Phdr) || eh.e_phnum == 0 || eh.e_phnum > 64) {
        set_error("%s: not an x86_64 ELF library", path);
        goto out;
    }
    ph = calloc(eh.e_phnum, sizeof(*ph));
    size_t phsz = eh.e_phnum * sizeof(*ph);
    if (pread(fd, ph, phsz, eh.e_phoff) != (ssize_t)phsz) goto bad;
    const Elf64_Phdr* pdyn = NULL;
    for (int i = 0; i < eh.e_phnum; i++) {
        if (ph[i].p_type == PT_DYNAMIC) pdyn = &ph[i];
    }
    if (!pdyn || pdyn->p_filesz > (1 << 20)) goto bad;
    dyn = malloc(pdyn->p_filesz);
    if (pread(fd, dyn, pdyn->p_filesz, pdyn->p_offset) != (ssize_t)pdyn->p_filesz) goto bad;
    size_t n_dyn = pdyn->p_filesz / sizeof(*dyn);
    Elf64_Addr strtab_va = 0;
    Elf64_Xword strsz = 0;
    for (size_t i = 0; i < n_dyn && dyn[i].d_tag != DT_NULL; i++) {
        if (dyn[i].d_tag == DT_STRTAB) strtab_va = dyn[i].d_un.d_ptr;
        if (dyn[i].d_tag == DT_STRSZ) strsz = dyn[i].d_un.d_val;
    }
    if (!strtab_va || !strsz || strsz > (16 << 20)) goto bad;
    off_t strtab_off = -1;
    for (int i = 0; i < eh.e_phnum; i++) {
        if (ph[i].p_type == PT_LOAD && strtab_va >= ph[i].p_vaddr &&
            strtab_va + strsz <= ph[i].p_vaddr + ph[i].p_filesz) {
            strtab_off = strtab_va - ph[i].p_vaddr + ph[i].p_offset;
        }
    }
    if (strtab_off < 0) goto bad;
    strtab = malloc(strsz + 1);
    if (pread(fd, strtab, strsz, strtab_off) != (ssize_t)strsz) goto bad;
    strtab[strsz] = '\0';
    for (size_t i = 0; i < n_dyn && dyn[i].d_tag != DT_NULL; i++) {
        if (dyn[i].d_un.d_val >= strsz) continue;
        const char* s = strtab + dyn[i].d_un.d_val;
        if (dyn[i].d_tag == DT_SONAME) {
            di->soname = strdup(s);
        } else if (dyn[i].d_tag == DT_NEEDED && di->n_needed < 64) {
            di->needed[di->n_needed++] = strdup(s);
        }
    }
    ok = true;
    goto out;
bad:
    set_error("%s: cannot read its dynamic section", path);
out:
    free(ph);
    free(dyn);
    free(strtab);
    close(fd);
    return ok;
}

// ---- the table of loaded libraries ----

static struct loaded* find_soname(const char* soname) {
    for (size_t i = 0; i < g_n_loaded; i++) {
        if (strcmp(g_loaded[i].soname, soname) == 0) return &g_loaded[i];
    }
    return NULL;
}

static struct loaded* find_handle(void* handle) {
    for (size_t i = 0; i < g_n_loaded; i++) {
        if (g_loaded[i].handle == handle) return &g_loaded[i];
    }
    return NULL;
}

static void add_loaded(const char* soname, const char* path, void* handle) {
    g_loaded = realloc(g_loaded, (g_n_loaded + 1) * sizeof(*g_loaded));
    g_loaded[g_n_loaded++] = (struct loaded){strdup(soname), strdup(path), handle};
    nvg_log(ANDROID_LOG_DEBUG, "loaded %s (%s) = %p", soname, path, handle);
}

static const char* base_name(const char* path) {
    const char* s = strrchr(path, '/');
    return s ? s + 1 : path;
}

// ---- glibc sonames: nvglibc's trampoline libraries ----

static const struct {
    const char* soname;
    const char* file;
} kStubs[] = {
    {"libc.so.6", "libnvglibc_c.so"},  // first: binds the forwards
    {"libpthread.so.0", "libnvglibc_pthread.so"},
    {"libdl.so.2", "libnvglibc_dl.so"},
    {"libm.so.6", "libnvglibc_m.so"},
    {"librt.so.1", "libnvglibc_rt.so"},
    {"ld-linux-x86-64.so.2", "libnvglibc_ld.so"},
    {"libgcc_s.so.1", "libnvglibc_gcc_s.so"},
    {"libX11.so.6", "libnvglibc_x11.so"},
    {"libXext.so.6", "libnvglibc_xext.so"},
};

// glibc libraries that bionic provides with the same C ABI.
static const struct {
    const char* glibc;
    const char* bionic;
} kBionicLibs[] = {
    {"libdrm.so.2", "libdrm.so"},
};

static void load_stubs(void) {
    for (size_t i = 0; i < sizeof(kStubs) / sizeof(kStubs[0]); i++) {
        char path[PATH_MAX];
        snprintf(path, sizeof(path), "%s/%s", g_stub_dir, kStubs[i].file);
        void* h = dlopen(path, RTLD_NOW | RTLD_LOCAL | RTLD_NODELETE);
        if (!h) {
            nvg_log(ANDROID_LOG_ERROR, "cannot load %s: %s", path, dlerror());
            return;
        }
        add_loaded(kStubs[i].soname, path, h);
        if (i == 0) g_stub_libc = h;
    }
    g_ready = true;
}

static void init_once(void) {
    pthread_mutex_lock(&g_lock);
    load_stubs();
    pthread_mutex_unlock(&g_lock);
}

static bool ensure_init(void) {
    pthread_once(&g_once, init_once);
    if (!g_ready) set_error("nvglibc's glibc sonames are not loaded (see logcat -s nvglibc)");
    return g_ready;
}

// Driver libraries: only NVIDIA's own, GLVND's libEGL/libGLdispatch (NVIDIA's Vulkan driver
// starts through EGL) and the extra names in $NVGLIBC_ALLOW are loaded from the driver
// directories, so a glibc library that the driver only dlopen()s optionally (libxcb, libdbus,
// the EGL platform libraries for Wayland/X11/GBM, ...) is reported as missing instead.
static bool driver_allowed(const char* name) {
    if (strncmp(name, "libnvidia-egl-", 14) == 0) return false;
    if (strncmp(name, "libnvidia-", 10) == 0 || strstr(name, "_nvidia.so")) return true;
    if (strcmp(name, "libEGL.so.1") == 0 || strcmp(name, "libGLdispatch.so.0") == 0) return true;
    const char* allow = getenv("NVGLIBC_ALLOW");
    if (!allow) return false;
    size_t n = strlen(name);
    for (const char* p = allow; *p;) {
        const char* e = strchr(p, ':');
        size_t len = e ? (size_t)(e - p) : strlen(p);
        if (len == n && strncmp(p, name, n) == 0) return true;
        p += len + (e ? 1 : 0);
    }
    return false;
}

static bool find_driver_file(const char* name, char* out, size_t out_len) {
    for (const char* p = g_driver_path; *p;) {
        const char* e = strchr(p, ':');
        size_t len = e ? (size_t)(e - p) : strlen(p);
        snprintf(out, out_len, "%.*s/%s", (int)len, p, name);
        if (access(out, R_OK) == 0) return true;
        p += len + (e ? 1 : 0);
    }
    return false;
}

static void* load_name(const char* name, int flags, int depth);

static void* load_path(const char* path, int flags, int depth) {
    if (depth > MAX_DEPTH) {
        set_error("%s: DT_NEEDED nesting too deep", path);
        return NULL;
    }
    struct dyninfo di;
    if (!read_dyninfo(path, &di)) return NULL;
    const char* soname = di.soname ? di.soname : base_name(path);
    struct loaded* l = find_soname(soname);
    void* h = NULL;
    if (l) {
        h = l->handle;
        goto out;
    }
    for (size_t i = 0; i < di.n_needed; i++) {
        if (!load_name(di.needed[i], RTLD_NOW | RTLD_LOCAL, depth + 1)) {
            char* why = t_error ? strdup(t_error) : NULL;
            set_error("%s needs %s: %s", soname, di.needed[i], why ? why : "?");
            free(why);
            goto out;
        }
    }
    h = dlopen(path, (flags & ~RTLD_DEEPBIND) | RTLD_NODELETE);
    if (!h) {
        set_error("%s", dlerror());
        goto out;
    }
    add_loaded(soname, path, h);
out:
    dyninfo_free(&di);
    return h;
}

static void* load_name(const char* name, int flags, int depth) {
    struct loaded* l = find_soname(base_name(name));
    if (l) return l->handle;
    if (flags & RTLD_NOLOAD) {
        set_error("%s: not loaded", name);
        return NULL;
    }
    if (strchr(name, '/')) return load_path(name, flags, depth);
    for (size_t i = 0; i < sizeof(kBionicLibs) / sizeof(kBionicLibs[0]); i++) {
        if (strcmp(name, kBionicLibs[i].glibc) == 0) {
            void* h = dlopen(kBionicLibs[i].bionic, flags & ~RTLD_DEEPBIND);
            if (!h) set_error("%s (bionic %s): %s", name, kBionicLibs[i].bionic, dlerror());
            return h;
        }
    }
    char path[PATH_MAX];
    if (driver_allowed(name) && find_driver_file(name, path, sizeof(path))) {
        return load_path(path, flags, depth);
    }
    set_error("%s: not provided by nvglibc or the driver directories (%s)", name, g_driver_path);
    return NULL;
}

void* nvglibc_dlopen(const char* name, int flags) {
    if (!name) return dlopen(NULL, flags);
    if (!ensure_init()) return NULL;
    pthread_mutex_lock(&g_lock);
    free(t_error);
    t_error = NULL;
    void* h = load_name(name, flags, 0);
    pthread_mutex_unlock(&g_lock);
    return h;
}

const char* nvglibc_dlerror(void) {
    return t_error;
}

// ---- the trampoline libraries' constructor ----

struct nvg_bind_entry {
    const char* name;
    void** slot;
};

static void* bionic_sym(const char* name) {
    void* p = g_libc ? dlsym(g_libc, name) : NULL;
    if (!p && g_libm) p = dlsym(g_libm, name);
    if (!p && g_libdl) p = dlsym(g_libdl, name);
    return p;
}

static char* progname_full(void) {
    static char buf[PATH_MAX];
    if (!buf[0]) {
        int fd = open("/proc/self/cmdline", O_RDONLY | O_CLOEXEC);
        if (fd >= 0) {
            ssize_t n = read(fd, buf, sizeof(buf) - 1);
            buf[n > 0 ? n : 0] = '\0';
            close(fd);
        }
        if (!buf[0]) snprintf(buf, sizeof(buf), "%s", getprogname());
    }
    return buf;
}

void nvglibc_bind(const struct nvg_bind_entry* fwd, const struct nvg_bind_entry* fwd_end,
                  const struct nvg_bind_entry* data, const struct nvg_bind_entry* data_end) {
    size_t missing = 0;
    for (; fwd < fwd_end; fwd++) {
        void* p = bionic_sym(fwd->name);
        if (p) {
            *fwd->slot = p;
        } else {
            missing++;
            nvg_log(ANDROID_LOG_DEBUG, "bionic has no %s (traps if called)", fwd->name);
        }
    }
    for (; data < data_end; data++) {
        const char* n = data->name;
        if (strcmp(n, "stdin") == 0 || strcmp(n, "stdout") == 0 || strcmp(n, "stderr") == 0) {
            void** v = bionic_sym(n);
            *data->slot = v ? *v : NULL;
        } else if (strcmp(n, "program_invocation_name") == 0 || strcmp(n, "__progname_full") == 0) {
            *data->slot = progname_full();
        } else if (strcmp(n, "program_invocation_short_name") == 0 || strcmp(n, "__progname") == 0) {
            *data->slot = (void*)getprogname();
        } else {
            nvg_log(ANDROID_LOG_WARN, "no value for glibc variable %s", n);
        }
    }
    if (missing) nvg_log(ANDROID_LOG_DEBUG, "%zu forwards have no bionic target", missing);
}

__attribute__((noreturn)) void nvglibc_missing(const char* name) {
    nvg_log(ANDROID_LOG_FATAL, "glibc function %s is not implemented by nvglibc", name);
    abort();
}

// ---- glibc's dl* ----

static const char* take_glibc_error(void) {
    if (!t_glibc_error) return NULL;
    snprintf(t_glibc_error_buf, sizeof(t_glibc_error_buf), "%s", t_glibc_error);
    free(t_glibc_error);
    t_glibc_error = NULL;
    return t_glibc_error_buf;
}

void* nvg_dlopen(const char* name, int flags) {
    void* h = nvglibc_dlopen(name, flags & ~RTLD_DEEPBIND);
    if (!h && !name) set_error("%s", dlerror());
    return h;
}

void* nvg_dlsym(void* handle, const char* sym) {
    void* p;
    if (handle == RTLD_DEFAULT || handle == RTLD_NEXT || handle == g_main) {
        // glibc's names first, so that a driver looking up libc functions gets glibc's ABI.
        p = g_stub_libc ? dlsym(g_stub_libc, sym) : NULL;
        if (!p) p = dlsym(RTLD_DEFAULT, sym);
    } else {
        // glibc's libpthread.so.0, libdl.so.2, ... are empty here: their names live in libc.so.6.
        pthread_mutex_lock(&g_lock);
        struct loaded* l = find_handle(handle);
        if (l && l->handle != g_stub_libc && strncmp(l->path, g_stub_dir, strlen(g_stub_dir)) == 0 &&
            dlsym(handle, sym) == NULL) {
            handle = g_stub_libc;
        }
        pthread_mutex_unlock(&g_lock);
        p = dlsym(handle, sym);
    }
    if (!p) set_error("undefined symbol: %s", sym);
    return p;
}

void* nvg_dlvsym(void* handle, const char* sym, const char* version) {
    (void)version;  // nvglibc's symbols are unversioned
    return nvg_dlsym(handle, sym);
}

char* nvg_dlerror(void) {
    return (char*)take_glibc_error();
}

// glibc's public part of struct link_map. NVIDIA's driver walks the l_prev/l_next chain from
// the entry dladdr1() returns to find its other libraries, so all loaded objects are chained in
// dl_iterate_phdr() order (the executable first, named "" as in glibc). Entries live forever and
// are relinked on each query.
struct glibc_link_map {
    ElfW(Addr) l_addr;
    char* l_name;
    ElfW(Dyn)* l_ld;
    struct glibc_link_map *l_next, *l_prev;
};

static struct glibc_link_map** g_lms;  // every entry ever made
static size_t g_n_lms;

struct lm_walk {
    struct glibc_link_map* prev;
    ElfW(Addr) addr;                   // find the object containing this address...
    const char* path;                  // ...or the one with this realpath
    struct glibc_link_map* found;
};

static int lm_cb(struct dl_phdr_info* info, size_t size, void* arg) {
    (void)size;
    struct lm_walk* w = arg;
    const char* name = w->prev ? (info->dlpi_name ? info->dlpi_name : "") : "";
    struct glibc_link_map* lm = NULL;
    for (size_t i = 0; i < g_n_lms; i++) {
        if (g_lms[i]->l_addr == info->dlpi_addr && strcmp(g_lms[i]->l_name, name) == 0) lm = g_lms[i];
    }
    if (!lm) {
        lm = calloc(1, sizeof(*lm));
        lm->l_addr = info->dlpi_addr;
        lm->l_name = strdup(name);
        for (int j = 0; j < info->dlpi_phnum; j++) {
            if (info->dlpi_phdr[j].p_type == PT_DYNAMIC) {
                lm->l_ld = (ElfW(Dyn)*)(info->dlpi_addr + info->dlpi_phdr[j].p_vaddr);
            }
        }
        g_lms = realloc(g_lms, (g_n_lms + 1) * sizeof(*g_lms));
        g_lms[g_n_lms++] = lm;
    }
    lm->l_prev = w->prev;
    lm->l_next = NULL;
    if (w->prev) w->prev->l_next = lm;
    w->prev = lm;
    if (!w->found && w->path && info->dlpi_name && strcmp(info->dlpi_name, w->path) == 0) w->found = lm;
    for (int j = 0; !w->found && w->addr && j < info->dlpi_phnum; j++) {
        const ElfW(Phdr)* ph = &info->dlpi_phdr[j];
        if (ph->p_type == PT_LOAD && w->addr >= info->dlpi_addr + ph->p_vaddr &&
            w->addr < info->dlpi_addr + ph->p_vaddr + ph->p_memsz) {
            w->found = lm;
        }
    }
    return 0;
}

// Call with g_lock held.
static struct link_map* find_link_map(const void* addr, const char* path) {
    char real[PATH_MAX];
    struct lm_walk w = {NULL, (ElfW(Addr))addr, path && realpath(path, real) ? real : path, NULL};
    dl_iterate_phdr(lm_cb, &w);
    return (struct link_map*)w.found;
}

int nvg_dladdr1(const void* addr, Dl_info* info, void** extra, int flags) {
    int r = dladdr(addr, info);
    if (r && extra) {
        *extra = NULL;
        if (flags == 2 /* RTLD_DL_LINKMAP */) {
            pthread_mutex_lock(&g_lock);
            *extra = find_link_map(addr, NULL);
            pthread_mutex_unlock(&g_lock);
        }
    }
    return r;
}

int nvg_dlinfo(void* handle, int request, void* arg) {
    pthread_mutex_lock(&g_lock);
    struct loaded* l = find_handle(handle);
    int ret = -1;
    if (!l) {
        set_error("dlinfo: handle %p was not opened by nvglibc", handle);
    } else if (request == 2 /* RTLD_DI_LINKMAP */) {
        *(void**)arg = find_link_map(NULL, l->path);
        if (*(void**)arg) {
            ret = 0;
        } else {
            set_error("dlinfo: cannot locate %s", l->path);
        }
    } else if (request == 6 /* RTLD_DI_ORIGIN */) {
        const char* slash = strrchr(l->path, '/');
        size_t n = slash ? (size_t)(slash - l->path) : 0;
        memcpy(arg, l->path, n);
        ((char*)arg)[n] = '\0';
        ret = 0;
    } else {
        set_error("dlinfo: request %d is not supported", request);
    }
    pthread_mutex_unlock(&g_lock);
    return ret;
}
