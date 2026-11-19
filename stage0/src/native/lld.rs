//! 組み込みの lld (L3、feature `embedded-llvm`)。ELF (x86_64 / aarch64 の linux-gnu) と wasm のリンクを、外部の `ld` / `wasm-ld` なしで行う。
//! 実行時に ホスト (aarch64 は sysroot) から crt と libgcc を探し、`clang -###` が ld に渡すのと同じ引数を組む。
//! `KSN_LINKER=external` なら使わない (呼び出し側が従来の clang / wasm-ld の経路へ戻す)。

use std::path::{Path, PathBuf};

/// リンクに組み込みの lld を使うか。feature が無い、または `KSN_LINKER=external` なら `false`。
pub fn enabled() -> bool {
    cfg!(feature = "embedded-llvm") && std::env::var("KSN_LINKER").map_or(true, |v| v != "external")
}

#[cfg(feature = "embedded-llvm")]
extern "C" {
    fn ksn_lld_link(flavor: i32, argc: i32, argv: *const *const std::os::raw::c_char, out: *mut *mut std::os::raw::c_char, err: *mut *mut std::os::raw::c_char) -> i32;
    fn free(p: *mut std::os::raw::c_void);
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Flavor {
    Elf,
    Wasm,
    /// COFF (MinGW-w64 の `x86_64-windows-gnu`)。
    MinGw,
}

/// lld を同じプロセスで走らせる。`args` に `ld.lld` / `wasm-ld` の名前 (argv[0]) は含めない。失敗なら lld の標準エラーを返す。
/// lld はプロセスの大域状態を使うので、同時に 1 本だけ (内部で排他する)。
#[cfg(feature = "embedded-llvm")]
pub fn lld_link(flavor: Flavor, args: &[String]) -> Result<(), String> {
    use std::ffi::CString;
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let name = match flavor { Flavor::Wasm => "wasm-ld", Flavor::MinGw => "ld.lld", Flavor::Elf => "ld.lld" };
    let mut cs = vec![CString::new(name).unwrap()];
    for a in args {
        cs.push(CString::new(a.as_str()).map_err(|_| format!("リンクの引数に NUL がある: {:?}", a))?);
    }
    let ptrs: Vec<*const std::os::raw::c_char> = cs.iter().map(|c| c.as_ptr()).collect();
    let (mut o, mut e) = (std::ptr::null_mut(), std::ptr::null_mut());
    let rc = unsafe { ksn_lld_link(match flavor { Flavor::Elf => 0, Flavor::Wasm => 1, Flavor::MinGw => 2 }, ptrs.len() as i32, ptrs.as_ptr(), &mut o, &mut e) };
    let take = |p: *mut std::os::raw::c_char| -> String {
        if p.is_null() {
            return String::new();
        }
        let s = unsafe { std::ffi::CStr::from_ptr(p) }.to_string_lossy().into_owned();
        unsafe { free(p as *mut _) };
        s
    };
    let (so, se) = (take(o), take(e));
    if rc == 0 {
        Ok(())
    } else {
        Err(format!("{} が失敗した (終了コード {}): {}{}", name, rc, so.trim(), se.trim()))
    }
}

#[cfg(not(feature = "embedded-llvm"))]
pub fn lld_link(_flavor: Flavor, _args: &[String]) -> Result<(), String> {
    Err("組み込みの lld は feature `embedded-llvm` でビルドしたときだけ使える".into())
}

fn first_existing(dirs: &[PathBuf], file: &str) -> Option<PathBuf> {
    dirs.iter().map(|d| d.join(file)).find(|p| p.exists())
}

/// `<base>/<版>` のうち版が最も新しく `file` を持つもの。
fn newest_gcc_dir(base: &Path, file: &str) -> Option<PathBuf> {
    let mut best: Option<(Vec<u32>, PathBuf)> = None;
    for e in std::fs::read_dir(base).ok()?.flatten() {
        let p = e.path();
        let ver: Option<Vec<u32>> = e.file_name().to_string_lossy().split('.').map(|x| x.parse().ok()).collect();
        if let Some(v) = ver.filter(|_| p.join(file).exists()) {
            if best.as_ref().is_none_or(|(b, _)| v > *b) {
                best = Some((v, p));
            }
        }
    }
    best.map(|(_, p)| p)
}

/// ELF の実行ファイルをつなぐ lld の引数。`objs` は目的ファイル、`name` は `x86_64-linux-gnu` / `aarch64-linux-gnu`、`root` はクロスのときの sysroot。
/// PIE と動的リンクの glibc (実行時の `dlopen` を保つ)。crt・libgcc が見つからなければ、入れるべき package を示して失敗する。
pub fn elf_args(name: &str, root: Option<&str>, objs: &[PathBuf], exe: &Path) -> Result<Vec<String>, String> {
    let (emul, dynld, deb, gccbase, host) = match name {
        "x86_64-linux-gnu" => ("elf_x86_64", "/lib64/ld-linux-x86-64.so.2", "x86_64-linux-gnu", "/usr/lib/gcc/x86_64-linux-gnu", root.is_none()),
        "aarch64-linux-gnu" => ("aarch64linux", "/lib/ld-linux-aarch64.so.1", "aarch64-linux-gnu", if root.is_none() { "/usr/lib/gcc/aarch64-linux-gnu" } else { "/usr/lib/gcc-cross/aarch64-linux-gnu" }, root.is_none()),
        _ => return Err(format!("組み込みの lld は {} を知らない", name)),
    };
    let (libdirs, hint): (Vec<PathBuf>, &str) = if host {
        ([format!("/usr/lib/{}", deb), format!("/lib/{}", deb), "/usr/lib64".into(), "/usr/lib".into()].iter().map(PathBuf::from).collect(), "libc6-dev")
    } else {
        let r = PathBuf::from(root.unwrap_or("/usr/aarch64-linux-gnu"));
        (vec![r.join("lib"), PathBuf::from(format!("/usr/lib/{}", deb))], "libc6-dev-arm64-cross")
    };
    let need = |f: &str, dirs: &[PathBuf], hint: &str| first_existing(dirs, f).ok_or_else(|| format!("{} が見つからない ({}): {} を入れる", f, dirs.iter().map(|d| d.display().to_string()).collect::<Vec<_>>().join(", "), hint));
    let crt1 = need("Scrt1.o", &libdirs, hint)?;
    let crti = need("crti.o", &libdirs, hint)?;
    let crtn = need("crtn.o", &libdirs, hint)?;
    let gcc = newest_gcc_dir(Path::new(gccbase), "crtbeginS.o").ok_or_else(|| format!("{}/<版>/crtbeginS.o が見つからない: {} を入れる", gccbase, if host { "gcc (libgcc-*-dev)" } else { "gcc-aarch64-linux-gnu" }))?;
    let mut a: Vec<String> = ["-z", "relro", "--hash-style=gnu", "--build-id", "--eh-frame-hdr", "-m", emul, "-pie", "-dynamic-linker", dynld].iter().map(|s| s.to_string()).collect();
    a.push("-o".into());
    a.push(exe.display().to_string());
    a.push(crt1.display().to_string());
    a.push(crti.display().to_string());
    a.push(gcc.join("crtbeginS.o").display().to_string());
    for d in libdirs.iter().chain([&gcc]) {
        if d.exists() {
            a.push(format!("-L{}", d.display()));
        }
    }
    for o in objs {
        a.push(o.display().to_string());
    }
    for s in ["-lm", "-lgcc", "--as-needed", "-lgcc_s", "--no-as-needed", "-lpthread", "-lc", "-lgcc", "--as-needed", "-lgcc_s", "--no-as-needed"] {
        a.push(s.into());
    }
    a.push(gcc.join("crtendS.o").display().to_string());
    a.push(crtn.display().to_string());
    Ok(a)
}

/// Windows (MinGW-w64) の実行ファイルをつなぐ lld の引数。`root` は mingw-w64 の sysroot (crt・import ライブラリ。既定 /usr/x86_64-w64-mingw32)。
/// libgcc 等は `/usr/lib/gcc/x86_64-w64-mingw32/<版>-posix` (winpthreads を使う posix の構成) から探す。無ければ入れるべき package を示して失敗する。
pub fn mingw_args(root: Option<&str>, objs: &[PathBuf], exe: &Path) -> Result<Vec<String>, String> {
    let r = PathBuf::from(root.unwrap_or("/usr/x86_64-w64-mingw32"));
    let libdir = r.join("lib");
    let hint = "mingw-w64 (gcc-mingw-w64-x86-64-posix と mingw-w64-x86-64-dev) を入れる";
    let crt2 = libdir.join("crt2.o");
    if !crt2.exists() {
        return Err(format!("{} が見つからない: {}", crt2.display(), hint));
    }
    // `13-posix` のような名前のうち版が最も新しく crtbegin.o を持つもの。
    let gccbase = Path::new("/usr/lib/gcc/x86_64-w64-mingw32");
    let mut best: Option<(Vec<u32>, PathBuf)> = None;
    for e in std::fs::read_dir(gccbase).ok().into_iter().flatten().flatten() {
        let n = e.file_name().to_string_lossy().to_string();
        let Some(v) = n.strip_suffix("-posix") else { continue };
        let ver: Option<Vec<u32>> = v.split('.').map(|x| x.parse().ok()).collect();
        if let Some(ver) = ver.filter(|_| e.path().join("crtbegin.o").exists()) {
            if best.as_ref().is_none_or(|(b, _)| ver > *b) {
                best = Some((ver, e.path()));
            }
        }
    }
    let gcc = best.map(|(_, p)| p).ok_or_else(|| format!("{}/<版>-posix/crtbegin.o が見つからない: {}", gccbase.display(), hint))?;
    let mut a: Vec<String> = ["-m", "i386pep", "--enable-auto-image-base"].iter().map(|s| s.to_string()).collect();
    a.push("-o".into());
    a.push(exe.display().to_string());
    a.push(crt2.display().to_string());
    a.push(gcc.join("crtbegin.o").display().to_string());
    a.push(format!("-L{}", libdir.display()));
    a.push(format!("-L{}", gcc.display()));
    for o in objs {
        a.push(o.display().to_string());
    }
    // winpthreads は静的に取り込む (実行ファイルが libwinpthread-1.dll に依存しない)。
    let pth = libdir.join("libwinpthread.a");
    if !pth.exists() {
        return Err(format!("{} が見つからない: {}", pth.display(), hint));
    }
    for s in ["-lmingw32", "-lgcc", "-lgcc_eh", "@PTHREAD@", "-lmoldname", "-lmingwex", "-lmsvcrt", "-lws2_32", "-ladvapi32", "-lshell32", "-luser32", "-lkernel32", "-lmingw32", "-lgcc", "-lgcc_eh", "@PTHREAD@", "-lmoldname", "-lmingwex", "-lmsvcrt"] {
        a.push(if s == "@PTHREAD@" { pth.display().to_string() } else { s.into() });
    }
    a.push(gcc.join("crtend.o").display().to_string());
    Ok(a)
}
