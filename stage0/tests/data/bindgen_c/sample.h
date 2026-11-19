/* stage0/tests/data/bindgen_c/sample.h — `ksn bindgen --c` の試験用の見出し。
   構造体 (入れ子・固定長配列)・enum・#define・typedef・関数ポインタと、
   Kasane で表現できないもの (ビットフィールド・union・可変長配列・varargs) を混ぜる。 */

#ifndef SAMPLE_H
#define SAMPLE_H

#include <stdint.h>
#include <stddef.h>

#define SAMPLE_MAX 64
#define SAMPLE_MASK 0xFFu
#define SAMPLE_BIG 0x100000000LL
#define SAMPLE_NEG (-3)
#define SAMPLE_NAME "sample" /* 整数でないので飛ばす */

typedef unsigned long sample_flags;
typedef struct SamplePoint SamplePoint; /* 前方宣言の typedef */

enum SampleColor { SAMPLE_RED = 1, SAMPLE_GREEN = 2, SAMPLE_BLUE = 4 };

struct SamplePoint { double x; double y; };
struct SampleRect { SamplePoint min, max; int tag; };
struct SampleBuf { char name[16]; size_t len; };

typedef int (*SampleFn)(int code, void *user);

struct SampleSkipped { int bits : 3; int ok; }; /* ビットフィールド → 飛ぶ */
union SampleUnion { int i; float f; };          /* union → 飛ぶ */
struct SampleFlex { int n; char data[]; };      /* 可変長配列 → 飛ぶ */

sample_flags sample_add(sample_flags a, sample_flags b);
int sample_area(struct SampleRect r);
struct SampleRect sample_rect(double x0, double y0, double x1, double y1);
void sample_grow(struct SampleRect *r, double d);
int sample_each(struct SampleRect r, SampleFn f, void *user);
int sample_varargs(const char *fmt, ...); /* varargs → 飛ぶ */
static inline int sample_inline(void) { return 1; } /* 外部リンケージでない → 飛ぶ */

#endif
