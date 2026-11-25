# m3-env-fix-cache

E1: KSN_BORROWCK / --borrowck の全モードを上位集合に (AST の診断を捨てない)。T1: fix の stack の数を桁区切りごと外す。T2: 退避キャッシュを mkdtemp で作り所有者・モードを確かめる。

他の env: KSN_FIX_ONLY (fix のみ)、KSN_CHECKER (同じ検査器)、KSN_STDLIB、KSN_CHECK_CACHE、KSN_S5_*_OLD、KSN_EXP は要確認 (検査を弱めうるものは未発見)。
