// Phase L2 (docs/nvidia-native.md): glibc functions whose ABI differs from bionic's on x86_64.

#define _GNU_SOURCE
#include <android/log.h>
#include <assert.h>
#include <ctype.h>
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <locale.h>
#include <malloc.h>
#include <signal.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>
#include <unwind.h>
#include <wchar.h>
#include <wctype.h>

#include "nvglibc_private.h"

// ---- signals ----

// glibc x86_64 struct sigaction: a 128-byte sigset_t (bionic's is 8 bytes; the first 8 bytes
// hold signals 1-64 in both, so plain sigset_t functions go to bionic directly).
struct glibc_sigaction {
    void* handler;
    uint64_t mask[16];
    int flags;
    void (*restorer)(void);
};

int nvg_sigaction(int sig, const struct glibc_sigaction* act, struct glibc_sigaction* old) {
    struct sigaction b, bold;
    if (act) {
        memset(&b, 0, sizeof(b));
        b.sa_handler = (void (*)(int))act->handler;
        memcpy(&b.sa_mask, act->mask, sizeof(b.sa_mask));
        b.sa_flags = act->flags & ~SA_RESTORER;
    }
    int r = sigaction(sig, act ? &b : NULL, old ? &bold : NULL);
    if (r == 0 && old) {
        memset(old, 0, sizeof(*old));
        old->handler = (void*)bold.sa_handler;
        memcpy(old->mask, &bold.sa_mask, sizeof(bold.sa_mask));
        old->flags = bold.sa_flags;
        old->restorer = bold.sa_restorer;
    }
    return r;
}

// ---- sysconf: glibc's _SC_ numbers ----

static const struct {
    int glibc;
    int bionic;
} kSysconf[] = {
#include "sysconf.inc"
};

long nvg_sysconf(int name) {
    for (size_t i = 0; i < sizeof(kSysconf) / sizeof(kSysconf[0]); i++) {
        if (kSysconf[i].glibc == name) return sysconf(kSysconf[i].bionic);
    }
    switch (name) {
        case 190:  // _SC_LEVEL1_DCACHE_LINESIZE
        case 193:  // _SC_LEVEL2_CACHE_LINESIZE
        case 196:  // _SC_LEVEL3_CACHE_LINESIZE
            return 64;
    }
    nvg_log(ANDROID_LOG_WARN, "sysconf(%d): unknown glibc name", name);
    errno = EINVAL;
    return -1;
}

// ---- malloc ----

// glibc's struct mallinfo has int fields, bionic's size_t.
struct glibc_mallinfo {
    int arena, ordblks, smblks, hblks, hblkhd, usmblks, fsmblks, uordblks, fordblks, keepcost;
};

struct glibc_mallinfo nvg_mallinfo(void) {
    struct mallinfo m = mallinfo();
    struct glibc_mallinfo g = {
        (int)m.arena, (int)m.ordblks, (int)m.smblks, (int)m.hblks, (int)m.hblkhd,
        (int)m.usmblks, (int)m.fsmblks, (int)m.uordblks, (int)m.fordblks, (int)m.keepcost,
    };
    return g;
}

// ---- stat (glibc < 2.33 exported versioned __xstat; struct stat is the kernel's on x86_64) ----

int nvg___xstat(int ver, const char* path, struct stat* st) {
    (void)ver;
    return stat(path, st);
}
int nvg___xstat64(int ver, const char* path, struct stat* st) {
    (void)ver;
    return stat(path, st);
}
int nvg___lxstat(int ver, const char* path, struct stat* st) {
    (void)ver;
    return lstat(path, st);
}
int nvg___lxstat64(int ver, const char* path, struct stat* st) {
    (void)ver;
    return lstat(path, st);
}
int nvg___fxstat(int ver, int fd, struct stat* st) {
    (void)ver;
    return fstat(fd, st);
}
int nvg___fxstat64(int ver, int fd, struct stat* st) {
    (void)ver;
    return fstat(fd, st);
}
int nvg___fxstatat(int ver, int dirfd, const char* path, struct stat* st, int flags) {
    (void)ver;
    return fstatat(dirfd, path, st, flags);
}
int nvg___xmknod(int ver, const char* path, mode_t mode, dev_t* dev) {
    (void)ver;
    return mknod(path, mode, *dev);
}

