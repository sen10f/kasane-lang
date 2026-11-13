/* perf/fib の C 版: 素朴な再帰 fib。 */
#include "common.h"

static uint64_t fib(uint64_t n) {
    return n < 2 ? n : fib(n - 1) + fib(n - 2);
}

int main(int argc, char **argv) {
    uint64_t n = perf_arg(argc, argv, "N", 27);
    printf("EXPECT=%llu\n", (unsigned long long)fib(n));
    return 0;
}
