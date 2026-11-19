#include <stdlib.h>

/* N3: 利用者のデータを添えたコールバック (設計_ネイティブUI.md)。`user` は C が受け渡しするだけ。
   捕まえたクロージャを C に渡すと、その環境へのポインタが `user` になり、処理系が用意した踏み台が
   環境からクロージャを呼び出す。*/
typedef void (*acc_fn)(void *user, int x);

/* lo から hi まで 1 ずつ呼ぶ。*/
void acc_ticks(acc_fn f, void *user, int lo, int hi) {
    for (int i = lo; i <= hi; i++) {
        f(user, i);
    }
}

/* a と b をこの順に呼ぶ。*/
void acc_twice(acc_fn f, void *user, int a, int b) {
    f(user, a);
    f(user, b);
}