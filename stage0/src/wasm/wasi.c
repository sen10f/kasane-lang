/* wasm32-wasi の実行時 (`ksn build --target wasm32-wasi`、修正プラン 5-3)。
 *
 * WASI preview 1 の import (`wasi_snapshot_preview1`) だけで、次を定義する:
 *   - 入口 `_start` (引数と環境を取り、`main` を呼び、戻り値で `proc_exit` する)。
 *   - `ksn_write` / `ksn_exit` (rt.c の `write` / `exit` が呼ぶ。`fd_write` / `proc_exit`)。
 *   - `getenv` (`environ_get`)。
 *   - kir_rt.c と生成 IR が呼ぶ `ksn_rt_sys_*` のうち、std.fs・実時刻・乱数。
 *     std.net・std.process・スレッドは wasm_unsupported が生成の前に止める (E9040)。
 *
 * std.fs の道は前開きのディレクトリ (fd_prestat_get で見つかる最初のもの) からの相対。先頭の '/' は外す。
 * 戻り値・エラーの規約は sysrt.c と同じ: 0 以上が成功、-1 が失敗で e[0] = ErrorKind (宣言順)、e[1] = OS のエラー番号 (Linux の番号に直す)。
 *
 * このファイルは `clang --target=wasm32-unknown-wasi -DKSN_WASI=1 -ffreestanding -fno-builtin` で作る (cli.rs の wasm_link)。 */
#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>
#include <errno.h>

typedef int64_t i64;
typedef uint64_t u64;
typedef int32_t i32;
typedef uint32_t u32;
typedef uint16_t u16;
typedef uint8_t u8;

#define WASI(name) __attribute__((import_module("wasi_snapshot_preview1"), import_name(#name)))

struct ciovec {
    const void *buf;
    u32 len;
};
struct iovec {
    void *buf;
    u32 len;
};

WASI(proc_exit) void w_proc_exit(u32 code) __attribute__((noreturn));
WASI(fd_write) u32 w_fd_write(i32 fd, const struct ciovec *iovs, u32 n, u32 *nw);
WASI(fd_read) u32 w_fd_read(i32 fd, const struct iovec *iovs, u32 n, u32 *nr);
WASI(fd_close) u32 w_fd_close(i32 fd);
WASI(fd_sync) u32 w_fd_sync(i32 fd);
WASI(fd_readdir) u32 w_fd_readdir(i32 fd, void *buf, u32 len, u64 cookie, u32 *used);
WASI(fd_prestat_get) u32 w_fd_prestat_get(i32 fd, void *prestat);
WASI(path_open) u32 w_path_open(i32 fd, u32 dirflags, const void *path, u32 len, u32 oflags, u64 base, u64 inheriting, u32 fdflags, i32 *opened);
WASI(path_filestat_get) u32 w_path_filestat_get(i32 fd, u32 flags, const void *path, u32 len, void *buf);
WASI(path_unlink_file) u32 w_path_unlink_file(i32 fd, const void *path, u32 len);
WASI(path_remove_directory) u32 w_path_remove_directory(i32 fd, const void *path, u32 len);
WASI(path_create_directory) u32 w_path_create_directory(i32 fd, const void *path, u32 len);
WASI(path_rename) u32 w_path_rename(i32 fd, const void *p, u32 n, i32 fd2, const void *q, u32 m);
WASI(clock_time_get) u32 w_clock_time_get(u32 id, u64 precision, u64 *t);
WASI(random_get) u32 w_random_get(void *buf, u32 len);
WASI(args_sizes_get) u32 w_args_sizes_get(u32 *argc, u32 *bufsz);
WASI(args_get) u32 w_args_get(char **argv, char *buf);
WASI(environ_sizes_get) u32 w_environ_sizes_get(u32 *n, u32 *bufsz);
WASI(environ_get) u32 w_environ_get(char **env, char *buf);

