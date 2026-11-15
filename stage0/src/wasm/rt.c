/* wasm32-unknown-unknown の最小の実行時 (`ksn build --target wasm32-unknown-unknown`、修正プラン 5-3)。
 *
 * 素の wasm には libc もホストの資源もない。ここでは、生成した LLVM IR が呼ぶ libc の関数だけを定義する:
 *   - 確保: 線形メモリ上の first-fit の空きリスト (`memory.grow` で伸ばす)。
 *   - 書式: printf 系の最小 (`%d %u %x %c %s %%` と `l` / `ll`、幅なし)。浮動小数の書式 (`%e %f %g`) は出さない。
 *   - 入出力: `env` の import 2 つだけ (WASM ABI の章): `ksn_write(fd, ptr, len) -> i32` と `ksn_exit(code)`。
 *     `KSN_WASI` を定義してビルドするとき (wasm32-wasi) は、この 2 つと getenv を `wasi.c` が定義する (WASI の fd_write / proc_exit / environ_get)。
 *   - 入口: `ksn_main() -> i32` を export する (IR の `main` を呼ぶ)。
 * 浮動小数の文字列化・getenv・時刻・ファイル・ソケット・プロセスは対応しない (`wasm_unsupported` が生成前に止める)。
 * このファイルは clang の `--target=wasm32-unknown-unknown -ffreestanding -fno-builtin` で作る (`kir_llvm` の `wasm_link`)。 */
#include <stdarg.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* wasm32-wasi は wasi.c が定義する (WASI の fd_write / proc_exit)。素の wasm は env の import。 */
#ifndef KSN_WASI
__attribute__((import_module("env"), import_name("ksn_write")))
#endif
int ksn_write(int fd, const void *p, size_t n);
#ifndef KSN_WASI
__attribute__((import_module("env"), import_name("ksn_exit")))
#endif
void ksn_exit(int code);

int main(int argc, char **argv);

__attribute__((export_name("ksn_main"))) int ksn_main(void) { return main(0, 0); }

/* ---- 確保 */

struct blk {
    size_t cap;        /* 本体の大きさ (ヘッダは含まない) */
    struct blk *next;  /* 空きリストの次 (使用中は使わない) */
    size_t pad[2];     /* ヘッダを 16 バイトにそろえる */
};

extern unsigned char __heap_base;
static unsigned char *heap_top;
static unsigned char *heap_end;
static struct blk *free_list;

/* オーバーフローチェック付きの整列関数 */
static size_t align16(size_t n) {
    if (n > SIZE_MAX - 15) return 0; /* 溢れを示す */
    return (n + 15) & ~(size_t)15;
}

static int heap_grow(size_t need) {
    if (!heap_top) {
        heap_top = (unsigned char *)(((uintptr_t)&__heap_base + 15) & ~(uintptr_t)15);
        heap_end = heap_top;
    }
    size_t have = (size_t)(heap_end - heap_top);
    if (have >= need) return 1;
    /* need - have が溢れないことは have < need より保証される */
    size_t diff = need - have;
    if (diff > SIZE_MAX - 65535) return 0; /* ページ計算で溢れる */
    size_t pages = (diff + 65535) / 65536;
    int old = __builtin_wasm_memory_grow(0, (int)pages);
    if (old < 0) return 0;
    /* (old + pages) * 65536 の溢れチェック: wasm32 では 4 GiB が上限なので問題ない */
    heap_end = (unsigned char *)((size_t)(old + (int)pages) * 65536);
    return 1;
}

void *malloc(size_t n) {
    size_t want = align16(n ? n : 1);
    if (want == 0) return 0; /* align16 で溢れた */
    struct blk **pp = &free_list;
    for (struct blk *b = free_list; b; pp = &b->next, b = b->next) {
        if (b->cap >= want) {
            *pp = b->next;
            return (void *)(b + 1);
        }
    }
    if (want > SIZE_MAX - sizeof(struct blk)) return 0; /* need 計算で溢れる */
    size_t need = sizeof(struct blk) + want;
    if (!heap_grow(need)) return 0;
    struct blk *b = (struct blk *)heap_top;
    heap_top += need;
    b->cap = want;
    b->next = 0;
    return (void *)(b + 1);
}

void free(void *p) {
    if (!p) return;
    struct blk *b = (struct blk *)p - 1;
    b->next = free_list;
    free_list = b;
}

void *realloc(void *p, size_t n) {
    if (!p) return malloc(n);
    struct blk *b = (struct blk *)p - 1;
    if (b->cap >= n) return p;
    void *q = malloc(n);
    if (!q) return 0;
    memcpy(q, p, b->cap);
    free(p);
    return q;
}

