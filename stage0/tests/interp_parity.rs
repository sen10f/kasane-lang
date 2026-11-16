//! インタプリタをネイティブの挙動に合わせる試験 (設計_インタプリタ一致.md)。
//! 段 1: `core.mem.drop` した所有する値を、借用越しに読んだら panic (終了コード 101) になる。
//! 検査器の穴 (借用検査は通ってしまう形) は、隠しの `KSN_SKIP_BORROW_DIAGS=1` で E30xx を外して走らせる。

#![allow(dead_code, unused_imports)]

mod common;

use common::*;

fn run_skipping_borrowck(name: &str, src: &str) -> (Option<i32>, String, String) {
    let t = Tmp::new(name, src);
    let o = run_with_env(&["run"], &t.0, &[("KSN_SKIP_BORROW_DIAGS", "1")]);
    (o.status.code(), text(&o.stdout), text(&o.stderr))
}

fn assert_dead_read(name: &str, src: &str) {
    let (code, out, err) = run_skipping_borrowck(name, src);
    assert_eq!(code, Some(101), "stdout: {} stderr: {}", out, err);
    assert!(err.contains("use after free"), "{}", err);
}

const B1: &str = r#"type t.Named
    name: alloc.string.String<std.alloc.System>
end t.Named

fn t.Named.pick(self: &t.Named, other: &str) -> &str uses [] panics never
    if self.name.is_empty() {
        return other
    }
    self.name.as_str()
end t.Named.pick

fn t.hello() -> alloc.string.String<std.alloc.System> uses [alloc] panics never
    match alloc.string.String.from_str(std.alloc.SYSTEM, "hello world, this is a heap string") {
        Ok(s) => s
        Err(_e) => alloc.string.String.new(std.alloc.SYSTEM)
    }
end t.hello

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics never
    let p = t.Named { name: alloc.string.String.new(std.alloc.SYSTEM) }
    let other = t.hello()
    let r = p.pick(other.as_str())
    core.mem.drop(other)
    std.io.print_line(r)
end t.main
"#;

#[test]
fn dead_read_panics_b1() {
    // B1: self を持つ関数が別の引数の借用を返す
    assert_dead_read("par_b1", B1);
}

const B4: &str = r#"fn t.hello() -> alloc.string.String<std.alloc.System> uses [alloc] panics never
    match alloc.string.String.from_str(std.alloc.SYSTEM, "hello world, this is a heap string") {
        Ok(s) => s
        Err(_e) => alloc.string.String.new(std.alloc.SYSTEM)
    }
end t.hello

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console, thread] panics never
    let s = t.hello()
    match std.sync.channel(std.alloc.SYSTEM, 2usize) {
        Ok((tx, rx)) => {
            core.mem.drop(tx.send(&s))
            core.mem.drop(s)
            match rx.recv() {
                Ok(x) => std.io.print_line(x.as_str())?
                Err(_e) => std.io.print_line("closed")?
            }
        }
        Err(_e) => std.io.print_line("alloc")?
    }
    Ok(())
end t.main
"#;

#[test]
fn dead_read_panics_b4() {
    // B4: チャネルで局所の借用を送る
    assert_dead_read("par_b4", B4);
}

const H1: &str = r#"type t.Res linear
    data: alloc.vec.Vec<u64, std.alloc.System>
end t.Res

fn t.Res.shut(self: t.Res) -> () uses [] panics never
    let t.Res { data: data } = self
    core.mem.drop(data)
end t.Res.shut

fn t.mk() -> t.Res uses [alloc] panics never
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    core.mem.drop(v.push(41u64))
    t.Res { data: v }
end t.mk

fn t.wrap(x: &t.Res) -> &t.Res uses [] panics never
    x
end t.wrap

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let p = with r = t.mk() close r.shut() {
        t.wrap(&r)
    }
    match p.data.get(0usize) {
        Some(x) => core.check.eq(*x, 41u64)
        None => std.io.print_line("none")?
    }
    std.io.print_line("done")
end t.main
"#;

#[test]
fn dead_read_panics_h1() {
    // round3 穴 1: with の本体の値が束縛の借用を外へ出す
    assert_dead_read("par_h1", H1);
}

const H3: &str = r#"type t.P['a, 'b]
    x: &'a alloc.string.String<std.alloc.System>
    y: Option<&'b alloc.string.String<std.alloc.System>>
end t.P

fn t.f4(p: &mut t.P) -> () uses [] panics never
    match p.y {
        Some(v) => {
            p.x = v
        }
        None => ()
    }
end t.f4

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let a = alloc.string.String.new(std.alloc.SYSTEM)
    let mut p = t.P { x: &a, y: None }
    let s = match alloc.string.String.from_str(std.alloc.SYSTEM, "hello") {
        Ok(s) => s
        Err(_e) => return Ok(())
    }
    p.y = Some(&s)
    t.f4(&mut p)
    p.y = None
    core.mem.drop(s)
    let xr = p.x
    if xr.as_str() == "hello" {
        std.io.print_line(xr.as_str())?
    }
    std.io.print_line("done")
end t.main
"#;

#[test]
fn dead_read_panics_h3() {
    // round3 穴 3: 省略したライフタイムの欄へ別の領域の借用を入れる
    assert_dead_read("par_h3", H3);
}

#[test]
fn dead_marks_do_not_touch_live_values() {
    // 正しいプログラム: drop したあとに同じ値を読まない。複製・別の String・Vec の drop は、他の値を巻き込まない。
    let src = r#"fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let a = match alloc.string.String.from_str(std.alloc.SYSTEM, "abc") {
        Ok(s) => s
        Err(_e) => return Ok(())
    }
    let b = match alloc.string.String.from_str(std.alloc.SYSTEM, "abc") {
        Ok(s) => s
        Err(_e) => return Ok(())
    }
    let r = a.as_str()
    std.io.print_line(r)?
    core.mem.drop(a)
    std.io.print_line(b.as_str())?
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    core.mem.drop(v.push(1u32))
    core.mem.drop(v)
    std.io.print_line("ok")
end t.main
"#;
    let t = Tmp::new("par_live", src);
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    assert_eq!(text(&o.stdout), "abc\nabc\nok\n");
}
