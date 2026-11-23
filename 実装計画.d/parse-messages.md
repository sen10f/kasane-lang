### 2.10?? 構文の誤りへの専用の診断と `fmt`/`fix` の空白行でのパニック対策

- `src/lint.rs` の本体の字下げの検査が空白だけの行で `Option::unwrap()` のパニックを起こし、`ksn fmt` / `ksn fmt --check` / `ksn fix` が診断を出さずに終了コード 2 で終わる。`if l.is_empty() { continue }` を `if l.trim().is_empty() { continue }` にして空白だけの行を飛ばし、構文の誤り (E0042 など) を普通に報告する。(完了: 7d86e1b1)
- 一般の E0002 に落ちている LLM の頻出の形に専用の文面を出す: `type X = ..` (E0063: 型の別名はなく、全型を書く)、`end test X` (E0064: `end <FQN>` で閉じる)、タプルの欄 `p.0` (E0065: 組は `let (a, b) = p` や `match` で分解)、予約語を名前に使った (E0066: 予約語)。`src/rustisms.rs` に検知を追加し、`stage0/tests/data/diag/E0063-66.ksnl` と `src/explain.rs` のエントリを追加。(完了)
- `.ksnl` の最上位のコメントについては、`ksn check` が既に E0059 ("コメントは .ksnl に置けない。.ksnh に書く") を出すため専用メッセージは済み。(済み)
- 字レベルの診断にソースの行と `^` を付ける (`src/cli.rs` の `flush`、col > 0 の診断)。JSON 出力は構造化フィールドのまま変更なし。(完了)
- 試験: `stage0/tests/check_diag.rs` (`diagnostics_each_case_reports_its_code` で E0063-66 をカバー) / `fmt_fix.rs` / `tooling`。
- ゲート: `cargo test --release --test check_diag --test fmt_fix --test tooling` (308 + 26 pass)、`cargo check --release --tests` (エラーなし)、`python3 -I spec/tools/check_all.py` (ALL CHECKS PASSED)。