/* WASI の定数 (preview 1)。 */
#define OFLAG_CREAT 1u
#define OFLAG_DIRECTORY 2u
#define OFLAG_TRUNC 8u
#define FDFLAG_APPEND 1u
#define LOOKUP_FOLLOW 1u
#define FT_DIRECTORY 3
#define FT_REGULAR 4
#define RIGHT_FD_DATASYNC (1ull << 0)
#define RIGHT_FD_READ (1ull << 1)
#define RIGHT_FD_SEEK (1ull << 2)
#define RIGHT_FD_SYNC (1ull << 4)
#define RIGHT_FD_TELL (1ull << 5)
#define RIGHT_FD_WRITE (1ull << 6)
#define RIGHT_FD_READDIR (1ull << 14)
#define RIGHT_FD_FILESTAT_GET (1ull << 21)
#define RIGHTS_READ (RIGHT_FD_READ | RIGHT_FD_SEEK | RIGHT_FD_TELL | RIGHT_FD_FILESTAT_GET)
#define RIGHTS_WRITE (RIGHT_FD_WRITE | RIGHT_FD_SEEK | RIGHT_FD_TELL | RIGHT_FD_SYNC | RIGHT_FD_DATASYNC | RIGHT_FD_FILESTAT_GET)
#define RIGHTS_READDIR (RIGHT_FD_READDIR | RIGHT_FD_FILESTAT_GET)

/* ---- エラー番号: WASI の番号を Linux の番号 (sysrt.c が使うもの) に直す。表にないものは EIO (5)。 */

static const struct {
    u16 wasi, linux;
} errno_map[] = {
    {1, 7},    {2, 13},   {6, 11},   {7, 114},  {8, 9},    {10, 16},  {12, 10},  {13, 103}, {14, 111}, {15, 104}, {16, 35},  {17, 89},
    {18, 33},  {20, 17},  {21, 14},  {22, 27},  {23, 113}, {24, 43},  {25, 84},  {26, 115}, {27, 4},   {28, 22},  {29, 5},   {30, 106},
    {31, 21},  {32, 40},  {33, 24},  {34, 31},  {35, 90},  {36, 72},  {37, 36},  {38, 100}, {39, 102}, {40, 101}, {41, 23},  {42, 105},
    {43, 19},  {44, 2},   {45, 8},   {46, 37},  {47, 67},  {48, 12},  {49, 42},  {50, 92},  {51, 28},  {52, 38},  {53, 107}, {54, 20},
    {55, 39},  {56, 131}, {57, 88},  {58, 95},  {59, 25},  {60, 6},   {61, 75},  {62, 130}, {63, 1},   {64, 32},  {65, 71},  {66, 93},
    {67, 91},  {68, 34},  {69, 30},  {70, 29},  {71, 3},   {72, 116}, {73, 110}, {74, 26},  {75, 18},  {76, 13},
};

static int lin_errno(u32 w) {
    for (u32 i = 0; i < sizeof errno_map / sizeof errno_map[0]; i++) {
        if (errno_map[i].wasi == w) return errno_map[i].linux;
    }
    return 5;
}

/* std.io.ErrorKind のバリアント番号 (sysrt.c と同じ)。 */
enum { K_NOT_FOUND = 0, K_PERMISSION_DENIED, K_ALREADY_EXISTS, K_INVALID_INPUT, K_UNEXPECTED_EOF, K_BROKEN_PIPE, K_INTERRUPTED, K_OUT_OF_MEMORY, K_TIMED_OUT, K_OTHER };

static i32 kind_of(int err) {
    switch (err) {
    case 2: return K_NOT_FOUND;
    case 1:
    case 13: return K_PERMISSION_DENIED;
    case 17: return K_ALREADY_EXISTS;
    case 22: return K_INVALID_INPUT;
    case 32: return K_BROKEN_PIPE;
    case 4: return K_INTERRUPTED;
    case 12: return K_OUT_OF_MEMORY;
    case 11:
    case 110: return K_TIMED_OUT;
    default: return K_OTHER;
    }
}

static i64 fail_lin(i32 *e, int err) {
    e[0] = kind_of(err);
    e[1] = (i32)err;
    return -1;
}

static i64 fail_w(i32 *e, u32 w) { return fail_lin(e, lin_errno(w)); }

/* OS エラー番号を持たない std.io.Error (InvalidInput、番号 0)。NUL を含む路径など。 */
static i64 fail_invalid(i32 *e) {
    e[0] = K_INVALID_INPUT;
    e[1] = 0;
    return -1;
}

