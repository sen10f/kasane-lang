/* examples/tutorial_ffi/c/ffi.c
   `build.sh` で libffi.so にし、Kasane から動的ロードで呼ぶ。
   構造体を値で渡す・値で返す、ポインタで書き換える、コールバックを呼ぶ。 */

typedef struct { double x; double y; } Point;
typedef struct { Point min; Point max; } Rect;

double rect_area(Rect r) { return (r.max.x - r.min.x) * (r.max.y - r.min.y); }

Rect rect_from(Point a, Point b) { Rect r = { a, b }; return r; }

void rect_grow(Rect *r, double d) {
    r->min.x -= d;
    r->min.y -= d;
    r->max.x += d;
    r->max.y += d;
}

typedef void (*vertex_fn)(double x, double y, void *user);
void rect_each_vertex(Rect r, vertex_fn f, void *user) {
    f(r.min.x, r.min.y, user);
    f(r.max.x, r.min.y, user);
    f(r.max.x, r.max.y, user);
    f(r.min.x, r.max.y, user);
}
