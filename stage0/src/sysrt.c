/* Cranelift バックエンドの OS ランタイム (std.fs / std.net)。
 * clif.rs が `include_str!` でこのファイルを埋め込み、ネイティブ実行ファイルが `ksn_rt_sys_` の関数を
 * 参照するときだけ、リンク時に C コンパイラで一緒にコンパイルする (strrt.c と同じ仕組み)。
 *
 * 規約: 戻り値は i64。0 以上なら成功 (値は関数ごと)、-1 なら失敗で、エラーを e[0..2] に書く。
 *   e[0] = std.io.ErrorKind のバリアント番号 (spec/stdlib/std/io.sig の宣言順)、e[1] = 発生源の OS エラー番号。
 * std.net.Error は e[1] だけを使う (OS エラー番号が無いときは -1)。
 * 振る舞いはインタプリタ (natives.rs / net.rs) と同じにする。 */
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <errno.h>
#include <fcntl.h>
#include <sys/types.h>
#include <sys/stat.h>

#ifdef _WIN32
#include <io.h>
#include <windows.h>
#else
#include <unistd.h>
#include <signal.h>
#include <sys/socket.h>
#include <netdb.h>
#include <netinet/in.h>
#include <arpa/inet.h>
#endif

typedef uint8_t u8;
typedef uint64_t u64;
typedef int64_t i64;
typedef int32_t i32;

/* std.io.ErrorKind のバリアント番号 */
enum { K_NOT_FOUND = 0, K_PERMISSION_DENIED, K_ALREADY_EXISTS, K_INVALID_INPUT, K_UNEXPECTED_EOF, K_BROKEN_PIPE, K_INTERRUPTED, K_OUT_OF_MEMORY, K_OTHER };

/* errno から ErrorKind を決める (Rust の標準ライブラリと同じ対応)。 */
static i32 kind_of(int err) {
    switch (err) {
    case ENOENT: return K_NOT_FOUND;
    case EACCES:
    case EPERM: return K_PERMISSION_DENIED;
    case EEXIST: return K_ALREADY_EXISTS;
    case EINVAL: return K_INVALID_INPUT;
    case EPIPE: return K_BROKEN_PIPE;
    case EINTR: return K_INTERRUPTED;
    case ENOMEM: return K_OUT_OF_MEMORY;
    default: return K_OTHER;
    }
}

static i64 fail(i32 *e, int err) {
    e[0] = kind_of(err);
    e[1] = (i32)err;
    return -1;
}

/* OS エラー番号を持たない std.io.Error (InvalidInput、番号 0)。 */
static i64 fail_invalid(i32 *e) {
    e[0] = K_INVALID_INPUT;
    e[1] = 0;
    return -1;
}

/* OS エラー番号を持たない std.net.Error (番号 -1)。 */
static i64 fail_net(i32 *e) {
    e[0] = K_OTHER;
    e[1] = -1;
    return -1;
}

/* str を NUL 終端の C 文字列にする。NUL を含むなら NULL (*nul = 1)、確保に失敗しても NULL (*nul = 0)。 */
static char *cstr(const u8 *p, u64 n, int *nul) {
    *nul = 0;
    if (n > 0 && memchr(p, 0, n) != NULL) { *nul = 1; return NULL; }
    char *s = (char *)malloc(n + 1);
    if (!s) return NULL;
    if (n > 0) memcpy(s, p, n);
    s[n] = 0;
    return s;
}

/* ---------------------------------------------------------------- std.fs */

/* create = 0: 読み取り専用で開く (std.fs.open)。create = 1: 作成して切り詰め、書き込み専用 (std.fs.create)。戻り値は記述子。 */
i64 ksn_rt_sys_open(const u8 *p, u64 n, i32 create, i32 *e) {
    int nul;
    char *path = cstr(p, n, &nul);
    if (!path) return nul ? fail_invalid(e) : fail(e, ENOMEM);
    int fd;
#ifdef _WIN32
    fd = create ? _open(path, _O_WRONLY | _O_CREAT | _O_TRUNC | _O_BINARY, _S_IREAD | _S_IWRITE) : _open(path, _O_RDONLY | _O_BINARY);
#else
    fd = create ? open(path, O_WRONLY | O_CREAT | O_TRUNC | O_CLOEXEC, 0666) : open(path, O_RDONLY | O_CLOEXEC);
#endif
    int err = errno;
    free(path);
    if (fd < 0) return fail(e, err);
    return fd;
}

/* 存在すれば 1 (リンクは辿る)。 */
i64 ksn_rt_sys_exists(const u8 *p, u64 n) {
    int nul;
    char *path = cstr(p, n, &nul);
    if (!path) return 0;
    struct stat st;
    int r = stat(path, &st);
    free(path);
    return r == 0;
}

