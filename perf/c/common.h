/* 計測用 C 版の共通部品。入力は KEY=VALUE 形式の引数で受け取る。
 * 出力は EXPECT_*=VALUE の行。run_perf.py がこれを読み、Kasane 側の
 * EXPECT 定数と突き合わせる。 */
#ifndef PERF_COMMON_H
#define PERF_COMMON_H

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static uint64_t perf_arg(int argc, char **argv, const char *key, uint64_t dflt) {
    size_t klen = strlen(key);
    for (int i = 1; i < argc; i++) {
        if (strncmp(argv[i], key, klen) == 0 && argv[i][klen] == '=') {
            return strtoull(argv[i] + klen + 1, NULL, 10);
        }
    }
    return dflt;
}

#endif
