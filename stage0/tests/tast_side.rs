//! TAST の側の表 (`SideTabs`) の中身を `KSN_DUMP_SIDE=1` で確かめる。試験関数は担当の接頭辞 (`s1a_` など) を付ける。
mod common;
use common::*;

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

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
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
