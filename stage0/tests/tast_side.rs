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
    let src = "fn t.main() -> i32\n    let p: (i32, i32) = (1, 2)\n    let (a, b) = &p\n    let mut q: (i32, i32) = (3, 4)\n    let (c, d) = &mut q\n    let (e, f) = p\n    0\nend t.main\n";
    let out = dump_side("s1d_modes", src);
    assert!(out.lines().any(|l| l == "# pat_tys"), "{out}");
    assert!(out.contains("PBind a : &i32 Ref"), "{out}");
    assert!(out.contains("PBind c : &mut i32 RefMut"), "{out}");
    assert!(out.contains("PBind e : i32 Move"), "{out}");
    assert!(out.contains("let_tys_mismatch=0"), "{out}");
}

#[test]
fn s1d_pat_tys_nested_shared_wins() {
    let src = "fn t.main() -> i32\n    let mut o: Option<(i32, i32)> = Some((1, 2))\n    let r = &mut o\n    match &r {\n        Some((x, y)) => 0\n        None => 1\n    }\nend t.main\n";
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

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
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
