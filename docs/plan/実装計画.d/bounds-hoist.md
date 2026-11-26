# kir/bounds-hoist

- bytescan / soa のホットループは既に数えるループ (長さの再読込なし、Vec.push は alwaysinline)。差は負荷の雑音で、直しは不要だった。
- 新しい 6 本の bench (itchain / closurehot / dyncall / smallalloc / fmtparse / enummatch) を `kasane-verify/agents/perf-codegen/bench/` に足した。
- 直した 2 点: (1) `ksn_parse_int` (kir_rt.c) の 128 ビット除算を 64 ビットの速い経路へ (溢れ・不正文字は元の経路に任せ、結果と種類は不変)。fmtparse 6.2s → 1.5s。 (2) 命令の途中の一時の `alloca` (ParseInt の出力欄など) を入口のブロックへ (ループ内で動的な alloca になり、1 回ごとにスタックが伸びていた)。
- 未対応: itchain は、閉包ごとの `.w` の深さ・スタックの検査 (ループに残り、ベクトル化を妨げる) が原因。設計の判断が要る。
