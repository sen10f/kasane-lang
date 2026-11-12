/* perf/structs の C 版: 構造体のメソッド呼び出し（&mut self の更新と &self の読み）。 */
#include "common.h"

typedef struct {
    int64_t x;
    int64_t y;
    int64_t vx;
    int64_t vy;
} Body;

static void body_step(Body *b) {
    b->x = (int64_t)((uint64_t)b->x + (uint64_t)b->vx);
    b->y = (int64_t)((uint64_t)b->y + (uint64_t)b->vy);
    if (b->x > 1000 || b->x < -1000) {
        b->vx = (int64_t)(0 - (uint64_t)b->vx);
    }
    if (b->y > 1000 || b->y < -1000) {
        b->vy = (int64_t)(0 - (uint64_t)b->vy);
    }
}

static int64_t body_energy(const Body *b) {
    return (int64_t)((uint64_t)b->x * 3u + (uint64_t)b->y);
}

int main(int argc, char **argv) {
    uint64_t n = perf_arg(argc, argv, "N", 1000000);

    Body b = {0, 0, 7, -5};
    uint64_t acc = 0;
    for (uint64_t i = 0; i < n; i++) {
        body_step(&b);
        acc += (uint64_t)body_energy(&b);
    }

    printf("EXPECT_ACC=%lld\n", (long long)(int64_t)acc);
    return 0;
}
