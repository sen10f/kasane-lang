/* Cranelift バックエンドのスレッドランタイム (std.thread / std.sync)。
 * clif.rs が `include_str!` でこのファイルを埋め込み、ネイティブ実行ファイルが `ksn_rt_thr_` の関数を
 * 参照するときだけ、リンク時に C コンパイラで `-pthread` 付きでコンパイルする (strrt.c / sysrt.c と同じ仕組み)。
 *
 * - スレッド: POSIX スレッド。`spawn` が確保した環境ブロック (結果の領域 + 取り込んだ値) を所有権ごと渡す。
 *   スレッドの本体 (clif.rs が生成する `void entry(void *env)`) は結果を環境ブロックに書く。
 *   `join` は終了を待って環境ブロックを返し、呼び出し側が結果を読んで解放する。
 * - Mutex: 先頭 8 バイトのロック語 (0 = 解放、1 = 取得中) だけで動く自前のスピン + 譲りロック。
 *   ゼロ初期化のまま (`static` のデータ領域) でも、値としてコピーして動かしても (未取得なら) 壊れない。
 * - チャネル: 有界のリング (容量 0 は 1 として扱う) + 条件変数。送信側・受信側は `clone` で増える (端ごとに数える)。
 *   送信側がすべて落ちた = 閉じた、受信側がすべて落ちた = 閉じた。複数の受信側は 1 つのキューから値を取り合う (各値は 1 つだけに渡る)。
 *   `recv` は、キューが空で送信側がいなければ Err (戻り値 1)、空で送信側が生きていれば待つ。
 *   `send` は、受信側がいなければ Err (戻り値 1。値は消費しない)、満杯なら空くまで待つ。
 *   チャネル本体は、すべての端 (送信側 + 受信側) が落ちたときに解放する。
 * Windows は未対応 (clif.rs がビルド時に理由付きでスキップする)。 */
#ifdef _WIN32
#error "thrrt.c is not supported on Windows"
#endif
#include <pthread.h>
#include <sched.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

void ksn_rt_thread_init(void);
void ksn_rt_thread_fini(void);

typedef struct {
    pthread_t t;
    void (*entry)(void *);
    void *env;
} ksn_thr;

static void *thr_main(void *p) {
    ksn_thr *h = (ksn_thr *)p;
    ksn_rt_thread_init(); /* 代替シグナルスタックとスタック溢れの判定 (rt.c) */
    h->entry(h->env);
    ksn_rt_thread_fini();
    return 0;
}

/* スレッドを起こす。失敗なら 0。 */
void *ksn_rt_thr_spawn(void (*entry)(void *), void *env) {
    ksn_thr *h = (ksn_thr *)malloc(sizeof(ksn_thr));
    if (!h) return 0;
    h->entry = entry;
    h->env = env;
    if (pthread_create(&h->t, 0, thr_main, h) != 0) {
        free(h);
        return 0;
    }
    return h;
}

/* 終了を待って環境ブロックを返す (ハンドルは解放する)。 */
void *ksn_rt_thr_join(void *hp) {
    ksn_thr *h = (ksn_thr *)hp;
    void *env = h->env;
    pthread_join(h->t, 0);
    free(h);
    return env;
}

/* ---- Mutex ---- */
void ksn_rt_thr_lock(void *m) {
    uint64_t *w = (uint64_t *)m;
    unsigned spins = 0;
    while (__atomic_exchange_n(w, 1, __ATOMIC_ACQUIRE) != 0) {
        /* 取得中: 手放して待つ (長く続くなら眠って CPU を空ける) */
        while (__atomic_load_n(w, __ATOMIC_RELAXED) != 0) {
            spins++;
            if (spins < 64) {
                continue;
            }
            if (spins < 2048) {
                sched_yield();
            } else {
                struct timespec ts = {0, 50000};
                nanosleep(&ts, 0);
            }
        }
    }
}

void ksn_rt_thr_unlock(void *m) { __atomic_store_n((uint64_t *)m, 0, __ATOMIC_RELEASE); }

/* ---- チャネル ---- */
typedef struct {
    pthread_mutex_t mu;
    pthread_cond_t not_empty, not_full;
    unsigned char *buf;
    uint64_t esz, cap, head, len;
    int64_t senders, receivers, ends;
} ksn_chan;

/* 確保に失敗したら 0。 */
void *ksn_rt_thr_chan_new(uint64_t esz, uint64_t cap) {
    ksn_chan *c = (ksn_chan *)malloc(sizeof(ksn_chan));
    if (!c) return 0;
    memset(c, 0, sizeof(ksn_chan));
    if (cap == 0) cap = 1;
    if (esz == 0 || cap > (UINT64_MAX / 2) / esz) {
        free(c);
        return 0;
    }
    c->buf = (unsigned char *)malloc(esz * cap);
    if (!c->buf) {
        free(c);
        return 0;
    }
    c->esz = esz;
    c->cap = cap;
    c->senders = 1;
    c->receivers = 1;
    c->ends = 2;
    pthread_mutex_init(&c->mu, 0);
    pthread_cond_init(&c->not_empty, 0);
    pthread_cond_init(&c->not_full, 0);
    return c;
}

static void chan_free(ksn_chan *c) {
    pthread_mutex_destroy(&c->mu);
    pthread_cond_destroy(&c->not_empty);
    pthread_cond_destroy(&c->not_full);
    free(c->buf);
    free(c);
}

