# 設計: S5 / S6 — 型付き IR の一本化 (KIR を TAST から下ろす、インタプリタを KIR に寄せる)

状態: 設計 (調査の結果。コードは変えていない)。行番号は `dev/plan` の `44eb5198` 時点。件数は grep の目安で、棚卸しの網羅は各段の最初に再取りする。
前提: `設計_S4の切り替え.md` / `設計_S4の旧い検査の取り外し.md` (TAST を唯一の検査にする)、`設計_型付き検査.md`、`設計_KIR.md`、`設計_単相化キャッシュ.md`、`設計_増分検査.md`。

## 0. 要約

- 今の型の決め場所は 4 つ: 旧検査 (`check.rs` / `types.rs` / `borrowck.rs`)、TAST (`tast.rs`、推論後に `tytab` + `SideTabs` から機械的に組む)、KIR の下ろし (`kir_lower*.rs`)、インタプリタ (`interp.rs` / `natives.rs`)。下ろしは `tytab` を `(def名, 式ID)` で引く (`kir_lower.rs:468 src_ty`、`kir_lower_coll.rs:164 table_ty`) が、その後に自前で `Kind` / `Src` / `Ty` を作り直し、名前も `p.resolve` で引き直し、単相化の型引数も `mono::unify` で再推論する。「表を引く所」と「再導出する所」が混ざっているのが、直近の `&String` の `Kind::String` 誤導出と N3a の Fn 境界の単一化順の食い違いの根。
- S5 = 下ろしの入力を `V` + 局所推論から **TAST + 追加の表**に替える。型は `TExpr::ty`、呼び先は `res`、型引数は `targs`、変換は `conv`、取り込みは `closure_caps` から読む。再導出は段ごとに消し、各段で「`ksn run` == ネイティブ」「ネイティブ比の下限」「`cargo test --release`」を保つ。見積り合計は中〜大 (約 9 段、各 0.5〜3 日相当)。
- S6 = 既定の実行系を **KIR → VM バイトコード (`vm/`)** にし、木走査 (`interp.rs`) は最後に外す。TAST 直走査は採らない (7 章で理由)。ただし今は VM が既定でなく (`cli.rs:1496 take_engine`、既定は木走査)、KIR が下ろせない関数は木走査へ退避する (`vm/mod.rs`)。**KIR の被覆が木走査と同じになる (退避 0 件) ことが S6 の前提**。
- キャッシュ: 単相化キャッシュ (鍵 = 定義の TAST 正規形のハッシュ + 型引数 + 依存インスタンス) と、定義単位のビルドキャッシュ (今の `cli.rs:3281 build_key` はパッケージ全体の `package_hash`) は S5 の後 (TAST が定義ごとに閉じた入力を渡せる状態) に作る。
- 依存: S4 (旧検査の取り外し) → S5 → S6。S5 の前半 (表を引く側の整理) は S4 と並行でよいが、旧検査が `tytab` を作り続ける間は表の出所を変えない。

## 1. 現状の型の決め場所 (棚卸しの結論)

| 層 | 型・種類の出所 | 問題 |
| --- | --- | --- |
| 旧検査 (`types.rs` の `TypeCk`) | 推論の本体。`tytab: BTreeMap<(String,u32), ExprTy>`、`call_res`、`let_tys`、`closure_caps` | S4 で TAST 側へ移す対象 |
| TAST (`tast.rs:319 build`) | `tytab` と `SideTabs` (`resolved_fn` `type_args` `coercions` `pat_tys` `closure_caps` `regions`) から組む。利用者の定義だけ。`TBody::complete` で被覆を持つ | 木の形が検査向け (`TKind::Other` に `For`/`While`/`Return`/`Try`/`TupleE`/`ArrayE`、`TStmt` が `Let/Expr/Assign` だけ)。標準ライブラリの定義が無い。単相化の実体が無い |
| KIR の下ろし (`kir_lower*.rs`、約 2 万行) | `tytab` を引いたあと `Src → Ty` / `Kind` を再構築。名前は `p.resolve`、メソッドは名前文字列と `method_of_ty`、総称の型引数は `mono::unify` | 下の 2〜4 章 |
| インタプリタ (`interp.rs` `natives.rs`) | 値 (`Val`) が実行時に型の印を持つ動的実行。型の決定は `coerce(&V)`、`find_method`、`prim_kind`、`pick_ty` など | 5 章 |

