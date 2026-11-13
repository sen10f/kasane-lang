/* Cranelift バックエンドの文字列ランタイム (UTF-8 のバイト列を扱う関数群)。
 * clif.rs が `include_str!` でこのファイルを埋め込み、ネイティブ実行ファイルが `ksn_rt_` の関数を
 * 参照するときだけ、リンク時に C コンパイラで一緒にコンパイルする。
 * str は {ptr, len}、String は {ptr, len, cap} (24 バイト)。確保は malloc / realloc / free だけ。 */
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

typedef uint8_t u8;
typedef uint64_t u64;
typedef int64_t i64;

/* s[from..] の中で最初に p が現れる位置。見つからなければ -1。p が空なら from。 */
static i64 find_at(const u8 *s, u64 sl, u64 from, const u8 *p, u64 pl) {
    if (from > sl || pl > sl - from) return -1;
    if (pl == 0) return (i64)from;
    u64 last = sl - pl;
    for (u64 i = from; i <= last; i++) {
        if (s[i] == p[0] && memcmp(s + i, p, pl) == 0) return (i64)i;
    }
    return -1;
}

i64 ksn_rt_find(const u8 *s, u64 sl, const u8 *p, u64 pl) { return find_at(s, sl, 0, p, pl); }

/* split / split_once 用: 空の区切りは「見つからない」。 */
i64 ksn_rt_split_find(const u8 *s, u64 sl, u64 from, const u8 *p, u64 pl) {
    if (pl == 0) return -1;
    return find_at(s, sl, from, p, pl);
}

/* 最後に p が現れる位置 (p が空なら sl)。見つからなければ -1。 */
i64 ksn_rt_rfind(const u8 *s, u64 sl, const u8 *p, u64 pl) {
    if (pl > sl) return -1;
    for (u64 i = sl - pl + 1; i-- > 0;) {
        if (pl == 0 || memcmp(s + i, p, pl) == 0) return (i64)i;
    }
    return -1;
}

int32_t ksn_rt_ends_with(const u8 *s, u64 sl, const u8 *p, u64 pl) {
    return pl <= sl && memcmp(s + (sl - pl), p, pl) == 0;
}

static int lower(int c) { return (c >= 'A' && c <= 'Z') ? c + 32 : c; }

int32_t ksn_rt_eq_nocase(const u8 *a, u64 al, const u8 *b, u64 bl) {
    if (al != bl) return 0;
    for (u64 i = 0; i < al; i++) {
        if (lower(a[i]) != lower(b[i])) return 0;
    }
    return 1;
}

static int is_ws(u8 c) { return c == ' ' || c == '\t' || c == '\n' || c == '\r' || c == 0x0C; }

/* mode: 1 = 先頭だけ, 2 = 末尾だけ, 3 = 両側。結果の str を out[0..2] に書く。 */
void ksn_rt_trim(const u8 *s, u64 sl, int32_t mode, u64 *out) {
    u64 a = 0, b = sl;
    if (mode & 1) while (a < b && is_ws(s[a])) a++;
    if (mode & 2) while (b > a && is_ws(s[b - 1])) b--;
    out[0] = (u64)(uintptr_t)(s + a);
    out[1] = b - a;
}

/* mode: 1 = 接頭辞, 2 = 接尾辞。一致すれば 1 を返して残りの str を out に書く。 */
int32_t ksn_rt_strip(const u8 *s, u64 sl, const u8 *p, u64 pl, int32_t mode, u64 *out) {
    if (pl > sl) return 0;
    if (mode == 1) {
        if (memcmp(s, p, pl) != 0) return 0;
        out[0] = (u64)(uintptr_t)(s + pl);
    } else {
        if (memcmp(s + (sl - pl), p, pl) != 0) return 0;
        out[0] = (u64)(uintptr_t)s;
    }
    out[1] = sl - pl;
    return 1;
}

/* 10 進の文字列を buf (24 バイト以上) の先頭に書いて、バイト数を返す。 */
u64 ksn_rt_fmt_u64(u8 *buf, u64 v) {
    u8 tmp[24];
    u64 n = 0;
    do {
        tmp[n++] = (u8)('0' + v % 10);
        v /= 10;
    } while (v != 0);
    for (u64 i = 0; i < n; i++) buf[i] = tmp[n - 1 - i];
    return n;
}

u64 ksn_rt_fmt_i64(u8 *buf, i64 v) {
    if (v >= 0) return ksn_rt_fmt_u64(buf, (u64)v);
    buf[0] = '-';
    /* 最小値でも溢れないよう、符号なしで絶対値を取る。 */
    return 1 + ksn_rt_fmt_u64(buf + 1, 0 - (u64)v);
}

/* out = {ptr, len, cap} の String を新しく作る。失敗 (確保できない) なら 1。 */
static int make_string(u64 *out, u64 total, u8 **dst) {
    out[0] = 0; out[1] = 0; out[2] = 0;
    if (total == 0) { *dst = 0; return 0; }
    u8 *m = (u8 *)malloc(total);
    if (!m) return 1;
    out[0] = (u64)(uintptr_t)m; out[2] = total;
    *dst = m;
    return 0;
}

/* text 中の from を to に置き換えた新しい String。from が空なら text の複製。 */
int32_t ksn_rt_replace(u64 *out, const u8 *s, u64 sl, const u8 *f, u64 fl, const u8 *t, u64 tl) {
    u64 count = 0;
    if (fl != 0) {
        for (i64 at = find_at(s, sl, 0, f, fl); at >= 0; at = find_at(s, sl, (u64)at + fl, f, fl)) count++;
    }
    u64 total = sl - count * fl + count * tl;
    u8 *d;
    if (make_string(out, total, &d)) return 1;
    u64 pos = 0, w = 0;
    if (fl != 0) {
        for (i64 at = find_at(s, sl, 0, f, fl); at >= 0; at = find_at(s, sl, pos, f, fl)) {
            memcpy(d + w, s + pos, (u64)at - pos); w += (u64)at - pos;
            memcpy(d + w, t, tl); w += tl;
            pos = (u64)at + fl;
        }
    }
    if (sl > pos) memcpy(d + w, s + pos, sl - pos);
    out[1] = total;
    return 0;
}

/* parts は {ptr, len} の並び (n 個)。sep で区切って連結した新しい String。 */
int32_t ksn_rt_join(u64 *out, const u64 *parts, u64 n, const u8 *sep, u64 sepl) {
    u64 total = 0;
    for (u64 i = 0; i < n; i++) total += parts[2 * i + 1];
    if (n > 1) total += (n - 1) * sepl;
    u8 *d;
    if (make_string(out, total, &d)) return 1;
    u64 w = 0;
    for (u64 i = 0; i < n; i++) {
        if (i > 0) { memcpy(d + w, sep, sepl); w += sepl; }
        u64 l = parts[2 * i + 1];
        if (l) memcpy(d + w, (const u8 *)(uintptr_t)parts[2 * i], l);
        w += l;
    }
    out[1] = total;
    return 0;
}
