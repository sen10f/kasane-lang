# m3-env-fix-cache

E1: KSN_BORROWCK / --borrowck の全モードを上位集合に (AST の診断を捨てない)。T1: fix の stack の数を桁区切りごと外す。T2: 退避キャッシュを mkdtemp で作り所有者・モードを確かめる。

他の env: KSN_FIX_ONLY (fix のみ)、KSN_CHECKER (同じ検査器)、KSN_STDLIB、KSN_CHECK_CACHE、KSN_S5_*_OLD、KSN_EXP は要確認 (検査を弱めうるものは未発見)。

## 環境変数の監査 (release、2026-11-25)

`unsound/` から 60 件を無作為に選び、各変数・各値で `ksn check` を回した (道具: `~/coord_tmp/m3audit/audit.py`)。基準は 60 件すべて拒否。どの値でも拒否は消えなかった。

| 変数 | 効果 | 安全か / 直し |
|---|---|---|
| KSN_BORROWCK / --borrowck | KIR の借用検査の足し方 | 直した (全モードが上位集合) |
| KSN_CHECKER | old/new/shadow は非推奨の警告だけ。同じ検査器 | 安全 (拒否は変わらず) |
| KSN_CHECK_CACHE | 検査結果の置き場の入切 (0/1)。キーは内容で、拒否は変わらず | 安全 |
| KSN_CHECK_CACHE_MAX / _ENTRIES, KSN_CACHE_MAX / _GRACE | 置き場の大きさ・削除の猶予 | 安全 (検査に関与しない) |
| KSN_STDLIB | 標準ライブラリの場所を明示。暗黙の探索は実行体の隣と開発の配置だけ (cwd・親は見ない。sec2-X2 で直済) | 利用者の選択。暗黙には設定されない |
| KSN_CACHE_DIR | キャッシュと展開した標準ライブラリの置き場。安全でなければ mkdtemp の退避 (T2) | 安全 |
| KSN_S5_KIND_OLD / _PAT_OLD / _TYIDX_TAB | KIR の下ろしの旧い経路 (出力の比較用)。AST の検査は残る | 安全 (拒否は変わらず) |
| KSN_S5_METER, KSN_TIMING, KSN_TIME, KSN_PANIC_LOC | 計測・表示 | 安全 |
| KSN_EXP | 効果推論の不確かさを stderr に出すだけ | 安全 |
| KSN_FIX_ONLY | `ksn fix` が当てる直しの絞り込み。check には効かない | 安全 |
| KSN_STRICT_TYPES | 廃止済み (=0 は終了 2) | 安全 |
| KSN_SKIP_BORROW_DIAGS | debug / test-hooks のみ (release では無効) | 安全 |
| KSN_DUMP_* / KSN_TYPE_COVERAGE(_TOP) / KSN_TYPE_SURVEY / KSN_VM_DUMP | 診断用の出力 | 安全 |
| KSN_CLANG / KSN_CLANG_ARGS / KSN_CLANG_RESOURCE_DIR / KSN_CODEGEN / KSN_LINKER / KSN_WASM_LD / KSN_SYSROOT | ビルドの道具の選択 (検査には効かない)。利用者が道具を選ぶ | 利用者の選択 |

メモ: 標準ライブラリの展開先の `.done` は同じ利用者の置き場のなかで中身を確かめ直さない (安全な置き場が前提)。