## 2. S5 の棚卸し: 下ろしが型・種類・名前を再導出している所

(grep の件数: `Kind::` はファイル別に coll 100 / agg 45 / own 24 / str 14 / method 13 / enum 10、`from_v(` は agg 22 / coll 27 / lower 15 / ext・method・mono 各 7 前後。)

### 2.1 型の再導出

| 箇所 | していること | 置き換える TAST の表 |
| --- | --- | --- |
| `kir_lower.rs:468 src_ty` / `478 ty_of` | `tytab` を引いて失敗なら `coll::local_slot_ty` (スロット型) で補い、`value_ty`・`conv_t` で `Ty` に変える。`Ref(false, スカラー)` のときだけスロット型を信じる特例 (コメント: `Some(c)` の束縛が `&スカラー` のまま) | `TExpr::ty` と `PatInfo::bind` (パターン束縛の型とモード)。特例は `BindMode` で消える (束縛の型を `&T` でなく値の型 + 「参照越し分解」の印にするかは S5-2 の決定事項) |
| `kir_lower_coll.rs:164 table_ty`、`:372 kind_of_expr`、`:316 kind_of_src` | 型から `Kind` (`Vec`/`String`/`Str`/`Slice`/`Range` …) を作る。局所変数は `coll.kinds` (スロットの種類) を先に見る。リテラルは式から直接 | `TExpr::ty` から **1 か所の純関数** `kind_of(Ty)` に集約 (`&String`/`&mut String` を剥いだ結果をここで 1 回決める)。スロットの `kinds` 表は `PatInfo` から作る |
| `kir_lower.rs:1076-1096` (`Kind::String` / `Str` を `Ty::Agg`・`Ptr`・名前 `alloc.string.String` から逆算) | KIR の型 → `Kind` の逆引き。`plain_vec(name)` の文字列比較 | `Kind` を値の生成時に確定して持ち回る (逆引きをやめる)。これが `&String` 反復要素の誤導出の型の穴 |
| `kir_lower_coll.rs:1178 pat_srcs` | パターンから束縛名の `Src` を再計算 (`for`・`match` の束縛) | `SideTabs::pat_tys` / `TPat::info` |
| `kir_lower_coll.rs:2684 local_slot_ty` | 型表に無い局所の型をスロットから | `TBody::complete` が真なら不要。`for` の変数は `TStmt` / `TExpr::pats` に型が入る |
| `kir_lower_agg.rs:810 conv_t`、`:779` | `Src → kir::Ty` の変換と構造体欄の型の再構築 (`from_v(宣言) + subst`) | 変換は残す (これは「型の表現の変換」であって再推論ではない)。入力を `TExpr::ty` と TAST 側の宣言の型表にそろえる |
| `kir_lower.rs:861,1382,1388,1392,1650,1651,2131`、`kir_lower_coll.rs:1606-1892`、`kir_lower_ext.rs:211,219`、`kir_lower_check.rs:271`、`kir_lower_enum.rs:1139,1194`、`kir_lower_dyn.rs:372` | 宣言の型 (`V`) を `from_v` で引き直す。`decl_ty` (`kir_lower_mono.rs:92`) が代表 | **定義の署名表**: `DefId → (params: Vec<Ty>, ret: Ty, generics)` を Program に 1 つ持つ (今の TAST は `TBody::params/ret` を利用者定義にだけ持つ)。標準ライブラリの `sig` にも必要 |
| `kir_lower_str.rs`、`kir_lower_strord.rs`、`kir_lower_own.rs:722 recv_elem` | 文字列・順序・所有の特殊処理が、受け手の `Src`/`Kind` から要素型を再計算 | `TExpr::ty` (受け手) と `conv` (`AutoRef`/`AutoDeref`) |

### 2.2 名前の再解決

