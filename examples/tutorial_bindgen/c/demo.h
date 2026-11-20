/* examples/tutorial_bindgen/c/demo.h */
#ifndef DEMO_H
#define DEMO_H

typedef enum { RED = 0, GREEN = 1, BLUE = 2 } Color;

typedef struct { double x; double y; } Point;

#define MAX_POINTS 100

double area(Point p);
Point make_point(double x, double y);
void for_each(Point p, void (*cb)(double x, double y, void *user), void *user);

#endif
