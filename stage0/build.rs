//! ksn の実行体の識別子 `KSN_BUILD_ID` を作る (`check_cache` の鍵に使う。設計_増分検査.md 9 章)。
//!
//! 実行時に実行体のバイト列を読んで SHA-256 にすると、15 MB で約 50 ms かかり、`ksn check` の
//! 固定費になる。代わりに、実行体へ埋め込まれるものすべてのハッシュをビルド時に作る:
//! - `src/` 全体 (`include_str!` で埋め込む `.c` を含む) と `../spec/grammar.ebnf` (`grammar.rs` が埋め込む)
//! - `Cargo.toml` / `Cargo.lock` / この build.rs、パッケージの版
//! - ツールチェーン (`rustc -vV`)、`RUSTFLAGS`、プロファイル・最適化・ターゲット、`KSN_VM_PROF`
//!
//! 同じ識別子なら同じ版・同じ検査の実装。ソースが 1 バイトでも変われば識別子が変わり、
//! 古い実行体のエントリは当たらない。
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else {
            out.push(p);
        }
    }
}

fn part(h: &mut Sha256, name: &str, bytes: &[u8]) {
    h.update((name.len() as u64).to_le_bytes());
    h.update(name.as_bytes());
    h.update((bytes.len() as u64).to_le_bytes());
    h.update(bytes);
}

/// llvm-config の場所。`LLVM_CONFIG` > `KSN_LLVM_PREFIX/bin/llvm-config` (`scripts/build_llvm.sh` の自前の LLVM) > distro の LLVM 20。
fn llvm_config_path() -> String {
    println!("cargo:rerun-if-env-changed=LLVM_CONFIG");
    println!("cargo:rerun-if-env-changed=KSN_LLVM_PREFIX");
    if let Some(c) = std::env::var("LLVM_CONFIG").ok().filter(|s| !s.is_empty()) {
        return c;
    }
    match std::env::var("KSN_LLVM_PREFIX").ok().filter(|s| !s.is_empty()) {
        Some(p) => format!("{}/bin/llvm-config", p),
        None => "/usr/lib/llvm-20/bin/llvm-config".to_string(),
    }
}

