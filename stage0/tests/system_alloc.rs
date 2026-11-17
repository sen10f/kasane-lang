//! `std.alloc.System.{alloc, dealloc, realloc}` を直接呼ぶ。`ksn run` (木走査) とネイティブ (LLVM) の出力が一致すること (G10)。

mod common;
use common::*;
use std::fs;
use std::process::Command;

const SRC: &str = "fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let lay = alloc.alloc.Layout { size: 16usize, align: 8usize }
    let p = match std.alloc.SYSTEM.alloc(lay) {
        Ok(p) => p
        Err(_) => core.panic.panic(\"alloc\")
    }
    let q = unsafe \"ptr\" \"write and read the fresh block\" {
        core.ptr.write::<u8>(p, 7u8)
        core.ptr.write::<u8>(core.ptr.add::<u8>(p, 15usize), 9u8)
        core.ptr.add::<u8>(p, 0usize)
    }
    let r = match std.alloc.SYSTEM.realloc(q, lay, 64usize) {
        Ok(r) => r
        Err(_) => core.panic.panic(\"realloc\")
    }
    let a = unsafe \"ptr\" \"read after realloc\" {
        core.ptr.read::<u8>(r)
    }
    let b = unsafe \"ptr\" \"read after realloc\" {
        core.ptr.read::<u8>(core.ptr.add::<u8>(r, 15usize))
    }
    std.alloc.SYSTEM.dealloc(r, alloc.alloc.Layout { size: 64usize, align: 8usize })
    // 大きさ 0 は 1 バイトとして確保する (木走査もネイティブも。`malloc(0)` の NULL を `Err` にしない)。
    let zl = alloc.alloc.Layout { size: 0usize, align: 1usize }
    let z = match std.alloc.SYSTEM.alloc(zl) {
        Ok(z) => z
        Err(_) => core.panic.panic(\"zero alloc\")
    }
    let z2 = match std.alloc.SYSTEM.realloc(z, zl, 0usize) {
        Ok(z2) => z2
        Err(_) => core.panic.panic(\"zero realloc\")
    }
    let zv = unsafe \"ptr\" \"write and read the one-byte block\" {
        core.ptr.write::<u8>(z2, 5u8)
        core.ptr.read::<u8>(z2)
    }
    std.alloc.SYSTEM.dealloc(z2, zl)
    core.check.eq(zv, 5u8)
    if a == 7u8 && b == 9u8 {
        std.io.print_line(\"kept\")?
    } else {
        std.io.print_line(\"lost\")?
    }
    Ok(())
end t.main
";

#[test]
fn system_alloc_realloc_dealloc_agree_between_interpreter_and_native() {
    let t = Tmp::new("g10_sysalloc", SRC);
    let toml = fs::read_to_string(t.0.join("kasane.toml")).unwrap();
    fs::write(t.0.join("kasane.toml"), format!("{}\n[unsafe]\nmodules = [\"t\"]\n", toml)).unwrap();
    let r = run(&["run"], &t.0);
    assert_eq!(text(&r.stdout), "kept\n", "{}{}", text(&r.stdout), text(&r.stderr));
    assert_eq!(r.status.code(), Some(0));
    let b = run(&["build", "--backend", "llvm"], &t.0);
    let bo = text(&b.stdout);
    assert_eq!(b.status.code(), Some(0), "{}{}", bo, text(&b.stderr));
    if !clang_available() {
        return;
    }
    assert!(!bo.contains("backend skipped"), "{}", bo);
    let exe_path = t.0.join("target").join("debug").join(if cfg!(windows) { "t.exe" } else { "t.native" });
    let n = Command::new(&exe_path).current_dir(&t.0).output().expect("native");
    same_output(&r, &n);
}
