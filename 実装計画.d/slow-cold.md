# slow-cold: P-04 `.slow` の複製を cold / minsize / optsize / noinline にする

## 変更 (`stage0/src/kir_llvm.rs`)
- 純粋域の `.slow` 複製 (`fe.slow = true` で出す `define {f}.slow`) の属性に `cold minsize optsize noinline` を足す (関数定義の `define` 行)。
- `.w` 入口の `br i1 %fits, label %fast, label %slow` に分岐重みを付け、速い側 (`fast`) に寄せる (`[ [1000, 1] ]` 的な重み)。
- 意味は変えない (上限付近での速度だけが下がる)。

## 根拠
`agents/opus-perf/report.md` P-04: `.slow` は release IR の 42%。clang -O3 -c の時間: 今のまま 5.3 s → cold minsize optsize 4.2 s → +noinline 3.6 s (http の例 5.2 → 4.2 / 3.6 s)。

## 検証
- `depth_limit` / `depth_explicit` / `kir_inline_depth` で終了コード 102 が同じ深さで出る。
- `tests/native_vs_interp.py`: `ksn run` と release ネイティブの出力一致。
- `ksn build --release` 時間と xlang-bench/src + examples の実行時間を前後で計測 (最小 3 回、`uptime` 記録)。
- `cargo test --release` 全件、FAST GATE (check_all / verify_reference / native_ratio / par_test fast)。
