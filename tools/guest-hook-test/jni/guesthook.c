// H3 test (docs/arm-translation.md, "H3"): an arm64 library that PLT-hooks the host
// libartbase.so the way NetEase's libunisec.so does. It finds libartbase.so in
// /proc/self/maps, walks its dynamic section to the R_X86_64_JUMP_SLOT of `read`, saves the
// old value (the host libc `read`) and writes the address of its own arm64 `HookedRead`.
// When ART then reads a dex file, host code calls arm64 code and the arm64 hook calls the
// host `read`: the two crossings libpc_native_bridge handles.
// Freestanding (raw syscalls, no libc), so the library has no DT_NEEDED at all.
#include <jni.h>
#include <stddef.h>
#include <stdint.h>

#define SYS_openat 56
#define SYS_close 57
#define SYS_read 63
#define SYS_mprotect 226
#define AT_FDCWD (-100)

#define PT_DYNAMIC 2
#define DT_NULL 0
#define DT_PLTRELSZ 2
#define DT_STRTAB 5
#define DT_SYMTAB 6
#define DT_JMPREL 23
#define R_X86_64_JUMP_SLOT 7

typedef struct {
  unsigned char e_ident[16];
  uint16_t e_type, e_machine;
  uint32_t e_version;
  uint64_t e_entry, e_phoff, e_shoff;
  uint32_t e_flags;
  uint16_t e_ehsize, e_phentsize, e_phnum, e_shentsize, e_shnum, e_shstrndx;
} Ehdr;
typedef struct {
  uint32_t p_type, p_flags;
  uint64_t p_offset, p_vaddr, p_paddr, p_filesz, p_memsz, p_align;
} Phdr;
typedef struct {
  int64_t d_tag;
  uint64_t d_val;
} Dyn;
typedef struct {
  uint64_t r_offset, r_info;
  int64_t r_addend;
} Rela;
typedef struct {
  uint32_t st_name;
  unsigned char st_info, st_other;
  uint16_t st_shndx;
  uint64_t st_value, st_size;
} Sym;

static long Syscall(long n, long a, long b, long c, long d) {
  register long x8 __asm__("x8") = n;
  register long x0 __asm__("x0") = a;
  register long x1 __asm__("x1") = b;
  register long x2 __asm__("x2") = c;
  register long x3 __asm__("x3") = d;
  __asm__ volatile("svc #0" : "+r"(x0) : "r"(x8), "r"(x1), "r"(x2), "r"(x3) : "memory");
  return x0;
}

static long (*g_orig_read)(int, void*, unsigned long);
static volatile int g_hooked_reads;
static char g_maps[4 << 20];

static long HookedRead(int fd, void* buf, unsigned long count) {
  g_hooked_reads++;
  return g_orig_read(fd, buf, count);  // a jump to the host libc `read`
}

static int EndsWith(const char* s, const char* end, const char* suffix) {
  const char* p = suffix;
  while (*p) p++;
  long n = p - suffix;
  if (end - s < n) return 0;
  for (long i = 0; i < n; i++)
    if (end[-n + i] != suffix[i]) return 0;
  return 1;
}

static int StrEq(const char* a, const char* b) {
  while (*a && *a == *b) a++, b++;
  return *a == *b;
}

// Load address of libartbase.so: the mapping with file offset 0.
static uintptr_t FindLibartbase(void) {
  int fd = (int)Syscall(SYS_openat, AT_FDCWD, (long)"/proc/self/maps", 0, 0);
  if (fd < 0) return 0;
  long len = 0, n;
  while (len < (long)sizeof(g_maps) - 1 &&
         (n = Syscall(SYS_read, fd, (long)(g_maps + len), sizeof(g_maps) - 1 - len, 0)) > 0)
    len += n;
  Syscall(SYS_close, fd, 0, 0, 0);
  g_maps[len] = '\0';
  for (char* line = g_maps; *line;) {
    char* end = line;
    while (*end && *end != '\n') end++;
    if (EndsWith(line, end, "/libartbase.so")) {
      // "start-end perms offset ...": the offset is the third field.
      char* p = line;
      int field = 0;
      while (p < end && field < 2) {
        if (*p == ' ') field++;
        p++;
      }
      int zero_offset = p + 8 < end && p[8] == ' ';
      for (int i = 0; zero_offset && i < 8; i++) zero_offset = p[i] == '0';
      if (zero_offset) {
        uintptr_t start = 0;
        for (char* q = line; *q != '-'; q++)
          start = start * 16 + (*q <= '9' ? *q - '0' : *q - 'a' + 10);
        return start;
      }
    }
    line = *end ? end + 1 : end;
  }
  return 0;
}

JNIEXPORT jint JNICALL Java_pc_test_guesthook_MainActivity_hookRead(JNIEnv* env, jclass clazz) {
  (void)env, (void)clazz;
  uintptr_t base = FindLibartbase();
  if (base == 0) return -1;
  const Ehdr* eh = (const Ehdr*)base;
  const Phdr* ph = (const Phdr*)(base + eh->e_phoff);
  const Dyn* dyn = 0;
  for (int i = 0; i < eh->e_phnum; i++)
    if (ph[i].p_type == PT_DYNAMIC) dyn = (const Dyn*)(base + ph[i].p_vaddr);
  if (dyn == 0) return -2;
  uint64_t jmprel = 0, pltrelsz = 0, symtab = 0, strtab = 0;
  for (; dyn->d_tag != DT_NULL; dyn++) {
    if (dyn->d_tag == DT_JMPREL) jmprel = dyn->d_val;
    if (dyn->d_tag == DT_PLTRELSZ) pltrelsz = dyn->d_val;
    if (dyn->d_tag == DT_SYMTAB) symtab = dyn->d_val;
    if (dyn->d_tag == DT_STRTAB) strtab = dyn->d_val;
  }
  if (!jmprel || !symtab || !strtab) return -3;
  const Rela* rel = (const Rela*)(base + jmprel);
  for (uint64_t i = 0; i < pltrelsz / sizeof(Rela); i++) {
    if ((rel[i].r_info & 0xffffffff) != R_X86_64_JUMP_SLOT) continue;
    const Sym* sym = (const Sym*)(base + symtab) + (rel[i].r_info >> 32);
    if (!StrEq((const char*)(base + strtab + sym->st_name), "read")) continue;
    uintptr_t slot = base + rel[i].r_offset;
    if (Syscall(SYS_mprotect, slot & ~4095UL, 4096, 3 /* PROT_READ|PROT_WRITE */, 0) != 0)
      return -4;
    g_orig_read = *(long (**)(int, void*, unsigned long))slot;
    *(long (**)(int, void*, unsigned long))slot = HookedRead;
    return 0;
  }
  return -5;
}

JNIEXPORT jint JNICALL Java_pc_test_guesthook_MainActivity_hookedReads(JNIEnv* env, jclass clazz) {
  (void)env, (void)clazz;
  return g_hooked_reads;
}
