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
