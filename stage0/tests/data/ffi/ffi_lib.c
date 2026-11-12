#include <stdlib.h>
#include <stdint.h>

typedef struct { int a; int b; } Small;
typedef struct { float x; float y; } Vec2;
typedef struct { double x; double y; } Vec2d;
typedef struct { long a; double b; } Mixed;
typedef struct { long a, b, c; } Big;
typedef struct { Small s; double d; } Nested;
typedef struct { char c; short s; int i; double d; } Padded;
typedef struct { int v[3]; } Arr3;
typedef struct { Small s; Vec2 v; double t; } Deep;
typedef struct { float a; float b; float c; } F3;

Small small_add(Small a, Small b) { Small r = { a.a + b.a, a.b + b.b }; return r; }
Vec2 vec2_scale(Vec2 v, float k) { Vec2 r = { v.x * k, v.y * k }; return r; }
Vec2d vec2d_add(Vec2d a, Vec2d b) { Vec2d r = { a.x + b.x, a.y + b.y }; return r; }
Mixed mixed_make(long a, double b) { Mixed m = { a, b }; return m; }
double mixed_sum(Mixed m) { return (double)m.a + m.b; }
Big big_make(long a) { Big b = { a, a * 2, a * 3 }; return b; }
long big_sum(Big b) { return b.a + b.b + b.c; }
Big big_scale(Big b, long k) { Big r = { b.a * k, b.b * k, b.c * k }; return r; }
Nested nested_make(int a, int b, double d) { Nested n = { { a, b }, d }; return n; }
double nested_sum(Nested n) { return (double)(n.s.a + n.s.b) + n.d; }
void small_inc(Small *s) { s->a += 1; s->b += 10; }
Small *small_new(int a, int b) { Small *s = malloc(sizeof(Small)); s->a = a; s->b = b; return s; }
void small_free(Small *s) { free(s); }
Padded padded_make(char c, short s, int i, double d) { Padded p = { c, s, i, d }; return p; }
double padded_sum(Padded p) { return (double)p.c + (double)p.s + (double)p.i + p.d; }
int arr3_sum(Arr3 a) { return a.v[0] + a.v[1] + a.v[2]; }
Arr3 arr3_make(int x) { Arr3 a = { { x, x + 1, x + 2 } }; return a; }
double deep_sum(Deep d) { return (double)(d.s.a + d.s.b) + (double)d.v.x + (double)d.v.y + d.t; }
F3 f3_make(float a) { F3 r = { a, a + 1, a + 2 }; return r; }
float f3_sum(F3 f) { return f.a + f.b + f.c; }

/* 6 個を超える整数・8 個を超える浮動小数・混在 */
double many(long a, int b, double c, float d, long e, int f, double g, long h, long i, double j,
            int k, float l, double m, double n, double o, double p, double q, double r) {
    return (double)a + b * 2.0 + c * 3.0 + d * 4.0 + e * 5.0 + f * 6.0 + g * 7.0 + h * 8.0 + i * 9.0 + j * 10.0 +
           k * 11.0 + l * 12.0 + m * 13.0 + n * 14.0 + o * 15.0 + p * 16.0 + q * 17.0 + r * 18.0;
}
/* 構造体を、レジスタが足りなくなる位置で渡す */
long spill(long a, long b, long c, long d, long e, Small s, long f) { return a + b + c + d + e + s.a * 100 + s.b * 1000 + f; }
long spill_big(long a, long b, long c, long d, long e, long f, Big big, long g) { return a + b + c + d + e + f + big.a + big.b * 10 + big.c * 100 + g * 1000; }

/* コールバック: 回数ぶん呼んで、ユーザーデータを渡す */
typedef void (*tick_fn)(int, void *);
void run_ticks(int n, tick_fn f, void *user) { for (int i = 1; i <= n; i++) f(i, user); }
typedef double (*dbl_fn)(double, float, int);
double apply_dbl(dbl_fn f, double a, float b, int c) { return f(a, b, c) + 1.0; }

/* コールバックを 1 回呼ぶ (スタックオーバーフローの試験用) */
typedef void (*fn0)(void);
void run_once(fn0 f) { f(); }