/// `embedded-llvm`: LLVM 20 の静的ライブラリのうち X86 / AArch64 / WebAssembly だけをつなぐ (llvm-sys は `no-llvm-linking`)。
/// `llvm-config` が無いクロスビルドでは `KSN_LLVM_PREFIX/lib` の `libLLVM*.a` をすべて静的リンクする。
fn link_llvm() {
    let cfg = llvm_config_path();
    let prefix = std::env::var("KSN_LLVM_PREFIX").ok().filter(|s| !s.is_empty());
    if Path::new(&cfg).is_file() {
        let run = |a: &[&str]| -> String {
            let o = Command::new(&cfg).args(a).output().unwrap_or_else(|e| panic!("llvm-config ({}) を起動できない: {}", cfg, e));
            assert!(o.status.success(), "llvm-config {:?} が失敗した", a);
            String::from_utf8_lossy(&o.stdout).to_string()
        };
        println!("cargo:rerun-if-env-changed=LLVM_CONFIG");
        println!("cargo:rustc-link-search=native={}", run(&["--libdir"]).trim());
        for l in run(&["--link-static", "--libs", "x86", "aarch64", "webassembly", "passes", "irreader", "target"]).split_whitespace() {
            println!("cargo:rustc-link-lib=static={}", l.trim_start_matches("-l"));
        }
        for l in run(&["--link-static", "--system-libs"]).split_whitespace() {
            let n = l.trim_start_matches("-l");
            // 静的に取り込む (配布する実行体が libz / libzstd に依存しない)。xml2 は Windows の manifest 用で要らない。
            match n {
                "z" | "zstd" => println!("cargo:rustc-link-lib=static={}", n),
                "xml2" => {}
                _ => println!("cargo:rustc-link-lib={}", n),
            }
        }
    } else {
        // llvm-config なし: KSN_LLVM_PREFIX/lib の libLLVM*.a をすべてリンク。
        // rustc は cargo:rustc-link-arg と cargo:rustc-link-lib の出力順を入れ替えるため
        // --start-group/--end-group が機能しない。代わりに全オブジェクトを 1 つの
        // アーカイブにまとめてリンクする (単一アーカイブなら循環参照の順序問題が起きない)。
        let libdir = prefix.map_or_else(String::new, |p| format!("{}/lib", p));
        if Path::new(&libdir).is_dir() {
            let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
            let all_dir = out.join("llvm_all_objects");
            let _ = fs::remove_dir_all(&all_dir);
            fs::create_dir_all(&all_dir).unwrap();
            let mut objs: Vec<PathBuf> = Vec::new();
            if let Ok(rd) = fs::read_dir(&libdir) {
                for e in rd.flatten() {
                    let p = e.path();
                    if p.is_file() {
                        if let Some(n) = p.file_name().and_then(|s| s.to_str()) {
                            if n.starts_with("libLLVM") && n.ends_with(".a") {
                                let sub = all_dir.join(n.trim_end_matches(".a"));
                                let _ = fs::create_dir_all(&sub);
                                assert!(
                                    Command::new("ar").arg("x").arg(&p).current_dir(&sub).status().expect("ar が要る").success(),
                                    "{} の展開に失敗",
                                    p.display()
                                );
                                if let Ok(ord) = fs::read_dir(&sub) {
                                    for oe in ord.flatten() {
                                        let op = oe.path();
                                        if op.extension().and_then(|s| s.to_str()) == Some("o") {
                                            objs.push(op);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            assert!(!objs.is_empty(), "libLLVM*.a が {} に見つからない", libdir);
            let a = out.join("libksn_llvm_all.a");
            let _ = fs::remove_file(&a);
            assert!(
                Command::new("ar").arg("crs").arg(&a).args(&objs).status().expect("ar が要る").success(),
                "{} の作成に失敗",
                a.display()
            );
            println!("cargo:rustc-link-search=native={}", out.display());
            println!("cargo:rustc-link-lib=static=ksn_llvm_all");
            // クロス LLVM (AArch64 のみ) には X86 / WebAssembly のターゲット初期化シンボルが無い。
            // llvm_stubs.c の弱いスタブをリンクし、embed_llvm::init() の未定義シンボルを埋める
            // (ホストのフル LLVM では強いシンボルが勝ち、クロスではスタブが使われる)。
            println!("cargo:rerun-if-changed=src/native/llvm_stubs.c");
            let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
            let stubs_obj = out.join("llvm_stubs.o");
            let st = Command::new(&cc)
                .args(["-c", "src/native/llvm_stubs.c", "-o"])
                .arg(&stubs_obj)
                .status()
                .expect("cc が要る");
            assert!(st.success(), "llvm_stubs.c のコンパイルに失敗");
            let stubs_a = out.join("libksn_llvm_stubs.a");
            let _ = fs::remove_file(&stubs_a);
            assert!(
                Command::new("ar")
                    .arg("crs")
                    .arg(&stubs_a)
                    .arg(&stubs_obj)
                    .status()
                    .expect("ar が要る")
                    .success(),
                "{} の作成に失敗",
                stubs_a.display()
            );
            println!("cargo:rustc-link-lib=static=ksn_llvm_stubs");
        }
    }
    for d in ["/usr/lib/x86_64-linux-gnu", "/usr/lib/aarch64-linux-gnu", "/usr/lib64"] {
        if Path::new(d).is_dir() {
            println!("cargo:rustc-link-search=native={}", d);
        }
    }
    println!("cargo:rustc-link-lib=static=stdc++");
}

/// `libdir` 内の `prefix` で始まる `.a` 静的ライブラリをすべてリンクする。
fn link_glob_static(libdir: &str, prefixes: &[&str]) {
    let mut names: Vec<String> = Vec::new();
    if let Ok(rd) = fs::read_dir(libdir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_file() {
                if let Some(n) = p.file_name().and_then(|n| n.to_str()) {
                    if n.ends_with(".a") && prefixes.iter().any(|pre| n.starts_with(pre)) {
                        names.push(n.strip_prefix("lib").unwrap().strip_suffix(".a").unwrap().to_string());
                    }
                }
            }
        }
    }
    names.sort();
    names.dedup();
    for name in names {
        println!("cargo:rustc-link-lib=static={}", name);
    }
}

// ---- L2: 実行時の C のビットコードと標準ライブラリの宣言を実行体に埋め込む ----

/// `OUT_DIR/assets.rs` を作る。`rt_bc(対象, 部品)` (実行時の C のビットコード、対象の sysroot が無ければ `None`) と `STDLIB`
/// (`spec/stdlib` の全ファイルを `[パスの長さ u32][パス][内容の長さ u64][内容]` で並べたもの)。
/// ビットコードはビルドする機械の clang で作る (`KSN_BUILD_CLANG`、既定 `clang`)。無ければ全部飛ばし、実行時は従来どおり clang を使う。
/// 返すのは識別子 (`KSN_BUILD_ID`) に混ぜる中身。
fn gen_assets(manifest: &Path, out: &Path) -> Vec<u8> {
    println!("cargo:rerun-if-env-changed=KSN_BUILD_CLANG");
    println!("cargo:rerun-if-changed=../spec/stdlib");
    let clang = std::env::var("KSN_BUILD_CLANG").unwrap_or_else(|_| "clang".to_string());
    let mut mixed: Vec<u8> = vec![];
    let mut rs = String::from("pub fn rt_bc(target: &str, piece: &str) -> Option<&'static [u8]> {\n    match (target, piece) {\n");
    let src = manifest.join("src");
    let inc = src.join("wasm").join("include");
    let host_x86 = std::env::consts::ARCH == "x86_64";
    let host_a64 = std::env::consts::ARCH == "aarch64";
    let aroot = std::env::var("KSN_SYSROOT").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/usr/aarch64-linux-gnu".to_string());
    // (対象, 部品, ソース, 追加の引数)
    let mut jobs: Vec<(&str, &str, PathBuf, Vec<String>)> = vec![];
    let elf = |t: &str, host: bool, root: Option<&str>| -> Option<Vec<String>> {
        let mut a: Vec<String> = vec![format!("--target={}", t), "-O0".into(), "-fPIC".into(), "-pthread".into()];
        if !host {
            let r = root?;
            if !Path::new(r).join("include").is_dir() {
                return None;
            }
            a.push("-isystem".into());
            a.push(format!("{}/include", r));
        }
        Some(a)
    };
    for (t, host, root) in [("x86_64-linux-gnu", host_x86, None), ("aarch64-linux-gnu", host_a64, Some(aroot.as_str()))] {
        if let Some(a) = elf(t, host, root) {
            jobs.push((t, "kir_rt", src.join("kir_rt.c"), a.clone()));
            jobs.push((t, "sysrt", src.join("sysrt.c"), a));
        }
    }
    // Windows (MinGW-w64): C の開発ファイルは mingw-w64 の sysroot (`KSN_WIN_SYSROOT`、既定 /usr/x86_64-w64-mingw32)。無ければ飛ばす (実行時に clang を使う)。
    println!("cargo:rerun-if-env-changed=KSN_WIN_SYSROOT");
    let wroot = std::env::var("KSN_WIN_SYSROOT").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/usr/x86_64-w64-mingw32".to_string());
    if Path::new(&wroot).join("include").is_dir() {
        let a: Vec<String> = vec!["--target=x86_64-w64-windows-gnu".into(), "-O0".into(), "-isystem".into(), format!("{}/include", wroot)];
        jobs.push(("x86_64-windows-gnu", "kir_rt", src.join("kir_rt.c"), a.clone()));
        jobs.push(("x86_64-windows-gnu", "sysrt", src.join("sysrt.c"), a));
    }
    let wasm = |t: &str, extra: &[&str]| -> Vec<String> {
        let mut a: Vec<String> = vec![format!("--target={}", t), "-O0".into(), "-ffreestanding".into(), "-fno-builtin".into(), format!("-isystem{}", inc.display())];
        a.extend(extra.iter().map(|s| s.to_string()));
        a
    };
    jobs.push(("wasm32-unknown-unknown", "rt", src.join("wasm").join("rt.c"), wasm("wasm32-unknown-unknown", &[])));
    jobs.push(("wasm32-unknown-unknown", "kir_rt", src.join("kir_rt.c"), wasm("wasm32-unknown-unknown", &[])));
    jobs.push(("wasm32-wasi", "rt", src.join("wasm").join("rt.c"), wasm("wasm32-unknown-wasi", &["-DKSN_WASI=1"])));
    jobs.push(("wasm32-wasi", "kir_rt", src.join("kir_rt.c"), wasm("wasm32-unknown-wasi", &["-DKSN_WASI=1"])));
    jobs.push(("wasm32-wasi", "wasi", src.join("wasm").join("wasi.c"), wasm("wasm32-unknown-wasi", &["-DKSN_WASI=1"])));
    let mut seen = vec![];
    for (t, piece, c, args) in jobs {
        let bc = out.join(format!("rt_{}_{}.bc", t, piece));
        let _ = fs::remove_file(&bc);
        let ok = Command::new(&clang).args(&args).args(["-c", "-emit-llvm", "-x", "c"]).arg(&c).arg("-o").arg(&bc).output().map(|o| {
            if !o.status.success() {
                println!("cargo:warning=実行時の C ({}/{}) をビットコードにできない: {}", t, piece, String::from_utf8_lossy(&o.stderr).lines().next().unwrap_or(""));
            }
            o.status.success()
        }).unwrap_or(false);
        if ok {
            rs.push_str(&format!("        ({:?}, {:?}) => Some(include_bytes!({:?})),\n", t, piece, bc.display().to_string()));
            part_raw(&mut mixed, &format!("{}/{}", t, piece), &fs::read(&bc).unwrap_or_default());
            seen.push(t);
        }
    }
    rs.push_str("        _ => None,\n    }\n}\n\n");
    // 標準ライブラリ
    let mut files = vec![];
    walk(&manifest.join("..").join("spec").join("stdlib"), &mut files);
    files.sort();
    let root = manifest.join("..").join("spec").join("stdlib");
    let mut blob: Vec<u8> = vec![];
    for f in files {
        if f.components().any(|c| c.as_os_str() == "__pycache__") {
            continue;
        }
        let rel = f.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/");
        let data = fs::read(&f).unwrap_or_default();
        blob.extend((rel.len() as u32).to_le_bytes());
        blob.extend(rel.as_bytes());
        blob.extend((data.len() as u64).to_le_bytes());
        blob.extend(data);
    }
    let bin = out.join("stdlib.bin");
    fs::write(&bin, &blob).expect("stdlib.bin を書けない");
    rs.push_str(&format!("pub static STDLIB: &[u8] = include_bytes!({:?});\n", bin.display().to_string()));
    fs::write(out.join("assets.rs"), rs).expect("assets.rs を書けない");
    mixed.extend(&blob);
    mixed
}

fn part_raw(v: &mut Vec<u8>, name: &str, bytes: &[u8]) {
    v.extend((name.len() as u64).to_le_bytes());
    v.extend(name.as_bytes());
    v.extend((bytes.len() as u64).to_le_bytes());
    v.extend(bytes);
}

fn main() {
    if std::env::var_os("CARGO_FEATURE_EMBEDDED_LLVM").is_some() {
        link_llvm();
    }
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let grammar = manifest.join("..").join("spec").join("grammar.ebnf");

    // 再ビルドの条件。`src` は directory を指定すると中のどれかの変更を見る。
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=Cargo.lock");
    println!("cargo:rerun-if-changed=../spec/grammar.ebnf");
    println!("cargo:rerun-if-env-changed=KSN_VM_PROF");
    println!("cargo:rerun-if-env-changed=RUSTFLAGS");
    println!("cargo:rerun-if-env-changed=CARGO_ENCODED_RUSTFLAGS");

    let mut h = Sha256::new();
    let env = |k: &str| std::env::var(k).unwrap_or_default();
    part(&mut h, "pkg", env("CARGO_PKG_VERSION").as_bytes());
    part(&mut h, "rustc", &Command::new(env("RUSTC")).arg("-vV").output().map(|o| o.stdout).unwrap_or_default());
    for k in ["CARGO_ENCODED_RUSTFLAGS", "PROFILE", "OPT_LEVEL", "DEBUG", "TARGET", "KSN_VM_PROF"] {
        part(&mut h, k, env(k).as_bytes());
    }

    let mut files = vec![manifest.join("Cargo.toml"), manifest.join("Cargo.lock"), manifest.join("build.rs"), grammar];
    walk(&manifest.join("src"), &mut files);
    files.sort();
    for f in &files {
        let rel = f.strip_prefix(&manifest).or_else(|_| f.strip_prefix(manifest.join(".."))).unwrap_or(f);
        let name = rel.to_string_lossy().replace('\\', "/");
        // 読めないもの (無い) は空として扱う。埋め込み側も読めなければビルドが失敗するので、ここで隠れることはない。
        part(&mut h, &name, &fs::read(f).unwrap_or_default());
    }

    let assets = gen_assets(&manifest, &PathBuf::from(env("OUT_DIR")));
    part(&mut h, "assets", &assets);
    let id: String = h.finalize().iter().map(|b| format!("{:02x}", b)).collect();
    println!("cargo:rustc-env=KSN_BUILD_ID={}", id);
    link_embedded_lld();
}

// ---- L3: 組み込みの lld (feature `embedded-llvm`) ----
#[allow(dead_code)]
fn link_embedded_lld() {
    if std::env::var("CARGO_FEATURE_EMBEDDED_LLVM").is_err() {
        return;
    }
    println!("cargo:rerun-if-changed=src/native/lld_shim.cpp");
    // llvm-config は L1 の `link_llvm` と同じもの (`LLVM_CONFIG`、既定は /usr/lib/llvm-20/bin/llvm-config)。
    // llvm-config が無いクロスビルドでは KSN_LLVM_PREFIX をそのまま使う。
    let llvm_config = llvm_config_path();
    let prefix = std::env::var("KSN_LLVM_PREFIX").ok().filter(|s| !s.is_empty());
    let (inc, lib) = if Path::new(&llvm_config).is_file() {
        let cfg = |a: &[&str]| -> String {
            let o = Command::new(&llvm_config).args(a).output().expect("llvm-config が要る (llvm-20-dev)");
            String::from_utf8_lossy(&o.stdout).trim().to_string()
        };
        (cfg(&["--includedir"]), cfg(&["--libdir"]))
    } else {
        let p = prefix.as_deref().unwrap_or("");
        (format!("{}/include", p), format!("{}/lib", p))
    };
    // lld のヘッダが無いクロスインストールでは組み込み lld はスキップする
    // (インタプリタ / C バックエンドの smoke テストには lld は不要)。
    if !Path::new(&format!("{}/lld/Common/Driver.h", inc)).is_file() {
        return;
    }
    // `cc` crate は build.rs 側に feature を渡せないので使わず、c++ と ar を直接呼ぶ。
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let obj = out.join("lld_shim.o");
    let st = Command::new(std::env::var("CXX").unwrap_or_else(|_| "c++".into()))
        .args(["-std=c++17", "-fno-exceptions", "-fno-rtti", "-fPIC", "-O2", "-c", "src/native/lld_shim.cpp", "-I", &inc, "-o"])
        .arg(&obj)
        .status()
        .expect("c++ が要る");
    assert!(st.success(), "lld_shim.cpp のコンパイルに失敗");
    let a = out.join("libksn_lld_shim.a");
    let _ = fs::remove_file(&a);
    assert!(Command::new("ar").arg("crs").arg(&a).arg(&obj).status().expect("ar が要る").success());
    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=static=ksn_lld_shim");
    println!("cargo:rustc-link-search=native={}", lib);
    if Path::new(&llvm_config).is_file() {
        let cfg = |a: &[&str]| -> String {
            let o = Command::new(&llvm_config).args(a).output().expect("llvm-config が要る (llvm-20-dev)");
            String::from_utf8_lossy(&o.stdout).trim().to_string()
        };
        for l in ["lldMinGW", "lldCOFF", "lldELF", "lldWasm", "lldCommon"] {
            println!("cargo:rustc-link-lib=static={}", l);
        }
        for l in cfg(&["--link-static", "--libs", "lto", "object", "option", "debuginfodwarf", "all-targets", "windowsdriver", "windowsmanifest", "libdriver", "dlltooldriver"]).split_whitespace().filter(|l| !l.contains("Polly")) {
            // Polly は -dev に静的ライブラリが無い。lld は使わないので外す。
            println!("cargo:rustc-link-lib=static={}", l.trim_start_matches("-l"));
        }
    } else {
        // llvm-config なし: lib の lld / LLVM 静的ライブラリをすべてリンク。循環参照は --start-group で解決。
        println!("cargo:rustc-link-arg=-Wl,--start-group");
        link_glob_static(&lib, &["liblld", "libLLVM"]);
        println!("cargo:rustc-link-arg=-Wl,--end-group");
    }
    // システムのライブラリ (z・zstd・stdc++) は L1 の `link_llvm` が静的につなぐ (配布する実行体が依存しない)。ここでは足さない。
}
