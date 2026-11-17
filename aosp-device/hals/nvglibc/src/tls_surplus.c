// nvglibc's own static TLS surplus, for a bionic without
// tools/patches/aosp17-bionic-static-tls-surplus.patch (Waydroid's Android 13, docs/waydroid-nvidia.md).
//
// NVIDIA's libnvidia-tls, libnvidia-glcore, libGLX_nvidia and GLVND's libGLdispatch use
// initial-exec TLS (R_X86_64_TPOFF64), which an unpatched bionic refuses in dlopen()ed libraries.
// tools/waydroid/nvtls-rewrite rewrites copies of them:
//  - every R_X86_64_TPOFF64 becomes R_X86_64_NONE with its symbol and addend kept, so bionic
//    loads the library and leaves those GOT words alone;
//  - DT_INIT, DT_INIT_ARRAY and DT_INIT_ARRAYSZ are retagged DT_NVG_*, so bionic does not run the
//    constructors before the GOT words are filled. DT_NVG_INIT also marks a rewritten library.
// The TLS memory is a block of static TLS that bionic gives every thread because
// libnvglibc_tls.so (aosp-device/waydroid/tls) is loaded at process start (a DT_NEEDED of the
// platform's libvulkan.so in Waydroid). After bionic's dlopen() this file places each library's
// PT_TLS in that block, writes the TP-relative offsets into the GOT and runs the constructors.
//
// A library's TLS initial image is copied into the calling thread and into libnvglibc_tls.so's own
// initial image, which bionic copies into every thread created afterwards. Threads that existed
// before see zeros (only libGLdispatch has a non-zero image: GLVND's current dispatch table, set
// again by any eglMakeCurrent).

#define _GNU_SOURCE
#include <android/log.h>
#include <elf.h>
#include <errno.h>
#include <limits.h>
#include <link.h>
#include <pthread.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#include "nvglibc_private.h"

// tools/waydroid/nvtls-rewrite's tags (in the OS-specific range, ignored by bionic)
#define DT_NVG_INIT 0x6e766701
#define DT_NVG_INIT_ARRAY 0x6e766702
#define DT_NVG_INIT_ARRAYSZ 0x6e766703

#define SURPLUS_LIB "/libnvglibc_tls.so"
#define SURPLUS_SIZE 512  // aosp-device/waydroid/tls/nvglibc_tls.c
#define MAX_MODS 8

extern char** environ;

struct obj {
    const char* path;  // in: realpath of the object, or NULL to find the surplus library
    ElfW(Addr) bias;
    const ElfW(Phdr)* phdr;
    ElfW(Half) phnum;
    void* tls_data;
    bool found;
};

struct dyn {
    const ElfW(Sym)* symtab;
    const char* strtab;
    const uint32_t* gnu_hash;
    const uint32_t* sysv_hash;
    const ElfW(Rela)* rela;
    size_t relasz;
    const ElfW(Rela)* jmprel;
    size_t pltrelsz;
    ElfW(Addr) init;
    ElfW(Addr) init_array;
    size_t init_arraysz;
    bool rewritten;
};

// A rewritten library with a PT_TLS, placed in the surplus.
struct tls_mod {
    ElfW(Addr) bias;
    struct dyn dyn;
    intptr_t tpoff;  // of its TLS block, from the thread pointer
};

static bool g_surplus_ready;
static intptr_t g_surplus_tpoff;
static uint8_t* g_surplus_image;  // libnvglibc_tls.so's initial image (bionic copies it)
static const ElfW(Phdr)* g_surplus_phdr;
static ElfW(Half) g_surplus_phnum;
static ElfW(Addr) g_surplus_bias;
static size_t g_surplus_used;
static struct tls_mod g_mods[MAX_MODS];
static size_t g_n_mods;

static uint8_t* thread_pointer(void) {
    uint8_t* tp;
    __asm__("mov %%fs:0, %0" : "=r"(tp));  // bionic's TLS_SLOT_SELF
    return tp;
}

