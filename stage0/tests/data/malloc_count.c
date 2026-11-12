#define _GNU_SOURCE
#include <dlfcn.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
#include <string.h>

static void *(*real_realloc)(void *, size_t);
static void (*real_free)(void *);
static void *(*real_malloc)(size_t);
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
    in_init = 0;
}

void *malloc(size_t n) {
    if (in_init) { void *p = boot + boot_used; boot_used += (n + 15) & ~15UL; return p; }
    init();
    allocs++;
    return real_malloc(n);
}

void *realloc(void *p, size_t n) {
    init();
    if (p == NULL) allocs++;
    return real_realloc(p, n);
}

void free(void *p) {
    if ((char *)p >= boot && (char *)p < boot + sizeof boot) return;
    init();
    if (p != NULL) frees++;
    real_free(p);
}

__attribute__((destructor)) static void report(void) {
    char buf[96];
    int n = snprintf(buf, sizeof buf, "allocs=%ld frees=%ld\n", allocs, frees);
    write(2, buf, n);
}
