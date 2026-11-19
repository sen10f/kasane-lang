//! TAST の側の表 (`SideTabs`) の中身を `KSN_DUMP_SIDE=1` の出力で確かめる。
//! 他の担当も同じファイルに足す: 試験関数名は担当の接頭辞 (`s1a_` `s1b_` `s1c_` ...)、出力の 1 行目 (列 1) は表の名前。

mod common;
use common::*;

/// `KSN_DUMP_SIDE=1` で検査し、表 `table` の行 (列は TAB 区切り、重複なし) を返す。
fn dump(name: &str, src: &str, table: &str) -> Vec<String> {
    let t = Tmp::new(name, src);
    let o = run_with_env(&["check"], &t.0, &[("KSN_DUMP_SIDE", "1")]);
    let mut v: Vec<String> = text(&o.stderr).lines().filter(|l| l.starts_with(&format!("{}\t", table))).map(|l| l.to_string()).collect();
    v.sort();
    v.dedup();
    v
}

const S1C_SRC: &str = "trait t.H
    fn handle(self: &Self) -> u32 uses [] panics never
end t.H

type t.X
    v: u32
end t.X

impl t.H for t.X
fn t.X.handle(self: &t.X) -> u32 uses [] panics never impls t.H
    self.v
end t.X.handle

fn t.X.bump(self: &mut t.X) -> () uses [] panics never
    self.v = self.v.wrapping_add(1u32)
end t.X.bump

fn t.sink(h: &dyn t.H) -> u32 uses [] panics never
    h.handle()
end t.sink

fn t.sl(x: &[u32]) -> usize uses [] panics never
    x.len()
end t.sl

fn t.ro(x: &t.X) -> u32 uses [] panics never
    x.handle()
end t.ro

fn t.main() -> Result<(), std.io.Error> uses [] panics may
    let mut x = t.X { v: 7u32 }
    x.bump()
    let a = [1u32, 2u32]
    let n = t.sl(&a)
    let m = t.ro(&mut x)
    let k = t.sink(&x)
    core.check.eq(k, 7u32)
    Ok(())
end t.main
";

#[test]
fn s1c_coercions_recorded() {
    let rows = dump("s1c_a", S1C_SRC, "coercions");
    let all = rows.join("\n");
    // `x.bump()` の受け手: 自動参照 (可変)。
    assert!(all.contains("AutoRef(true)"), "{}", all);
    // `&[u32; 2]` → `&[u32]`。
    assert!(all.contains("ArrayToSlice"), "{}", all);
    // `&mut X` → `&X` の再借用。
    assert!(all.contains("Reborrow { to_mut: false }"), "{}", all);
    // `&X` → `&dyn H`。
    assert!(all.contains("Unsize") && all.contains("Dyn(\"t.H\""), "{}", all);
    // 受け手がもう参照の `x.handle()` (`t.ro` の中) には印が付かない。
    assert!(!rows.iter().any(|r| r.contains("\tt.ro\t")), "{}", all);
}

// --- S1D: pat_tys ---

fn dump_side(name: &str, src: &str) -> String {
    let t = Tmp::new(name, src);
    let o = run_with_env(&["check"], &t.0, &[("KSN_DUMP_SIDE", "1")]);
    text(&o.stderr)
}

#[test]
fn s1d_pat_tys_modes() {
    let src = "fn t.main() -> i32 uses [] panics never\n    let p: (i32, i32) = (1, 2)\n    let (a, b) = &p\n    let mut q: (i32, i32) = (3, 4)\n    let (c, d) = &mut q\n    let (e, f) = p\n    0\nend t.main\n";
    let out = dump_side("s1d_modes", src);
    assert!(out.lines().any(|l| l == "# pat_tys"), "{out}");
    assert!(out.contains("PBind a : &i32 Ref"), "{out}");
    assert!(out.contains("PBind c : &mut i32 RefMut"), "{out}");
    assert!(out.contains("PBind e : i32 Move"), "{out}");
    assert!(out.contains("let_tys_mismatch=0"), "{out}");
}

#[test]
fn s1d_pat_tys_nested_shared_wins() {
    let src = "fn t.main() -> i32 uses [] panics never\n    let mut o: Option<(i32, i32)> = Some((1, 2))\n    let r = &mut o\n    match &r {\n        Some((x, y)) => 0\n        None => 1\n    }\nend t.main\n";
    let out = dump_side("s1d_nested", src);
    assert!(out.contains("PBind x : &i32 Ref"), "{out}");
    assert!(out.contains("let_tys_mismatch=0"), "{out}");
}