| 箇所 | 内容 | 置き換え |
| --- | --- | --- |
| `kir_lower.rs:841,1289,1343,2178`、`agg.rs:107,258,893,1302,1561`、`coll.rs:990,1154,1546` | `self.p.resolve(&segs, &def.file)` で経路を局所・関数・型・列挙子へ。`Tgt::{Fn,Sig,Type,Variant}` で分岐 | `TKind::Path` に **解決先** (`Res::{Local(PatId),Def(DefId),Variant(..),Const,Type}`) を足す。`Call` は既存の `resolved_fn` (`Callee`) を使う |
| `kir_lower.rs:429 lookup`、`kir_lower_ext.rs:66 find_def` | 名前で定義を探す | `DefId` で直接引く |
| `kir_lower.rs:2158 ufcs_rewrite` | `x.f()` ⇄ `f(x)` の書き換えを `V` に対して下ろしの前に実施 | `MCall` + `res: Callee::Def` の呼び先 + 受け手の `conv`。書き換えは不要になる |
| `kir_lower_method.rs:25 recv_type_name`、`:62 method_of_ty`、`:194 recv_src`、`:330-425 recv_path/recv_expr`、`kir_lower_coll.rs:2053 method_on` (Kind × メソッド名の文字列) | メソッドを「受け手の型の名前 + メソッド名」で引き、標準ライブラリのメソッドは `match (kind, name)` の表 | `Callee::Def(DefId)` で `sig` の定義へ。組み込み (Vec/String/…) の下ろしは **`DefId` → 下ろし関数の登録表**に変える (`match (kind, "push")` をやめる)。`Callee::Methods(Vec<DefId>)` が残る (候補複数 6.3%) ので、S4 で 1 つに絞るか、型で確定させる |

### 2.3 単相化・クロージャ・所有

| 箇所 | 内容 | 置き換え |
| --- | --- | --- |
| `kir_lower_mono.rs:19-90 subst_ty/concrete/unify`、`kir_lower_coll.rs:1606-1770` (呼び出しごとに引数の型から型引数を決める) | 下ろしが総称の型引数を自前で単一化。N3a の食い違いはここ (`Fn` 境界の順) | `SideTabs::type_args` (`TExpr::targs`)。外側の総称関数のインスタンスの中では `subst` で具体化 (今も `mono::subst_ty`)。**単一化は下ろしから消える** |
| `kir_lower_coll.rs:1809-1892`、`:2320` (クロージャ引数を持つ関数の個別のインスタンス化) | クロージャ型をそのインスタンスの鍵に混ぜる | `Ty::Closure { id }` と `closure_caps` を鍵に使う (取り込みは TAST にある) |
| `kir_lower_own.rs` (24 の `Kind::`、`own_emit(..Consume/Part/Use/Init/Owe)`) | 移動・部分移動・線形の義務を `V` を歩いて再計算 (`kir_lower.rs:777-828,1192-1238,1700`) | TAST の **move/borrow 事実** (後述 3.4)。今の TAST の規則 (`tast_rules/borrows.rs`、`captures.rs`、`discard.rs`) は結果を診断に使うだけで、事実の表として保存していない |
| `kir_lower_env.rs` (82 行) / `closure` の取り込みの再計算 | 自由変数を `V` から再抽出 | `TKind::Closure { captures }` (既にある) |
| `kir_lower_enum.rs`、`kir_lower_for.rs`、`kir_lower_iter.rs` | `for` の反復子の要素型・`Kind` を型から再計算 (`&String` の誤導出の現場) | `TExpr::pats` + `Item` 型を `For` の節点に保存 (3.3) |

## 3. TAST に足りないもの (追加する表・欄)

1. **パス・名前の解決先** (`TKind::Path` / `PatInfo` / 構造体リテラルの型): 局所は `PatId`、定義は `DefId`、列挙子は `(DefId, 変種番号)`、定数。`Callee` は呼び出しだけに限られている。
2. **標準ライブラリを含む定義の署名表** (`DefId → Sig { params, ret, generics, bounds, effects }`) と、型の定義表 (構造体の欄・列挙子の変種・`derive`・`Copy` かの印)。今の TAST は `bodies` が利用者の定義だけで、署名は `from_v` で引き直す前提 (`TBinding` の doc コメント)。
3. **形の被覆**: `TKind::Other` に隠れた `For` `While` `Loop` `Return` `Try` `TupleE` `ArrayE` `Break/Continue` `Range` `Cast` 等を、型付きの専用の形にする (または `Other` に型・解決を持たせる)。`For` は反復子の型と要素型 (`Item`) を持つ。
4. **所有と借用の事実**: 式ごとの使い方 `Use::{Copy,Move,Borrow(共有/可変),Part(field path),Drop}`、スコープ終端の drop 列 (線形の `Owe` を含む)。`Place` は既にあるので、その上の「この `Place` をこの式が消費する」を表にする。規則 W (借用) / M (移動) の中間結果を保存する。
5. **単相化インスタンスの表** `MonoInst { def, targs, closure_ids }` と、呼び出し節点 → インスタンスの写し。総称の本体は TAST では型パラメータのまま、インスタンスは `subst` で具体化 (KIR 側の `Lw::subst` を TAST の `Ty::subst` へ移す)。
6. **閉包の型**: `Capture` は既にある。`closure_id` を安定した文字列 (定義 FQN + 連番) にする (今の KIR 側の名前は節点の `dump()` のハッシュ。単相化キャッシュ設計 §0(b) の問題)。
7. **強制・脱糖の印の完全性**: `Coercion` は設計済みだが、整数リテラルの型・`as`・演算子オーバーロード (`+` が `Add` の呼び出しになるか) の解決先が無い。
8. **行情報**: `TExpr::line` はあるが、列・ファイルは無い。デバッグ情報 (`kir_debug.rs`) とスタックトレース用に `Span { file, line, col }` を足す。
9. **完全性の保証**: `TBody::complete` を、下ろしの前提として「E2044 0 件」と同等に扱う (今は被覆の統計)。

