/* ネイティブ生成 (LLVM) の OS ランタイム (std.fs / std.net / std.process)。
 * cli.rs (`runtime_c_args`) がこのファイルを `include_str!` で埋め込み、IR が `ksn_rt_sys_` の関数を
 * 参照するときだけ、リンク時に C コンパイラで一緒にコンパイルする (kir_rt.c と同じ仕組み)。
 *
 * 規約: 戻り値は i64。0 以上なら成功 (値は関数ごと)、-1 なら失敗で、エラーを e[0..2] に書く。
 *   e[0] = std.io.ErrorKind のバリアント番号 (spec/stdlib/std/io.sig の宣言順)、e[1] = 発生源の OS エラー番号。
 * std.net.Error は e[1] だけを使う (OS エラー番号が無いときは -1)。
 * 振る舞いはインタプリタ (natives.rs / net.rs) と同じにする。 */
#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif
#ifdef _WIN32
#define _CRT_RAND_S /* rand_s (乱数の OS 源) を出す */
#endif
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <errno.h>
#include <fcntl.h>
#include <sys/types.h>
#include <sys/stat.h>
#if defined(__linux__)
#include <sys/random.h> /* getrandom(2) */
#endif

#ifdef _WIN32
#define WIN32_LEAN_AND_MEAN
#include <winsock2.h>
#include <ws2tcpip.h>
#include <io.h>
#include <direct.h>
#include <windows.h>
#else
#include <unistd.h>
#include <signal.h>
#include <spawn.h>
#include <sys/wait.h>
#include <sys/mman.h>
#include <pthread.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <netdb.h>
#include <netinet/in.h>
#include <arpa/inet.h>
#endif

typedef uint8_t u8;
typedef uint64_t u64;
typedef int64_t i64;
typedef int32_t i32;

/* std.io.ErrorKind のバリアント番号 (spec/stdlib/std/io.sig の宣言順。TimedOut は Other の前)。 */
enum { K_NOT_FOUND = 0, K_PERMISSION_DENIED, K_ALREADY_EXISTS, K_INVALID_INPUT, K_UNEXPECTED_EOF, K_BROKEN_PIPE, K_INTERRUPTED, K_OUT_OF_MEMORY, K_TIMED_OUT, K_OTHER };

/* errno から ErrorKind を決める (Rust の標準ライブラリと同じ対応。タイムアウトの EAGAIN は WouldBlock だが、木走査と同じく TimedOut にする)。 */
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
    case EAGAIN:
    case ETIMEDOUT: return K_TIMED_OUT;
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

/* OS の乱数で `n` バイトを `p` に書く (std.rand.fill_secure と Rng.from_entropy の源。設計_KIR.md 26 章)。
 * Linux は getrandom(2) (無ければ /dev/urandom)、Windows は rand_s、ほかの Unix は /dev/urandom。
 * 成功は 0、失敗は -1 (e[0..2] にエラー)。インタプリタの natives.rs の os_random と同じ源。 */
/* 失敗: `e` が NULL なら (致命的な形。`Rng.from_entropy`) 文言を出して終了コード 101 で終わる。そうでなければ Err にする。 */
static i64 random_fail(i32 *e, int err) {
    if (!e) {
        fputs("ksn: std.rand.Rng.from_entropy: OS の乱数源が読めない\n", stderr);
        exit(101);
    }
    return fail(e, err);
}

