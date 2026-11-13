/* Kasane の機械語の小さな実行時補助 (設計_KIR.md 17.11)。`ksn` が生成した LLVM IR と一緒に clang へ渡す。
 * 浮動小数の表示は、木走査の `py_float` / `py_float32` (`stage0/src/interp.rs`) と同じ規則:
 * NaN は `nan`、±inf は `inf` / `-inf`、整数値で |f| < 1e16 は小数 1 桁 (`1.0`、`-0.0`)、
 * それ以外は Rust の `{}` (往復できる最短の桁を、指数表記を使わずに 10 進で書く)。 */
#include <math.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static char ksn_fbuf[2][512];

/* `m` (`[-]d[.ddd]e[+-]X`) を、指数表記を使わない 10 進に直して `out` に書く。 */
static void ksn_plain(const char *m, char *out) {
    char dig[32];
    int nd = 0, neg = 0;
    const char *p = m;
    if (*p == '-') {
        neg = 1;
        p++;
    }
    for (; *p && *p != 'e'; p++) {
        if (*p >= '0' && *p <= '9') dig[nd++] = *p;
    }
    int x = atoi(p + 1); /* 先頭の桁の位は 10^x */
    while (nd > 1 && dig[nd - 1] == '0') nd--; /* 末尾の 0 は桁に含めない */
    char *o = out;
    if (neg) *o++ = '-';
    if (x >= 0) {
        for (int i = 0; i <= x; i++) *o++ = i < nd ? dig[i] : '0';
        if (nd > x + 1) {
            *o++ = '.';
            for (int i = x + 1; i < nd; i++) *o++ = dig[i];
        }
    } else {
        *o++ = '0';
        *o++ = '.';
        for (int i = 0; i < -x - 1; i++) *o++ = '0';
        for (int i = 0; i < nd; i++) *o++ = dig[i];
    }
    *o = 0;
}

/* `m` (`[-]d[.ddd]e[+-]X`) の最後の桁に `dir` (±1) を足した、同じ桁数の候補を `out` に書く。
 * 桁上がりで 10.0 になれば 1.0 と指数 +1、桁下がりで 0.9.. になれば 9.9.. と指数 -1。作れなければ 0。 */
static int ksn_bump(const char *m, int dir, char *out) {
    char dig[32];
    int nd = 0, neg = 0;
    const char *p = m;
    if (*p == '-') {
        neg = 1;
        p++;
    }
    for (; *p && *p != 'e'; p++) {
        if (*p >= '0' && *p <= '9') dig[nd++] = *p;
    }
    int x = atoi(p + 1);
    int i = nd - 1;
    if (dir > 0) {
        while (i >= 0 && dig[i] == '9') dig[i--] = '0';
        if (i < 0) {
            dig[0] = '1';
            x++;
        } else {
            dig[i]++;
        }
    } else {
        while (i >= 0 && dig[i] == '0') dig[i--] = '9';
        if (i < 0) return 0;
        dig[i]--;
        if (dig[0] == '0') {
            if (nd == 1) return 0;
            memmove(dig, dig + 1, nd - 1);
            dig[nd - 1] = '9';
            x--;
        }
    }
    char *o = out;
    if (neg) *o++ = '-';
    *o++ = dig[0];
    if (nd > 1) {
        *o++ = '.';
        for (int k = 1; k < nd; k++) *o++ = dig[k];
    }
    sprintf(o, "e%d", x);
    return 1;
}

/* 往復できる最短の桁を探す。同じ桁数で往復する候補が複数あるときは、値に近い方 (snprintf の丸め) を取る。
 * snprintf の最近接の候補が往復しないとき (2 の冪の境界で区間が非対称)、同じ桁数の隣の候補も試す。 */
static const char *ksn_fmt(double f, int is32, char *out) {
    if (isnan(f)) return "nan";
    if (isinf(f)) return f > 0 ? "inf" : "-inf";
    if (f == trunc(f) && fabs(f) < 1e16) {
        snprintf(out, 512, "%.1f", f);
        return out;
    }
    char m[64];
    for (int p = 0; p < 17; p++) {
        snprintf(m, sizeof m, "%.*e", p, f);
        int ok = is32 ? (strtof(m, 0) == (float)f) : (strtod(m, 0) == f);
        if (!ok) {
            /* 最後の桁を 1 つ上下にずらした候補を、10 進の桁の列の上で作る (浮動小数で足すと非正規数で桁が落ちる)。 */
            for (int dir = -1; dir <= 1 && !ok; dir += 2) {
                char m2[64];
                if (ksn_bump(m, dir, m2) && (is32 ? (strtof(m2, 0) == (float)f) : (strtod(m2, 0) == f))) {
                    memcpy(m, m2, sizeof m);
                    ok = 1;
                }
            }
        }
        if (ok) {
            /* ちょうど中間 (正確な 10 進展開で、p 桁より後が 5000..) のとき、snprintf は偶数へ丸めるが、
             * Rust は絶対値の大きい方を取る。上の候補も往復するならそちらにする。 */
            static char big[1200];
            snprintf(big, sizeof big, "%.*e", p + 800, f);
            char *e = strchr(big, 'e');
            char *q = strchr(big, '.');
            int tie = 0;
            if (q && e && q + 1 + p < e && q[1 + p] == '5') {
                tie = 1;
                for (char *r = q + 2 + p; r < e; r++) {
                    if (*r != '0') {
                        tie = 0;
                        break;
                    }
                }
            }
            if (tie) {
                char m2[64];
                /* m の最後の桁が、正確な値の p 桁目を切り捨てたものなら上へずらす。 */
                if (strncmp(m, big, (size_t)(q + 1 + p - big)) == 0 && ksn_bump(m, 1, m2) &&
                    (is32 ? (strtof(m2, 0) == (float)f) : (strtod(m2, 0) == f))) {
                    memcpy(m, m2, sizeof m);
                }
            }
            break;
        }
    }
    ksn_plain(m, out);
    return out;
}

