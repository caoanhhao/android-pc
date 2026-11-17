// Phase L2 (docs/nvidia-native.md): glibc's pthread mutexes and condition variables on futexes.
//
// bionic's pthread_mutex_t/pthread_cond_t have glibc's sizes on x86_64 but other layouts and
// static initializers (glibc's PTHREAD_RECURSIVE_MUTEX_INITIALIZER_NP sets __kind at offset 16,
// which bionic reads as a normal mutex), and bionic's pthread_mutexattr_t/pthread_condattr_t are
// 8 bytes where glibc's are 4. So these are implemented here on glibc's layout. The other
// pthread objects (rwlock, once, attr, sem) have compatible layouts and go to bionic. Thread-
// specific keys are kept here too (below), so NVIDIA's libraries do not use up bionic's.

#define _GNU_SOURCE
#include <errno.h>
#include <limits.h>
#include <linux/futex.h>
#include <pthread.h>
#include <sched.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

#include "nvglibc_private.h"

// glibc x86_64 struct __pthread_mutex_s (40 bytes).
struct gmutex {
    _Atomic int lock;  // 0 unlocked, 1 locked, 2 locked with waiters
    unsigned int count;
    _Atomic int owner;
    unsigned int nusers;
    int kind;
    short spins;
    short elision;
    void *prev, *next;
};
_Static_assert(sizeof(struct gmutex) == 40, "glibc pthread_mutex_t");

// glibc's mutex kinds and the flag bits kept in __kind.
#define G_MUTEX_NORMAL 0
#define G_MUTEX_RECURSIVE 1
#define G_MUTEX_ERRORCHECK 2
#define G_MUTEX_ADAPTIVE 3
#define G_MUTEX_KIND_MASK 3
#define G_MUTEX_PSHARED 128

// glibc x86_64 pthread_cond_t (48 bytes); only the first word is used here.
struct gcond {
    _Atomic unsigned int seq;
    unsigned int pshared_clock;  // bit 0: process-shared, bit 1: CLOCK_MONOTONIC
    char pad[40];
};
_Static_assert(sizeof(struct gcond) == 48, "glibc pthread_cond_t");

static int futex_wait(_Atomic int* addr, int val, const struct timespec* abs, int clock_rt,
                      int shared) {
    int op = FUTEX_WAIT_BITSET | (shared ? 0 : FUTEX_PRIVATE_FLAG) |
             (abs && clock_rt ? FUTEX_CLOCK_REALTIME : 0);
    long r = syscall(SYS_futex, addr, op, val, abs, NULL, FUTEX_BITSET_MATCH_ANY);
    return r == -1 ? errno : 0;
}

static void futex_wake(_Atomic int* addr, int n, int shared) {
    syscall(SYS_futex, addr, FUTEX_WAKE | (shared ? 0 : FUTEX_PRIVATE_FLAG), n);
}

static int timespec_valid(const struct timespec* ts) {
    return ts == NULL || (ts->tv_nsec >= 0 && ts->tv_nsec < 1000000000);
}

// Acquires m->lock (Drepper's three-state mutex). Returns 0, or ETIMEDOUT/EINVAL.
static int lock_word(struct gmutex* m, const struct timespec* abs) {
    int c = 0;
    if (atomic_compare_exchange_strong(&m->lock, &c, 1)) return 0;
    if (!timespec_valid(abs)) return EINVAL;
    int shared = m->kind & G_MUTEX_PSHARED;
    if ((m->kind & G_MUTEX_KIND_MASK) == G_MUTEX_ADAPTIVE) {
        for (int i = 0; i < 100; i++) {
            c = 0;
            if (atomic_load_explicit(&m->lock, memory_order_relaxed) == 0 &&
                atomic_compare_exchange_strong(&m->lock, &c, 1)) {
                return 0;
            }
            __builtin_ia32_pause();
        }
    }
    while (atomic_exchange(&m->lock, 2) != 0) {
        int r = futex_wait(&m->lock, 2, abs, 1, shared);
        if (r == ETIMEDOUT) return ETIMEDOUT;
    }
    return 0;
}

static void unlock_word(struct gmutex* m) {
    if (atomic_exchange(&m->lock, 0) == 2) futex_wake(&m->lock, 1, m->kind & G_MUTEX_PSHARED);
}

