# 設計: 木走査を捨てて VM に一本化する

決定 (2026-11-26、利用者): 実行系は VM と LLVM の 2 つだけにする。木走査 (`interp.rs` / `natives.rs` / `value.rs`) とインタプリタ同梱のバンドルは最後に消す。この章は計画で、今回の作業では何も消さない。`docs/engine-matrix.md` (ブランチ `docs/engine-matrix`) の表と opus-stab の報告 (`~/kasane-verify/agents/opus-stab/report.md`) を前提にする。

## 1. 木走査が今も要る箇所の一覧と方針

方針の記号: 移す = VM に実装、下ろす = KIR/下ろしの段を直す、捨てる = 機能ごと廃止。

| # | 箇所 | 今の依存 | 方針 | 順 | 等価性を守る試験 |
|---|---|---|---|---|---|
| 1 | デバッガ `ksn debug` / `ksn dap` (`debug.rs`、`cli.rs` `load_for_debug`) | 木走査の評価ループに差し込み。手書き Drop は E9031 (`cli.rs:1738`) | 移す。VM の命令ごとに行表 (関数・ファイル・行) を引く。ブレーク・ステップ・局所変数・呼び出しスタックを VM のフレームから出す。Drop は VM が走らせるので E9031 を外す | M1 | 既存のデバッガ試験 (tooling.rs の debug/dap) を `--engine=vm` でも同じ期待で回す。Drop 付きパッケージの新規試験 |
| 2 | ホットリロード `--watch` (`vm/reload.rs`) | VM 側は実装済み。`--engine=vm` の明示が要る箇所と、木走査側の `spawn_watcher` | 移す (VM 既定で通す) | M2 | `tests/` のホットリロードの実動作テスト (`run_tests.py` の段) を既定の engine で回す |
| 3 | `--engine=tree` / `KSN_ENGINE=tree` / `--engine=both` | `cli.rs` の選択 | M-last まで残す。その後 `tree` は廃止を告げて終了 2。`both` は VM と LLVM の比較に読み替える | M-last | `engine_compare.py` を VM 対 LLVM にする |
| 4 | VM の退避 (`cli.rs:1964`、`cli.rs:2232` の `vm engine skipped ... falling back to the tree walker`) | VM に下ろせない形の退避 | 理由ごとに塞ぐ (下の 2 章)。塞ぎ終えたら退避の枝を消し、下ろせない形は診断で止める | M3〜M5 | `engine_compare.py` の skip 0 件を回帰基準にする |
| 5 | `ksn test --both` のバンドル退避 | 下ろせない形は木走査同梱のバンドルで走らせる | 下ろす (理由の解消後は退避なし)。その後 `--both` は VM と LLVM | M5 | `kir_native.rs` の `--both` 系、`tooling.rs` |
| 6 | `ksn build` の下ろし失敗時のインタプリタ同梱バンドル (`run_in_bundle` 近辺、デバッグのみ) | LLVM に下ろせない形の退避 | 当面は VM を同梱するバンドルに差し替える (木走査は載せない)。LLVM へ下ろせない形を塞ぎ切ったら退避ごと捨て、E9040 で止める | M6 | `bundle_copy.rs`、`build_reasons.rs`、`cache_incremental.rs` |
| 7 | `interp_parity` 系の試験 (interp == native) | 木走査が基準 | 基準を VM に替える (VM == native)。`run_interp.rs` は `run_vm.rs` 相当に改名 | M3 から段階的 | 置き換え前後で件数が減らないこと (削除行を見る) |
| 8 | `KSN_TEST_SPAWN_FAIL` (`natives.rs:818`、`threads.rs:1189`) | 木走査だけ。VM・ネイティブは失敗で panic | 仕様を VM に合わせる (spawn の失敗は `Err(SpawnError<F>)` で閉包を返す、ピン留め済みの決定 (10))。VM とネイティブにも旗を実装し、同じ試験を `--engine=vm` で回す | M4 | `threads.rs` の spawn 失敗の試験 (VM 版) |
| 9 | `std.task.block_on_threads` | 木走査は ok、VM は要確認、KIR は `kir_lower_task.rs` で実装済み | VM を確かめて足りなければ移す。取り消しは次の中断点で止まる (仕様直し済み) | M4 | `task` 系の試験を VM でも (`threads.rs`、`async` 系) |
| 10 | `std.tls` (`net.rs`) | 木走査は実 fd。VM・KIR は要確認 | 確かめて、足りなければ VM の native 表に載せる。wasm は未対応のまま | M4 | `net` 系の試験 |
| 11 | 構造体を値で受け渡す extern (`vm/compile.rs:625` ほか) | VM 未対応、`ffi_interp.rs` が木走査側 | 移す。VM に構造体の値渡し (ABI 分類を LLVM と共有) を足す。難しければ ffi 経路だけ LLVM へ回す (診断で案内) | M5 | `ffi_native.rs`、`ffi_userdata.rs` を VM でも |
| 12 | extern 関数のアドレス (`fnaddr`、`compile.rs:638` `857`) | VM 未対応 | 移す (トランポリン経由) | M5 | 同上 |
| 13 | クロージャ (KIR に閉包値がない) | 下ろしの段で環境と呼び先に展開。展開できない形は退避 | 下ろす: 展開できない形 (取り込みの間の保存・返す閉包・欄に持つ閉包) を理由ごとに数え、`kir_lower` に閉包値 (環境 + 関数ポインタ) を足して VM・LLVM で共有する。E2074 など検査の規則は変えない | M3 | `closure` 系の試験と `engine_compare.py` の退避理由の件数 |
| 14 | 大きな配列・Option の配列 | スロット数の上限 (`MAX_SLOTS`、`compile.rs:335`)・領域の余白 | 下ろす: 大きな値はスロットに載せず領域 (arena) に置く。`ulimit -v` で縮む件は既知の制限のまま | M3 | `arena` 系、`kir_*` の大配列の試験 |
| 15 | `std.process` の Windows (`vm/run.rs:1514`) | VM 未対応 | Windows の VM は対象外とし、診断で止める | M6 | `windows_target.rs` |
| 16 | `mem.load` / `store` の型が読めない形 (`compile.rs:676` `756` `764` `781` `802`) | 一部の型で退避 | 型の種類を足して塞ぐ | M4 | `engine_compare.py` |
| 17 | デバッガの Drop 拒否・`ksn debug` 同梱 UI | 木走査前提 | 1 に含める | M1 | — |

