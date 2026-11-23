### 2.10?? 小さな標準ライブラリの厳密化 (std/strict-small)

- `core.slice.ct_eq(a: &[u8], b: &[u8]) -> bool` (`spec/stdlib/core/slice.ksnl`): 長さが違えば即 `false` (長さは秘密にしない)。同じ長さなら全バイトの `^` を `|` で集める。分岐は長さだけで決まり、内容には依らない (ネイティブの LLVM IR を確かめた: 内容に依る `br` は無い)。木走査とネイティブは同じ Ksn の本体なので一致する。
- `<int>.parse_decimal(s)` (`num.sig` は `gen_num_sig.py` で生成): 符号つきの型だけ先頭の `-` を受け、その後は 1 桁以上の ASCII 数字。先頭の 0 は `"0"` だけ、`-0` は拒む。`ParseInt` 命令に `strict` 欄を足し、木走査 (`natives.rs`・`vm`)・参照実行器 (`kir_exec::parse_int`)・LLVM (`kir_rt.c` の `ksn_parse_int` の新しい引数) が同じ規則で拒む。
- `std.http`: 要求の対象 (path) に 0x20 未満と 0x7f があれば `BadRequest` (`has_ctl`)。`Content-Length` と chunk の大きさは `+` を既に拒んでいた (数字だけを読む・16 進の検査) ので、試験で固定した。`Request.header_unique(name)` は同名が 2 つ以上あれば `BadRequest`。
- 未対応: `u128.parse_decimal` / `i128.parse_decimal` の機械語への下ろし (既存の `parse` と同じく 128 ビットの `ParseInt` は段階 1 の対象外)。木走査では動く。