void *calloc(size_t n, size_t m) {
    if (n != 0 && m > SIZE_MAX / n) return 0; /* 溢れ */
    size_t total = n * m;
    void *p = malloc(total);
    if (p) memset(p, 0, total);
    return p;
}

/* ---- 文字列・メモリ (ループは `-fno-builtin` の下で、自分自身を呼ばない形にする) */

void *memcpy(void *d, const void *s, size_t n) {
    unsigned char *o = (unsigned char *)d;
    const unsigned char *i = (const unsigned char *)s;
    for (size_t k = 0; k < n; k++) o[k] = i[k];
    return d;
}

void *memmove(void *d, const void *s, size_t n) {
    unsigned char *o = (unsigned char *)d;
    const unsigned char *i = (const unsigned char *)s;
    if (o < i) {
        for (size_t k = 0; k < n; k++) o[k] = i[k];
    } else {
        for (size_t k = n; k > 0; k--) o[k - 1] = i[k - 1];
    }
    return d;
}

void *memset(void *d, int c, size_t n) {
    unsigned char *o = (unsigned char *)d;
    for (size_t k = 0; k < n; k++) o[k] = (unsigned char)c;
    return d;
}

int memcmp(const void *a, const void *b, size_t n) {
    const unsigned char *x = (const unsigned char *)a, *y = (const unsigned char *)b;
    for (size_t k = 0; k < n; k++) {
        if (x[k] != y[k]) return x[k] < y[k] ? -1 : 1;
    }
    return 0;
}

size_t strlen(const char *s) {
    size_t n = 0;
    while (s[n]) n++;
    return n;
}

char *strchr(const char *s, int c) {
    for (;; s++) {
        if (*s == (char)c) return (char *)s;
        if (!*s) return 0;
    }
}

int strncmp(const char *a, const char *b, size_t n) {
    for (size_t k = 0; k < n; k++) {
        unsigned char x = (unsigned char)a[k], y = (unsigned char)b[k];
        if (x != y) return x < y ? -1 : 1;
        if (!x) return 0;
    }
    return 0;
}

/* ---- 入出力と終了 */

/* errno: 書けなかったときは EIO (5)。IR の `ksn.write` は失敗の番号を読んで `ErrorKind` に直す。 */
static int errno_value;
int *__errno_location(void) { return &errno_value; }

/* POSIX の `write` (ssize_t は wasm32 では long の 32 ビット)。IR の `ksn.write` / `ksn.eprint` が呼ぶ。
 * `ksn_write` の結果が負なら -1 を返し、errno を EIO にする。 */
long write(int fd, const void *p, size_t n) {
    int r = ksn_write(fd, p, n);
    if (r < 0) {
        errno_value = 5;
        return -1;
    }
    return (long)r;
}

/* `exit` を通った (panic・終了コード) なら 1。JS が `ksn_exit` の例外を捕まえた後の `ksn_resume` を拒む。 */
int ksn_exited;

void exit(int code) {
    ksn_exited = 1;
    ksn_exit(code);
    __builtin_unreachable();
}

/* 書式: 出力先の関数と、一時の小さな領域。`limit` が 0 なら無制限 (`dst` へ書かず、`emit` の関数へ流す)。 */
struct sink {
    int fd;          /* 0 なら `buf` へ書く */
    char *buf;
    size_t cap;      /* `buf` の大きさ (NUL の分を含む) */
    size_t used;     /* 書いた総バイト数 (切り詰めても数える) */
    char tmp[128];
    size_t ntmp;
};

static void sink_flush(struct sink *k) {
    if (k->fd && k->ntmp) ksn_write(k->fd, k->tmp, k->ntmp);
    k->ntmp = 0;
}

static void sink_put(struct sink *k, char c) {
    k->used++;
    if (k->fd) {
        k->tmp[k->ntmp++] = c;
        if (k->ntmp == sizeof k->tmp) sink_flush(k);
    } else if (k->buf && k->used < k->cap) {
        k->buf[k->used - 1] = c;
    }
}

static void sink_end(struct sink *k) {
    if (k->fd) {
        sink_flush(k);
    } else if (k->buf && k->cap) {
        size_t at = k->used < k->cap ? k->used : k->cap - 1;
        k->buf[at] = 0;
    }
}

static void put_unsigned(struct sink *k, unsigned long long v, unsigned base, int upper) {
    char tmp[24];
    int n = 0;
    do {
        unsigned d = (unsigned)(v % base);
        tmp[n++] = (char)(d < 10 ? '0' + d : (upper ? 'A' : 'a') + d - 10);
        v /= base;
    } while (v);
    while (n) sink_put(k, tmp[--n]);
}