// --- S1E: 木の組み立て (`KSN_DUMP_TAST=1`) ---




/// 木のダンプ (stderr) を返す。診断は旧いまま 0 件であること。
fn dump_of(name: &str, src: &str) -> String {
    let t = Tmp::new(name, src);
    let out = run_with_env(&["check"], &t.0, &[("KSN_DUMP_TAST", "1")]);
    assert_eq!(out.status.code(), Some(0), "{}{}", text(&out.stdout), text(&out.stderr));
    text(&out.stderr)
}

const SRC: &str = "\
type t.P derive [Debug] pub
    x: u32
end t.P

fn t.go(p: &mut t.P, v: &[u32]) -> u32 uses [] panics may pub
    let a = 1u32
    let c = fn(y: u32) -> u32 capture [move a] {
        y.wrapping_add(a)
    }
    p.x = c(v[0])
    match v.len() {
        0 => *&p.x
        _ => a
    }
end t.go

fn t.main() -> Result<(), std.io.Error> uses [] panics never
    Ok(())
end t.main
";

#[test]
fn s1e_tree_has_types_places_and_captures() {
    let d = dump_of("s1e_tree", SRC);
    assert!(d.contains("body t.go complete=true"), "{}", d);
    // 引数・欄・添字・参照外しの場所
    assert!(d.contains("place=p@0.x"), "{}", d);
    assert!(d.contains("place=v@0[]"), "{}", d);
    assert!(d.contains("place=*"), "{}", d);
    // `let` の束縛は PBind の ID で指される
    assert!(d.lines().any(|l| l.contains("Path a") && l.contains("place=a@") && !l.contains("place=a@0")), "{}", d);
    // 閉包の取り込みは式 ID に直されて `a` の型付きで入る
    assert!(d.contains("Closure caps=[move a: u32]"), "{}", d);
    // `Match` の被検査式と腕のパターン
    assert!(d.contains("Arm") && d.contains("PLit") && d.contains("PWild"), "{}", d);
    // 型付き (呼び先の経路は別勘定)
    let st = d.lines().find(|l| l.starts_with("stats")).unwrap();
    let n = |k: &str| -> usize { st.split(&format!(" {}=", k)).nth(1).unwrap().split(' ').next().unwrap().parse().unwrap() };
    assert!(n("exprs") > 10 && n("typed") * 100 >= n("exprs") * 95, "{}", st);
}

#[test]
fn s1e_empty_side_tables_build_without_res() {
    // A〜D の表が空でも組める (res / targs / conv / PatInfo は付かないだけ)。埋まれば同じ形で入る。
    let d = dump_of("s1e_empty", SRC);
    let st = d.lines().find(|l| l.starts_with("stats")).unwrap();
    assert!(st.contains("pats_info=") && st.contains("res="), "{}", st);
}

// --- S1B: type_args ---


/// stderr から表 `name` の行 (`# name` の次から、次の `# ` の前まで) を取り出す。
fn table(err: &str, name: &str) -> Vec<String> {
    let head = format!("# {}", name);
    let mut it = err.lines().skip_while(|l| *l != head).skip(1);
    let mut out = vec![];
    while let Some(l) = it.next() {
        if l.starts_with("# ") || !l.contains('\t') {
            break;
        }
        out.push(l.to_string());
    }
    out
}

fn s1b_dump(src: &str, name: &str) -> Vec<String> {
    let t = Tmp::new(name, src);
    let o = run_with_env(&["check"], &t.0, &[("KSN_DUMP_SIDE", "1")]);
    table(&text(&o.stderr), "type_args")
}

const GEN: &str = "fn t.id[T](x: T) -> T uses [] panics never\n    x\nend t.id\n\n";

#[test]
fn s1b_type_args_free_fn() {
    let rows = s1b_dump(
        &format!("{GEN}fn t.main() -> () uses [] panics never pub\n    let a = t.id(5u8)\n    let b = t.id(true)\n    core.mem.drop(a)\n    core.mem.drop(b)\nend t.main\n"),
        "s1b_free",
    );
    let main: Vec<&String> = rows.iter().filter(|r| r.starts_with("t.main\t")).collect();
    assert!(main.iter().any(|r| r.ends_with("[u8]")), "{rows:?}");
    assert!(main.iter().any(|r| r.ends_with("[bool]")), "{rows:?}");
}