i64 ksn_rt_sys_remove(const u8 *p, u64 n, i32 *e) {
    int nul;
    char *path = cstr(p, n, &nul);
    if (!path) return nul ? fail_invalid(e) : fail(e, ENOMEM);
#ifdef _WIN32
    int r = _unlink(path);
#else
    int r = unlink(path);
#endif
    int err = errno;
    free(path);
    if (r != 0) return fail(e, err);
    return 0;
}

/* 一度の read。戻り値は読んだバイト数 (0 は終端)。 */
i64 ksn_rt_sys_read(i32 fd, u8 *buf, u64 len, i32 *e) {
#ifdef _WIN32
    int r = _read(fd, buf, len > 0x7fffffffu ? 0x7fffffffu : (unsigned)len);
#else
    ssize_t r = read(fd, buf, len);
#endif
    if (r < 0) return fail(e, errno);
    return (i64)r;
}

/* 全部書く (write_all)。戻り値は len。 */
i64 ksn_rt_sys_write(i32 fd, const u8 *buf, u64 len, i32 *e) {
    u64 done = 0;
    while (done < len) {
        u64 want = len - done;
#ifdef _WIN32
        int w = _write(fd, buf + done, want > 0x7fffffffu ? 0x7fffffffu : (unsigned)want);
#else
        ssize_t w = write(fd, buf + done, want);
#endif
        if (w < 0) {
            if (errno == EINTR) continue;
            return fail(e, errno);
        }
        if (w == 0) { e[0] = K_OTHER; e[1] = 0; return -1; } /* WriteZero */
        done += (u64)w;
    }
    return (i64)len;
}

/* 記述子を閉じる。失敗は無視する (インタプリタが drop するだけなのと同じ)。 */
i64 ksn_rt_sys_close(i32 fd) {
#ifdef _WIN32
    _close(fd);
#else
    close(fd);
#endif
    return 0;
}

/* 書き込んだ内容を OS に永続化させる (POSIX は fsync、Windows は FlushFileBuffers)。成功は 0。 */
i64 ksn_rt_sys_sync(i32 fd, i32 *e) {
#ifdef _WIN32
    HANDLE h = (HANDLE)_get_osfhandle(fd);
    if (h == INVALID_HANDLE_VALUE || !FlushFileBuffers(h)) return fail(e, EIO);
    return 0;
#else
    int r;
    do { r = fsync(fd); } while (r < 0 && errno == EINTR);
    if (r < 0) return fail(e, errno);
    return 0;
#endif
}

/* 終端まで読んで vec (= {ptr, len, cap} の Vec<u8>) の後ろに足す。戻り値は足したバイト数。失敗時は vec の長さを変えない。 */
i64 ksn_rt_sys_read_to_end(i32 fd, u64 *vec, i32 *e) {
    u8 *ptr = (u8 *)(uintptr_t)vec[0];
    u64 len = vec[1], cap = vec[2], got = 0;
    for (;;) {
        if (cap - len - got < 32) {
            u64 ncap = cap * 2 > len + got + 32 ? cap * 2 : len + got + 32;
            u8 *np = (u8 *)realloc(ptr, ncap);
            if (!np) return fail(e, ENOMEM);
            ptr = np; cap = ncap;
            vec[0] = (u64)(uintptr_t)ptr; vec[2] = cap;
        }
        u64 room = cap - len - got;
#ifdef _WIN32
        int r = _read(fd, ptr + len + got, room > 0x7fffffffu ? 0x7fffffffu : (unsigned)room);
#else
        ssize_t r = read(fd, ptr + len + got, room);
#endif
        if (r < 0) {
            if (errno == EINTR) continue;
            return fail(e, errno);
        }
        if (r == 0) break;
        got += (u64)r;
    }
    vec[1] = len + got;
    return (i64)got;
}

/* ---------------------------------------------------------------- std.net (POSIX のみ) */
#ifndef _WIN32

static void init_net(void) {
    static int done;
    if (!done) {
        /* 閉じた相手への write で落ちず EPIPE を返す (Rust の標準ライブラリと同じ)。 */
        signal(SIGPIPE, SIG_IGN);
        done = 1;
    }
}

