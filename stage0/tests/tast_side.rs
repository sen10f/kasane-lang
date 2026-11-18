//! 側の表 (`SideTabs`) を埋める担当ごとの試験。関数名は担当の接頭辞 (`s1a_` `s1b_` ...) で始める。
//! 表は `KSN_DUMP_SIDE=1 ksn check` が stderr に出す (表ごとに、1 行目が `# 表の名前`)。

mod common;
use common::*;

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

fn dump(src: &str, name: &str) -> Vec<String> {
    let t = Tmp::new(name, src);
    let o = run_with_env(&["check"], &t.0, &[("KSN_DUMP_SIDE", "1")]);
    table(&text(&o.stderr), "type_args")
}

const GEN: &str = "fn t.id[T](x: T) -> T uses [] panics never\n    x\nend t.id\n\n";

#[test]
fn s1b_type_args_free_fn() {
    let rows = dump(
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
    let rows = dump(
        "fn t.main() -> () uses [] panics never pub\n    let o: Option<u8> = Some(3u8)\n    let m = o.map(fn(x: u8) -> u32 capture [] {\n        u32.from_u8(x)\n    })\n    core.mem.drop(m)\nend t.main\n",
        "s1b_method",
    );
    let row = rows.iter().find(|r| r.contains("u32")).unwrap_or_else(|| panic!("{rows:?}"));
    assert!(row.contains("u8") && !row.contains('_'), "{row}");
}
