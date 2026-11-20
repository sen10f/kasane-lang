/* examples/tutorial_bindgen/c/demo.c
   `build.sh` で libdemo.so にし、Kasane から動的ロードで呼ぶ。 */

#include "demo.h"

double area(Point p) { return (p.x > 0.0 ? p.x : -p.x) * (p.y > 0.0 ? p.y : -p.y); }

Point make_point(double x, double y) { Point p = { x, y }; return p; }

void for_each(Point p, void (*cb)(double x, double y, void *user), void *user) {
    cb(p.x, p.y, user);
    cb(-p.x, -p.y, user);
}