## 4. S5 の移行順 (独立に検証できる段)

各段の合格基準 (共通): `cargo test --release` 全件、`ksn run` == `ksn build` (LLVM) の一致テスト (`kir_native.rs` 系、`ksn test --both`)、`perf/` のネイティブ比の下限 (今の基準値を段の前後で比べ、劣化 2% 超は不合格)、`examples/*` と `bench/tasks/*/reference.ksnl` の出力が同じ。

| 段 | 内容 | 規模 | 検証の要点 |
| --- | --- | --- | --- |
| S5-0 | **差分の計器**: 下ろしが `tytab` から作る型と TAST (`TExpr::ty`) の型を、環境変数 `KSN_TAST_CROSSCHECK=1` で全式突き合わせ、食い違いを出す (動作は変えない)。再導出の種類を数える | 小 (1〜2 日) | 食い違い 0 件が以降の前提 (S4 の `SAME` と同じ流儀) |
| S5-1 | 下ろしの入口を `Tast` に替える (`Lw` が `&TBody` を持つ)。`src_ty` の本体を `TExpr::ty` 引きにし、`tytab` 直引きをやめる (中身は同じ型) | 中 (2〜3 日) | 出力 KIR がバイト単位で同じ (`ksn kir` のダンプの差分 0) |
| S5-2 | パターン・束縛: `pat_srcs`、`local_slot_ty`、`Ref(スカラー)` の特例を `PatInfo` / `BindMode` で置き換える。表 3.1 (束縛の解決先) を足す | 中 (3 日) | `for`/`match`/`let` の例と `&String` 反復の回帰試験 (`stage0/tests/data/` に追加) |
| S5-3 | `Kind` の一本化: `kind_of(Ty)` を 1 か所にし、逆引き (`kir_lower.rs:1076-1096`) とスロットの `kinds` の推測をやめる | 中 (3 日) | `Kind` を使う全ファイルの KIR ダンプが同じ。`kir_lower_coll.rs` (3970 行) が最大の山 |
| S5-4 | 名前の解決: `Path`/`Variant`/`Type`/`Const` を TAST の解決先で引く。`p.resolve` の呼び出しを下ろしから消す (9 か所 + α)。署名表 (3.2) を足し、`from_v` を消す (約 80 か所) | 大 (4〜5 日) | `grep -n "p.resolve\|from_v(" kir_lower*.rs` が 0 件。診断の行番号が同じ |
| S5-5 | 呼び出し・メソッドの解決: `ufcs_rewrite` と `method_of_ty`/`recv_*` を `Callee::Def` に。組み込みの下ろしを `DefId` の登録表に (3.2 の署名表と同時) | 大 (5 日) | メソッドごとの下ろしの回帰 (既存の `kir_*.rs`)。`Callee::Methods` の残りを S4 側で 0 件にしておく |
| S5-6 | 単相化: `mono::unify` を消し、`targs` と `MonoInst` で下ろす。N3a の `Fn` 境界の順の問題は TAST (推論側) だけが持つ | 大 (4 日) | 総称・`Fn` 境界・N3a の回帰。インスタンス数が前後で同じ |
| S5-7 | 所有・借用・drop: `own_emit` を TAST の使い方の事実 (3.4) から出す。`kir_borrowck.rs` (KIR の借用検査) は残し、TAST の事実との整合を検査にする | 大 (5 日) | 線形・部分移動・drop の回帰、`stage0/tests/data/unsound/` が全件エラーのまま |
| S5-8 | 形の被覆 (3.3) と `Other` の分解、`V` を下ろしから外す (`use parser::V` を `kir_lower*.rs` から消す)。`ksn kir` / `ksn build` は TAST を直接渡す | 中 (3 日) | `grep "parser::V" kir_lower*.rs` が 0 件。ネイティブ比の下限 |

