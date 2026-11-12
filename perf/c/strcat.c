/* perf/strcat の C 版: 文字列への追記（末尾へ 2 バイトずつ）。 */
#include "common.h"

int main(int argc, char **argv) {
    uint64_t n = perf_arg(argc, argv, "N", 200000);

    size_t cap = 16;
    size_t len = 0;
    char *buf = malloc(cap);
    if (buf == NULL) {
        return 1;
    }
    for (uint64_t i = 0; i < n; i++) {
        if (len + 2 > cap) {
            cap *= 2;
            char *grown = realloc(buf, cap);
            if (grown == NULL) {
                return 1;
            }
            buf = grown;
        }
        memcpy(buf + len, "ab", 2);
        len += 2;
    }
    free(buf);

    printf("EXPECT_LEN=%llu\n", (unsigned long long)len);
    return 0;
}
