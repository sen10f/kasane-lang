# 書き方は1通り (fmt) の実装計画

### 2.10?? 書き方は1通り: 整形の形の規則 (`ksn fmt`、2026-11-21)

- 形: `設計_書き方は1通り.md` 12 章と 2026-11-21 の利用者の決定。規則は `stage0/src/fmt.rs` の `fix_forms` (字句・行の形・字下げの形)、リテラルは `canon.rs` の `canon_int`。`check` の E0020 は既存の「`ksn fmt` の出力と違う」の仕組みで出る (規則を足すだけ)。
- 入れた形 (`fmt` が直す。試験は `stage0/tests/fmt_fix.rs` の `canon_*`。悪い形が E0020 で拒まれ、`fmt` 後に検査が通り、冪等):
  - `if c { true } else { false }` → `c`、`{ false } else { true }` → `!c` (1 行の形と複数行の形。`let x = if ..` / `return if ..` も)。
  - `let 名前 = *&*r` → `let 名前 = *r`、`let 名前 = *&x` → `let 名前 = x` (右辺の全体だけ。値として読む形)、`&*&x` → `&x` (共有から共有の形だけ)。
  - 整数の 0x / 0b / 0o → 10 進 (桁区切りは既存の規則。桁数に上限は無い)。
  - 本体の中の空行は 0 (定義の間は 1 つ)。
  - `if c { return .. } else { E }` → `if c { return .. }` の後ろへ `E` を出す。`E` に `let` があるときは、名前の見え方が変わる (E1021 にもなる) ので触らない。
  - `let x = e` の次が同じ字下げの `x` で、その次がブロックの終わり → `e` (注釈・`mut`・複数行の `e` は触らない)。
  - 空の `else {}` → 無し。
  - 1 行で終わる match の腕 `_名前 => e` で、パターンの `_名前` を本体が使わないもの → `_` (`=> {` や `=> f(` で続く腕、ガードのある腕、文字列を含む行は触らない。続く行で名前を使うかもしれないため)。`let _名前 = e` は直さない (`let _ = e` は E2041 で禁止なので、正規形が決まらない。未決)。
  - `let 名前: &'static str = "文字列"` → `&str` (文字列の値から同じ意味になる場合だけ。他の位置の `'static` は触らない)。
- 直していないもの (型の情報が要る。`fmt` には無い。`ksn fix` で診断として扱う別の段が要る): (1) ターボフィッシュ `f::<T>(..)` (引数や注釈から型が決まるとき)、(3) `Copy` の型の `.clone()` → 値、(5) 整数の `!(a < b)` → `a >= b` (浮動小数は NaN のため対象外)。
- 直していないもの (参照の流れが要る): `&mut *r` → `r` (`r` を動かすか再借用するかで意味が変わることがある。`r` が no-op の再借用だと言える場合の規則が要る)。
- 注意: `*&` は可変性を変える。`&mut *&x` や `*&x = ..` は拒まれる形で、`&mut x` や `x = ..` に直すと通ってしまう (試験の言い換え台帳で見つかった)。そこで `*&` は `let` の右辺の全体だけ直し、`&mut` を含む形は触らない。非 Copy の `let p = *&s` は元から拒まれていた形が通るようになる (型が要るので `fmt` では直せない。注意として残す)。
- 正しいコードへの影響 (`fmt` の結果を入れた。ab862a01 の後に、腕の規則の不具合 (複数行の腕の名前を消していた) を直して入れ直した):
  - `examples/*`・`apps/*`・`selfhost/*`・`ui/kasane/*`: 腕の `_名前` が大半、`let` の末尾が 1 件 (`ui/kasane/tree`)、空行と 16 進の桁。`*&`・空の else は無し。`ksn check` は全パッケージで診断 0 件。
  - `spec/stdlib` (`.ksnh` は fmt して `.ksnl` を作り直した): 19 ファイル。`bench/tasks/*/reference.ksnl`: 9 本 (腕の `_名前` と `Err(_e)` 類)。
  - `stage0/tests/data/` の入力: 89 本 (`unsound/`・`linear_ok/`・`threads/` ほか。`diag/` は触っていない: 期待するコードを変えるおそれがあるため。`diag/E3010c`・`E3010e` は未対応)。`unsound/` の `&*&x` → `&x` は共有から共有の形だけ (`&mut *&x` は直さない)。
  - 注意: `let x = e; x` を直すと、その形を試験していた入力 (`linear_ok/`・`unsound/` の let の別名) から、let の形が消える。試験は `Tmp::new_strict` で入力を `fmt` に通すので、中身の検査は同じだが、生のファイルを読む試験では形の範囲が狭くなる。
- 試験の入力・期待値の変更 (依頼の範囲外だが、新しい規則の直接の影響):
  - `tests/fmt_fix.rs` の `a_tail_return_is_fixed_by_fmt_and_is_an_error_in_check` の `pick` の正規形を、新しい規則 (真偽・空行・return の後の else) に合わせた (`if n < 2u32 { return n }` の後ろに末尾の式)。
  - `tests/debug.rs` の `SRC` (`t.add`・`t.work`) は `let s` / `let twice` の直後に `core.mem.drop(..)` を 1 行入れた (末尾の `s` / `twice` だと let-tail で消えるため、ブレークポイントの行と局所変数を残した)。`step_in_next_out_and_finish` は、`t.add` に 1 行増えた分の step を 1 つ足し、期待の停止を 1 つ増やした。
  - 言い換えの台帳 (`rephrase_known*.txt`): 差は 3 件 (valid 側: `param_borrowed#3`→`#2` の番号ずれ、`return_arg_borrow_ok2#1` が消えた。unsound 側: `match2iflet exh_match_result_option_nonexhaustive#0` が新規。`if let Ok(Some(n)) = r { n } else { 0u32 }` は全ての場合を覆う正しいプログラムで、言い換えの元の形 (`Err(_e)`) が変わったため現れた、安全な形)。`fold ref_of_ref_deref#1`・`fold return_bound_owner_c2#0`・`id const_init_nested_fn_panic#1` は消えた (入力の形が変わった)。
  - `shadow_baseline.json`: 更新 (1031 件)。新しく出た差は `unsound/` の 8 本の入力の形の変化による (旧新の検査器の差の見え方が変わったもの)。