順序の理由: 先に「同じ答えを別の入口で引く」だけの段 (0〜2) を置いて、動作の同一を保ったまま計器を張る。次に種類 (3)、名前 (4)、呼び出し (5)、単相 (6)、所有 (7) と、**他の再導出の入力になる順**に進める。5 と 6 は互いに独立でなく、`targs` が呼び先の署名表に依存する。

## 5. S6 の前提: インタプリタ側の型の決定の棚卸し

インタプリタは `Val` が実行時の型の印を持つ動的実行で、型表を使わない。そのため **型の再導出というより型の再発明** が起きる。

- 値の強制: `interp.rs:319 coerce(&V)` (宣言の型の `V` を実行時に読んで整数の幅・`f32`・`Box` などへ寄せる)、`:165 scalar_ty`、`natives.rs:85 pick_ty` (引数の値から整数型を選ぶ)、`natives.rs:80 known_num`。
- メソッドの解決: `interp.rs:1744 find_method` (値の型名 + メソッド名)、`:1766 prim_kind`、`:1778 invoke_method`。標準ライブラリの本体は `natives.rs` (約 2100 行、文字列名の `match`)。
- 構造体・コピーの判定: `:392 is_copy(tyfqn)`、`:412 mk_struct`。
- 整数演算の幅: `:2134 int_arith(op, x, y, Ity)` (値の `Ity` から)。リテラルの型は推論の結果でなく文脈から。
- これらが、`ksn run` (木走査)・`ksn run --engine=vm`・ネイティブの 3 つの結果が食い違いうる元 (`設計_インタプリタ一致.md`)。

## 6. S6 の選択: KIR (VM) か TAST の直走査か → **KIR を VM で走らせる**

| 観点 | TAST の直走査 | KIR → `vm/` (採用) |
| --- | --- | --- |
| 意味の唯一性 | 木走査の意味を 3 つ目として持つ。ネイティブ (KIR → LLVM) とは別のまま | ネイティブと **同じ KIR** なので、`ksn test --both` の差は VM とバックエンドの差だけ |
| 速度 | 木走査のまま (型は引けても分岐の費用は残る) | レジスタ型バイトコード (`vm/bc.rs` `run.rs`) で既に木走査より速い。静的型で `int_arith` の分岐が消える |
| ホットリロード | `V` を差し替えれば済む (今) | `vm/reload.rs` が **関数単位のバージョン表** と安全点 (呼び出し) で実現済み。外部面 (署名・型) が変わらない本体の差し替え。S5 後は TAST の定義単位の差分 + 再単相化で同じ |
| 組み込み (`natives.rs`) | 今のまま使える | KIR の組み込みは `kir_lower_*` が下ろす (大半) か、`Op::Call` の外部呼び出しで `natives` を呼ぶ。**`natives.rs` の値表現 (`Val`) と KIR のメモリ表現が違う** のが最大の工事 |
| FFI (`ffi_interp.rs` 869 行) | `Val` から C の ABI へ動的に | `kir_cabi.rs` / `kir_lower_ffi.rs` が既に ABI の分類を持つ。VM は `dlopen`/呼び出し台を、型付きの KIR 情報から呼ぶ形に寄せられる (1 本化できる) |
| デバッグ・行情報 | 節点の行 | `vm/bc.rs:310 line_at(pc)` と `kir_debug.rs` に既にある。S5 で `Span` を TAST から運ぶ |
| 被覆 | 木走査は全言語を走る | **KIR は「段階 1 の対象外」で下ろせない関数がある** (`vm/mod.rs` の木走査への退避、`unsupported`) ← 最大の障害 |

結論: 最終形は **KIR + VM のみ**。ただし被覆が揃うまでは木走査を残す (7 章の段階移行)。TAST の直走査は採らない (意味の 3 重化、`V` に依存し続けるため)。

## 7. S6 の移行順