static void vformat(struct sink *k, const char *fmt, va_list ap) {
    for (; *fmt; fmt++) {
        if (*fmt != '%') {
            sink_put(k, *fmt);
            continue;
        }
        fmt++;
        int longs = 0;
        while (*fmt == 'l') {
            longs++;
            fmt++;
        }
        switch (*fmt) {
        case 'd':
        case 'i': {
            long long v = longs ? va_arg(ap, long long) : (long long)va_arg(ap, int);
            if (v < 0) {
                sink_put(k, '-');
                put_unsigned(k, (unsigned long long)(-(v + 1)) + 1u, 10, 0);
            } else {
                put_unsigned(k, (unsigned long long)v, 10, 0);
            }
            break;
        }
        case 'u':
            put_unsigned(k, longs ? va_arg(ap, unsigned long long) : (unsigned long long)va_arg(ap, unsigned), 10, 0);
            break;
        case 'x':
        case 'X':
            put_unsigned(k, longs ? va_arg(ap, unsigned long long) : (unsigned long long)va_arg(ap, unsigned), 16, *fmt == 'X');
            break;
        case 'c':
            sink_put(k, (char)va_arg(ap, int));
            break;
        case 's': {
            const char *s = va_arg(ap, const char *);
            if (!s) s = "(null)";
            for (; *s; s++) sink_put(k, *s);
            break;
        }
        case '%':
            sink_put(k, '%');
            break;
        default:
            /* 浮動小数などは生成しない (wasm_unsupported が止める)。綴りはそのまま出す。 */
            sink_put(k, '%');
            sink_put(k, *fmt);
            break;
        }
    }
}

int dprintf(int fd, const char *fmt, ...) {
    struct sink k = {0};
    k.fd = fd ? fd : 2;
    va_list ap;
    va_start(ap, fmt);
    vformat(&k, fmt, ap);
    va_end(ap);
    sink_end(&k);
    return (int)k.used;
}

int printf(const char *fmt, ...) {
    struct sink k = {0};
    k.fd = 1;
    va_list ap;
    va_start(ap, fmt);
    vformat(&k, fmt, ap);
    va_end(ap);
    sink_end(&k);
    return (int)k.used;
}

int sprintf(char *buf, const char *fmt, ...) {
    struct sink k = {0};
    k.buf = buf;
    k.cap = (size_t)-1 / 2;
    va_list ap;
    va_start(ap, fmt);
    vformat(&k, fmt, ap);
    va_end(ap);
    buf[k.used] = 0;
    return (int)k.used;
}

int snprintf(char *buf, size_t n, const char *fmt, ...) {
    struct sink k = {0};
    k.buf = buf;
    k.cap = n;
    va_list ap;
    va_start(ap, fmt);
    vformat(&k, fmt, ap);
    va_end(ap);
    sink_end(&k);
    return (int)k.used;
}

/* ---- 変換 (浮動小数は対応しない。kir_rt.c の書式補助だけが参照し、IR からは呼ばれない) */

double strtod(const char *s, char **end) {
    (void)s;
    if (end) *end = (char *)s;
    __builtin_trap();
}

float strtof(const char *s, char **end) {
    (void)s;
    if (end) *end = (char *)s;
    __builtin_trap();
}

unsigned long long strtoull(const char *s, char **end, int base) {
    unsigned long long v = 0;
    while (*s == ' ') s++;
    if (base == 0) base = 10;
    for (;; s++) {
        int d;
        if (*s >= '0' && *s <= '9') d = *s - '0';
        else if (*s >= 'a' && *s <= 'z') d = *s - 'a' + 10;
        else if (*s >= 'A' && *s <= 'Z') d = *s - 'A' + 10;
        else break;
        if (d >= base) break;
        v = v * (unsigned long long)base + (unsigned long long)d;
    }
    if (end) *end = (char *)s;
    return v;
}

int atoi(const char *s) {
    int neg = 0, v = 0;
    while (*s == ' ') s++;
    if (*s == '-') {
        neg = 1;
        s++;
    }
    while (*s >= '0' && *s <= '9') v = v * 10 + (*s++ - '0');
    return neg ? -v : v;
}

#ifndef KSN_WASI
/* 環境変数はない (std.env は wasm32-unknown-unknown では未対応。wasm32-wasi は wasi.c の getenv)。 */
char *getenv(const char *name) {
    (void)name;
    return 0;
}
#endif

/* ---- 128 ビット整数の組み込み (libcall)。
 * wasm32 には i128 の命令がないので、LLVM は `__multi3` などを呼ぶ。呼び出しの形: 結果はポインタの第 1 引数へ書き
 * (lo が +0、hi が +8)、引数は (lo, hi) の 64 ビットの組。ここは 64 ビットの演算だけで書く (自分自身の i128 を使うと再帰する)。
 * `ksn_parse_int` (kir_rt.c) が使う乗算・除算・シフトを足す。 */