static int obj_cb(struct dl_phdr_info* info, size_t size, void* arg) {
    (void)size;
    struct obj* o = arg;
    const char* name = info->dlpi_name;
    if (!name) return 0;
    if (o->path) {
        if (strcmp(name, o->path) != 0) return 0;
    } else {
        size_t n = strlen(name), s = strlen(SURPLUS_LIB);
        if (n < s || strcmp(name + n - s, SURPLUS_LIB) != 0) return 0;
    }
    o->bias = info->dlpi_addr;
    o->phdr = info->dlpi_phdr;
    o->phnum = info->dlpi_phnum;
    o->tls_data = info->dlpi_tls_data;
    o->found = true;
    return 1;
}

static const ElfW(Phdr)* find_phdr(const ElfW(Phdr)* ph, ElfW(Half) n, ElfW(Word) type) {
    for (ElfW(Half) i = 0; i < n; i++)
        if (ph[i].p_type == type) return &ph[i];
    return NULL;
}

// bionic leaves d_ptr unrelocated (glibc relocates l_ld's).
static void read_dyn(ElfW(Addr) bias, const ElfW(Phdr)* ph, ElfW(Half) phnum, struct dyn* d) {
    memset(d, 0, sizeof(*d));
    const ElfW(Phdr)* pd = find_phdr(ph, phnum, PT_DYNAMIC);
    if (!pd) return;
    for (const ElfW(Dyn)* e = (const ElfW(Dyn)*)(bias + pd->p_vaddr); e->d_tag != DT_NULL; e++) {
        const ElfW(Addr) p = bias + e->d_un.d_ptr;
        switch (e->d_tag) {
            case DT_SYMTAB: d->symtab = (const ElfW(Sym)*)p; break;
            case DT_STRTAB: d->strtab = (const char*)p; break;
            case DT_GNU_HASH: d->gnu_hash = (const uint32_t*)p; break;
            case DT_HASH: d->sysv_hash = (const uint32_t*)p; break;
            case DT_RELA: d->rela = (const ElfW(Rela)*)p; break;
            case DT_RELASZ: d->relasz = e->d_un.d_val; break;
            case DT_JMPREL: d->jmprel = (const ElfW(Rela)*)p; break;
            case DT_PLTRELSZ: d->pltrelsz = e->d_un.d_val; break;
            case DT_NVG_INIT: d->init = p; d->rewritten = true; break;
            case DT_NVG_INIT_ARRAY: d->init_array = p; break;
            case DT_NVG_INIT_ARRAYSZ: d->init_arraysz = e->d_un.d_val; break;
        }
    }
}

static uint32_t gnu_hash(const char* s) {
    uint32_t h = 5381;
    for (; *s; s++) h = h * 33 + (uint8_t)*s;
    return h;
}

static uint32_t sysv_hash(const char* s) {
    uint32_t h = 0;
    for (; *s; s++) {
        h = (h << 4) + (uint8_t)*s;
        uint32_t g = h & 0xf0000000;
        h ^= g >> 24;
        h &= ~g;
    }
    return h;
}

// A defined dynamic symbol of the object, or NULL.
static const ElfW(Sym)* lookup(const struct dyn* d, const char* name) {
    if (d->gnu_hash) {
        const uint32_t* t = d->gnu_hash;
        const uint32_t nbuckets = t[0], symoffset = t[1], bloom_size = t[2];
        const uint32_t* buckets = (const uint32_t*)((const ElfW(Addr)*)(t + 4) + bloom_size);
        const uint32_t* chain = buckets + nbuckets;
        const uint32_t h = gnu_hash(name);
        for (uint32_t i = buckets[h % nbuckets]; i >= symoffset && i != 0; i++) {
            const uint32_t ch = chain[i - symoffset];
            const ElfW(Sym)* s = &d->symtab[i];
            if ((ch | 1) == (h | 1) && s->st_shndx != SHN_UNDEF &&
                strcmp(d->strtab + s->st_name, name) == 0)
                return s;
            if (ch & 1) break;
        }
        return NULL;
    }
    if (d->sysv_hash) {
        const uint32_t nbucket = d->sysv_hash[0];
        const uint32_t* bucket = d->sysv_hash + 2;
        const uint32_t* chain = bucket + nbucket;
        for (uint32_t i = bucket[sysv_hash(name) % nbucket]; i != 0; i = chain[i]) {
            const ElfW(Sym)* s = &d->symtab[i];
            if (s->st_shndx != SHN_UNDEF && strcmp(d->strtab + s->st_name, name) == 0) return s;
        }
    }
    return NULL;
}

