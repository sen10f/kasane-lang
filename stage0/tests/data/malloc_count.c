#define _GNU_SOURCE
#include <dlfcn.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
#include <string.h>

static void *(*real_realloc)(void *, size_t);
static void (*real_free)(void *);
static void *(*real_malloc)(size_t);
static void *(*real_calloc)(size_t, size_t);
static long allocs = 0, frees = 0;
static char boot[1 << 16];
static size_t boot_used = 0;
static int in_init = 0;

static void init(void) {
    if (real_realloc) return;
    in_init = 1;
    real_realloc = dlsym(RTLD_NEXT, "realloc");
    real_free = dlsym(RTLD_NEXT, "free");
    real_malloc = dlsym(RTLD_NEXT, "malloc");
    real_calloc = dlsym(RTLD_NEXT, "calloc");
    in_init = 0;
}

void *malloc(size_t n) {
    if (in_init) { void *p = boot + boot_used; boot_used += (n + 15) & ~15UL; return p; }
    init();
    __atomic_fetch_add(&allocs, 1, __ATOMIC_RELAXED);
    return real_malloc(n);
}

/* LLVM のネイティブ (kir) はゼロ初期化の確保に calloc を使う。数えないと確保より解放が多く見える。
 * `dlsym` 自身が calloc を呼ぶので、初期化中は静的な領域 (ゼロ) から返す。 */
void *calloc(size_t a, size_t b) {
    if (in_init) { size_t n = a * b; void *p = boot + boot_used; boot_used += (n + 15) & ~15UL; return p; }
    init();
    __atomic_fetch_add(&allocs, 1, __ATOMIC_RELAXED);
    return real_calloc(a, b);
}

void *realloc(void *p, size_t n) {
    init();
    if (p == NULL) __atomic_fetch_add(&allocs, 1, __ATOMIC_RELAXED);
    return real_realloc(p, n);
}

void free(void *p) {
    if ((char *)p >= boot && (char *)p < boot + sizeof boot) return;
    init();
    if (p != NULL) __atomic_fetch_add(&frees, 1, __ATOMIC_RELAXED);
    real_free(p);
}

__attribute__((destructor)) static void report(void) {
    char buf[96];
    int n = snprintf(buf, sizeof buf, "allocs=%ld frees=%ld\n", allocs, frees);
    write(2, buf, n);
}