#[test]
fn s1b_type_args_method_and_resolved() {
    // メソッド (`Option.map`): impl の型引数が先、メソッド自身の型引数が後。決まらない `_` は残らない。
    let rows = s1b_dump(
        "fn t.main() -> () uses [] panics never pub\n    let o: Option<u8> = Some(3u8)\n    let m = o.map(fn(x: u8) -> u32 capture [] {\n        u32.from_u8(x)\n    })\n    core.mem.drop(m)\nend t.main\n",
        "s1b_method",
    );
    let row = rows.iter().find(|r| r.contains("u32")).unwrap_or_else(|| panic!("{rows:?}"));
    assert!(row.contains("u8") && !row.contains('_'), "{row}");
}

// --- S1A: resolved_fn ---

/// `check` の stderr から、`side <名前>` で始まる表の行だけを取り出す。
fn side(table: &str, src: &str, name: &str) -> Vec<String> {
    let t = Tmp::new(name, src);
    let o = run_with_env(&["check"], &t.0, &[("KSN_DUMP_SIDE", "1")]);
    let err = text(&o.stderr);
    let mut on = false;
    let mut rows = vec![];
    for l in err.lines() {
        if let Some(n) = l.strip_prefix("side ") {
            on = n == table;
            continue;
        }
        if on {
            rows.push(l.to_string());
        }
    }
    rows
}

#[test]
fn s1a_resolved_fn_kinds() {
    let src = "\
trait t.Speak pub
    fn say(self: &Self) -> i64 uses [] panics never
end t.Speak

enum t.Shape pub
    A(u32)
    B
end t.Shape

fn t.helper(x: u32) -> u32 uses [] panics never
    x
end t.helper

fn t.call_dyn(s: &dyn t.Speak) -> i64 uses [] panics never
    s.say()
end t.call_dyn

fn t.main() -> Result<(), std.io.Error> uses [] panics never
    let a = t.helper(1u32)
    let s = t.Shape.A(a)
    let f = fn(x: u32) -> u32 capture [] {
        x
    }
    let b = f(a)
    let n = a.wrapping_add(b)
    Ok(())
end t.main
";
    let rows = side("resolved_fn", src, "s1a_kinds");
    let has = |pat: &str| rows.iter().any(|r| r.contains(pat));
    assert!(has("t.main\t") && has("Def t.helper"), "{rows:?}");
    assert!(has("Variant t.Shape.A"), "{rows:?}");
    assert!(has("Local f"), "{rows:?}");
    assert!(has("Dyn t.Speak"), "{rows:?}");
    assert!(has("Def core.num.u32.wrapping_add") || has("wrapping_add"), "{rows:?}");
}


// --- S1C2: 変換の印の残り ---

const S1C2_SRC: &str = "trait t.H
    fn handle(self: &Self) -> u32 uses [] panics never
end t.H

type t.X
    v: u32
end t.X

impl t.H for t.X
fn t.X.handle(self: &t.X) -> u32 uses [] panics never impls t.H
    self.v
end t.X.handle

fn t.X.get(self: &t.X) -> u32 uses [] panics never
    self.v
end t.X.get

fn t.dd(r: &&t.X) -> u32 uses [] panics never
    r.get()
end t.dd

fn t.dm(r: &mut &t.X) -> u32 uses [] panics never
    r.get()
end t.dm

fn t.ret(a: &[u32; 2]) -> &[u32] uses [] panics never
    return a
end t.ret

fn t.tail(a: &[u32; 2]) -> &[u32] uses [] panics never
    a
end t.tail

fn t.main() -> () uses [] panics never pub
    let a = [1u32, 2u32]
    let s: &[u32] = &a
    let mut m: &[u32] = &a
    m = &a
    let x = t.X { v: 1u32 }
    let h: &dyn t.H = &x
    core.mem.drop(s)
    core.mem.drop(m)
    core.mem.drop(h)
end t.main
";

#[test]
fn s1c2_deref_and_other_positions() {
    let rows = dump("s1c2_a", S1C2_SRC, "coercions").join("\n");
    assert!(rows.contains("AutoDeref { n: 1 }"), "{}", rows);
    // `&mut &X` も 1 段外す。`Box<X>` の受け手はまだメソッドが引けない (検査が E2044 にする) ので印は付かない。
    assert!(rows.lines().any(|l| l.contains("\tt.dm\t") && l.contains("AutoDeref { n: 1 }")), "{}", rows);
    for f in ["t.ret", "t.tail", "t.main"] {
        assert!(rows.lines().any(|l| l.contains(&format!("\t{}\t", f)) && l.contains("ArrayToSlice")), "{} {}", f, rows);
    }
    // let / 代入 / dyn の 3 件 (main)
    let n = rows.lines().filter(|l| l.contains("\tt.main\t")).count();
    assert!(n >= 3, "{}", rows);
}

