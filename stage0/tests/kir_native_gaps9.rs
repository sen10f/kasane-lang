//! 2026-11-26 VM 一本化の線 6: VM が退避していた単発の理由を潰した形。
//! 木走査 (`--engine=tree`)・VM (`--engine=vm`、退避なし)・ネイティブ (debug/release) が同じ表示を出すことを確かめる。

mod common;

use common::{build_native_strict, run, text, Tmp};

fn check_gap(name: &str, src: &str, want: &str) {
    let t = Tmp::new(name, src);
    for eng in ["--engine=tree", "--engine=vm"] {
        let o = run(&["run", eng], &t.0);
        assert_eq!(o.status.code(), Some(0), "{}: {}{}", eng, text(&o.stderr), text(&o.stdout));
        assert!(!text(&o.stderr).contains("vm engine skipped"), "{} が退避した: {}", eng, text(&o.stderr));
        assert_eq!(text(&o.stdout), want, "{} の表示が想定と違う", eng);
    }
    for release in [false, true] {
        let (bt, exe, out) = build_native_strict(&format!("{}_{}", name, release), src, release);
        let p = exe.unwrap_or_else(|| panic!("ネイティブの実行ファイルが無い (release: {}): {}", release, out));
        let o = std::process::Command::new(&p).current_dir(&bt.0).output().unwrap();
        assert_eq!(o.status.code(), Some(0), "release {}: {}{}", release, text(&o.stdout), text(&o.stderr));
        assert_eq!(text(&o.stdout), want, "native (release: {}) と表示が違う", release);
    }
}

/// `with` で束縛した String の `clear` (種類の手がかりを `with` の初期化式の型から付ける)。
#[test]
fn with_string_clear() {
    check_gap(
        "ngaps9_with_clear",
        r#"fn t.go() -> usize uses [] panics never
    with mut s = std.string.String.new() close core.mem.drop(s) {
        let n = s.as_str().len()
        s.clear()
        let m = s.len()
        n.wrapping_add(m)
    }
end t.go

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics never
    if t.go() == 0usize {
        std.io.print_line("ok")?
    }
    Ok(())
end t.main
"#,
        "ok\n",
    );
}
