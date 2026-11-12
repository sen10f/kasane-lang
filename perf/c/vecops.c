/* perf/vecops の C 版: 動的配列への push・添字読み・添字書き。 */
#include "common.h"

int main(int argc, char **argv) {
    uint64_t n = perf_arg(argc, argv, "N", 300000);

    uint64_t *v = malloc((size_t)n * sizeof(uint64_t));
    if (v == NULL) {
        return 1;
    }
    for (uint64_t i = 0; i < n; i++) {
        v[i] = i * 7 + 1;
    }
    uint64_t before = 0;
    for (uint64_t i = 0; i < n; i++) {
        before += v[i];
    }
    for (uint64_t i = 0; i < n; i++) {
        v[i] = v[i] * 3;
    }
    uint64_t after = 0;
    for (uint64_t i = 0; i < n; i++) {
        after += v[i];
    }
    free(v);

    printf("EXPECT_BEFORE=%llu\n", (unsigned long long)before);
    printf("EXPECT_AFTER=%llu\n", (unsigned long long)after);
    return 0;
}