static int mutex_lock(struct gmutex* m, const struct timespec* abs, int try_only) {
    int kind = m->kind & G_MUTEX_KIND_MASK;
    int tid = gettid();
    if (kind == G_MUTEX_RECURSIVE || kind == G_MUTEX_ERRORCHECK) {
        if (atomic_load_explicit(&m->owner, memory_order_relaxed) == tid) {
            if (kind == G_MUTEX_ERRORCHECK) return EDEADLK;
            if (m->count == UINT_MAX) return EAGAIN;
            m->count++;
            return 0;
        }
    }
    if (try_only) {
        int c = 0;
        if (!atomic_compare_exchange_strong(&m->lock, &c, 1)) return EBUSY;
    } else {
        int r = lock_word(m, abs);
        if (r) return r;
    }
    atomic_store_explicit(&m->owner, tid, memory_order_relaxed);
    m->count = 1;
    m->nusers++;
    return 0;
}

static int mutex_unlock(struct gmutex* m) {
    int kind = m->kind & G_MUTEX_KIND_MASK;
    if (kind == G_MUTEX_RECURSIVE || kind == G_MUTEX_ERRORCHECK) {
        if (atomic_load_explicit(&m->owner, memory_order_relaxed) != gettid()) return EPERM;
        if (--m->count > 0) return 0;
    }
    atomic_store_explicit(&m->owner, 0, memory_order_relaxed);
    m->nusers--;
    unlock_word(m);
    return 0;
}

// glibc's pthread_mutexattr_t is 4 bytes: kind in bits 0-27 and flags (pshared bit 31).
#define G_MATTR_PSHARED 0x80000000u
#define G_MATTR_KIND_MASK 0x0fffffffu

int nvg_pthread_mutexattr_init(uint32_t* a) {
    *a = G_MUTEX_NORMAL;
    return 0;
}

int nvg_pthread_mutexattr_destroy(uint32_t* a) {
    (void)a;
    return 0;
}

int nvg_pthread_mutexattr_settype(uint32_t* a, int kind) {
    if (kind < G_MUTEX_NORMAL || kind > G_MUTEX_ADAPTIVE) return EINVAL;
    *a = (*a & ~G_MATTR_KIND_MASK) | (uint32_t)kind;
    return 0;
}

int nvg_pthread_mutexattr_gettype(const uint32_t* a, int* kind) {
    *kind = (int)(*a & G_MATTR_KIND_MASK);
    return 0;
}

int nvg_pthread_mutexattr_setpshared(uint32_t* a, int pshared) {
    if (pshared != PTHREAD_PROCESS_PRIVATE && pshared != PTHREAD_PROCESS_SHARED) return EINVAL;
    *a = pshared == PTHREAD_PROCESS_SHARED ? (*a | G_MATTR_PSHARED) : (*a & ~G_MATTR_PSHARED);
    return 0;
}

int nvg_pthread_mutexattr_setprotocol(uint32_t* a, int protocol) {
    // Priority inheritance/protection are accepted and treated as PTHREAD_PRIO_NONE.
    (void)a;
    return protocol >= 0 && protocol <= 2 ? 0 : EINVAL;
}

int nvg_pthread_mutex_init(struct gmutex* m, const uint32_t* a) {
    memset(m, 0, sizeof(*m));
    if (a) {
        m->kind = (int)(*a & G_MUTEX_KIND_MASK) | ((*a & G_MATTR_PSHARED) ? G_MUTEX_PSHARED : 0);
    }
    return 0;
}

int nvg_pthread_mutex_destroy(struct gmutex* m) {
    return atomic_load(&m->lock) != 0 ? EBUSY : 0;
}

int nvg_pthread_mutex_lock(struct gmutex* m) {
    return mutex_lock(m, NULL, 0);
}

int nvg_pthread_mutex_trylock(struct gmutex* m) {
    return mutex_lock(m, NULL, 1);
}

int nvg_pthread_mutex_timedlock(struct gmutex* m, const struct timespec* abs) {
    return mutex_lock(m, abs, 0);
}

int nvg_pthread_mutex_unlock(struct gmutex* m) {
    return mutex_unlock(m);
}

// glibc's pthread_condattr_t is 4 bytes: bit 0 pshared, bits 1-7 the clock id.
int nvg_pthread_condattr_init(uint32_t* a) {
    *a = 0;
    return 0;
}

int nvg_pthread_condattr_destroy(uint32_t* a) {
    (void)a;
    return 0;
}

int nvg_pthread_condattr_setclock(uint32_t* a, clockid_t clock) {
    if (clock != CLOCK_REALTIME && clock != CLOCK_MONOTONIC) return EINVAL;
    *a = (*a & 1) | ((uint32_t)clock << 1);
    return 0;
}