typedef unsigned long long ksn_u64;
typedef struct {
    ksn_u64 lo;
    ksn_u64 hi;
} ksn_u128;

static void ksn_mul64(ksn_u64 a, ksn_u64 b, ksn_u64 *hi, ksn_u64 *lo) {
    ksn_u64 a0 = (unsigned)a, a1 = a >> 32, b0 = (unsigned)b, b1 = b >> 32;
    ksn_u64 p00 = a0 * b0, p01 = a0 * b1, p10 = a1 * b0, p11 = a1 * b1;
    ksn_u64 mid = (p00 >> 32) + (unsigned)p01 + (unsigned)p10;
    *lo = (mid << 32) | (unsigned)p00;
    *hi = p11 + (p01 >> 32) + (p10 >> 32) + (mid >> 32);
}

void __multi3(ksn_u128 *r, ksn_u64 al, ksn_u64 ah, ksn_u64 bl, ksn_u64 bh) {
    ksn_u64 hi, lo;
    ksn_mul64(al, bl, &hi, &lo);
    hi += al * bh + ah * bl;
    r->lo = lo;
    r->hi = hi;
}

void __ashlti3(ksn_u128 *r, ksn_u64 lo, ksn_u64 hi, int n) {
    n &= 127;
    if (n == 0) {
        r->lo = lo;
        r->hi = hi;
    } else if (n >= 64) {
        r->lo = 0;
        r->hi = lo << (n - 64);
    } else {
        r->lo = lo << n;
        r->hi = (hi << n) | (lo >> (64 - n));
    }
}

void __lshrti3(ksn_u128 *r, ksn_u64 lo, ksn_u64 hi, int n) {
    n &= 127;
    if (n == 0) {
        r->lo = lo;
        r->hi = hi;
    } else if (n >= 64) {
        r->lo = hi >> (n - 64);
        r->hi = 0;
    } else {
        r->lo = (lo >> n) | (hi << (64 - n));
        r->hi = hi >> n;
    }
}

void __ashrti3(ksn_u128 *r, ksn_u64 lo, ksn_u64 hi, int n) {
    n &= 127;
    ksn_u64 sign = (ksn_u64)0 - (hi >> 63);
    if (n == 0) {
        r->lo = lo;
        r->hi = hi;
    } else if (n >= 64) {
        r->lo = (hi >> (n - 64)) | (n == 64 ? 0 : sign << (128 - n));
        r->hi = sign;
    } else {
        r->lo = (lo >> n) | (hi << (64 - n));
        r->hi = (hi >> n) | (sign << (64 - n));
    }
}

/* 符号なしの除算: 1 ビットずつ (128 回)。商と余りを返す。0 で割れば 0 と元の値。 */
static void ksn_udivmod(ksn_u64 alo, ksn_u64 ahi, ksn_u64 blo, ksn_u64 bhi, ksn_u128 *q, ksn_u128 *m) {
    ksn_u64 qlo = 0, qhi = 0, rlo = 0, rhi = 0;
    if (blo == 0 && bhi == 0) {
        q->lo = 0;
        q->hi = 0;
        m->lo = alo;
        m->hi = ahi;
        return;
    }
    for (int i = 127; i >= 0; i--) {
        /* r = r << 1 | bit i of a */
        rhi = (rhi << 1) | (rlo >> 63);
        rlo <<= 1;
        ksn_u64 bit = i >= 64 ? (ahi >> (i - 64)) & 1 : (alo >> i) & 1;
        rlo |= bit;
        /* r >= b なら r -= b、商の bit i を立てる */
        if (rhi > bhi || (rhi == bhi && rlo >= blo)) {
            ksn_u64 nlo = rlo - blo;
            rhi = rhi - bhi - (rlo < blo ? 1 : 0);
            rlo = nlo;
            if (i >= 64) qhi |= (ksn_u64)1 << (i - 64);
            else qlo |= (ksn_u64)1 << i;
        }
    }
    q->lo = qlo;
    q->hi = qhi;
    m->lo = rlo;
    m->hi = rhi;
}

void __udivti3(ksn_u128 *r, ksn_u64 alo, ksn_u64 ahi, ksn_u64 blo, ksn_u64 bhi) {
    ksn_u128 m;
    ksn_udivmod(alo, ahi, blo, bhi, r, &m);
}

void __umodti3(ksn_u128 *r, ksn_u64 alo, ksn_u64 ahi, ksn_u64 blo, ksn_u64 bhi) {
    ksn_u128 q;
    ksn_udivmod(alo, ahi, blo, bhi, &q, r);
}