i64 ksn_rt_sys_random(u8 *p, u64 n, i32 *e) {
#ifdef _WIN32
    u64 i = 0;
    while (i < n) {
        unsigned int w;
        if (rand_s(&w) != 0) return random_fail(e, EIO);
        for (int k = 0; k < 4 && i < n; k++) p[i++] = (u8)(w >> (8 * k));
    }
    return 0;
#else
#if defined(__linux__)
    u64 got = 0;
    while (got < n) {
        ssize_t r = getrandom(p + got, (size_t)(n - got), 0);
        if (r < 0) {
            if (errno == EINTR) continue;
            if (errno == ENOSYS) break; /* 古いカーネル: /dev/urandom へ */
            return random_fail(e, errno);
        }
        got += (u64)r;
    }
    if (got == n) return 0;
    p += got;
    n -= got;
#endif
    int fd = open("/dev/urandom", O_RDONLY | O_CLOEXEC);
    if (fd < 0) return random_fail(e, errno);
    u64 k = 0;
    while (k < n) {
        ssize_t r = read(fd, p + k, (size_t)(n - k));
        if (r < 0) {
            if (errno == EINTR) continue;
            int err = errno;
            close(fd);
            return random_fail(e, err);
        }
        if (r == 0) {
            close(fd);
            return random_fail(e, EIO);
        }
        k += (u64)r;
    }
    close(fd);
    return 0;
#endif
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

/* create = 2: 追記で開く (なければ作る) std.fs.open_append。
 * create = 0: 読み取り専用で開く (std.fs.open)。create = 1: 作成して切り詰め、書き込み専用 (std.fs.create)。戻り値は記述子。 */
i64 ksn_rt_sys_open(const u8 *p, u64 n, i32 create, i32 *e) {
    int nul;
    char *path = cstr(p, n, &nul);
    if (!path) return nul ? fail_invalid(e) : fail(e, ENOMEM);
    int fd;
#ifdef _WIN32
    fd = create == 2 ? _open(path, _O_WRONLY | _O_CREAT | _O_APPEND | _O_BINARY, _S_IREAD | _S_IWRITE) : create ? _open(path, _O_WRONLY | _O_CREAT | _O_TRUNC | _O_BINARY, _S_IREAD | _S_IWRITE) : _open(path, _O_RDONLY | _O_BINARY);
#else
    fd = create == 2 ? open(path, O_WRONLY | O_CREAT | O_APPEND | O_CLOEXEC, 0666) : create ? open(path, O_WRONLY | O_CREAT | O_TRUNC | O_CLOEXEC, 0666) : open(path, O_RDONLY | O_CLOEXEC);
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


i64 ksn_rt_sys_rename(const u8 *p, u64 n, const u8 *q, u64 m, i32 *e) {
    int nul, nul2;
    char *a = cstr(p, n, &nul);
    if (!a) return nul ? fail_invalid(e) : fail(e, ENOMEM);
    char *b = cstr(q, m, &nul2);
    if (!b) {
        free(a);
        return nul2 ? fail_invalid(e) : fail(e, ENOMEM);
    }
    int r = rename(a, b);
    int err = errno;
    free(a);
    free(b);
    if (r != 0) return fail(e, err);
    return 0;
}

i64 ksn_rt_sys_mkdir(const u8 *p, u64 n, i32 *e) {
    int nul;
    char *path = cstr(p, n, &nul);
    if (!path) return nul ? fail_invalid(e) : fail(e, ENOMEM);
#ifdef _WIN32
    int r = _mkdir(path);
#else
    int r = mkdir(path, 0777);
#endif
    int err = errno;
    free(path);
    if (r != 0) return fail(e, err);
    return 0;
}

i64 ksn_rt_sys_rmdir(const u8 *p, u64 n, i32 *e) {
    int nul;
    char *path = cstr(p, n, &nul);
    if (!path) return nul ? fail_invalid(e) : fail(e, ENOMEM);
#ifdef _WIN32
    int r = _rmdir(path);
#else
    int r = rmdir(path);
#endif
    int err = errno;
    free(path);
    if (r != 0) return fail(e, err);
    return 0;
}

/* リンクを辿る。field 0 = 大きさ、1 = 種別 (0 ファイル / 1 ディレクトリ / 2 それ以外)、2 = 更新時刻 (UNIX 時刻からのナノ秒。範囲外は 0)。 */
static int kind_of_mode(unsigned mode) {
    if ((mode & S_IFMT) == S_IFREG) return 0;
    if ((mode & S_IFMT) == S_IFDIR) return 1;
    return 2;
}

i64 ksn_rt_sys_stat(const u8 *p, u64 n, i32 field, i32 *e) {
    int nul;
    char *path = cstr(p, n, &nul);
    if (!path) return nul ? fail_invalid(e) : fail(e, ENOMEM);
    struct stat st;
    int r = stat(path, &st);
    int err = errno;
    free(path);
    if (r != 0) return fail(e, err);
    if (field == 0) return (i64)st.st_size;
    if (field == 1) return kind_of_mode((unsigned)st.st_mode);
#ifdef _WIN32
    i64 secs = (i64)st.st_mtime, ns = 0;
#elif defined(__APPLE__)
    i64 secs = (i64)st.st_mtimespec.tv_sec, ns = (i64)st.st_mtimespec.tv_nsec;
#else
    i64 secs = (i64)st.st_mtim.tv_sec, ns = (i64)st.st_mtim.tv_nsec;
#endif
    if (secs < 0 || secs > 9223372035LL) return 0;
    return secs * 1000000000LL + ns;
}

#ifndef _WIN32
#include <dirent.h>
struct dent { u8 *name; int kind; };
static int dent_cmp(const void *x, const void *y) {
    const struct dent *a = (const struct dent *)x, *b = (const struct dent *)y;
    size_t la = strlen((const char *)a->name), lb = strlen((const char *)b->name);
    int c = memcmp(a->name, b->name, la < lb ? la : lb);
    if (c != 0) return c;
    return la < lb ? -1 : la > lb ? 1 : 0;
}

/* ディレクトリの列挙。名前のバイト順に整列した `[種別][名前][0]` の並びを buf (容量 cap) に書き、必要な総バイト数を返す
 * (cap を超えるときは書かない)。種別は stat (リンクを辿る) の結果。 */
i64 ksn_rt_sys_readdir(const u8 *p, u64 n, u8 *buf, u64 cap, i32 *e) {
    int nul;
    char *path = cstr(p, n, &nul);
    if (!path) return nul ? fail_invalid(e) : fail(e, ENOMEM);
    DIR *d = opendir(path);
    if (!d) {
        int err = errno;
        free(path);
        return fail(e, err);
    }
    size_t plen = strlen(path), count = 0, room = 16;
    struct dent *ents = (struct dent *)malloc(room * sizeof(struct dent));
    int err = 0;
    if (!ents) err = ENOMEM;
    struct dirent *de;
    while (!err && (errno = 0, de = readdir(d)) != NULL) {
        if (strcmp(de->d_name, ".") == 0 || strcmp(de->d_name, "..") == 0) continue;
        size_t nl = strlen(de->d_name);
        char *full = (char *)malloc(plen + nl + 2);
        u8 *nm = (u8 *)malloc(nl + 1);
        if (!full || !nm) {
            free(full);
            free(nm);
            err = ENOMEM;
            break;
        }
        memcpy(full, path, plen);
        full[plen] = '/';
        memcpy(full + plen + 1, de->d_name, nl + 1);
        memcpy(nm, de->d_name, nl + 1);
        struct stat st;
        int kind = stat(full, &st) == 0 ? kind_of_mode((unsigned)st.st_mode) : 2;
        free(full);
        if (count == room) {
            struct dent *g = (struct dent *)realloc(ents, room * 2 * sizeof(struct dent));
            if (!g) {
                free(nm);
                err = ENOMEM;
                break;
            }
            ents = g;
            room *= 2;
        }
        ents[count].name = nm;
        ents[count].kind = kind;
        count++;
    }
    if (!err && errno != 0) err = errno;
    closedir(d);
    free(path);
    u64 total = 0;
    if (!err) {
        if (count > 1) qsort(ents, count, sizeof(struct dent), dent_cmp);
        for (size_t i = 0; i < count; i++) total += 2 + strlen((const char *)ents[i].name);
        if (total <= cap) {
            u8 *w = buf;
            for (size_t i = 0; i < count; i++) {
                size_t l = strlen((const char *)ents[i].name);
                *w++ = (u8)ents[i].kind;
                memcpy(w, ents[i].name, l);
                w += l;
                *w++ = 0;
            }
        }
    }
    for (size_t i = 0; i < count; i++) free(ents[i].name);
    free(ents);
    if (err) return fail(e, err);
    return (i64)total;
}
#else
i64 ksn_rt_sys_readdir(const u8 *p, u64 n, u8 *buf, u64 cap, i32 *e) {
    (void)p; (void)n; (void)buf; (void)cap;
    return fail(e, ENOSYS);
}
#endif

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

/* ---------------------------------------------------------------- std.net (POSIX と Windows の winsock) */
#ifdef _WIN32
#define net_close closesocket
#define SHUT_RD SD_RECEIVE
#define SHUT_WR SD_SEND
#define SHUT_RDWR SD_BOTH
typedef int ksn_sl;
/* winsock のエラーを errno の番号に写す (std.io.ErrorKind の対応 `kind_of` を共有する)。 */
static int net_errno(void) {
    switch (WSAGetLastError()) {
    case WSAECONNRESET: return ECONNRESET;
    case WSAECONNABORTED: return ECONNABORTED;
    case WSAECONNREFUSED: return ECONNREFUSED;
    case WSAEADDRINUSE: return EADDRINUSE;
    case WSAENOTCONN: return ENOTCONN;
    case WSAETIMEDOUT: case WSAEWOULDBLOCK: return ETIMEDOUT;
    case WSAEINTR: return EINTR;
    case WSAEINVAL: return EINVAL;
    case WSAEACCES: return EACCES;
    default: return EIO;
    }
}
#else
#define net_close close
typedef socklen_t ksn_sl;
static int net_errno(void) { return errno; }
#endif


static void init_net(void) {
    static int done;
    if (!done) {
#ifdef _WIN32
        WSADATA wd;
        WSAStartup(MAKEWORD(2, 2), &wd);
#else
        /* 閉じた相手への write で落ちず EPIPE を返す (Rust の標準ライブラリと同じ)。 */
        signal(SIGPIPE, SIG_IGN);
#endif
        done = 1;
    }
}

/* "host:port" を分けて、getaddrinfo で解決する (socktype は SOCK_STREAM / SOCK_DGRAM)。失敗は fail_net。 */
static int resolve_t(const u8 *a, u64 n, int socktype, struct addrinfo **out, i32 *e) {
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
    hints.ai_socktype = socktype;
    int rc = getaddrinfo(host, portbuf, &hints, out);
    free(inner);
    free(s);
    if (rc != 0 || *out == NULL) { fail_net(e); return 0; }
    return 1;
}

static int resolve(const u8 *a, u64 n, struct addrinfo **out, i32 *e) {
    return resolve_t(a, n, SOCK_STREAM, out, e);
}

static void set_cloexec(int fd) {
#ifdef _WIN32
    (void)fd;
#else
    int fl = fcntl(fd, F_GETFD);
    if (fl >= 0) fcntl(fd, F_SETFD, fl | FD_CLOEXEC);
#endif
}

i64 ksn_rt_sys_connect(const u8 *a, u64 n, i32 *e) {
    init_net();
    struct addrinfo *res;
    if (!resolve(a, n, &res, e)) return -1;
    int last = 0;
    for (struct addrinfo *ai = res; ai; ai = ai->ai_next) {
        int s = socket(ai->ai_family, ai->ai_socktype, ai->ai_protocol);
        if (s < 0) { last = net_errno(); continue; }
        set_cloexec(s);
        if (connect(s, ai->ai_addr, ai->ai_addrlen) == 0) { freeaddrinfo(res); return s; }
        last = net_errno();
        net_close(s);
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
        if (s < 0) { last = net_errno(); continue; }
        set_cloexec(s);
        int one = 1;
        setsockopt(s, SOL_SOCKET, SO_REUSEADDR, &one, sizeof one);
        if (bind(s, ai->ai_addr, ai->ai_addrlen) == 0 && listen(s, 128) == 0) { freeaddrinfo(res); return s; }
        last = net_errno();
        net_close(s);
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
        if (net_errno() == EINTR) continue;
        e[0] = K_OTHER;
        e[1] = (i32)net_errno();
        return -1;
    }
}

i64 ksn_rt_sys_local_port(i32 fd, i32 *e) {
    struct sockaddr_storage ss;
    ksn_sl len = sizeof ss;
    if (getsockname(fd, (struct sockaddr *)&ss, &len) != 0) { e[0] = K_OTHER; e[1] = (i32)net_errno(); return -1; }
    if (ss.ss_family == AF_INET) return ntohs(((struct sockaddr_in *)&ss)->sin_port);
    if (ss.ss_family == AF_INET6) return ntohs(((struct sockaddr_in6 *)&ss)->sin6_port);
    return fail_net(e);
}

/* ---- std.net の追加分 (タイムアウト・shutdown・アドレス・resolve・UDP)。木走査 net.rs と同じ結果にする。 */

/* ソケットアドレスを "ip:port" (IPv6 は "[ip]:port") の文字列にする。 */
static int format_addr(const struct sockaddr *sa, char *txt, size_t cap) {
    char ip[INET6_ADDRSTRLEN];
    if (sa->sa_family == AF_INET) {
        const struct sockaddr_in *s4 = (const struct sockaddr_in *)sa;
        if (!inet_ntop(AF_INET, &s4->sin_addr, ip, sizeof ip)) return -1;
        snprintf(txt, cap, "%s:%u", ip, (unsigned)ntohs(s4->sin_port));
        return 0;
    }
    if (sa->sa_family == AF_INET6) {
        const struct sockaddr_in6 *s6 = (const struct sockaddr_in6 *)sa;
        if (!inet_ntop(AF_INET6, &s6->sin6_addr, ip, sizeof ip)) return -1;
        snprintf(txt, cap, "[%s]:%u", ip, (unsigned)ntohs(s6->sin6_port));
        return 0;
    }
    return -1;
}

/* 0 = 秒 (ms = 0 はタイムアウトなし)。SO_RCVTIMEO (読み) / SO_SNDTIMEO (書き)。 */
i64 ksn_rt_sys_net_timeout(i32 fd, i32 write_side, u64 ms, i32 *e) {
#ifdef _WIN32
    DWORD tv = (DWORD)ms; /* winsock はミリ秒の DWORD */
    if (setsockopt((SOCKET)fd, SOL_SOCKET, write_side ? SO_SNDTIMEO : SO_RCVTIMEO, (const char *)&tv, sizeof tv) != 0)
#else
    struct timeval tv;
    tv.tv_sec = (time_t)(ms / 1000);
    tv.tv_usec = (suseconds_t)((ms % 1000) * 1000);
    if (setsockopt(fd, SOL_SOCKET, write_side ? SO_SNDTIMEO : SO_RCVTIMEO, &tv, sizeof tv) != 0)
#endif return fail(e, net_errno());
    return 0;
}

/* how: 0 = 読み、1 = 書き、2 = 両方。 */
i64 ksn_rt_sys_net_shutdown(i32 fd, i32 how, i32 *e) {
    int h = how == 0 ? SHUT_RD : (how == 1 ? SHUT_WR : SHUT_RDWR);
    if (shutdown(fd, h) != 0) return fail(e, net_errno());
    return 0;
}

/* 相手 (peer != 0) か自分のアドレスを "ip:port" で out へ書く。入りきらなければ InvalidInput。書いたバイト数。 */
i64 ksn_rt_sys_net_addr(i32 fd, i32 peer, u8 *out, u64 cap, i32 *e) {
    struct sockaddr_storage ss;
    ksn_sl len = sizeof ss;
    int rc = peer ? getpeername(fd, (struct sockaddr *)&ss, &len) : getsockname(fd, (struct sockaddr *)&ss, &len);
    if (rc != 0) return fail(e, net_errno());
    char txt[80];
    if (format_addr((struct sockaddr *)&ss, txt, sizeof txt) != 0) return fail_invalid(e);
    u64 n = strlen(txt);
    if (n > cap) return fail_invalid(e);
    memcpy(out, txt, n);
    return (i64)n;
}

/* host (ポートは 10 進の文字列) を getaddrinfo で解決し、"アドレス\0" の並びを out へ書く。 */
i64 ksn_rt_sys_net_resolve(const u8 *host, u64 hn, i32 port, u8 *out, u64 cap, i32 *e) {
    init_net();
    int nul;
    char *h = cstr(host, hn, &nul);
    if (!h) { e[0] = K_OTHER; e[1] = 0; return -1; }
    char portbuf[8];
    snprintf(portbuf, sizeof portbuf, "%d", (int)port);
    struct addrinfo hints, *res = NULL;
    memset(&hints, 0, sizeof hints);
    hints.ai_family = AF_UNSPEC;
    hints.ai_socktype = SOCK_STREAM;
    int rc = getaddrinfo(h, portbuf, &hints, &res);
    free(h);
    if (rc != 0 || res == NULL) {
        /* 解決の失敗は OS の番号を持たない (Other、番号 0)。 */
        if (res) freeaddrinfo(res);
        e[0] = K_OTHER;
        e[1] = 0;
        return -1;
    }
    u64 total = 0;
    for (struct addrinfo *ai = res; ai; ai = ai->ai_next) {
        char txt[80];
        if (format_addr(ai->ai_addr, txt, sizeof txt) != 0) continue;
        u64 len = strlen(txt) + 1;
        if (total + len > cap) { freeaddrinfo(res); return fail_invalid(e); }
        memcpy(out + total, txt, len);
        total += len;
    }
    freeaddrinfo(res);
    return (i64)total;
}

/* UDP の束縛。"host:port" (`resolve` と同じ書き方) を読んで、最初に束縛できたソケットを返す。 */
i64 ksn_rt_sys_net_udp_bind(const u8 *a, u64 n, i32 *e) {
    init_net();
    struct addrinfo *res;
    if (!resolve_t(a, n, SOCK_DGRAM, &res, e)) return -1;
    int last = 0;
    for (struct addrinfo *ai = res; ai; ai = ai->ai_next) {
        int s = socket(ai->ai_family, ai->ai_socktype, ai->ai_protocol);
        if (s < 0) { last = net_errno(); continue; }
        set_cloexec(s);
        if (bind(s, ai->ai_addr, ai->ai_addrlen) == 0) { freeaddrinfo(res); return s; }
        last = net_errno();
        net_close(s);
    }
    freeaddrinfo(res);
    e[0] = K_OTHER;
    e[1] = (i32)last;
    return -1;
}

/* UDP の送信。宛先 "host:port" を解決して、最初に送れたアドレスへ送る。送ったバイト数。 */
i64 ksn_rt_sys_net_sendto(i32 fd, const u8 *buf, u64 n, const u8 *a, u64 an, i32 *e) {
    init_net();
    struct addrinfo *res;
    if (!resolve_t(a, an, SOCK_DGRAM, &res, e)) { e[0] = K_OTHER; e[1] = 0; return -1; }
    i64 r = -1;
    int last = 0;
    for (struct addrinfo *ai = res; ai; ai = ai->ai_next) {
        ssize_t k = sendto(fd, buf, n, 0, ai->ai_addr, ai->ai_addrlen);
        if (k >= 0) { r = (i64)k; break; }
        last = net_errno();
    }
    freeaddrinfo(res);
    if (r < 0) return fail(e, last);
    return r;
}

/* UDP の受信。受けたバイト数を返し、送り元を "ip:port\0" で from へ書く。 */
i64 ksn_rt_sys_net_recvfrom(i32 fd, u8 *buf, u64 n, u8 *from, u64 fcap, i32 *e) {
    init_net();
    struct sockaddr_storage ss;
    memset(&ss, 0, sizeof ss);
    ksn_sl sl = sizeof ss;
    ssize_t got;
    for (;;) {
        got = recvfrom(fd, buf, n, 0, (struct sockaddr *)&ss, &sl);
        if (got >= 0) break;
        if (net_errno() == EINTR) continue;
        return fail(e, net_errno());
    }
    char txt[80];
    /* カーネルが送信元を書かなかった (接続済みの socket など。sl が family の分も無い) ときは、空の文字列を返す。読み捨てたデータを失わない。 */
    if (sl < offsetof(struct sockaddr_storage, ss_family) + sizeof ss.ss_family) {
        txt[0] = 0;
    } else if (format_addr((struct sockaddr *)&ss, txt, sizeof txt) != 0) {
        return fail_invalid(e);
    }
    u64 tl = strlen(txt) + 1;
    if (tl > fcap) return fail_invalid(e);
    memcpy(from, txt, tl);
    return (i64)got;
}


/* ---- std.env / std.time ---- */
#include <time.h>
#ifndef _WIN32
#include <sched.h>
#endif
#ifdef _WIN32
#include <windows.h>
#endif

/* `main` の引数は rt.c が覚える。引数 0 (プログラム名) は `std.env` の見える範囲に含めない。 */
extern int ksn_rt_argc;
extern char **ksn_rt_argv;
#define g_argc ksn_rt_argc
#define g_argv ksn_rt_argv

/* LLVM パスでは kir_rt.c もリンクされ、そこで同名の強い定義がある。弱いシンボルにして競合を避ける (clif パスでは sysrt.c のみリンクするため、ここで定義が必要)。 */
#if defined(__GNUC__)
__attribute__((weak))
#endif
i64 ksn_rt_sys_arg_count(void) { return g_argc > 0 ? (i64)(g_argc - 1) : 0; }

/* `std.env.arg(i)`: 範囲内なら NUL 終端の文字列 (長さは *len)、範囲外なら 0。 */
#if defined(__GNUC__)
__attribute__((weak))
#endif
i64 ksn_rt_sys_arg(i64 i, i64 *len) {
    if (i < 0 || i >= ksn_rt_sys_arg_count()) return 0;
    char *s = g_argv[i + 1];
    *len = (i64)strlen(s);
    return (i64)(intptr_t)s;
}

/* `std.env.var(name)`: 見つかれば値 (長さは *len)、なければ 0。 */
#if defined(__GNUC__)
__attribute__((weak))
#endif
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

/* 実時刻 (UNIX 時刻からのナノ秒、`std.time.system_nanos`)。失敗したら 0 (木走査と同じ)。 */
i64 ksn_rt_sys_wall_ns(void) {
#ifdef _WIN32
    FILETIME ft;
    GetSystemTimeAsFileTime(&ft);
    u64 t = ((u64)ft.dwHighDateTime << 32) | (u64)ft.dwLowDateTime; /* 1601 年からの 100 ns */
    return (i64)((t - 116444736000000000ULL) * 100ULL);
#else
    struct timespec ts;
    if (clock_gettime(CLOCK_REALTIME, &ts) != 0) return 0;
    return (i64)ts.tv_sec * 1000000000LL + (i64)ts.tv_nsec;
#endif
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

#ifndef _WIN32
#include <pthread.h>
static pthread_mutex_t ksn_dlsym_mu = PTHREAD_MUTEX_INITIALIZER;
static void *ksn_rt_sys_dlsym_locked(const char *lib, const char *sym);
/* 表と dlopen の再入を 1 本の mutex で守る (実スレッドから同時に呼ばれうる)。 */
void *ksn_rt_sys_dlsym(const char *lib, const char *sym) {
    pthread_mutex_lock(&ksn_dlsym_mu);
    void *r = ksn_rt_sys_dlsym_locked(lib, sym);
    pthread_mutex_unlock(&ksn_dlsym_mu);
    return r;
}
static void *ksn_rt_sys_dlsym_locked(const char *lib, const char *sym) {
#else
void *ksn_rt_sys_dlsym(const char *lib, const char *sym) {
#endif
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

/* ---------------------------------------------------------------- std.process (POSIX のみ)
 * Command の設定は、この表のハンドル (i32) で持つ。spawn / output / cancel が消費して表から外す。
 * 起動は posix_spawnp (複数スレッドから呼んでも安全。パイプの親側の記述子は O_CLOEXEC)。
 * 命令の番号は kir.rs の `ProcFn::ALL` の並び。 */
#ifndef _WIN32
extern char **environ;

enum { P_NEW = 0, P_ARG, P_ENV_REMOVE, P_DIR, P_ENV, P_ENV_CLEAR, P_IN_PIPED, P_IN_NULL, P_OUT_PIPED, P_OUT_NULL, P_ERR_PIPED, P_ERR_NULL, P_CANCEL };
enum { IO_INHERIT = 0, IO_NULL, IO_PIPED };

typedef struct { int set; char *k; char *v; } penv;
typedef struct {
    char *prog;
    char **args; u64 nargs, capargs;
    penv *env; u64 nenv, capenv;
    int clear;
    char *dir;
    int io[3];
    int bad; /* NUL を含む文字列が渡された (spawn は InvalidInput) */
} pcmd;

static pcmd **ptab;
static u64 pcap;
static pthread_mutex_t plock = PTHREAD_MUTEX_INITIALIZER;

static char *pdup(const u8 *p, u64 n, int *bad) {
    if (n > 0 && memchr(p, 0, n) != NULL) { *bad = 1; n = (u64)((const u8 *)memchr(p, 0, n) - p); }
    char *s = (char *)malloc(n + 1);
    if (!s) { *bad = 1; return NULL; }
    if (n > 0) memcpy(s, p, n);
    s[n] = 0;
    return s;
}

static pcmd *pget(i32 id) {
    pcmd *c = NULL;
    pthread_mutex_lock(&plock);
    if (id >= 0 && (u64)id < pcap) c = ptab[id];
    pthread_mutex_unlock(&plock);
    return c;
}

static void pfree(pcmd *c) {
    if (!c) return;
    free(c->prog);
    for (u64 i = 0; i < c->nargs; i++) free(c->args[i]);
    free(c->args);
    for (u64 i = 0; i < c->nenv; i++) { free(c->env[i].k); free(c->env[i].v); }
    free(c->env);
    free(c->dir);
    free(c);
}

/* 表から外して返す。 */
static pcmd *ptake(i32 id) {
    pcmd *c = NULL;
    pthread_mutex_lock(&plock);
    if (id >= 0 && (u64)id < pcap) { c = ptab[id]; ptab[id] = NULL; }
    pthread_mutex_unlock(&plock);
    return c;
}

/* Command.new。戻り値はハンドル (確保に失敗したら -1。spawn が ENOMEM で失敗する)。 */
i32 ksn_rt_sys_proc_new(const u8 *p, u64 n) {
    pcmd *c = (pcmd *)calloc(1, sizeof(pcmd));
    if (!c) return -1;
    c->prog = pdup(p, n, &c->bad);
    pthread_mutex_lock(&plock);
    u64 id = 0;
    while (id < pcap && ptab[id]) id++;
    if (id == pcap) {
        u64 nc = pcap ? pcap * 2 : 8;
        pcmd **nt = (pcmd **)realloc(ptab, nc * sizeof(pcmd *));
        if (!nt) { pthread_mutex_unlock(&plock); pfree(c); return -1; }
        memset(nt + pcap, 0, (nc - pcap) * sizeof(pcmd *));
        ptab = nt; pcap = nc;
    }
    ptab[id] = c;
    pthread_mutex_unlock(&plock);
    return (i32)id;
}

void ksn_rt_sys_proc_cmd(i32 id, i32 sel, const u8 *a, u64 an, const u8 *b, u64 bn) {
    if (sel == P_CANCEL) { pfree(ptake(id)); return; }
    pcmd *c = pget(id);
    if (!c) return;
    switch (sel) {
    case P_ARG: {
        if (c->nargs == c->capargs) {
            u64 nc = c->capargs ? c->capargs * 2 : 8;
            char **na = (char **)realloc(c->args, nc * sizeof(char *));
            if (!na) { c->bad = 1; return; }
            c->args = na; c->capargs = nc;
        }
        c->args[c->nargs] = pdup(a, an, &c->bad);
        if (c->args[c->nargs]) c->nargs++;
        break;
    }
    case P_ENV:
    case P_ENV_REMOVE: {
        if (c->nenv == c->capenv) {
            u64 nc = c->capenv ? c->capenv * 2 : 8;
            penv *ne = (penv *)realloc(c->env, nc * sizeof(penv));
            if (!ne) { c->bad = 1; return; }
            c->env = ne; c->capenv = nc;
        }
        penv *x = &c->env[c->nenv++];
        x->set = sel == P_ENV;
        x->k = pdup(a, an, &c->bad);
        x->v = sel == P_ENV ? pdup(b, bn, &c->bad) : NULL;
        break;
    }
    case P_ENV_CLEAR:
        c->clear = 1;
        for (u64 i = 0; i < c->nenv; i++) { free(c->env[i].k); free(c->env[i].v); }
        c->nenv = 0;
        break;
    case P_DIR: free(c->dir); c->dir = pdup(a, an, &c->bad); break;
    case P_IN_PIPED: c->io[0] = IO_PIPED; break;
    case P_IN_NULL: c->io[0] = IO_NULL; break;
    case P_OUT_PIPED: c->io[1] = IO_PIPED; break;
    case P_OUT_NULL: c->io[1] = IO_NULL; break;
    case P_ERR_PIPED: c->io[2] = IO_PIPED; break;
    case P_ERR_NULL: c->io[2] = IO_NULL; break;
    default: break;
    }
}

/* 子の環境 (`KEY=VALUE` の NULL 終端配列)。`own` には新しく確保した文字列を足す。 */
static char **build_env(pcmd *c, char ***own, u64 *nown) {
    u64 base = 0;
    if (!c->clear) while (environ && environ[base]) base++;
    u64 cap = base + c->nenv + 1, n = 0;
    char **ev = (char **)calloc(cap, sizeof(char *));
    *own = (char **)calloc(c->nenv + 1, sizeof(char *));
    *nown = 0;
    if (!ev || !*own) { free(ev); free(*own); *own = NULL; return NULL; }
    if (!c->clear) for (u64 i = 0; i < base; i++) ev[n++] = environ[i];
    for (u64 i = 0; i < c->nenv; i++) {
        penv *x = &c->env[i];
        size_t kl = strlen(x->k);
        for (u64 j = 0; j < n; ) {
            if (strncmp(ev[j], x->k, kl) == 0 && ev[j][kl] == '=') { memmove(ev + j, ev + j + 1, (n - j - 1) * sizeof(char *)); n--; }
            else j++;
        }
        if (x->set) {
            size_t vl = strlen(x->v);
            char *s = (char *)malloc(kl + vl + 2);
            if (!s) { free(ev); return NULL; }
            memcpy(s, x->k, kl); s[kl] = '='; memcpy(s + kl + 1, x->v, vl + 1);
            (*own)[(*nown)++] = s;
            ev[n++] = s;
        }
    }
    ev[n] = NULL;
    return ev;
}

/* 無名の一時ファイル (output の受け皿)。 */
static int anon_file(void) {
    int fd = -1;
#if defined(__linux__) && defined(MFD_CLOEXEC)
    fd = memfd_create("ksn-output", MFD_CLOEXEC);
    if (fd >= 0) return fd;
#endif
    char t[] = "/tmp/ksn_out_XXXXXX";
    fd = mkstemp(t);
    if (fd < 0) return -1;
    unlink(t);
    fcntl(fd, F_SETFD, FD_CLOEXEC);
    return fd;
}

static void decode_status(int st, i32 *code, i32 *sig) {
    if (WIFEXITED(st)) { *code = WEXITSTATUS(st); *sig = 0; }
    else if (WIFSIGNALED(st)) { *code = -1; *sig = WTERMSIG(st); }
    else { *code = -1; *sig = 0; }
}

static int wait_pid(i32 pid, int *st) {
    for (;;) {
        pid_t r = waitpid(pid, st, 0);
        if (r >= 0) return 0;
        if (errno != EINTR) return errno;
    }
}

/* spawn (capture = 0) は out[0..4] = { pid, stdin, stdout, stderr } (パイプの親側。ないものは -1)。
 * output (capture = 1) は out[0..4] = { 終了コード, シグナル, 標準出力の記述子, 標準エラーの記述子 } (先頭へ巻き戻し済み)。 */
i64 ksn_rt_sys_proc_spawn(i32 id, i32 capture, i32 *out, i32 *e) {
    pcmd *c = ptake(id);
    if (!c) return fail(e, ENOMEM);
    if (c->bad || !c->prog) { pfree(c); return fail_invalid(e); }
    int io[3] = { c->io[0], c->io[1], c->io[2] };
    if (capture) { io[0] = IO_NULL; io[1] = io[2] = IO_PIPED; }
    int parent[3] = { -1, -1, -1 }, child[3] = { -1, -1, -1 };
    int err = 0;
    for (int i = 0; i < 3 && !err; i++) {
        if (io[i] != IO_PIPED) continue;
        if (capture) {
            int f = anon_file();
            if (f < 0) { err = errno; break; }
            parent[i] = f; child[i] = f;
        } else {
            int p[2];
            if (pipe2(p, O_CLOEXEC) < 0) { err = errno; break; }
            parent[i] = i == 0 ? p[1] : p[0];
            child[i] = i == 0 ? p[0] : p[1];
        }
    }
    posix_spawn_file_actions_t fa;
    posix_spawnattr_t at;
    int have_fa = 0, have_at = 0;
    char **argv = NULL, **ev = NULL, **own = NULL;
    u64 nown = 0;
    pid_t pid = 0;
    if (!err) { err = posix_spawn_file_actions_init(&fa); have_fa = err == 0; }
    if (!err) { err = posix_spawnattr_init(&at); have_at = err == 0; }
    if (!err) {
        sigset_t none, dfl;
        sigemptyset(&none);
        sigemptyset(&dfl);
        sigaddset(&dfl, SIGPIPE);
        posix_spawnattr_setsigmask(&at, &none);
        posix_spawnattr_setsigdefault(&at, &dfl);
        posix_spawnattr_setflags(&at, POSIX_SPAWN_SETSIGMASK | POSIX_SPAWN_SETSIGDEF);
        for (int i = 0; i < 3 && !err; i++) {
            if (io[i] == IO_NULL) err = posix_spawn_file_actions_addopen(&fa, i, "/dev/null", i == 0 ? O_RDONLY : O_WRONLY, 0);
            else if (io[i] == IO_PIPED && child[i] != i) err = posix_spawn_file_actions_adddup2(&fa, child[i], i);
        }
    }
    if (!err && c->dir) {
#if (defined(__GLIBC__) && (__GLIBC__ > 2 || (__GLIBC__ == 2 && __GLIBC_MINOR__ >= 29))) || defined(__APPLE__)
        err = posix_spawn_file_actions_addchdir_np(&fa, c->dir);
#else
        err = ENOSYS;
#endif
    }
    if (!err) {
        argv = (char **)calloc(c->nargs + 2, sizeof(char *));
        ev = build_env(c, &own, &nown);
        if (!argv || !ev) err = ENOMEM;
    }
    if (!err) {
        argv[0] = c->prog;
        for (u64 i = 0; i < c->nargs; i++) argv[i + 1] = c->args[i];
        err = posix_spawnp(&pid, c->prog, &fa, &at, argv, ev);
    }
    if (have_fa) posix_spawn_file_actions_destroy(&fa);
    if (have_at) posix_spawnattr_destroy(&at);
    free(argv); free(ev);
    for (u64 i = 0; i < nown; i++) free(own[i]);
    free(own);
    pfree(c);
    if (err) {
        for (int i = 0; i < 3; i++) {
            if (parent[i] >= 0) close(parent[i]);
            if (child[i] >= 0 && child[i] != parent[i]) close(child[i]);
        }
        return fail(e, err);
    }
    /* 子の側の端は親では不要 */
    for (int i = 0; i < 3; i++) if (child[i] >= 0 && child[i] != parent[i]) close(child[i]);
    if (!capture) {
        out[0] = (i32)pid;
        for (int i = 0; i < 3; i++) out[1 + i] = parent[i];
        return 0;
    }
    int st = 0;
    int werr = wait_pid((i32)pid, &st);
    if (werr) {
        close(parent[1]); close(parent[2]);
        return fail(e, werr);
    }
    decode_status(st, &out[0], &out[1]);
    lseek(parent[1], 0, SEEK_SET);
    lseek(parent[2], 0, SEEK_SET);
    out[2] = parent[1];
    out[3] = parent[2];
    return 0;
}

/* st[0] = 終了コード (シグナルで死んだら -1)、st[1] = シグナル (なければ 0)。 */
i64 ksn_rt_sys_proc_wait(i32 pid, i32 *st2, i32 *e) {
    int st = 0;
    int err = wait_pid(pid, &st);
    if (err) return fail(e, err);
    decode_status(st, &st2[0], &st2[1]);
    return 0;
}

i64 ksn_rt_sys_proc_kill(i32 pid, i32 *e) {
    if (kill((pid_t)pid, SIGKILL) < 0) return fail(e, errno);
    return 0;
}
#else
i32 ksn_rt_sys_proc_new(const u8 *p, u64 n) { (void)p; (void)n; return -1; }
void ksn_rt_sys_proc_cmd(i32 id, i32 sel, const u8 *a, u64 an, const u8 *b, u64 bn) { (void)id; (void)sel; (void)a; (void)an; (void)b; (void)bn; }
i64 ksn_rt_sys_proc_spawn(i32 id, i32 capture, i32 *out, i32 *e) { (void)id; (void)capture; (void)out; return fail(e, ENOSYS); }
i64 ksn_rt_sys_proc_wait(i32 pid, i32 *st2, i32 *e) { (void)pid; (void)st2; return fail(e, ENOSYS); }
i64 ksn_rt_sys_proc_kill(i32 pid, i32 *e) { (void)pid; return fail(e, ENOSYS); }
#endif
