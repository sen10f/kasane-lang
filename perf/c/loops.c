/* perf/loops の C 版: 整数ループの総和と素数ふるい。 */
#include "common.h"

int main(int argc, char **argv) {
    uint64_t n_sum = perf_arg(argc, argv, "N_SUM", 3000000);
    uint64_t n_sieve = perf_arg(argc, argv, "N_SIEVE", 200000);

    uint64_t sum = 0;
    for (uint64_t i = 0; i < n_sum; i++) {
        sum += i;
    }

    unsigned char *flags = malloc((size_t)n_sieve);
    if (flags == NULL) {
        return 1;
    }
    memset(flags, 1, (size_t)n_sieve);
    flags[0] = 0;
    flags[1] = 0;
    for (uint64_t p = 2; p * p < n_sieve; p++) {
        if (flags[p]) {
            for (uint64_t m = p * p; m < n_sieve; m += p) {
                flags[m] = 0;
            }
        }
    }
    uint64_t primes = 0;
    for (uint64_t q = 0; q < n_sieve; q++) {
        if (flags[q]) {
            primes++;
        }
    }
    free(flags);

    printf("EXPECT_SUM=%llu\n", (unsigned long long)sum);
    printf("EXPECT_PRIMES=%llu\n", (unsigned long long)primes);
    return 0;
}
