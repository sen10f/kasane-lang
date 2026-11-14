/* wasm32 の最小の math: 命令に直る組み込みだけ (libm は持たない)。 */
#ifndef KSN_WASM_MATH_H
#define KSN_WASM_MATH_H
#define isnan(x) __builtin_isnan(x)
#define isinf(x) __builtin_isinf(x)
#define fabs(x) __builtin_fabs(x)
#define trunc(x) __builtin_trunc(x)
#endif