/* ---- 小さな読み書き (線形メモリは小端) */

static u64 rd64(const u8 *p) {
    u64 v = 0;
    for (int k = 7; k >= 0; k--) v = (v << 8) | p[k];
    return v;
}

static u32 rd32(const u8 *p) { return (u32)p[0] | ((u32)p[1] << 8) | ((u32)p[2] << 16) | ((u32)p[3] << 24); }

static i64 kind_of_filetype(u8 ft) {
    if (ft == FT_REGULAR) return 0;
    if (ft == FT_DIRECTORY) return 1;
    return 2;
}

/* ---- 入出力と終了 (rt.c の `write` / `exit` が使う) */

int ksn_write(int fd, const void *p, size_t n);

static void wr2(const char *s) {
    size_t n = 0;
    while (s[n]) n++;
    ksn_write(2, s, n);
}

int ksn_write(int fd, const void *p, size_t n) {
    const u8 *b = (const u8 *)p;
    size_t done = 0;
    while (done < n) {
        struct ciovec v = {b + done, (u32)(n - done)};
        u32 nw = 0;
        u32 e = w_fd_write((i32)fd, &v, 1, &nw);
        if (e) return -lin_errno(e);
        if (nw == 0) break;
        done += nw;
    }
    return (int)done;
}

void ksn_exit(int code) { w_proc_exit((u32)code); }

/* ---- 環境 (argv と environ) */

static char **env_tab;
static u32 env_n;
static int env_ready;

static void env_load(void) {
    if (env_ready) return;
    env_ready = 1;
    u32 n = 0, sz = 0;
    if (w_environ_sizes_get(&n, &sz) != 0) return;
    /* ホストが渡す個数・大きさは 32 ビットの掛け算・足し算を溢れさせうる。大きすぎるなら環境なしにする。 */
    if (n >= 0x10000000u || sz == 0xffffffffu) return;
    char **tab = (char **)malloc((n + 1) * sizeof(char *));
    char *buf = (char *)malloc(sz + 1);
    if (!tab || !buf) return;
    if (w_environ_get(tab, buf) != 0) return;
    tab[n] = 0;
    env_tab = tab;
    env_n = n;
}

char *getenv(const char *name) {
    env_load();
    for (u32 i = 0; i < env_n; i++) {
        const char *kv = env_tab[i];
        size_t k = 0;
        while (name[k] && name[k] == kv[k]) k++;
        if (name[k] == 0 && kv[k] == '=') return (char *)(kv + k + 1);
    }
    return 0;
}

extern int main(int argc, char **argv);

/* 入口: 引数を取り、`main` を呼び、戻り値を終了コードにする (ksn_main は rt.c の口で、引数を持たない)。 */
void _start(void) {
    u32 argc = 0, sz = 0;
    if (w_args_sizes_get(&argc, &sz) != 0) {
        argc = 0;
        sz = 0;
    }
    if (argc >= 0x10000000u || sz == 0xffffffffu) w_proc_exit(101);
    char **argv = (char **)malloc((argc + 1) * sizeof(char *));
    char *buf = (char *)malloc(sz + 1);
    if (!argv || !buf) w_proc_exit(101);
    if (w_args_get(argv, buf) != 0) argc = 0;
    argv[argc] = 0;
    int r = main((int)argc, argv);
    w_proc_exit((u32)r);
}

/* ---- 実時刻と乱数 */

/* 実時刻 (UNIX 時刻からのナノ秒、`std.time.system_nanos`)。失敗したら 0 (sysrt.c と同じ)。 */
i64 ksn_rt_sys_wall_ns(void) {
    u64 t = 0;
    if (w_clock_time_get(0, 1, &t) != 0) return 0;
    return (i64)t;
}

/* OS の乱数で `n` バイトを `p` に書く (random_get)。失敗は Err、`e` が NULL なら (Rng.from_entropy) 文言を出して終了コード 101。 */
i64 ksn_rt_sys_random(u8 *p, u64 n, i32 *e) {
    u64 done = 0;
    while (done < n) {
        u64 left = n - done;
        u32 chunk = left > 0x40000000ull ? 0x40000000u : (u32)left;
        u32 r = w_random_get(p + done, chunk);
        if (r != 0) {
            if (!e) {
                wr2("ksn: std.rand.Rng.from_entropy: OS の乱数源が読めない\n");
                w_proc_exit(101);
            }
            return fail_w(e, r);
        }
        done += chunk;
    }
    return 0;
}