#[test]
fn s1c2_struct_field_and_variant_payload() {
    let src = "type t.W\n    s: &[u32]\nend t.W\n\nenum t.E\n    A(&[u32])\nend t.E\n\nfn t.main() -> () uses [] panics never pub\n    let a = [1u32, 2u32]\n    let w = t.W { s: &a }\n    let e = t.E.A(&a)\n    core.mem.drop(w)\n    core.mem.drop(e)\nend t.main\n";
    let rows = dump("s1c2_b", src, "coercions");
    assert!(rows.len() >= 2 && rows.iter().all(|r| r.contains("ArrayToSlice")), "{:?}", rows);
}

#[test]
fn s1c2_try_error_conversion() {
    let src = "enum t.E1\n    X\nend t.E1\n\nenum t.E2 derive [From]\n    A(t.E1)\nend t.E2\n\nfn t.f() -> Result<u32, t.E1> uses [] panics never\n    Err(t.E1.X)\nend t.f\n\nfn t.g() -> Result<u32, t.E2> uses [] panics never\n    let v = t.f()?\n    Ok(v)\nend t.g\n\nfn t.h() -> Result<u32, t.E1> uses [] panics never\n    let v = t.f()?\n    Ok(v)\nend t.h\n";
    let rows = dump("s1c2_c", src, "coercions");
    assert!(rows.iter().any(|r| r.contains("\tt.g\t") && r.contains("ErrInto") && r.contains("\"A\"")), "{:?}", rows);
    // 完全一致の `?` には印が付かない。
    assert!(!rows.iter().any(|r| r.contains("\tt.h\t")), "{:?}", rows);
}

#[test]
fn s1c2_unmarked_reasons_listed() {
    let src = "trait t.H\n    fn handle(self: &Self) -> u32 uses [] panics never\nend t.H\n\nfn t.d(h: &dyn t.H) -> u32 uses [] panics never\n    h.handle()\nend t.d\n";
    let out = dump_side("s1c2_d", src);
    assert!(out.lines().any(|l| l.starts_with("coerce_none\t") && l.contains("t.d")), "{}", out);
}

// --- G35: `Box<T>` の受け手のメソッド ---

const G35_SRC: &str = "type t.X
    v: u32
end t.X

fn t.X.bump(self: &mut t.X) -> () uses [] panics never
    self.v = 7u32
end t.X.bump

fn t.X.get(self: &t.X) -> u32 uses [] panics never
    self.v
end t.X.get

fn t.main() -> Result<(), alloc.alloc.AllocError> uses [alloc] panics may
    let mut b = alloc.boxed.Box.new(std.alloc.SYSTEM, t.X { v: 3u32 })?
    b.bump()
    let n = b.get()
    let inner = alloc.boxed.Box.new(std.alloc.SYSTEM, t.X { v: 5u32 })?
    let bb = alloc.boxed.Box.new(std.alloc.SYSTEM, inner)?
    let m = bb.get()
    core.check.eq(n, 7u32)
    core.check.eq(m, 5u32)
    core.mem.drop(b)
    core.mem.drop(bb)
    Ok(())
end t.main
";

