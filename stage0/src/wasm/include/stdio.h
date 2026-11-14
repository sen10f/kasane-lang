/* wasm32 の素の実行時 (`kir_wasm_rt.c` が実体を持つ) の最小の stdio。`ksn build --target wasm32-unknown-unknown` 専用。 */
#ifndef KSN_WASM_STDIO_H
#define KSN_WASM_STDIO_H
#include <stddef.h>
#include <stdarg.h>
int printf(const char *fmt, ...);
int dprintf(int fd, const char *fmt, ...);
int sprintf(char *buf, const char *fmt, ...);
int snprintf(char *buf, size_t n, const char *fmt, ...);
#endif
