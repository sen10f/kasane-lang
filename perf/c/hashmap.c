/* perf/hashmap の C 版: 整数キーの挿入と検索。
 * 参照の表は配列で持つ（検索は O(1)）。Kasane 側の HashMap は実体が
 * 組の列で検索が線形なので、両者は同じ結果を返すが速度は大きく違う。 */
#include "common.h"

int main(int argc, char **argv) {
    uint64_t n = perf_arg(argc, argv, "N", 2000);

    uint64_t *table = malloc((size_t)n * sizeof(uint64_t));
    if (table == NULL) {
        return 1;
    }
    for (uint64_t i = 0; i < n; i++) {
        table[i] = i * 3 + 1;
    }
    uint64_t hits = 0;
    uint64_t misses = 0;
    for (uint64_t k = 0; k < 2 * n; k++) {
        if (k < n) {
            hits += table[k];
        } else {
            misses++;
        }
    }
    free(table);

    printf("EXPECT_HITS=%llu\n", (unsigned long long)hits);
    printf("EXPECT_MISSES=%llu\n", (unsigned long long)misses);
    return 0;
}
