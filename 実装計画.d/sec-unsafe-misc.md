### 2.10?? unsafe の境界の仕上げ (U1・C1・E1)

- U1: E5001 を、書き方 (経路・メソッド) でなく解決した呼び先の署名で判定 (`tast_rules/unsafe_calls.rs`)。許可の無いモジュールでは、呼び先 (`Def`/`Methods`/`Dyn` のトレイトのメソッド) の署名が生ポインタを持つ呼び出しを、メソッド・総称の境界・閉包の中でも拒む。`std.alloc.SYSTEM.dealloc(p, fit)` の二重解放が E5001。試験は `unsound/sec_u1_allocator_method_dealloc`。
- C1: 何も描かない詰め物 (U+115F U+1160 U+3164 U+FFA0 U+2800 U+034F U+17B4/5) と異体字選択子 (U+180B..D, U+FE00..0D, U+E0100..) を `lexrules::forbidden` に足し E0004。U+FE0E/FE0F は書いてよい。試験は `lexical_strict`。
- E1: `KSN_SKIP_BORROW_DIAGS` は debug か feature `test-hooks` のときだけ効く。`interp_parity` は `--features test-hooks` で回す。