#[test]
fn g35_box_receiver_autoderef_marked() {
    let rows = dump("g35_a", G35_SRC, "coercions");
    let all = rows.join("\n");
    assert!(all.contains("AutoDeref { n: 1 }"), "{}", all);
    assert!(all.contains("AutoDeref { n: 2 }"), "{}", all);
    let t = Tmp::new("g35_b", G35_SRC);
    let o = run(&["check"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
}

#[test]
fn g35_box_receiver_run_and_native_agree() {
    let t = Tmp::new("g35_c", G35_SRC);
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    let (_t, exe_path, out) = build_native("g35_d", G35_SRC, true);
    let p = exe_path.unwrap_or_else(|| panic!("no native: {}", out));
    let o = std::process::Command::new(&p).output().unwrap();
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
}

// ---- S3A: 型の領域 (設計 9 章)。表 `regions` は `borrow` / `sig` / `bind` / `cstr` の行 (列 1 が種類、列 2 が定義名、列 3 が式 ID)。

const S3A_SRC: &str = "type t.X
    v: u8
end t.X

fn t.X.g['a](self: &'a t.X, a: &u8) -> &'a u8 uses [] panics never
    core.mem.drop(a)
    &self.v
end t.X.g

fn t.X.h(self: &t.X, a: &u8) -> &u8 uses [] panics never
    core.mem.drop(a)
    &self.v
end t.X.h

fn t.one(x: &u8) -> &u8 uses [] panics never
    x
end t.one

fn t.st() -> &'static str uses [] panics never
    \"s\"
end t.st

fn t.main() -> () uses [] panics never
    let a = 1u8
    let r = t.one(&a)
    let s = t.st()
    core.mem.drop(r)
    core.mem.drop(s)
end t.main
";

const S3A_TWO: &str = "fn t.two(x: &u8, y: &u8) -> &u8 uses [] panics never
    core.mem.drop(y)
    x
end t.two
";

fn s3a_sig(rows: &[String], def: &str) -> String {
    rows.iter().find(|l| l.starts_with(&format!("sig\t{}\t", def))).cloned().unwrap_or_default()
}

#[test]
fn s3a_signature_regions_follow_elision() {
    let rows = dump("s3a_a", S3A_SRC, "sig");
    // 名前付き + 省略: `a` の省略した領域は 0 番目、戻り値は `'a`。
    assert!(s3a_sig(&rows, "t.X.g").contains("params self:['a] a:['e0] | ret ['a]"), "{:?}", rows);
    // `&self` があれば戻り値の省略はその領域 (省略した領域の 0 番目は self)。
    assert!(s3a_sig(&rows, "t.X.h").contains("params self:['e0] a:['e1] | ret ['e0]"), "{:?}", rows);
    // 入力の領域が 1 つならそれ。2 つなら決まらない (E3021 の領分で、ここは `'_` のまま)。
    assert!(s3a_sig(&rows, "t.one").contains("params x:['e0] | ret ['e0]"), "{:?}", rows);
    let rows2 = dump("s3a_a2", S3A_TWO, "sig");
    assert!(s3a_sig(&rows2, "t.two").contains("params x:['e0] y:['e1] | ret ['_]"), "{:?}", rows2);
    assert!(s3a_sig(&rows, "t.st").contains("ret ['static]"), "{:?}", rows);
}

#[test]
fn s3a_borrow_and_call_regions_are_collected() {
    let t = Tmp::new("s3a_b", S3A_SRC);
    let o = run_with_env(&["check"], &t.0, &[("KSN_DUMP_SIDE", "1")]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let err = text(&o.stderr);
    let rows: Vec<&str> = err.lines().collect();
    // `&a` は借用ごとの領域の変数。呼び出し `t.one(&a)` は「借用の領域 ⊒ 呼び出し先の `'e0` の実体」を残す。
    let borrow = rows.iter().find(|l| l.starts_with("borrow\tt.main\t")).expect("borrow row");
    let var = borrow.rsplit('\t').next().unwrap();
    assert!(var.starts_with("'?"), "{}", borrow);
    assert!(rows.iter().any(|l| l.starts_with("cstr\tt.main\t") && l.contains(&format!("{} : 'e0@", var)) && l.ends_with("(arg)")), "{:?}", rows);
}

#[test]
fn s3a_static_decided_by_type() {
    // G40 の `static_expr` は式の形で `'static` を追っていた。型に領域があれば、束縛の型だけで決まる。
    let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/check_ok/static_through_qmark_let_ok.ksnl")).unwrap();
    let rows = dump("s3a_c", &src, "bind");
    // `t.pick(0usize)?` で取り出した値: `Result<&'static str, _>` の `?` の結果は `&'static str`。
    assert!(rows.iter().any(|l| l.starts_with("bind\tt.main\t") && l.contains("r : &'static str static=true")), "{:?}", rows);
    // 対照: `&a` の借用や `&u8` を返す関数の結果は 'static ではない。
    let rows = dump("s3a_d", S3A_SRC, "bind");
    assert!(rows.iter().any(|l| l.contains("\tr : &") && l.ends_with("static=false")), "{:?}", rows);
    assert!(rows.iter().any(|l| l.contains("\ts : &'static str static=true")), "{:?}", rows);
}

#[test]
fn s3a_regions_do_not_change_type_equality() {
    // 領域が違っても型は同じ: `&'static str` を `&str` の引数・`&'a` と `&` の入り交じりが従来どおり通る。
    let t = Tmp::new("s3a_e", S3A_SRC);
    let o = run(&["check"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stdout));
}
