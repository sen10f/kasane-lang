# N3: 利用者のデータを添えたコールバック (捕まえたクロージャ → C)

docs/design/設計_ネイティブUI.md の N3。C のコールバック (`void (*cb)(void *user, int x)`) に、外側の変数を
捕まえたクロージャを渡す。C の `user` は捕まえた変数の環境へのポインタになり、処理系が用意した
踏み台 (trampoline) が環境からクロージャを呼び出す。

## 現状

- E5005: `extern` の関数型の引数に渡すクロージャは `capture []` (キャプチャなし) でなければならない。
- E2023: 捕まえたクロージャは `fn(..) -> ..` の値にもできない。
- 名前付きの関数 + `*mut t.Acc` の user データは動く (`tests/data/ffi/many.ksnl` の `run_ticks`)。
- `tests/ffi_userdata.rs` は `#[ignore]`。実装したら外す。

## C 側 (`acc.c`)

```c
typedef void (*acc_fn)(void *user, int x);
void acc_ticks(acc_fn f, void *user, int lo, int hi);  /* lo..hi を 1 ずつ呼ぶ */
void acc_twice(acc_fn f, void *user, int a, int b);    /* a, b をこの順に呼ぶ */
```

`user` は C が受け渡しするだけ。捕まえたクロージャを渡すと、その環境へのポインタが `user` になる。

## Kasane 側と期待される出力 (`main.ksnh` / `main.ksnl`)

`&mut acc` を捕まえたクロージャを 2 回 (1..=10 と 5, 7) 渡し、累積を確かめる。

```text
total=55 count=10   ← acc_ticks の 1..=10
total=67 count=12   ← 続けて acc_twice の 5, 7
```

標準出力は `userdata ok` の 1 行 (検査は `core.check.eq`)。`.ksnh` (人向け・コメント付き) が正で、
`ksn strip` が `.ksnl` (LLM 向け) を作る (食い違いは E9050)。

## 書き方の候補 (未決。C 側はどれを選んでも変わらない)

いま試験の入力に書いてあるのは A。

- **A (環境が user データ)**: `extern` に関数の型 `f: fn(*mut (), i32) -> ()` と `user: *mut ()` を
  書き、捕まえたクロージャを `f` に渡すと呼び出し側は `user` を書かない (クロージャの環境で埋まる)。
  踏み台はクロージャ式ごとに処理系が生成する (`fn(user: *mut (), x: i32) -> ()` で環境から捕まえた
  名前を復元し、クロージャの本体を呼ぶ)。環境の寿命は C の呼び出しの間。C が持ち帰る約束のものは
  逃がす (ヒープ) 必要がある。
- **B (両方を書く)**: `extern` は今のまま (f と user の両方)。捕まえたクロージャの値を
  `let cb = fn(..) capture [..] { .. }` で受けておき、`user` にはそのアドレスを渡す
  (`core.ptr.from_mut(&mut cb)`)。C の `user` はクロージャの値へのポインタで、踏み台がコードと環境を
  取り出す。呼び出し側が両方を書くので、どんな隠れた約束もない。
- **C (型に捕まえた場所を書く)**: `extern` の関数型の種類を増やし、捕まえた環境を型で表す
  (`f: closure(i32) -> () capture [&mut t.Acc] uses [] panics never` のような形)。E5004 / E5005 の
  検査も capture の有無を構文で確かめられる。

## 採用時の作業

1. `main.ksnl` を選んだ書き方に直し、`ksn strip` / `ksn fmt` で `.ksnh` とそろえる。
2. `tests/ffi_userdata.rs` の `#[ignore = ..]` を外す。
3. 診断を新設したら `ksn explain` と `stage0/tests/data/diag/` にのせる (E5005 を緩めるだけなら不要)。