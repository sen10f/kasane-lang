/* stage0/tests/data/bindgen_c/sample.c — sample.h の実装。共有ライブラリにして bindgen の試験で呼ぶ。 */

#include "sample.h"

sample_flags sample_add(sample_flags a, sample_flags b) { return a + b; }

int sample_area(struct SampleRect r) {
    return (int)((r.max.x - r.min.x) * (r.max.y - r.min.y));
}

struct SampleRect sample_rect(double x0, double y0, double x1, double y1) {
    struct SampleRect r;
    r.min.x = x0; r.min.y = y0; r.max.x = x1; r.max.y = y1; r.tag = 0;
    return r;
}

void sample_grow(struct SampleRect *r, double d) {
    r->min.x -= d; r->min.y -= d; r->max.x += d; r->max.y += d;
}

int sample_each(struct SampleRect r, SampleFn f, void *user) {
    (void)r;
    return f(1, user) + f(2, user);
}