/* ---- std.fs (前開きのディレクトリからの相対) */

/* 前開きのディレクトリの記述子 (fd 3 から順に探す。`fd_prestat_get` が EBADF で終わり)。無ければ -1。 */
static i32 preopen_root(void) {
    static i32 cached = -2;
    if (cached != -2) return cached;
    cached = -1;
    for (i32 fd = 3; fd < 4096; fd++) {
        u8 pre[8] = {0};
        if (w_fd_prestat_get(fd, pre) != 0) break;
        if (pre[0] == 0) {
            cached = fd;
            break;
        }
    }
    return cached;
}

/* 路径を前開きの根からの相対にする。NUL を含めば InvalidInput、根が無ければ EBADF。
 * 先頭の '/' は外す (絶対パスは根の下の相対と読む)。外したあとに空なら "." にする。 */
static i32 path_root(const u8 *p, u64 n, const u8 **out, u32 *len, i32 *e) {
    for (u64 k = 0; k < n; k++) {
        if (p[k] == 0) {
            fail_invalid(e);
            return -1;
        }
    }
    i32 d = preopen_root();
    if (d < 0) {
        fail_lin(e, 9);
        return -1;
    }
    u64 k = 0;
    if (n > 0) {
        while (k < n && p[k] == '/') k++;
        if (k == n) {
            *out = (const u8 *)".";
            *len = 1;
            return d;
        }
    }
    *out = p + k;
    *len = (u32)(n - k);
    return d;
}

/* create = 2: 追記で開く、1: 作成して切り詰め、0: 読み取り (sysrt.c と同じ)。戻り値は記述子。 */
i64 ksn_rt_sys_open(const u8 *p, u64 n, i32 create, i32 *e) {
    const u8 *q = 0;
    u32 m = 0;
    i32 d = path_root(p, n, &q, &m, e);
    if (d < 0) return -1;
    u32 oflags = 0, fdflags = 0;
    u64 rights = RIGHTS_READ;
    if (create == 2) {
        oflags = OFLAG_CREAT;
        fdflags = FDFLAG_APPEND;
        rights = RIGHTS_WRITE;
    } else if (create) {
        oflags = OFLAG_CREAT | OFLAG_TRUNC;
        rights = RIGHTS_WRITE;
    }
    i32 fd = -1;
    u32 r = w_path_open(d, LOOKUP_FOLLOW, q, m, oflags, rights, 0, fdflags, &fd);
    if (r != 0) return fail_w(e, r);
    return fd;
}

/* 存在すれば 1 (リンクは辿る)。 */
i64 ksn_rt_sys_exists(const u8 *p, u64 n) {
    const u8 *q = 0;
    u32 m = 0;
    i32 e2[2];
    i32 d = path_root(p, n, &q, &m, e2);
    if (d < 0) return 0;
    u8 st[64];
    return w_path_filestat_get(d, LOOKUP_FOLLOW, q, m, st) == 0;
}

i64 ksn_rt_sys_remove(const u8 *p, u64 n, i32 *e) {
    const u8 *q = 0;
    u32 m = 0;
    i32 d = path_root(p, n, &q, &m, e);
    if (d < 0) return -1;
    u32 r = w_path_unlink_file(d, q, m);
    return r ? fail_w(e, r) : 0;
}

i64 ksn_rt_sys_rename(const u8 *p, u64 n, const u8 *q2, u64 m2, i32 *e) {
    const u8 *a = 0, *b = 0;
    u32 la = 0, lb = 0;
    i32 d = path_root(p, n, &a, &la, e);
    if (d < 0) return -1;
    i32 d2 = path_root(q2, m2, &b, &lb, e);
    if (d2 < 0) return -1;
    u32 r = w_path_rename(d, a, la, d2, b, lb);
    return r ? fail_w(e, r) : 0;
}

