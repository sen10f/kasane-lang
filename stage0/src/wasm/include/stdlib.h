/* wasm32 の最小の stdlib (`kir_wasm_rt.c` が実体を持つ)。 */
#ifndef KSN_WASM_STDLIB_H
#define KSN_WASM_STDLIB_H
#include <stddef.h>
void *malloc(size_t n);
void *calloc(size_t n, size_t m);
void *realloc(void *p, size_t n);
void free(void *p);
void exit(int code) __attribute__((noreturn));
double strtod(const char *s, char **end);
float strtof(const char *s, char **end);
unsigned long long strtoull(const char *s, char **end, int base);
int atoi(const char *s);
char *getenv(const char *name);
#endif