// ---- stdio ----

int nvg___isoc99_sscanf(const char* s, const char* fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    int r = vsscanf(s, fmt, ap);
    va_end(ap);
    return r;
}

int nvg___isoc99_fscanf(FILE* f, const char* fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    int r = vfscanf(f, fmt, ap);
    va_end(ap);
    return r;
}

int nvg___vfprintf_chk(FILE* f, int flag, const char* fmt, va_list ap) {
    (void)flag;
    return vfprintf(f, fmt, ap);
}

size_t nvg___fread_chk(void* buf, size_t buflen, size_t size, size_t n, FILE* f) {
    if (size && n > buflen / size) {
        nvg_log(ANDROID_LOG_FATAL, "__fread_chk: buffer overflow");
        abort();
    }
    return fread(buf, size, n, f);
}

void nvg___assert_fail(const char* expr, const char* file, unsigned int line, const char* func) {
    __assert2(file, (int)line, func, expr);
}

// ---- misc ----

const char* nvg_gnu_get_libc_version(void) {
    return "2.38";
}

char* nvg_dgettext(const char* domain, const char* msgid) {
    (void)domain;
    return (char*)msgid;
}

char* nvg_bind_textdomain_codeset(const char* domain, const char* codeset) {
    (void)domain;
    return (char*)codeset;
}

int nvg___popcountdi2(long x) {
    return __builtin_popcountl((unsigned long)x);
}

// ---- locales ----
//
// glibc's locale_t points at struct __locale_struct, whose ctype table pointers libstdc++ (linked
// statically into NVIDIA's libraries) reads directly. nvglibc has one locale, "C", whose tables
// are filled in glibc's encoding; the *_l functions ignore the locale argument.

#define G_ISupper 0x0100
#define G_ISlower 0x0200
#define G_ISalpha 0x0400
#define G_ISdigit 0x0800
#define G_ISxdigit 0x1000
#define G_ISspace 0x2000
#define G_ISprint 0x4000
#define G_ISgraph 0x8000
#define G_ISblank 0x0001
#define G_IScntrl 0x0002
#define G_ISpunct 0x0004
#define G_ISalnum 0x0008

static unsigned short g_ctype_b[384];  // indexed -128..255 through g_ctype_b + 128
static int g_ctype_tolower[384];
static int g_ctype_toupper[384];
static const unsigned short* g_ctype_b_ptr = g_ctype_b + 128;
static const int* g_ctype_tolower_ptr = g_ctype_tolower + 128;
static const int* g_ctype_toupper_ptr = g_ctype_toupper + 128;

struct glibc_locale {
    void* locales[13];
    const unsigned short* ctype_b;
    const int* ctype_tolower;
    const int* ctype_toupper;
    const char* names[13];
};
_Static_assert(sizeof(struct glibc_locale) == 232, "glibc struct __locale_struct");

static char g_locale_data[13][512];  // the __locale_data blocks: zeros, never read for "C"
static struct glibc_locale g_c_locale;
static __thread struct glibc_locale* t_locale;  // __uselocale(); NULL means the global locale
#define G_LC_GLOBAL_LOCALE ((struct glibc_locale*)-1L)

