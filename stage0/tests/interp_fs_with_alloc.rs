//! `with` の中で `std.fs.read_to_end` した `Vec<u8>` へ、後から `push` する形 (check_ok の question_ok_fs_read_utf8_with)。
//! インタプリタ (`ksn run`) とネイティブ (`ksn build`) の出力が一致することを確かめる。
//! 以前はインタプリタだけ `read_to_end` が容量を更新せず、続く `push` が `AllocError { code: 1 }` になった。

mod common;

use common::*;
use std::process::Command as Cmd;

const SRC: &str = r#"enum t.Error derive [Debug, Clone, Copy, PartialEq, Eq, From] pub
    Io(std.io.Error)
    Alloc(alloc.alloc.AllocError)
    Utf8(core.str.Utf8Error)
end t.Error

fn t.main() -> Result<(), t.Error> uses [alloc, io.console, io.fs] panics may
    with mut f = std.fs.create("PATH")? close core.mem.drop(f.close()) {
        core.mem.drop(f.write("hello".as_bytes())?)
    }
    let mut buf: std.vec.Vec<u8> = std.vec.Vec.new()
    with mut f = std.fs.open("PATH")? close core.mem.drop(f.close()) {
        core.mem.drop(std.fs.read_to_end(&mut f, &mut buf)?)
        match core.str.from_utf8(buf.as_slice()) {
            Ok(s) => core.check.eq(s, "hello")
            Err(e) => return Err(t.Error.Utf8(e))
        }
    }
    core.mem.drop(buf.push(33u8)?)
    core.check.eq(buf.len(), 6usize)
    match std.io.print_line("fs read utf8 with ok") {
        Ok(_done) => {
        }
        Err(error) => return Err(t.Error.Io(error))
    }
    core.mem.drop(buf)
    Ok(())
end t.main
"#;

const EXPECT: &str = "fs read utf8 with ok\n";

#[test]
fn fs_read_to_end_then_push_interp_and_native_agree() {
    let path = std::env::temp_dir().join(format!("ksn_interpalloc_{}.dat", std::process::id()));
    let p = path.to_string_lossy().replace('\\', "/");
    let src = SRC.replace("PATH", &p);
    let _ = std::fs::remove_file(&path);

    // インタプリタ
    let t = Tmp::new("interpalloc_run", &src);
    let o = run(&["run"], &t.0);
    let interp_out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "interp stdout: {} stderr: {}", interp_out, text(&o.stderr));
    assert_eq!(interp_out, EXPECT);

    // ネイティブ
    let (nt, native, out) = build_native_strict("interpalloc_native", &src, false);
    match native {
        Some(exe) => {
            let n = Cmd::new(&exe).current_dir(&nt.0).output().unwrap();
            assert_eq!(n.status.code(), Some(0), "native stderr: {}", text(&n.stderr));
            assert_eq!(text(&n.stdout), interp_out, "インタプリタとネイティブの出力が違う");
        }
        None => assert_native_or_reasoned_skip(&out),
    }
    let _ = std::fs::remove_file(&path);
}