| 段 | 内容 | 規模 | 検証 |
| --- | --- | --- | --- |
| S6-0 | **被覆の計測**: `examples/*`、`bench/tasks`、`stage0/tests/data` の全入口で、KIR に下ろせない関数の数と理由の一覧 (`Lowered::unsupported`)。木走査への退避 0 件を目標に、理由別に残りを数える | 小 (1 日) | 一覧の表を文書に載せる |
| S6-1 | 被覆の穴埋め (S5 と並行): 段階 1 の対象外を 1 理由ずつ KIR に入れる。ネイティブと VM が同時に得をする | 大 (理由ごとに 1〜3 日、合計 2〜3 週) | 各理由ごとに `ksn run` == `--engine=vm` == ネイティブ |
| S6-2 | `natives.rs` の統合: 標準ライブラリの本体を KIR に下ろす形 (`std` を Kasane の本体で書く `設計_stdlib_kasane.md` の道) か、VM が `Val` を介さず KIR の値を直接渡す形に寄せる。**どちらかに決める** (推奨: 実装済みの下ろしがあるものは KIR、未下ろしだけ当面は VM のホスト関数) | 大 (3〜5 日 + 末尾の長い尾) | `natives.rs` の関数ごとに VM のホスト関数との同値試験 |
| S6-3 | 既定を `--engine=vm` にし、木走査は `--engine=tree` の退避に降格。退避時は警告を出す (今は 1 行) | 小 (1 日) | `ksn test --both` が既定の 2 系統 (VM とネイティブ) になる |
| S6-4 | FFI: `ffi_interp.rs` の動的 ABI 呼び出しを、KIR の ABI 分類 (`kir_cabi.rs`) を使う呼び出し台に置き換える | 中 (3 日) | `ffi` の既存試験、`N3a` (コールバック) |
| S6-5 | ホットリロード: TAST の定義単位の差分 (§8 の鍵) で「どの関数のバイトコードを差し替えるか」を決める。署名が変わる定義は今どおり再起動の案内 | 中 (3 日) | `tests/run_tests.py` の実動作のホットリロード試験 |
| S6-6 | `interp.rs` / `natives.rs` / `ffi_interp.rs` の削除 (行数で約 5000)。`V` の木走査をやめる | 中 (2 日) | 全試験、`--engine=tree` の廃止を `実装計画.md` に記録 |

### `ksn test --both` の一致の保ち方
- S6-0〜S6-2 の間は `--both` を **3 系統** (木走査・VM・ネイティブ) にして、入口ごとに 3 つの出力を突き合わせる。VM と木走査が違ったら、その入口を「退避」に回し、理由を S6-1 の一覧に足す (被覆 0 件になるまで木走査が正)。
- 木走査と VM の食い違いが出たら、先に **どちらが仕様か** を `設計_インタプリタ一致.md` の表に書いてから直す (片方に合わせるだけにしない)。
- S6-3 以降は VM とネイティブの 2 系統。木走査は `--engine=tree` で比べるだけ (削除まで残す)。
- 試験は常に `taskset` を要しないこのマシンで回す (AGENTS.md)。再試行で隠さない。

## 8. キャッシュ (S5 の後)

1. **単相化キャッシュ** (`設計_単相化キャッシュ.md` の続き。今の設計は Cranelift 前提で、`clif.rs` は 2026-11-14 に削除済みなので、LLVM / KIR に読み替える)。
   - 単位 = KIR 関数のインスタンス 1 つ。鍵 = SHA-256(定義の **TAST 正規形** + `targs` の正規形 + 呼び先のインスタンスの鍵 + 生成条件 (版・edition・ターゲット・最適化・clang の版))。
   - TAST 正規形 = 定義 1 つ分の `TBody` (行番号・ファイル名・式 ID を除く、構造のみ) を決定的にシリアライズしたもの。`DefId` は添字なので鍵に入れず、呼び先は FQN + その定義の鍵で表す (再帰は強連結成分で 1 つの鍵)。
   - 前提 (単相化キャッシュ設計 §0 の 3 つ): 定数パラメータを鍵に入れる、閉包の名前を `定義FQN#連番` にする (3.6)、シンボル名を生成順の連番でなく鍵から決める。
   - 保存するもの: LLVM IR (またはビットコード) と、できれば `.o`。まず IR でよい (clang の費用の割合を S5-0 の後に計測して決める。`設計_単相化キャッシュ.md` の Step 0 に従い、利得が小さければ中止)。