/* 値を送る。0 = 送った (値の所有はチャネルへ移る)、1 = 受信側が落とされていて送れなかった (値は呼び出し側のまま)。 */
int64_t ksn_rt_thr_send(void *cp, const void *item) {
    ksn_chan *c = (ksn_chan *)cp;
    pthread_mutex_lock(&c->mu);
    while (c->len == c->cap && c->receivers > 0) {
        pthread_cond_wait(&c->not_full, &c->mu);
    }
    if (c->receivers == 0) {
        pthread_mutex_unlock(&c->mu);
        return 1;
    }
    memcpy(c->buf + ((c->head + c->len) % c->cap) * c->esz, item, c->esz);
    c->len++;
    pthread_cond_signal(&c->not_empty);
    pthread_mutex_unlock(&c->mu);
    return 0;
}

/* 値を受け取る。0 = 受け取った (所有は呼び出し側へ)、1 = 空で送信側がいない。 */
int64_t ksn_rt_thr_recv(void *cp, void *out) {
    ksn_chan *c = (ksn_chan *)cp;
    pthread_mutex_lock(&c->mu);
    while (c->len == 0 && c->senders > 0) {
        pthread_cond_wait(&c->not_empty, &c->mu);
    }
    if (c->len == 0) {
        pthread_mutex_unlock(&c->mu);
        return 1;
    }
    memcpy(out, c->buf + c->head * c->esz, c->esz);
    c->head = (c->head + 1) % c->cap;
    c->len--;
    pthread_cond_signal(&c->not_full);
    pthread_mutex_unlock(&c->mu);
    return 0;
}

/* 待たずに 1 つ取り出す (受信側が溜まった値を解放するのに使う)。1 = 取り出した、0 = 空。 */
int64_t ksn_rt_thr_try_pop(void *cp, void *out) {
    ksn_chan *c = (ksn_chan *)cp;
    pthread_mutex_lock(&c->mu);
    if (c->len == 0) {
        pthread_mutex_unlock(&c->mu);
        return 0;
    }
    memcpy(out, c->buf + c->head * c->esz, c->esz);
    c->head = (c->head + 1) % c->cap;
    c->len--;
    pthread_cond_signal(&c->not_full);
    pthread_mutex_unlock(&c->mu);
    return 1;
}

/* 送信側を増やす / 受信側を増やす (同じチャネルへの端がもう 1 つ)。 */
void ksn_rt_thr_sender_clone(void *cp) {
    ksn_chan *c = (ksn_chan *)cp;
    pthread_mutex_lock(&c->mu);
    c->senders++;
    c->ends++;
    pthread_mutex_unlock(&c->mu);
}

void ksn_rt_thr_receiver_clone(void *cp) {
    ksn_chan *c = (ksn_chan *)cp;
    pthread_mutex_lock(&c->mu);
    c->receivers++;
    c->ends++;
    pthread_mutex_unlock(&c->mu);
}

/* 端 1 つぶんの参照を手放す。すべての端が落ちたらチャネルを解放する。 */
static void chan_end_release(ksn_chan *c) {
    pthread_mutex_lock(&c->mu);
    c->ends--;
    int dead = c->ends == 0;
    pthread_mutex_unlock(&c->mu);
    if (dead) chan_free(c);
}

/* 送信側を落とす: 最後の送信側なら待っている受信側を起こす (キューが空なら Err になる)。 */
void ksn_rt_thr_sender_drop(void *cp) {
    ksn_chan *c = (ksn_chan *)cp;
    pthread_mutex_lock(&c->mu);
    c->senders--;
    if (c->senders == 0) pthread_cond_broadcast(&c->not_empty);
    pthread_mutex_unlock(&c->mu);
    chan_end_release(c);
}

/* 受信側が数から外れる (まだチャネルは解放しない)。最後の受信側なら 1 を返し、待っている送信側を起こす
 * (以後の `send` は Err)。最後の受信側は、続けて `try_pop` で残りを取り出して破棄し、`receiver_release` を呼ぶ。 */
int64_t ksn_rt_thr_receiver_leave(void *cp) {
    ksn_chan *c = (ksn_chan *)cp;
    pthread_mutex_lock(&c->mu);
    c->receivers--;
    int last = c->receivers == 0;
    if (last) pthread_cond_broadcast(&c->not_full);
    pthread_mutex_unlock(&c->mu);
    return last;
}

void ksn_rt_thr_receiver_release(void *cp) { chan_end_release((ksn_chan *)cp); }

/* `Receiver.close`: 0 = 閉じた (所有は手放した)、1 = 最後の受信側でキューに値が残っているので閉じない (受信側は呼び出し側のまま)。
 * 他に受信側がいれば自分の分を畳むだけで 0。空の確認と閉じるのは 1 つのロックの下で行う。 */
int64_t ksn_rt_thr_receiver_close(void *cp) {
    ksn_chan *c = (ksn_chan *)cp;
    pthread_mutex_lock(&c->mu);
    if (c->receivers == 1 && c->len != 0) {
        pthread_mutex_unlock(&c->mu);
        return 1;
    }
    c->receivers--;
    if (c->receivers == 0) pthread_cond_broadcast(&c->not_full);
    pthread_mutex_unlock(&c->mu);
    chan_end_release(c);
    return 0;
}
