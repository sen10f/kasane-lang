### 2.10?? 構文の誤りへの専用の診達と `fmt`/`fix` の空白行でのパニック対策

- `src/lint.rs` の本体の字下げの検査が空白だけの行で `Option::unwrap()` のパニックを起こし、`ksn fmt` / `ksn fmt --check` / `ksn fix` が診断を出さずに終了コード 2 で終わる。`if l.is_empty() { continue }` を `if l.trim().is_empty() { continue }` にして空白だけの行を飛ばし、構文の誤り (E0042 など) を普通に報告する。
- 一般の E0002 に落ちている LLM の頻出の形に専用の文面を出す: `type X = ..` (型の別名はなく、全型を書く)、`end test X` (`end <FQN>` で閉じる)、タプルの欄 `p.0` (組は `let (a, b) = p` や `match` で分解)、予約語 (`end` / `link` / `ref`) を名前に使った (予約語)。新しいコードは `stage0/tests/data/diag/E****.ksnl` (+ `.expect`) と `explain.rs` のエントリを追加する。
- `.ksnl` の最上位のコメントについて、`ksn fmt` が `ksn check` の E0059 と同じ文面 ("コメントは .ksnl に置けない。.ksnh に書く") を出すようにする (今は "blank line after comment")。
- 字レベルの診断にソースの行を付ける。
- 試験: `stage0/tests/check_diag.rs` / `fmt_fix.rs` に追加。ゲート: `cargo test --release --test check_diag --test fmt_fix --test tooling`、`cargo check --release --tests`、`python3 -I spec/tools/check_all.py`。
