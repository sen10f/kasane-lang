/* examples/ffi/shapes の C ライブラリ。`build.sh` で libshapes.so にして、Kasane から動的ロードで呼ぶ。 */

typedef struct { double x; double y; } Point;        /* SSE, SSE (16 バイト) */
typedef struct { Point min; Point max; } Rect;       /* MEMORY (32 バイト) */
typedef struct { int w; int h; } Size;               /* INTEGER (8 バイト) */

/* 構造体を値で受ける */
double rect_area(Rect r) { return (r.max.x - r.min.x) * (r.max.y - r.min.y); }
int size_area(Size s) { return s.w * s.h; }

/* 構造体を値で返す (メモリ・レジスタの両方) */
Rect rect_from(Point a, Point b) { Rect r = { a, b }; return r; }
Size size_make(int w, int h) { Size s = { w, h }; return s; }

/* ポインタで受けて書き換える */
void rect_grow(Rect *r, double d) {
    r->min.x -= d;
    r->min.y -= d;
    r->max.x += d;
    r->max.y += d;
}

/* コールバック: 頂点ごとに呼ぶ。`user` は呼び出し側の状態 (`void *`) */
typedef void (*vertex_fn)(double x, double y, void *user);
void rect_each_vertex(Rect r, vertex_fn f, void *user) {
    f(r.min.x, r.min.y, user);
    f(r.max.x, r.min.y, user);
    f(r.max.x, r.max.y, user);
    f(r.min.x, r.max.y, user);
}
