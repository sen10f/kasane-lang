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

実際に残る退避理由は、M3 の最初に `engine_compare.py` を回して理由ごとの件数 (この表の 13〜16 以外にあるもの) を `実装計画.d/vm-unify-plan.md` に書き足す。ここまでの表は読み取りで作ったもので、件数は測っていない。

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

## 6. 複数スレッドの実行器と非同期の入出力を VM に (線 5、2026-11-26)

対象: `std.task.block_on_threads` + `spawn` と、`std.net` の `accept_async` / `read_async` / `write_async` (`std.task.wait_io`)。これまで VM は退避していた (実行器の核 `ksn.mt.*` が KIR では空の殻で、`ksn.io.*` はリアクターがなく、止まった)。

### 6.1 選択肢

- 案 R (実スレッド): ワーカーごとに別の `Vm` を持ち、アリーナを共有して走らせる。`kir_llvm.rs` の `thread_override` が持つ約 40 個の補助 (mutex の futex・チャネルごとの cond・全員待ちの数え上げ・rwlock・原子操作) を Rust で書き直す必要があり、TSan で確かめる範囲も広い。アリーナの確保・`dead` 印の検査・`static` の領域など VM の共有状態に同期を足すことになる。
- 案 C (協調、1 本のスレッド): VM は今の 1 本のスレッドのまま、`sysrt.c` の `ksn_rt_sys_mt_*` と `ksn_rt_sys_io_*` を Rust に移植して VM の組み込み (`Ins::Rt`) にする。ワーカーは呼び出しスレッド 1 本だけ (補助のワーカーは起こさない)。実行器の KIR (`kir_suspend_exec*.kir`、LLVM と共有) はそのまま走る。

選ぶのは案 C。理由: (1) 取り消し (次の中断点で止まる)・panic の伝わり方・入れ子の拒否・`Send` の要求・セッション番号は実行器の KIR と核の状態機械 (注入列・state 0/1/2/3・cancel の印) が決めており、案 C はそれを同じ規則で動かす。(2) 順序が決定的 (注入列の先頭から)。(3) 同期が要らない。

### 6.2 意味

- `ksn.mt.begin(n)`: `n > 1` なら `on` を立てて 1 を返す (呼び出し側の「補助ワーカーを足す個数」は 0)。核の全員待ちの数は 1。`n <= 1` は `block_on` と同じ (on しない)。
- `ksn.mt.sleep(seen)`: 世代が進んでいなければ「二度と起きない」ので 3 (全員待ち = deadlock、実行器の KIR が `deadlock: every std.task task is waiting ...` で panic、終了 101)。止めの旗なら 2。
- `ksn.task.session`: `on` の間は核のセッション番号 (根もタスクも同じ)。それ以外は KIR の本体と同じ。
- `ksn.io.*`: `sysrt.c` と同じ状態機械 (park → epoll 端起こし ONESHOT ではなく EPOLLET で読み書きを両方載せる・`rdy` で端の取りこぼしを防ぐ・期限は `set_*_deadline` と `set_*_timeout`)。`poll(block)` は block != 0 のとき最も近い期限まで `epoll_wait` で待つ (他のワーカーがいないので、待っても他の仕事を止めない)。VM のソケットは仮想の記述子なので、`socks` から実際の fd を引いて登録する。`close` は `forget` で記録を消す。
- `listen` の待ち行列は 1024 (ネイティブと同じ。標準の 128 だと 256 本の同時接続で `connect` が詰まる)。

### 6.3 既知の差 (ネイティブとの)

- 本物の並列はしない。ワーカーが 2 本以上いることに頼る書き方 (あるタスクが同期のチャネルの `recv` / `Mutex` で別のワーカーのタスクを待つ) は、VM では全員待ちの deadlock (101) になる。`async` の `join_task` / `spawn` / `cancel` は問題ない。
- 順序は注入列の順で決まる (ネイティブは非決定)。比べる試験は順序に依存しない出力だけを比べる。

### 6.4 足りない所: 本物のワーカースレッドが要る形 (2026-11-26 の確認)

次の形はネイティブでは動くが、VM では動かない (ワーカーが 1 本で、`std.thread.sleep` や同期の待ちは協調の切り替え点ではないため)。
- あるタスクが `std.thread.sleep` / ポーリングで待つ間に、別のタスクが進まねばならない (`tests/data/vm_mt_blocking_sleep.ksnl`: VM は止まる、ネイティブは 8 を出して終わる)。
- あるタスクが同期の `std.sync` の `recv` / `Mutex` で、別のワーカーのタスクが送る・離すのを待つ (VM は全員待ちの 101)。この形はチャネルの端を取り込む閉包を VM に下ろせず、木走査へ退避するので、木走査も同じ 101。

木走査 (タスクごとの OS スレッドではなく 1 本のスレッドの協調実行) も同じ形で止まるので、VM の既定化による退行ではなく、退避を戻しても直らない (退避先も同じ止まり方をする)。ゆえに `block_on_threads` の退避は戻さない。直すには、VM のワーカーを別の `Vm` (アリーナを共有) の OS スレッドにし、`thread_override` の補助 (mutex・チャネルの待ち・全員待ちの数え上げ・原子操作) を Rust に書き直す必要がある (6.1 の案 R)。この形を使うプログラムは `ksn build` のネイティブで走らせる。試験 `vm_task_mt::blocking_sleep_vm_matches_native` (`#[ignore]`) が、直ったときの基準。
