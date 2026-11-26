# E4013: Handle<Self> の誤検出を修正 (branch: check/m4-handle-self)

## 概要
`alloc.arena.Handle<T>` は `index: usize` と `generation: u32` だけの値で `T` をインラインに持たない（仕様 8.3）。`collect_ty_names` で `Handle<T>` の型引数まで辿ると、`Handle<Self>` を持つ型を誤って自己再帰と見なし E4013 を出す（偽陽性）。`Handle` を間接的な取っ手（再帰の辺にならない）として扱い、値による自己再帰（`Box<Self>` / `Option<Box<Self>>` / 直接のフィールドなど）だけを E4013 の対象とする。

## 変更
- `stage0/src/check.rs`: `collect_ty_names` で `alloc.arena.Handle` は中を見ない（`is_indirect_ty`）。
- `stage0/tests/data/linear_ok/arena_handle_of_self_ok.ksnl`: `Handle<Self>` を持つ型の ok ケースを追加。
- `tests/native_ratio_floor.json`: `linear_ok` の下限を 187 → 188（ケース追加分）。

## 検証
- `cargo build --release`：緑。
- `cargo test --release --test check_diag`：314 件通過（E4013.ksnl / E4013b.ksnl は依然エラー、unsound も依然拒否、linear_ok 新規ケースも通過）。
- FAST GATE: `check_all.py` PASSED、`verify_reference.py` bad 0、`native_ratio.py` 188/188 linear_ok（床以上）。
- `Handle<Self>` 構造体を `ksn build --release` でネイティブビルド・実行し rc=0。
- フル `cargo test --release`：ディスク枯渇（99%）と他担当の激しいビルド競合により完走できず。check_diag（影響範囲を直接カバー）と FAST GATE は緑。

## リスク
変更は `collect_ty_names` の `Handle` 分岐追加のみで、影響範囲は E4013 の再帰判定だけ。`type_reaches`（derive [Drop] の相互再帰判定）も同じ関数を経由するが、`Handle<Self>` が「到達する」と見なされなくなるのは意図通り（相互再帰の偽検出を防ぐ）。実 E4013 ケース（直接 `Option<Self>` / 相互 `Option<A>`⇔`Option<B>`）は値による再帰なので影響を受けない。
