# perf-long-fns (P-11/P-12)

KIR 借用検査 (`kir_borrowck.rs`) の二乗の箇所を直した: `is_load_def` と `def_of` の全命令の走査を表引きに、要求の線形探索を (block, idx) の索引に。結果は同じ (`tests/check_diff.py` で unsound/linear_ok/diag と生成 4324 件が 1 バイトも違わない)。selfhost/lower の check は 20.0 秒から 10.7 秒。残りは TAST の build_prog / checker と KIR の下ろし (ll_fns)。