__attribute__((constructor)) static void locale_init(void) {
    for (int c = -128; c < 256; c++) {
        unsigned short m = 0;
        int u = c & 0xff;
        if (c >= 0 && c < 128) {
            if (isupper(u)) m |= G_ISupper;
            if (islower(u)) m |= G_ISlower;
            if (isalpha(u)) m |= G_ISalpha;
            if (isdigit(u)) m |= G_ISdigit;
            if (isxdigit(u)) m |= G_ISxdigit;
            if (isspace(u)) m |= G_ISspace;
            if (isprint(u)) m |= G_ISprint;
            if (isgraph(u)) m |= G_ISgraph;
            if (isblank(u)) m |= G_ISblank;
            if (iscntrl(u)) m |= G_IScntrl;
            if (ispunct(u)) m |= G_ISpunct;
            if (isalnum(u)) m |= G_ISalnum;
        }
        g_ctype_b[c + 128] = m;
        g_ctype_tolower[c + 128] = (c >= 'A' && c <= 'Z') ? c + 32 : c;
        g_ctype_toupper[c + 128] = (c >= 'a' && c <= 'z') ? c - 32 : c;
    }
    for (int i = 0; i < 13; i++) {
        g_c_locale.locales[i] = g_locale_data[i];
        g_c_locale.names[i] = "C";
    }
    g_c_locale.ctype_b = g_ctype_b_ptr;
    g_c_locale.ctype_tolower = g_ctype_tolower_ptr;
    g_c_locale.ctype_toupper = g_ctype_toupper_ptr;
}

const unsigned short** nvg___ctype_b_loc(void) {
    return &g_ctype_b_ptr;
}
const int** nvg___ctype_tolower_loc(void) {
    return &g_ctype_tolower_ptr;
}
const int** nvg___ctype_toupper_loc(void) {
    return &g_ctype_toupper_ptr;
}

struct glibc_locale* nvg___newlocale(int mask, const char* name, struct glibc_locale* base) {
    (void)mask;
    if (name && strcmp(name, "C") != 0 && strcmp(name, "POSIX") != 0 && name[0] != '\0' &&
        strcmp(name, "C.UTF-8") != 0 && strcmp(name, "en_US.UTF-8") != 0) {
        errno = ENOENT;
        return NULL;
    }
    if (base && base != &g_c_locale && base != G_LC_GLOBAL_LOCALE) free(base);
    struct glibc_locale* l = malloc(sizeof(*l));
    if (l) *l = g_c_locale;
    return l;
}

struct glibc_locale* nvg___duplocale(struct glibc_locale* l) {
    (void)l;
    return nvg___newlocale(0, "C", NULL);
}

void nvg___freelocale(struct glibc_locale* l) {
    if (l && l != &g_c_locale && l != G_LC_GLOBAL_LOCALE) free(l);
}

struct glibc_locale* nvg___uselocale(struct glibc_locale* l) {
    struct glibc_locale* old = t_locale ? t_locale : G_LC_GLOBAL_LOCALE;
    if (l) t_locale = l == G_LC_GLOBAL_LOCALE ? NULL : l;
    return old;
}