/* "host:port" を分けて、getaddrinfo で解決する。失敗は fail_net。 */
static int resolve(const u8 *a, u64 n, struct addrinfo **out, i32 *e) {
    int nul;
    char *s = cstr(a, n, &nul);
    if (!s) { fail_net(e); return 0; }
    char *colon = strrchr(s, ':');
    if (!colon) { free(s); fail_net(e); return 0; }
    *colon = 0;
    const char *host = s, *port = colon + 1;
    /* ポートは u16 (10 進、先頭の + は許す)。 */
    const char *q = port;
    if (*q == '+') q++;
    if (*q == 0) { free(s); fail_net(e); return 0; }
    unsigned long pv = 0;
    for (; *q; q++) {
        if (*q < '0' || *q > '9') { free(s); fail_net(e); return 0; }
        pv = pv * 10 + (unsigned long)(*q - '0');
        if (pv > 65535) { free(s); fail_net(e); return 0; }
    }
    char portbuf[8];
    portbuf[0] = 0;
    {
        unsigned long v = pv; char t[8]; int k = 0;
        do { t[k++] = (char)('0' + v % 10); v /= 10; } while (v);
        for (int i = 0; i < k; i++) portbuf[i] = t[k - 1 - i];
        portbuf[k] = 0;
    }
    /* "[::1]" の角括弧は、中身が IPv6 リテラルのときだけ外す。 */
    size_t hl = strlen(host);
    char *inner = NULL;
    if (hl >= 2 && host[0] == '[' && host[hl - 1] == ']') {
        inner = (char *)malloc(hl);
        if (inner) {
            memcpy(inner, host + 1, hl - 2);
            inner[hl - 2] = 0;
            struct in6_addr tmp;
            if (inet_pton(AF_INET6, inner, &tmp) == 1) host = inner;
        }
    }
    struct addrinfo hints;
    memset(&hints, 0, sizeof hints);
    hints.ai_family = AF_UNSPEC;
    hints.ai_socktype = SOCK_STREAM;
    int rc = getaddrinfo(host, portbuf, &hints, out);
    free(inner);
    free(s);
    if (rc != 0 || *out == NULL) { fail_net(e); return 0; }
    return 1;
}

static void set_cloexec(int fd) {
    int fl = fcntl(fd, F_GETFD);
    if (fl >= 0) fcntl(fd, F_SETFD, fl | FD_CLOEXEC);
}

i64 ksn_rt_sys_connect(const u8 *a, u64 n, i32 *e) {
    init_net();
    struct addrinfo *res;
    if (!resolve(a, n, &res, e)) return -1;
    int last = 0;
    for (struct addrinfo *ai = res; ai; ai = ai->ai_next) {
        int s = socket(ai->ai_family, ai->ai_socktype, ai->ai_protocol);
        if (s < 0) { last = errno; continue; }
        set_cloexec(s);
        if (connect(s, ai->ai_addr, ai->ai_addrlen) == 0) { freeaddrinfo(res); return s; }
        last = errno;
        close(s);
    }
    freeaddrinfo(res);
    e[0] = K_OTHER;
    e[1] = (i32)last;
    return -1;
}

i64 ksn_rt_sys_listen(const u8 *a, u64 n, i32 *e) {
    init_net();
    struct addrinfo *res;
    if (!resolve(a, n, &res, e)) return -1;
    int last = 0;
    for (struct addrinfo *ai = res; ai; ai = ai->ai_next) {
        int s = socket(ai->ai_family, ai->ai_socktype, ai->ai_protocol);
        if (s < 0) { last = errno; continue; }
        set_cloexec(s);
        int one = 1;
        setsockopt(s, SOL_SOCKET, SO_REUSEADDR, &one, sizeof one);
        if (bind(s, ai->ai_addr, ai->ai_addrlen) == 0 && listen(s, 128) == 0) { freeaddrinfo(res); return s; }
        last = errno;
        close(s);
    }
    freeaddrinfo(res);
    e[0] = K_OTHER;
    e[1] = (i32)last;
    return -1;
}

i64 ksn_rt_sys_accept(i32 lfd, i32 *e) {
    init_net();
    for (;;) {
        int s = accept(lfd, NULL, NULL);
        if (s >= 0) { set_cloexec(s); return s; }
        if (errno == EINTR) continue;
        e[0] = K_OTHER;
        e[1] = (i32)errno;
        return -1;
    }
}

i64 ksn_rt_sys_local_port(i32 fd, i32 *e) {
    struct sockaddr_storage ss;
    socklen_t len = sizeof ss;
    if (getsockname(fd, (struct sockaddr *)&ss, &len) != 0) { e[0] = K_OTHER; e[1] = (i32)errno; return -1; }
    if (ss.ss_family == AF_INET) return ntohs(((struct sockaddr_in *)&ss)->sin_port);
    if (ss.ss_family == AF_INET6) return ntohs(((struct sockaddr_in6 *)&ss)->sin6_port);
    return fail_net(e);
}

#endif

/* ---- std.env / std.time ---- */
#include <time.h>
#ifndef _WIN32
#include <sched.h>
#endif
#ifdef _WIN32
#include <windows.h>
#endif