i64 ksn_rt_sys_mkdir(const u8 *p, u64 n, i32 *e) {
    const u8 *q = 0;
    u32 m = 0;
    i32 d = path_root(p, n, &q, &m, e);
    if (d < 0) return -1;
    u32 r = w_path_create_directory(d, q, m);
    return r ? fail_w(e, r) : 0;
}

i64 ksn_rt_sys_rmdir(const u8 *p, u64 n, i32 *e) {
    const u8 *q = 0;
    u32 m = 0;
    i32 d = path_root(p, n, &q, &m, e);
    if (d < 0) return -1;
    u32 r = w_path_remove_directory(d, q, m);
    return r ? fail_w(e, r) : 0;
}

/* リンクを辿る。field 0 = 大きさ、1 = 種別 (0 ファイル / 1 ディレクトリ / 2 それ以外)、2 = 更新時刻 (UNIX 時刻からのナノ秒。範囲外は 0)。 */
i64 ksn_rt_sys_stat(const u8 *p, u64 n, i32 field, i32 *e) {
    const u8 *q = 0;
    u32 m = 0;
    i32 d = path_root(p, n, &q, &m, e);
    if (d < 0) return -1;
    u8 st[64];
    u32 r = w_path_filestat_get(d, LOOKUP_FOLLOW, q, m, st);
    if (r != 0) return fail_w(e, r);
    if (field == 0) return (i64)rd64(st + 32);
    if (field == 1) return kind_of_filetype(st[16]);
    u64 ns = rd64(st + 48);
    if (ns / 1000000000ull > 9223372035ull) return 0;
    return (i64)ns;
}

/* 名前のバイト順 (同じ先頭なら短いほうが先) で並べるための項目。 */
struct ent {
    u8 *name;
    u32 len;
    i32 kind;
};

static int ent_less(const struct ent *a, const struct ent *b) {
    u32 m = a->len < b->len ? a->len : b->len;
    for (u32 i = 0; i < m; i++) {
        if (a->name[i] != b->name[i]) return a->name[i] < b->name[i];
    }
    return a->len < b->len;
}

/* ディレクトリの列挙。名前のバイト順に整列した `[種別][名前][0]` の並びを buf (容量 cap) に書き、必要な総バイト数を返す
 * (cap を超えるときは書かない)。種別は stat (リンクを辿る) の結果。 */
i64 ksn_rt_sys_readdir(const u8 *p, u64 n, u8 *buf, u64 cap, i32 *e) {
    const u8 *q = 0;
    u32 m = 0;
    i32 d = path_root(p, n, &q, &m, e);
    if (d < 0) return -1;
    i32 dfd = -1;
    u32 r = w_path_open(d, LOOKUP_FOLLOW, q, m, OFLAG_DIRECTORY, RIGHTS_READDIR, 0, 0, &dfd);
    if (r != 0) return fail_w(e, r);
    const u32 dbuf_len = 4096;
    u8 *dbuf = (u8 *)malloc(dbuf_len);
    u32 room = 16, count = 0;
    struct ent *ents = (struct ent *)malloc(room * sizeof(struct ent));
    int err = (!dbuf || !ents) ? 12 : 0;
    u64 cookie = 0;
    while (!err) {
        u32 used = 0;
        r = w_fd_readdir(dfd, dbuf, dbuf_len, cookie, &used);
        if (r != 0) {
            err = lin_errno(r);
            break;
        }
        u32 off = 0;
        /* 完全な項目だけを読む (終わりの切れた項目は次の呼び出しで cookie から読み直す)。 */
        while (off + 24 <= used) {
            u64 next = rd64(dbuf + off);
            u32 nl = rd32(dbuf + off + 16);
            /* 名前の長さはホストの値。u32 の足し算を溢れさせない。1 項目が buffer に収まらないなら、黙って切らずに失敗にする。 */
            if (nl > dbuf_len - 24) {
                err = 36;
                break;
            }
            if (nl > used - off - 24) break;
            const u8 *nm = dbuf + off + 24;
            cookie = next;
            off += 24 + nl;
            if ((nl == 1 && nm[0] == '.') || (nl == 2 && nm[0] == '.' && nm[1] == '.')) continue;
            if (count == room) {
                struct ent *g = (struct ent *)realloc(ents, room * 2 * sizeof(struct ent));
                if (!g) {
                    err = 12;
                    break;
                }
                ents = g;
                room *= 2;
            }
            u8 *copy = (u8 *)malloc(nl + 1);
            if (!copy) {
                err = 12;
                break;
            }
            for (u32 i = 0; i < nl; i++) copy[i] = nm[i];
            copy[nl] = 0;
            u8 st[64];
            ents[count].name = copy;
            ents[count].len = nl;
            ents[count].kind = w_path_filestat_get(dfd, LOOKUP_FOLLOW, copy, nl, st) == 0 ? (i32)kind_of_filetype(st[16]) : 2;
            count++;
        }
        if (err || off == 0 || used < dbuf_len) break;
    }
    w_fd_close(dfd);
    u64 total = 0;
    if (!err) {
        /* 挿入順の整列 (件数は少ないと見込む)。 */
        for (u32 i = 1; i < count; i++) {
            struct ent x = ents[i];
            u32 j = i;
            while (j > 0 && ent_less(&x, &ents[j - 1])) {
                ents[j] = ents[j - 1];
                j--;
            }
            ents[j] = x;
        }
        for (u32 i = 0; i < count; i++) total += 2 + ents[i].len;
        if (total <= cap) {
            u8 *w = buf;
            for (u32 i = 0; i < count; i++) {
                *w++ = (u8)ents[i].kind;
                for (u32 k = 0; k < ents[i].len; k++) *w++ = ents[i].name[k];
                *w++ = 0;
            }
        }
    }
    for (u32 i = 0; i < count; i++) free(ents[i].name);
    free(ents);
    free(dbuf);
    if (err) return fail_lin(e, err);
    return (i64)total;
}