char* nvg___nl_langinfo_l(int item, struct glibc_locale* l) {
    (void)l;
    static const char* const kDays[] = {"Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"};
    static const char* const kAbDays[] = {"Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"};
    static const char* const kMons[] = {"January", "February", "March", "April", "May", "June", "July",
                                        "August", "September", "October", "November", "December"};
    static const char* const kAbMons[] = {"Jan", "Feb", "Mar", "Apr", "May", "Jun",
                                          "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"};
    switch (item) {
        case 0xe: return "ANSI_X3.4-1968";                      // CODESET
        case 0x10000: return ".";                               // RADIXCHAR
        case 0x10001: return "";                                // THOUSEP
        case 0x10002: return "";                                // grouping
        case 0x10003: return (char*)(uintptr_t)L'.';            // decimal point, wide
        case 0x10004: return NULL;                              // thousands separator, wide
        case 0x20026: return "AM";
        case 0x20027: return "PM";
        case 0x20028: return "%a %b %e %H:%M:%S %Y";            // D_T_FMT
        case 0x20029: return "%m/%d/%y";                        // D_FMT
        case 0x2002a: return "%H:%M:%S";                        // T_FMT
        case 0x2002b: return "%I:%M:%S %p";                     // T_FMT_AMPM
        case 0x2005a: return (char*)L"AM";
        case 0x2005b: return (char*)L"PM";
        case 0x2005c: return (char*)L"%a %b %e %H:%M:%S %Y";
        case 0x2005d: return (char*)L"%m/%d/%y";
        case 0x2005e: return (char*)L"%H:%M:%S";
        case 0x2005f: return (char*)L"%I:%M:%S %p";
        case 0x40002: return ".";                               // monetary decimal point
        case 0x40007:                                           // int_frac_digits
        case 0x40008: return "\x7f";                            // frac_digits (CHAR_MAX)
        case 0x4002b: return (char*)(uintptr_t)L'.';
        case 0x4002c: return NULL;
        case 0x50000: return "^[yY]";                           // YESEXPR
        case 0x50001: return "^[nN]";                           // NOEXPR
    }
    if (item >= 0x20000 && item < 0x20007) return (char*)kAbDays[item - 0x20000];
    if (item >= 0x20007 && item < 0x2000e) return (char*)kDays[item - 0x20007];
    if (item >= 0x2000e && item < 0x2001a) return (char*)kAbMons[item - 0x2000e];
    if (item >= 0x2001a && item < 0x20026) return (char*)kMons[item - 0x2001a];
    if (item >= 0x20034 && item < 0x2003b) {
        static wchar_t w[7][4];
        int i = item - 0x20034;
        mbstowcs(w[i], kAbDays[i], 4);
        return (char*)w[i];
    }
    if (item >= 0x2003b && item < 0x20042) {
        static wchar_t w[7][10];
        int i = item - 0x2003b;
        mbstowcs(w[i], kDays[i], 10);
        return (char*)w[i];
    }
    if (item >= 0x20042 && item < 0x2004e) {
        static wchar_t w[12][4];
        int i = item - 0x20042;
        mbstowcs(w[i], kAbMons[i], 4);
        return (char*)w[i];
    }
    if (item >= 0x2004e && item < 0x2005a) {
        static wchar_t w[12][10];
        int i = item - 0x2004e;
        mbstowcs(w[i], kMons[i], 10);
        return (char*)w[i];
    }
    return "";
}

// glibc's struct lconv for "C" (it orders the int_* fields differently from bionic's).
struct glibc_lconv {
    char *decimal_point, *thousands_sep, *grouping, *int_curr_symbol, *currency_symbol,
        *mon_decimal_point, *mon_thousands_sep, *mon_grouping, *positive_sign, *negative_sign;
    char int_frac_digits, frac_digits, p_cs_precedes, p_sep_by_space, n_cs_precedes,
        n_sep_by_space, p_sign_posn, n_sign_posn, int_p_cs_precedes, int_p_sep_by_space,
        int_n_cs_precedes, int_n_sep_by_space, int_p_sign_posn, int_n_sign_posn;
};

struct glibc_lconv* nvg_localeconv(void) {
    static struct glibc_lconv c = {
        ".", "", "", "", "", "", "", "", "", "",
        CHAR_MAX, CHAR_MAX, CHAR_MAX, CHAR_MAX, CHAR_MAX, CHAR_MAX, CHAR_MAX,
        CHAR_MAX, CHAR_MAX, CHAR_MAX, CHAR_MAX, CHAR_MAX, CHAR_MAX, CHAR_MAX,
    };
    return &c;
}

int nvg___strcoll_l(const char* a, const char* b, void* l) {
    (void)l;
    return strcmp(a, b);
}
size_t nvg___strxfrm_l(char* d, const char* s, size_t n, void* l) {
    (void)l;
    return strxfrm(d, s, n);
}
int nvg___wcscoll_l(const wchar_t* a, const wchar_t* b, void* l) {
    (void)l;
    return wcscmp(a, b);
}
size_t nvg___wcsxfrm_l(wchar_t* d, const wchar_t* s, size_t n, void* l) {
    (void)l;
    return wcsxfrm(d, s, n);
}
double nvg___strtod_l(const char* s, char** end, void* l) {
    (void)l;
    return strtod(s, end);
}
float nvg___strtof_l(const char* s, char** end, void* l) {
    (void)l;
    return strtof(s, end);
}
size_t nvg___strftime_l(char* s, size_t n, const char* fmt, const struct tm* tm, void* l) {
    (void)l;
    return strftime(s, n, fmt, tm);
}
size_t nvg___wcsftime_l(wchar_t* s, size_t n, const wchar_t* fmt, const struct tm* tm, void* l) {
    (void)l;
    return wcsftime(s, n, fmt, tm);
}
wint_t nvg___towlower_l(wint_t c, void* l) {
    (void)l;
    return towlower(c);
}
wint_t nvg___towupper_l(wint_t c, void* l) {
    (void)l;
    return towupper(c);
}

