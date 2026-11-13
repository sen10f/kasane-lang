/* Cranelift バックエンドのコアランタイム (起動・panic・スタック溢れ検出)。
 * clif.rs が `include_str!` でこのファイルを埋め込み、ネイティブ実行ファイルには常にリンクする
 * (strrt.c / sysrt.c / thrrt.c は、参照があるときだけ)。仕様 5.2 / 7 / 8.2。
 *
 * - `main`: C の入口。引数 (`std.env` 用) を覚え、シグナルを設定し、Kasane の入口 `ksn_main` を呼ぶ。
 * - `ksn_rt_panic(msg, len)`: 標準エラーに `panic: <msg>` と改行を 1 回の書き込みで出し、終了コード 101 で `_exit`。
 *   どのスレッドからでも呼べる。stdio も `exit` の後始末 (atexit) も使わないので、他のスレッドが出力中でもデッドロックしない。
 * - `ksn_rt_entry_error()`: 入口が `Err` を返した。標準エラーに出して終了コード 1。
 * - スタック溢れ: スレッドごとに代替シグナルスタックと SIGSEGV / SIGBUS のハンドラを用意し、
 *   ガード領域への fault なら `stack overflow` を出して終了コード 102 で `_exit`。それ以外の fault は既定の動作 (クラッシュ) のまま。
 *   スタックの範囲は、Linux は `pthread_getattr_np` (メインスレッドは `getrlimit` と起動時のスタックアドレス)、
 *   macOS は `pthread_get_stackaddr_np` / `pthread_get_stacksize_np`。Windows は VEH で `EXCEPTION_STACK_OVERFLOW` を見る (未検証)。 */
#ifndef _WIN32
#define _GNU_SOURCE
#endif
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#ifdef _WIN32
#include <io.h>
#include <windows.h>
#else
#include <pthread.h>
#include <signal.h>
#include <sys/mman.h>
#include <sys/resource.h>
#include <sys/uio.h>
#include <unistd.h>
#endif

/* 入口 (clif.rs が生成する。0 なら正常終了)。 */
extern int ksn_main(void);

/* `std.env` (sysrt.c) が読む。引数 0 (プログラム名) は std.env からは見えない。 */
int ksn_rt_argc = 0;
char **ksn_rt_argv = 0;

static void write_all(int fd, const char *p, size_t n) {
    while (n > 0) {
#ifdef _WIN32
        int w = _write(fd, p, (unsigned)n);
#else
        ssize_t w = write(fd, p, n);
#endif
        if (w <= 0) return;
        p += w;
        n -= (size_t)w;
    }
}

void ksn_rt_panic(const char *msg, uint64_t len) {
#ifdef _WIN32
    write_all(2, "panic: ", 7);
    write_all(2, msg, (size_t)len);
    write_all(2, "\n", 1);
#else
    /* 1 回の writev で出す (他のスレッドの出力と混ざりにくい)。部分書き込みは残りを書く。 */
    static const char head[] = "panic: ";
    static const char nl[] = "\n";
    struct iovec iov[3] = {{(void *)head, 7}, {(void *)msg, (size_t)len}, {(void *)nl, 1}};
    ssize_t w = writev(2, iov, 3);
    size_t total = 8 + (size_t)len;
    if (w >= 0 && (size_t)w < total) {
        size_t done = (size_t)w;
        if (done < 7) {
            write_all(2, head + done, 7 - done);
            done = 7;
        }
        size_t m = done - 7;
        if (m < (size_t)len) write_all(2, msg + m, (size_t)len - m);
        write_all(2, nl, 1);
    }
#endif
    _exit(101);
}

void ksn_rt_entry_error(void) {
    static const char m[] = "Error: main returned an error\n";
    write_all(2, m, sizeof m - 1);
    exit(1);
}

static void stack_overflow_exit(void) {
    static const char m[] = "stack overflow\n";
    write_all(2, m, sizeof m - 1);
    _exit(102);
}

#ifdef _WIN32
static LONG WINAPI veh(EXCEPTION_POINTERS *ep) {
    if (ep->ExceptionRecord->ExceptionCode == EXCEPTION_STACK_OVERFLOW) stack_overflow_exit();
    return EXCEPTION_CONTINUE_SEARCH;
}
void ksn_rt_thread_init(void) {}
void ksn_rt_thread_fini(void) {}
static void signals_init(void) {
    ULONG g = 32768; /* 溢れた後に stderr へ書く余裕 */
    SetThreadStackGuarantee(&g);
    AddVectoredExceptionHandler(1, veh);
}
#else
#define ALT_SIZE (64 * 1024)
/* fault をスタック溢れとみなすアドレスの範囲 [win_lo, win_hi)。win_hi == 0 なら判定しない。 */
static __thread uintptr_t t_win_lo, t_win_hi;
static __thread void *t_alt;