2. **定義単位のビルドキャッシュ**: 今の `build_key` (`cli.rs:3281`) は `package_hash` でパッケージ全体を 1 鍵にしている。TAST の定義単位の鍵 (上の鍵 + その定義が読む型・署名の鍵) を使い、変更のない定義の KIR / オブジェクトを再利用してリンクだけやり直す。`設計_増分検査.md` の検査キャッシュの鍵 (定義単位) と **同じ鍵を共有**する (検査 → TAST → KIR → オブジェクトの順に再利用が連なる)。
3. 順序: (a) S5-8 の後に TAST の正規形シリアライズを作る、(b) 単相化キャッシュ、(c) 定義単位のビルドキャッシュ、(d) S6-5 のホットリロードが同じ鍵を使う。

## 9. リスクと「残すもの」

リスク:
- **被覆の尾** (S6-1/6-2): KIR の対象外と `natives.rs` の統合が最も長い。S6 は被覆の計測 (S6-0) の結果で日程を決め直す。
- **性能の劣化**: 下ろしが TAST の表を引く方式でも、定義ごとの表の生成・引きが `ksn check` の遅さに足される。S5-0 で計測し、`TExpr` の木を `Rc`/アリーナにする案は S5-8 の後に判断する。
- **S4 の未了と二重の真実**: 旧検査が `tytab` を作る間に S5 が TAST を入力にすると、TAST ≠ `tytab` の食い違いの調査が下ろしの不具合に見える。S5-0 の突き合わせで先に潰す。
- **`Callee::Methods` の残り** (候補複数、6.3%) と **整数リテラル・演算子の解決**: TAST に確定がなければ下ろしは再び推論する。S4 の側の完了条件に入れる。
- **標準ライブラリの本体が TAST に無い**: 組み込みの意味 (`spec/stdlib/**/*.sig`) は署名表で見る。下ろしの組み込み表との登録の不一致を試験で強制 (`sig` に載るメソッドは下ろしか natives のどちらかに必ずある)。
- **大きな書き換えのマージ衝突**: `kir_lower_coll.rs` (3970 行) は段ごとに 1 担当だけが触る。段は 1 本ずつ直列に統合する。
- 開発機の segfault 事情 (AGENTS.md) はこのマシンでは関係しない。

残すもの:
- `kir_borrowck.rs` (KIR の借用検査) は、TAST の事実との整合を見る後ろの網として残す (最後の防衛線。S5-7 以降も)。
- `Src (types::Ty)` → `kir::Ty` の変換 (`conv_t`)。再推論ではなく表現の変換。
- `kir_opt.rs` / `kir_llvm.rs` / `kir_suspend.rs` / `vm/` は TAST に依存させない (KIR より後ろ)。
- `ksn test --both` の比較の仕組みと `KNOWN_HOLES` / `unsound/` の試験。
- 診断の安定した順序 (`diagnostics_are_sorted_stably`)、E0020 の「書き方は 1 通り」。

## 10. S4 との依存グラフ

```
S4-1 (TAST の規則を唯一の検査に)
  └─ S4-2 (旧検査の取り外し: check.rs / types.rs / borrowck.rs の検査部)
        └─ S4-3 (推論の本体を TAST の入力生成へ移す: tytab → TAST を直接作る)
                │
S5-0 (突き合わせ) ←── S4-1 と並行可
S5-1 ── S5-2 ── S5-3 ── S5-4 ── S5-5 ── S5-6 ── S5-7 ── S5-8   (直列。S5-1 以降は S4-2 を待つのが安全)
                                                      │
S6-0 (被覆の計測) ←── S5-0 と並行可                     │
S6-1 (被覆の穴埋め) ←── S5-3〜S5-6 と並行 (同じファイルを触るので担当を分ける)
S6-2 (natives の統合) ──┐
S6-3 (既定を VM に) ←── S6-1 の退避 0 件 + S5-8
S6-4 (FFI) ── S6-5 (ホットリロード、§8 の鍵) ── S6-6 (木走査の削除)
§8 キャッシュ ←── S5-8 の後
```

要点: S5-0 と S6-0 (計器) は今すぐ始めてよい。S5-4 以降は S4-2 の旧検査の取り外し (表の出所が TAST の推論になること) の後。S6-3 以降は被覆 0 件が条件。