実際に残る退避理由は、M3 の最初に `engine_compare.py` を回して理由ごとの件数 (この表の 13〜16 以外にあるもの) を `docs/plan/実装計画.d/vm-unify-plan.md` に書き足す。ここまでの表は読み取りで作ったもので、件数は測っていない。

## 2. 順序

- M0 本書。
- M1 デバッガ (`ksn debug` / DAP) を VM に。木走査版は `--engine=tree` の裏に残す。
- M2 `--watch` を VM 既定で通す。
- M3 クロージャ・大きな値の下ろし (退避理由の最大のもの)。
- M4 spawn 失敗・`block_on_threads`・`std.tls`・`mem.load` 型。
- M5 extern の値渡し・アドレス、`ksn test --both` の退避なし。
- M6 ビルドのバンドルを VM 同梱にし、さらに退避ごと捨てる。Windows の扱い。
- M-last 木走査 (`interp.rs`・`natives.rs`・`value.rs` の木走査固有部・`ffi_interp.rs`・デバッガの木走査版)・`--engine=tree`・バンドルを削除。`engine_compare.py` を VM 対 LLVM に。
各段は「VM が受け持つ範囲を広げ、木走査との出力が一致」を条件に進め、木走査側は M-last まで消さない。

## 3. 仕様に反映する変更

- 仕様 8 章: 「インタプリタ (木走査) == ネイティブ」の同一性を「VM == ネイティブ」に替える。
- 仕様 3.2 (`ksn test` の `?`): VM・ネイティブで同一。
- `ksn run`/`test` の `--engine`: 値は `vm` / `llvm` / `both` だけにする。`tree` は廃止を告げる (M-last)。
- spawn の失敗 (決定 (10)) を VM・ネイティブでも `Err(SpawnError<F>)` にそろえる。現在の「panic」は仕様の誤差で、M4 で直す。
- 手書き Drop: 実行系は VM と LLVM のどちらでも走る。E9031 は M1 で廃止する (`explain` と `tests/data/diag/E9031.*` も整理)。
- `docs/engine-matrix.md` から木・バンドル・デバッガの列を消す (M-last)。