static int g_argc = 0;
static char **g_argv = 0;

/* `main` の引数を覚える (entry から最初に呼ぶ)。引数 0 (プログラム名) は `std.env` の見える範囲に含めない。 */
void ksn_rt_sys_set_args(i32 argc, char **argv) {
    g_argc = argc;
    g_argv = argv;
}

i64 ksn_rt_sys_arg_count(void) { return g_argc > 0 ? (i64)(g_argc - 1) : 0; }

/* `std.env.arg(i)`: 範囲内なら NUL 終端の文字列 (長さは *len)、範囲外なら 0。 */
i64 ksn_rt_sys_arg(i64 i, i64 *len) {
    if (i < 0 || i >= ksn_rt_sys_arg_count()) return 0;
    char *s = g_argv[i + 1];
    *len = (i64)strlen(s);
    return (i64)(intptr_t)s;
}

/* `std.env.var(name)`: 見つかれば値 (長さは *len)、なければ 0。 */
i64 ksn_rt_sys_var(const u8 *name, i64 nlen, i64 *len) {
    char buf[1024];
    if (nlen < 0 || nlen >= (i64)sizeof buf) return 0;
    memcpy(buf, name, (size_t)nlen);
    buf[nlen] = 0;
    char *v = getenv(buf);
    if (!v) return 0;
    *len = (i64)strlen(v);
    return (i64)(intptr_t)v;
}

/* 単調時計 (ナノ秒)。`std.time.Instant.ticks`。 */
u64 ksn_rt_sys_now_ns(void) {
#ifdef _WIN32
    LARGE_INTEGER f, c;
    QueryPerformanceFrequency(&f);
    QueryPerformanceCounter(&c);
    return (u64)((c.QuadPart / f.QuadPart) * 1000000000ULL + (c.QuadPart % f.QuadPart) * 1000000000ULL / f.QuadPart);
#else
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (u64)ts.tv_sec * 1000000000ULL + (u64)ts.tv_nsec;
#endif
}

/* `std.thread.sleep(ms)` / `std.thread.yield_now()` (スレッドが無くても使える) */
void ksn_rt_sys_sleep_ms(u64 ms) {
#ifdef _WIN32
    Sleep((DWORD)ms);
#else
    struct timespec ts;
    ts.tv_sec = (time_t)(ms / 1000);
    ts.tv_nsec = (long)((ms % 1000) * 1000000);
    while (nanosleep(&ts, &ts) != 0) {
    }
#endif
}

void ksn_rt_sys_yield(void) {
#ifdef _WIN32
    SwitchToThread();
#else
    sched_yield();
#endif
}

/* ---- FFI の動的ロード: `extern ... link "lib:name"` (3.8) ----
 * 共有ライブラリを `dlopen` (Windows は `LoadLibraryA`) して、シンボルを引く。同じ (ライブラリ, 名前) は 1 度だけ引く。
 * 読み込めない・見つからないときは panic と同じ扱い: メッセージを標準エラーに出して終了コード 101 (インタプリタと同じ)。 */
#ifndef _WIN32
#include <dlfcn.h>
#endif
#include <stdio.h>

void *ksn_rt_sys_dlsym(const char *lib, const char *sym) {
    enum { N = 64 };
    static struct { const char *lib; const char *sym; void *h; void *p; } tab[N];
    static int n = 0;
    for (int i = 0; i < n; i++) {
        if (strcmp(tab[i].lib, lib) == 0 && strcmp(tab[i].sym, sym) == 0) return tab[i].p;
    }
    void *h = 0;
    for (int i = 0; i < n; i++) {
        if (strcmp(tab[i].lib, lib) == 0) h = tab[i].h;
    }
    void *p = 0;
#ifdef _WIN32
    if (!h) h = (void *)LoadLibraryA(lib);
    if (!h) {
        fprintf(stderr, "panic: cannot load the shared library `%s`\n", lib);
        exit(101);
    }
    p = (void *)GetProcAddress((HMODULE)h, sym);
#else
    if (!h) h = dlopen(lib, RTLD_NOW);
    if (!h) {
        const char *why = dlerror();
        fprintf(stderr, "panic: cannot load the shared library `%s`%s%s\n", lib, why ? ": " : "", why ? why : "");
        exit(101);
    }
    p = dlsym(h, sym);
#endif
    if (!p) {
        fprintf(stderr, "panic: symbol `%s` was not found in `%s`\n", sym, lib);
        exit(101);
    }
    if (n < N) {
        tab[n].lib = lib;
        tab[n].sym = sym;
        tab[n].h = h;
        tab[n].p = p;
        n++;
    }
    return p;
}