// glibc's wctype_t is an opaque unsigned long; here it is bionic's wctype_t (also a small
// integer) so __iswctype_l can hand it back to bionic.
unsigned long nvg___wctype_l(const char* name, void* l) {
    (void)l;
    return (unsigned long)wctype(name);
}
int nvg___iswctype_l(wint_t c, unsigned long desc, void* l) {
    (void)l;
    return iswctype(c, (wctype_t)desc);
}

// strtold_l returns glibc's 80-bit x87 long double in %st(0); bionic's long double is binary128.
// The value goes through double, which is all NVIDIA's code needs from it.
__asm__(
    ".text\n"
    ".globl nvg_strtold_l\n"
    ".type nvg_strtold_l, @function\n"
    "nvg_strtold_l:\n"
    "  subq $8, %rsp\n"
    "  call strtod@PLT\n"
    "  movsd %xmm0, (%rsp)\n"
    "  fldl (%rsp)\n"
    "  addq $8, %rsp\n"
    "  ret\n"
    ".size nvg_strtold_l, . - nvg_strtold_l\n");

// ---- libgcc_s's unwinder (libnvglibc_gcc_s): LLVM libunwind, linked in statically ----

void nvg__Unwind_DeleteException(struct _Unwind_Exception* e) {
    _Unwind_DeleteException(e);
}
uintptr_t nvg__Unwind_GetDataRelBase(struct _Unwind_Context* c) {
    return _Unwind_GetDataRelBase(c);
}
uintptr_t nvg__Unwind_GetIPInfo(struct _Unwind_Context* c, int* before) {
    return _Unwind_GetIPInfo(c, before);
}
void* nvg__Unwind_GetLanguageSpecificData(struct _Unwind_Context* c) {
    return (void*)_Unwind_GetLanguageSpecificData(c);
}
uintptr_t nvg__Unwind_GetRegionStart(struct _Unwind_Context* c) {
    return _Unwind_GetRegionStart(c);
}
uintptr_t nvg__Unwind_GetTextRelBase(struct _Unwind_Context* c) {
    return _Unwind_GetTextRelBase(c);
}
_Unwind_Reason_Code nvg__Unwind_RaiseException(struct _Unwind_Exception* e) {
    return _Unwind_RaiseException(e);
}
void nvg__Unwind_Resume(struct _Unwind_Exception* e) {
    _Unwind_Resume(e);
}
_Unwind_Reason_Code nvg__Unwind_Resume_or_Rethrow(struct _Unwind_Exception* e) {
    return _Unwind_Resume_or_Rethrow(e);
}
void nvg__Unwind_SetGR(struct _Unwind_Context* c, int i, uintptr_t v) {
    _Unwind_SetGR(c, i, v);
}
void nvg__Unwind_SetIP(struct _Unwind_Context* c, uintptr_t v) {
    _Unwind_SetIP(c, v);
}
uintptr_t nvg__Unwind_GetGR(struct _Unwind_Context* c, int i) {
    return _Unwind_GetGR(c, i);
}
uintptr_t nvg__Unwind_GetIP(struct _Unwind_Context* c) {
    return _Unwind_GetIP(c);
}
uintptr_t nvg__Unwind_GetCFA(struct _Unwind_Context* c) {
    return _Unwind_GetCFA(c);
}
_Unwind_Reason_Code nvg__Unwind_Backtrace(_Unwind_Trace_Fn fn, void* arg) {
    return _Unwind_Backtrace(fn, arg);
}
void* nvg__Unwind_FindEnclosingFunction(void* pc) {
    return _Unwind_FindEnclosingFunction(pc);
}
_Unwind_Reason_Code nvg__Unwind_ForcedUnwind(struct _Unwind_Exception* e, _Unwind_Stop_Fn stop, void* arg) {
    return _Unwind_ForcedUnwind(e, stop, arg);
}