// Writes `len` bytes at `addr` of an object whose pages bionic may have made read-only (RELRO),
// and restores the protection afterwards.
static bool poke(ElfW(Addr) bias, const ElfW(Phdr)* ph, ElfW(Half) phnum, void* addr,
                 const void* src, size_t len) {
    const ElfW(Addr) a = (ElfW(Addr))addr;
    int prot = -1;
    for (ElfW(Half) i = 0; i < phnum; i++) {
        if (ph[i].p_type == PT_LOAD && a >= bias + ph[i].p_vaddr &&
            a + len <= bias + ph[i].p_vaddr + ph[i].p_memsz) {
            prot = ((ph[i].p_flags & PF_R) ? PROT_READ : 0) |
                   ((ph[i].p_flags & PF_W) ? PROT_WRITE : 0) |
                   ((ph[i].p_flags & PF_X) ? PROT_EXEC : 0);
        }
    }
    const ElfW(Phdr)* relro = find_phdr(ph, phnum, PT_GNU_RELRO);
    if (relro && a >= bias + relro->p_vaddr && a < bias + relro->p_vaddr + relro->p_memsz)
        prot = PROT_READ;
    if (prot < 0) {
        nvg_log(ANDROID_LOG_ERROR, "TLS surplus: %p is outside the object", addr);
        return false;
    }
    const size_t page = (size_t)sysconf(_SC_PAGESIZE);
    const ElfW(Addr) start = a & ~(page - 1);
    const size_t span = (a + len - start + page - 1) & ~(page - 1);
    if (!(prot & PROT_WRITE) && mprotect((void*)start, span, prot | PROT_WRITE) != 0) {
        nvg_log(ANDROID_LOG_ERROR, "TLS surplus: mprotect: %s", strerror(errno));
        return false;
    }
    memcpy(addr, src, len);
    if (!(prot & PROT_WRITE)) mprotect((void*)start, span, prot);
    return true;
}

static intptr_t surplus_offset(void) {
    struct obj o = {0};
    dl_iterate_phdr(obj_cb, &o);
    return o.found && o.tls_data ? (uint8_t*)o.tls_data - thread_pointer() : INTPTR_MIN;
}

static void* surplus_offset_thread(void* arg) {
    *(intptr_t*)arg = surplus_offset();
    return NULL;
}

static bool find_surplus(void) {
    if (g_surplus_ready) return true;
    struct obj o = {0};
    dl_iterate_phdr(obj_cb, &o);
    const ElfW(Phdr)* tls = o.found ? find_phdr(o.phdr, o.phnum, PT_TLS) : NULL;
    if (!tls || !o.tls_data) {
        nvg_log(ANDROID_LOG_ERROR,
                "NVIDIA's libraries were rewritten for nvglibc's TLS surplus, but libnvglibc_tls.so "
                "is not loaded in this process (it must be a DT_NEEDED of a library the process "
                "loads at start, e.g. libvulkan.so)");
        return false;
    }
    // Static TLS is at the same offset from the thread pointer in every thread; a dynamic TLS
    // block (the library was dlopen()ed after start) is allocated per thread, on first use.
    const intptr_t off = (uint8_t*)o.tls_data - thread_pointer();
    intptr_t other = INTPTR_MIN;
    pthread_t t;
    if (pthread_create(&t, NULL, surplus_offset_thread, &other) == 0) pthread_join(t, NULL);
    if (other != off || tls->p_filesz < SURPLUS_SIZE || tls->p_align < 64) {
        nvg_log(ANDROID_LOG_ERROR,
                "libnvglibc_tls.so's TLS is not static here (offset %zd, another thread's %zd, "
                "size %zu): it was dlopen()ed after start",
                (ssize_t)off, (ssize_t)other, (size_t)tls->p_filesz);
        return false;
    }
    g_surplus_tpoff = off;
    g_surplus_image = (uint8_t*)(o.bias + tls->p_vaddr);
    g_surplus_bias = o.bias;
    g_surplus_phdr = o.phdr;
    g_surplus_phnum = o.phnum;
    g_surplus_ready = true;
    nvg_log(ANDROID_LOG_DEBUG, "TLS surplus: %d bytes at %zd from the thread pointer",
            SURPLUS_SIZE, (ssize_t)off);
    return true;
}

