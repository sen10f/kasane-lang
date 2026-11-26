//! M3 (設計_VM一本化.md): VM が木走査へ退避していた形を、VM で実際に走るようにした (退避しない) ことの確認。理由ごとに 1 本。

mod common;
use common::*;

/// `src` を `ksn run --engine=vm` で走らせ、退避せずに成功し、標準出力が `want` であることを確かめる。
fn vm_runs(name: &str, src: &str, want: &str) {
    let t = Tmp::new(name, src);
    let o = run(&["run", "--engine=vm"], &t.0);
    let (out, err) = (text(&o.stdout), text(&o.stderr));
    assert!(!err.contains("vm engine skipped"), "VM で走るはず: {}", err);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out, err);
    assert_eq!(out, want, "{}", err);
}

#[test]
fn for_tuple_pattern_over_an_array() {
    vm_runs(
        "vmc_for_tuple",
        r#"fn t.main() -> () uses [io.console] panics may
    let v = [(1u32, 2u32), (3u32, 4u32)]
    let mut s = 0u32
    for (a, b) in v {
        s = s.wrapping_add(a.wrapping_mul(b))
    }
    core.check.eq(s, 14u32)
    core.mem.drop(std.io.print_line("ok"))
end t.main
"#,
        "ok\n",
    );
}

#[test]
fn option_array_with_const_length() {
    vm_runs(
        "vmc_opt_array",
        r#"const t.N: usize = 3usize

fn t.main() -> () uses [io.console] panics may
    let d: [Option<u32>; t.N] = [None; t.N]
    let e = [Some(1u32), None]
    let mut n = 0usize
    for x in e {
        if let Some(v) = x {
            n = n.wrapping_add(usize.from_u32(v))
        }
    }
    core.check.eq(d.len(), 3usize)
    core.check.eq(n, 1usize)
    core.mem.drop(std.io.print_line("ok"))
end t.main
"#,
        "ok\n",
    );
}