int nvg_pthread_condattr_setpshared(uint32_t* a, int pshared) {
    if (pshared != PTHREAD_PROCESS_PRIVATE && pshared != PTHREAD_PROCESS_SHARED) return EINVAL;
    *a = (*a & ~1u) | (pshared == PTHREAD_PROCESS_SHARED ? 1u : 0u);
    return 0;
}

int nvg_pthread_cond_init(struct gcond* c, const uint32_t* a) {
    memset(c, 0, sizeof(*c));
    if (a) c->pshared_clock = (*a & 1) | (((*a >> 1) == CLOCK_MONOTONIC) ? 2 : 0);
    return 0;
}

int nvg_pthread_cond_destroy(struct gcond* c) {
    (void)c;
    return 0;
}

// A sequence-counter condition variable: a waiter sleeps on the value it saw before it released
// the mutex, so a signal in between makes the futex wait return at once. Spurious wakeups are
// allowed by POSIX.
static int cond_wait(struct gcond* c, struct gmutex* m, const struct timespec* abs) {
    if (!timespec_valid(abs)) return EINVAL;
    int kind = m->kind & G_MUTEX_KIND_MASK;
    if ((kind == G_MUTEX_RECURSIVE || kind == G_MUTEX_ERRORCHECK) &&
        atomic_load_explicit(&m->owner, memory_order_relaxed) != gettid()) {
        return EPERM;
    }
    unsigned int seq = atomic_load(&c->seq);
    unsigned int count = m->count;
    atomic_store_explicit(&m->owner, 0, memory_order_relaxed);
    m->count = 0;
    m->nusers--;
    unlock_word(m);

    int r = futex_wait((_Atomic int*)&c->seq, (int)seq, abs, !(c->pshared_clock & 2),
                       c->pshared_clock & 1);

    // Re-acquire as contended, so that unlock wakes the other waiters a broadcast released.
    while (atomic_exchange(&m->lock, 2) != 0) futex_wait(&m->lock, 2, NULL, 0, m->kind & G_MUTEX_PSHARED);
    atomic_store_explicit(&m->owner, gettid(), memory_order_relaxed);
    m->count = count;
    m->nusers++;
    return r == ETIMEDOUT ? ETIMEDOUT : 0;
}

int nvg_pthread_cond_wait(struct gcond* c, struct gmutex* m) {
    return cond_wait(c, m, NULL);
}

int nvg_pthread_cond_timedwait(struct gcond* c, struct gmutex* m, const struct timespec* abs) {
    return cond_wait(c, m, abs);
}

int nvg_pthread_cond_signal(struct gcond* c) {
    atomic_fetch_add(&c->seq, 1);
    futex_wake((_Atomic int*)&c->seq, 1, c->pshared_clock & 1);
    return 0;
}

int nvg_pthread_cond_broadcast(struct gcond* c) {
    atomic_fetch_add(&c->seq, 1);
    futex_wake((_Atomic int*)&c->seq, INT_MAX, c->pshared_clock & 1);
    return 0;
}

// glibc's PTHREAD_INHERIT_SCHED is 0 and PTHREAD_EXPLICIT_SCHED 1; bionic has them swapped.
int nvg_pthread_attr_setinheritsched(pthread_attr_t* attr, int inherit) {
    if (inherit != 0 && inherit != 1) return EINVAL;
    return pthread_attr_setinheritsched(attr, inherit == 0 ? PTHREAD_INHERIT_SCHED : PTHREAD_EXPLICIT_SCHED);
}

// glibc threads get RLIMIT_STACK (8 MiB) by default, bionic's 1 MiB; NVIDIA's shader compiler
// threads (LLVM) recurse deeply, so threads created with the default size get glibc's.
#define GLIBC_DEFAULT_STACK (8u << 20)

int nvg_pthread_create(pthread_t* t, const pthread_attr_t* attr, void* (*fn)(void*), void* arg) {
    pthread_attr_t def;
    pthread_attr_init(&def);
    size_t def_size = 0;
    pthread_attr_getstacksize(&def, &def_size);
    pthread_attr_t a;
    if (attr) {
        a = *attr;
    } else {
        a = def;
    }
    size_t size = 0;
    pthread_attr_getstacksize(&a, &size);
    void* stack = NULL;
    pthread_attr_getstack(&a, &stack, &size);
    if (stack == NULL && size == def_size) pthread_attr_setstacksize(&a, GLIBC_DEFAULT_STACK);
    int r = pthread_create(t, &a, fn, arg);
    pthread_attr_destroy(&def);
    return r;
}