/* 一度の read。戻り値は読んだバイト数 (0 は終端)。 */
i64 ksn_rt_sys_read(i32 fd, u8 *buf, u64 len, i32 *e) {
    struct iovec v = {buf, (u32)(len > 0x40000000ull ? 0x40000000ull : len)};
    u32 nr = 0;
    u32 r = w_fd_read(fd, &v, 1, &nr);
    if (r != 0) return fail_w(e, r);
    return (i64)nr;
}

/* 全部書く (write_all)。戻り値は len。 */
i64 ksn_rt_sys_write(i32 fd, const u8 *buf, u64 len, i32 *e) {
    u64 done = 0;
    while (done < len) {
        u64 left = len - done;
        u32 want = left > 0x40000000ull ? 0x40000000u : (u32)left;
        struct ciovec v = {buf + done, want};
        u32 nw = 0;
        u32 r = w_fd_write(fd, &v, 1, &nw);
        if (r != 0) return fail_w(e, r);
        if (nw == 0) {
            e[0] = K_OTHER; /* WriteZero */
            e[1] = 0;
            return -1;
        }
        done += nw;
    }
    return (i64)len;
}

/* 記述子を閉じる。失敗は無視する。 */
i64 ksn_rt_sys_close(i32 fd) {
    w_fd_close(fd);
    return 0;
}

/* 書き込んだ内容を OS に永続化させる (fd_sync)。成功は 0。 */
i64 ksn_rt_sys_sync(i32 fd, i32 *e) {
    u32 r = w_fd_sync(fd);
    return r ? fail_w(e, r) : 0;
}

/* std.fs.Dir と open_file_beneath (sysrt.c と同じ呼び出し)。WASI の成分ごとの検査は未対応なので、
 * リンクを辿る経路を作らずに ENOSYS (Unsupported 相当) で失敗させる。 */
i64 ksn_rt_sys_opendir(const u8 *p, u64 n, i32 *e) {
    (void)p;
    (void)n;
    return fail_lin(e, ENOSYS);
}

i64 ksn_rt_sys_openbeneath(i32 dirfd, const u8 *p, u64 n, i32 *e) {
    (void)dirfd;
    (void)p;
    (void)n;
    return fail_lin(e, ENOSYS);
}

i64 ksn_rt_sys_fstat(i32 fd, i32 field, i32 *e) {
    (void)fd;
    (void)field;
    return fail_lin(e, ENOSYS);
}