static void on_fault(int sig, siginfo_t *si, void *uc) {
    (void)uc;
    uintptr_t a = (uintptr_t)si->si_addr;
    if (si->si_code > 0 && t_win_hi != 0 && a >= t_win_lo && a < t_win_hi) stack_overflow_exit();
    /* それ以外は既定の動作 (SA_RESETHAND で既定に戻してある)。同期 fault は戻ると同じ命令で再度落ちる。シグナル送信ならここで再送する。 */
    signal(sig, SIG_DFL);
    if (si->si_code <= 0) raise(sig);
}

/* 現在のスレッドのスタックの範囲を求めて、判定範囲を決める。 */
static void compute_window(int is_main) {
    uintptr_t lo = 0, hi = 0;
#if defined(__APPLE__)
    hi = (uintptr_t)pthread_get_stackaddr_np(pthread_self());
    lo = hi - (uintptr_t)pthread_get_stacksize_np(pthread_self());
#elif defined(__linux__)
    if (!is_main) {
        pthread_attr_t at;
        if (pthread_getattr_np(pthread_self(), &at) == 0) {
            void *addr = 0;
            size_t sz = 0;
            if (pthread_attr_getstack(&at, &addr, &sz) == 0) {
                lo = (uintptr_t)addr;
                hi = lo + sz;
            }
            pthread_attr_destroy(&at);
        }
    }
#endif
    if (hi == 0) {
        /* メインスレッド (または範囲が分からない環境): 起動時のスタックの位置と `RLIMIT_STACK` から決める。 */
        struct rlimit rl;
        char here;
        hi = (uintptr_t)&here;
        if (getrlimit(RLIMIT_STACK, &rl) != 0 || rl.rlim_cur == RLIM_INFINITY || (uintptr_t)rl.rlim_cur >= hi) return;
        lo = hi - (uintptr_t)rl.rlim_cur;
    }
    if (is_main) {
        /* 環境変数・引数の分だけ実際の上限は上にずれる。余裕を大きめに取る (これより下はスタックの外の未割り当て領域)。 */
        t_win_lo = lo > ((uintptr_t)1 << 28) ? lo - ((uintptr_t)1 << 28) : 0;
        t_win_hi = lo + ((uintptr_t)4 << 20);
    } else {
        t_win_lo = lo > ((uintptr_t)1 << 20) ? lo - ((uintptr_t)1 << 20) : 0;
        t_win_hi = lo + ((uintptr_t)1 << 16);
    }
}

static void thread_setup(int is_main) {
    stack_t ss;
    /* malloc は使わない (メモリ計測のテストが数える確保に混ぜない)。 */
    void *m = mmap(0, ALT_SIZE, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    t_alt = m == MAP_FAILED ? 0 : m;
    if (t_alt) {
        ss.ss_sp = t_alt;
        ss.ss_size = ALT_SIZE;
        ss.ss_flags = 0;
        if (sigaltstack(&ss, 0) != 0) {
            munmap(t_alt, ALT_SIZE);
            t_alt = 0;
        }
    }
    compute_window(is_main);
}

/* 新しいスレッドの入口で呼ぶ (thrrt.c)。 */
void ksn_rt_thread_init(void) { thread_setup(0); }

void ksn_rt_thread_fini(void) {
    if (t_alt) {
        stack_t ss;
        memset(&ss, 0, sizeof ss);
        ss.ss_flags = SS_DISABLE;
        sigaltstack(&ss, 0);
        munmap(t_alt, ALT_SIZE);
        t_alt = 0;
    }
}

static void signals_init(void) {
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_sigaction = on_fault;
    sa.sa_flags = SA_SIGINFO | SA_ONSTACK | SA_RESETHAND;
    sigemptyset(&sa.sa_mask);
    sigaction(SIGSEGV, &sa, 0);
    sigaction(SIGBUS, &sa, 0);
    thread_setup(1);
}
#endif

int main(int argc, char **argv) {
    ksn_rt_argc = argc;
    ksn_rt_argv = argv;
    signals_init();
    return ksn_main();
}
