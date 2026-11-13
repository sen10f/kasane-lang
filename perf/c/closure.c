/* perf/closure の C 版: 関数（ここでは 1 引数の関数ポインタ）を繰り返し呼ぶ。
 * 関数ポインタは別翻訳単位へ逃がし、呼び出しを間接にしておく。 */
#include "common.h"

static uint64_t scale_k = 7;

__attribute__((noinline)) static uint64_t step_fn(uint64_t x) {
    return x * scale_k + 1;
}

static uint64_t apply_n(uint64_t (*f)(uint64_t), uint64_t n) {
    uint64_t acc = 0;
    for (uint64_t i = 0; i < n; i++) {
        acc += f(i);
    }
    return acc;
}

int main(int argc, char **argv) {
    uint64_t n = perf_arg(argc, argv, "N", 1000000);
    printf("EXPECT=%llu\n", (unsigned long long)apply_n(step_fn, n));
    return 0;
}