const char *ksn_fmt_f64(double f) { return ksn_fmt(f, 0, ksn_fbuf[0]); }
const char *ksn_fmt_f32(float f) { return ksn_fmt((double)f, 1, ksn_fbuf[1]); }

/* `p` から `n` バイトを UTF-8 として検証し、正しい接頭辞の長さ (Rust の `Utf8Error::valid_up_to`) を返す。
 * 全体が正しければ `n`。規則は Rust の `core::str::from_utf8` と同じ (過長な符号化・サロゲート・U+10FFFF 超を拒む)。 */
long long ksn_utf8_valid(const unsigned char *p, long long n) {
    long long i = 0;
    while (i < n) {
        unsigned char b = p[i];
        int w;
        unsigned char lo = 0x80, hi = 0xbf;
        if (b < 0x80) {
            i++;
            continue;
        } else if (b >= 0xc2 && b <= 0xdf) {
            w = 2;
        } else if (b >= 0xe0 && b <= 0xef) {
            w = 3;
            if (b == 0xe0) lo = 0xa0;
            if (b == 0xed) hi = 0x9f;
        } else if (b >= 0xf0 && b <= 0xf4) {
            w = 4;
            if (b == 0xf0) lo = 0x90;
            if (b == 0xf4) hi = 0x8f;
        } else {
            return i;
        }
        if (i + 1 >= n || p[i + 1] < lo || p[i + 1] > hi) return i;
        for (int k = 2; k < w; k++) {
            if (i + k >= n || p[i + k] < 0x80 || p[i + k] > 0xbf) return i;
        }
        i += w;
    }
    return n;
}

/* `core.num.<int>.from_str_radix(text, radix)` の実行時補助。
 * 規則は `stage0/src/natives.rs` の from_str_radix / parse と同一:
 *   - 空文字列、符号だけの文字列は kind 0。
 *   - 符号なし型は先頭の '-' ("-0" も含む) を kind 0 で拒む。'+' は両方受け付ける。
 *   - 各桁は 0-9 / a-z (A-Z も可)。radix (2..=36) 以上の桁・不正文字は kind 0。
 *   - 128 ビットで `v = v * radix + d` を累積。桁あふれは bits==128 なら kind 0、
 *     それ以外は kind 1 (範囲外)。
 *   - 符号付き: '-' なら |v| <= 2^(bits-1)、そうでなければ <= 2^(bits-1)-1。
 *     違反は kind 1。符号なし: v <= 2^bits-1 (bits>=64 なら常に範囲内)。違反は kind 1。
 * 戻り値: 0 = 成功 (out_lo/out_hi に 128 ビットの値)、1 = kind 0、2 = kind 1。 */
int ksn_parse_int(const unsigned char *p, long len, unsigned radix,
                  int bits, int is_signed,
                  unsigned long long *out_lo, unsigned long long *out_hi) {
    if (radix < 2 || radix > 36) return 1;
    if (len == 0) return 1;
    int is_minus = p[0] == '-';
    int is_plus = p[0] == '+';
    if (!is_signed && is_minus) return 1;
    int has_sign = is_minus || is_plus;
    long start = has_sign ? 1 : 0;
    if (start == len) return 1;
    unsigned __int128 v = 0;
    unsigned __int128 lim = ~(unsigned __int128)0;
    for (long i = start; i < len; i++) {
        unsigned c = p[i] | 0x20;
        unsigned d;
        if (c >= '0' && c <= '9') d = c - '0';
        else if (c >= 'a' && c <= 'z') d = c - 'a' + 10;
        else return 1;
        if (d >= radix) return 1;
        if (v > (lim - (unsigned __int128)d) / (unsigned __int128)radix) {
            return bits == 128 ? 1 : 2;
        }
        v = v * (unsigned __int128)radix + (unsigned __int128)d;
    }
    if (is_signed) {
        unsigned __int128 half = (unsigned __int128)1 << (bits - 1);
        unsigned __int128 bound = is_minus ? half : half - 1;
        if (v > bound) return 2;
    } else if (bits < 128) {
        unsigned __int128 bound = ((unsigned __int128)1 << bits) - 1;
        if (v > bound) return 2;
    }
    if (is_minus && is_signed) v = 0 - v;
    *out_lo = (unsigned long long)(uint64_t)v;
    *out_hi = (unsigned long long)(uint64_t)(v >> 64);
    return 0;
}