int nvg_pthread_setaffinity_np(pthread_t t, size_t size, const cpu_set_t* set) {
    pid_t tid = pthread_gettid_np(t);
    if (tid < 0) return ESRCH;
    return sched_setaffinity(tid, size, set) == 0 ? 0 : errno;
}

int nvg_pthread_getaffinity_np(pthread_t t, size_t size, cpu_set_t* set) {
    pid_t tid = pthread_gettid_np(t);
    if (tid < 0) return ESRCH;
    return sched_getaffinity(tid, size, set) == 0 ? 0 : errno;
}

// ---- thread-specific keys ----
//
// bionic has a fixed number of pthread keys per process (PTHREAD_KEYS_MAX, 128) and NVIDIA's
// libraries create a good share of them as soon as an app opens EGL or Vulkan: Firefox's Rust
// code then failed to get one ("out of TLS keys", an abort right after its GL start). glibc's keys
// for NVIDIA are numbers in a table here instead, and all their values live in one array per
// thread behind a single bionic key. A key's generation tells a value set before the key was
// deleted (and its number reused) from a current one, as glibc's sequence numbers do.

#define NVG_KEYS_MAX 256
#define NVG_DESTRUCTOR_ITERATIONS 4  // glibc's PTHREAD_DESTRUCTOR_ITERATIONS

struct nvg_key {
    _Atomic unsigned int gen;  // odd: in use
    void (*_Atomic dtor)(void*);
};
struct nvg_value {
    unsigned int gen;
    void* value;
};

static struct nvg_key nvg_keys[NVG_KEYS_MAX];
static pthread_key_t nvg_values_key;
static pthread_once_t nvg_values_once = PTHREAD_ONCE_INIT;

static void nvg_values_free(void* p) {
    struct nvg_value* values = p;
    for (int round = 0; round < NVG_DESTRUCTOR_ITERATIONS; round++) {
        int called = 0;
        for (int k = 0; k < NVG_KEYS_MAX; k++) {
            unsigned int gen = atomic_load(&nvg_keys[k].gen);
            void (*dtor)(void*) = atomic_load(&nvg_keys[k].dtor);
            void* v = values[k].value;
            if (!v || values[k].gen != gen || !(gen & 1) || !dtor) continue;
            values[k].value = NULL;
            dtor(v);
            called = 1;
        }
        if (!called) break;
    }
    free(values);
}

static void nvg_values_init(void) {
    pthread_key_create(&nvg_values_key, nvg_values_free);
}

int nvg_pthread_key_create(unsigned int* key, void (*dtor)(void*)) {
    pthread_once(&nvg_values_once, nvg_values_init);
    for (int k = 0; k < NVG_KEYS_MAX; k++) {
        unsigned int gen = atomic_load(&nvg_keys[k].gen);
        if (gen & 1) continue;
        atomic_store(&nvg_keys[k].dtor, dtor);
        if (atomic_compare_exchange_strong(&nvg_keys[k].gen, &gen, gen + 1)) {
            *key = k;
            return 0;
        }
    }
    return EAGAIN;
}

int nvg___pthread_key_create(unsigned int* key, void (*dtor)(void*)) {
    return nvg_pthread_key_create(key, dtor);
}

int nvg_pthread_key_delete(unsigned int key) {
    if (key >= NVG_KEYS_MAX) return EINVAL;
    unsigned int gen = atomic_load(&nvg_keys[key].gen);
    if (!(gen & 1) || !atomic_compare_exchange_strong(&nvg_keys[key].gen, &gen, gen + 1))
        return EINVAL;
    return 0;
}

void* nvg_pthread_getspecific(unsigned int key) {
    if (key >= NVG_KEYS_MAX) return NULL;
    struct nvg_value* values = pthread_getspecific(nvg_values_key);
    if (!values || values[key].gen != atomic_load(&nvg_keys[key].gen)) return NULL;
    return values[key].value;
}

int nvg_pthread_setspecific(unsigned int key, const void* value) {
    if (key >= NVG_KEYS_MAX) return EINVAL;
    unsigned int gen = atomic_load(&nvg_keys[key].gen);
    if (!(gen & 1)) return EINVAL;
    struct nvg_value* values = pthread_getspecific(nvg_values_key);
    if (!values) {
        if (!value) return 0;
        values = calloc(NVG_KEYS_MAX, sizeof(*values));
        if (!values) return ENOMEM;
        pthread_setspecific(nvg_values_key, values);
    }
    values[key].gen = gen;
    values[key].value = (void*)value;
    return 0;
}
