/* wasm32 の最小の string (`kir_wasm_rt.c` が実体を持つ)。 */
#ifndef KSN_WASM_STRING_H
#define KSN_WASM_STRING_H
#include <stddef.h>
void *memcpy(void *d, const void *s, size_t n);
void *memmove(void *d, const void *s, size_t n);
void *memset(void *d, int c, size_t n);
int memcmp(const void *a, const void *b, size_t n);
size_t strlen(const char *s);
char *strchr(const char *s, int c);
int strncmp(const char *a, const char *b, size_t n);
#endif