## 4. 危険と対策

- VM と木走査で食い違う挙動 (整数のオーバーフロー、深さ 102、出力のフラッシュ): `engine_compare.py` の不一致 0 を各段の条件にする。
- テストの入力・期待値は書き換えない。基準を替える試験は、新しい名前で足してから古い名前を M-last に消す。
- 木走査を消す前に `examples/` と `bench/tasks/*/reference.ksnl` が VM で 0 退避であること。

## 5. M1 のデバッガの制限 (M-later)

VM 版デバッガ (M1) は次を後回しにしている。木走査版を消す (M-last) 前に埋める。
- 式の評価でメソッド呼び出し (`x.len()` など) が使えない。純粋な関数の呼び出し (`t.add(1i32, 2i32)`) は使える。
- `Vec` / `String` / `HashMap` の中身は `<vec>` と出す (ヘッダの配置を読んで要素を出す表示が要る)。
- `unsafe` ブロックを含むパッケージは VM でデバッグできない (`vm::has_unsafe_block` の退避と同じ理由。生ポインタの検査を VM に移すとき一緒に解く)。
- スレッドは main だけ (`threads` は 1 本)。複数スレッドの実行器・`std.thread.spawn` の停止は未対応。
- DAP の `column` は字下げ幅 + 1 の近似 (1 行 1 文の正規形に依存)。

## 6. 線 3: 閉包値と関数値 (`vm/lane-closure`)

測った退避の理由 (2026-11-26、`engine_compare.py`): 関数値が総称の型引数 (`t.id(t.eff)`)・戻り値 (`t.mk`)・閉包の引数 / 戻り値・変数への代入 (`g = t.say`) に出る 5 本、取り込みを持つ閉包の保存 (`let c = fn .. capture [move g]`) 1 本、`Box::into_dyn` 2 本。
表現: 値は 2 種類。
- 関数値 `fn(A) -> R` (取り込みなし) = `ptr` (`Op::FnAddr`、`CallIndirect`)。インダイレクトコールの対象は欄だけだったので、`sig_ty` / 戻り値 / 総称の単相化 / 可変の局所 / Option・タプルの要素にも `ptr` として通す。取り込みなしの閉包リテラルが `fn(..)` 型の位置に来たら、本体を合成関数に下ろして `FnAddr` にする。
- 取り込みありの閉包 (型は式ごとの固有型) = 環境ブロック `{code: ptr, 取り込み...}` を指す `ptr`。呼び出しは `CallIndirect(code, [env, 引数...])`、本体は環境を第 1 引数に取る合成関数。環境は定義した関数の枠 (alloca) に置き、検査器の取り込みの意味 (move / 参照の借用期間) は変えない。move で取り込んだ線形の値は環境ブロックが持ち主になり、閉包を使い切る (`core.mem.drop(c)`・呼び出し) 所で手書き Drop を含めて取り込みを解放する (規則は `docs/known-limitations.md` と `docs/plan/実装計画.d/user-drop.md`)。
- `Box::into_dyn`: 既存の `dyn` の表 (kir_lower_dyn) の単相化で `into_dyn` を `Box<dyn T>` の作成として扱う。
段階: (1) 関数値を `ptr` として全位置に通す (2) 環境ブロックの閉包 (3) into_dyn。各段で VM・LLVM・木走査の出力を比べる。

### 6.1 段階 2 の残り (2026-11-26 時点)
未対応: 取り込みを持つ閉包を変数に持ち、呼ばずに `core.mem.drop(c)` する形 (`lock_guard_closure_called_before_suspend`)。参照の変数 (ロックのガード) の `move` の取り込みを素通しにすると、ガードが `drop(c)` で外れずにデッドロックする (試して確かめた)。直すには、閉包の値に「取り込み済みの所有物の印 (旗)」を持たせ、呼び出し (本体が消費) と `drop(c)` / スコープの終わりで、旗の立っているものだけを破棄する。`once_flags` (FnOnce + 手書き Drop) と `guard_slots` の仕組みを閉包の種類に統合する作業になる。対応前は従来どおり木走査へ退避する (診断の文面も変えていない)。