// Places the object's PT_TLS in the surplus: this thread's copy and the image for new threads.
static bool place_tls(const struct obj* o, const ElfW(Phdr)* tls, const struct dyn* d) {
    const size_t align = tls->p_align ? tls->p_align : 1;
    const size_t at = (g_surplus_used + align - 1) & ~(align - 1);
    if (align > 64 || at + tls->p_memsz > SURPLUS_SIZE || g_n_mods == MAX_MODS) {
        nvg_log(ANDROID_LOG_ERROR, "TLS surplus full: %s needs %zu bytes (align %zu), %zu used",
                o->path, (size_t)tls->p_memsz, align, g_surplus_used);
        return false;
    }
    uint8_t* block = calloc(1, tls->p_memsz ? tls->p_memsz : 1);
    memcpy(block, (const void*)(o->bias + tls->p_vaddr), tls->p_filesz);
    memcpy(thread_pointer() + g_surplus_tpoff + at, block, tls->p_memsz);
    bool ok = poke(g_surplus_bias, g_surplus_phdr, g_surplus_phnum, g_surplus_image + at, block,
                   tls->p_memsz);
    free(block);
    if (!ok) return false;
    g_mods[g_n_mods++] = (struct tls_mod){o->bias, *d, g_surplus_tpoff + (intptr_t)at};
    g_surplus_used = at + tls->p_memsz;
    nvg_log(ANDROID_LOG_DEBUG, "TLS surplus: %s at +%zu (%zu bytes)", o->path, at,
            (size_t)tls->p_memsz);
    return true;
}

static bool fix_relocs(const struct obj* o, const struct dyn* d, const ElfW(Rela)* r, size_t sz) {
    for (const ElfW(Rela)* e = r; r && (const char*)e < (const char*)r + sz; e++) {
        if (ELF64_R_TYPE(e->r_info) != R_X86_64_NONE || ELF64_R_SYM(e->r_info) == 0) continue;
        const ElfW(Sym)* ref = &d->symtab[ELF64_R_SYM(e->r_info)];
        const char* name = d->strtab + ref->st_name;
        const struct tls_mod* mod = NULL;
        const ElfW(Sym)* def = NULL;
        for (size_t i = g_n_mods; i-- > 0 && !def;) {
            def = g_mods[i].bias == o->bias && ref->st_shndx != SHN_UNDEF ?
                          ref :
                          lookup(&g_mods[i].dyn, name);
            mod = &g_mods[i];
        }
        if (!def || ELF64_ST_TYPE(def->st_info) != STT_TLS) {
            nvg_log(ANDROID_LOG_ERROR, "TLS surplus: %s: no TLS definition of %s", o->path, name);
            return false;
        }
        const int64_t v = mod->tpoff + (int64_t)def->st_value + e->r_addend;
        if (!poke(o->bias, o->phdr, o->phnum, (void*)(o->bias + e->r_offset), &v, sizeof(v)))
            return false;
    }
    return true;
}

bool nvg_tls_fixup(const char* path) {
    char real[PATH_MAX];
    struct obj o = {realpath(path, real) ? real : path};
    dl_iterate_phdr(obj_cb, &o);
    if (!o.found) return true;  // nothing to do for a library bionic cannot list
    struct dyn d;
    read_dyn(o.bias, o.phdr, o.phnum, &d);
    if (!d.rewritten) return true;
    if (!find_surplus()) return false;
    const ElfW(Phdr)* tls = find_phdr(o.phdr, o.phnum, PT_TLS);
    if (tls && !place_tls(&o, tls, &d)) return false;
    if (!fix_relocs(&o, &d, d.rela, d.relasz) || !fix_relocs(&o, &d, d.jmprel, d.pltrelsz))
        return false;

    // What bionic would have run: DT_INIT, then DT_INIT_ARRAY.
    typedef void (*init_fn)(int, char**, char**);
    char* argv[] = {(char*)getprogname(), NULL};
    if (d.init) ((init_fn)d.init)(1, argv, environ);
    const ElfW(Addr)* fns = (const ElfW(Addr)*)d.init_array;
    for (size_t i = 0; fns && i < d.init_arraysz / sizeof(*fns); i++) {
        if (fns[i] != 0 && fns[i] != (ElfW(Addr))-1) ((init_fn)fns[i])(1, argv, environ);
    }
    nvg_log(ANDROID_LOG_DEBUG, "TLS surplus: %s fixed up", o.path);
    return true;
}
