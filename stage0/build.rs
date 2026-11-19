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

fn main() {
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
    // `KSN_LLVM_PREFIX` があれば自前の LLVM (`scripts/build_llvm.sh`) を静的に使い、libstdc++ も静的にする。無ければ distro の LLVM 20。
    println!("cargo:rerun-if-env-changed=KSN_LLVM_PREFIX");
    let prefix = std::env::var("KSN_LLVM_PREFIX").ok().filter(|s| !s.is_empty());
    let llvm_config = prefix.as_ref().map_or("llvm-config-20".to_string(), |p| format!("{}/bin/llvm-config", p));
    let cfg = |a: &[&str]| -> String {
        let o = Command::new(&llvm_config).args(a).output().expect("llvm-config-20 が要る (llvm-20-dev)");
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    };
    let inc = cfg(&["--includedir"]);
    let lib = cfg(&["--libdir"]);
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
    for l in ["lldELF", "lldWasm", "lldCommon"] {
        println!("cargo:rustc-link-lib=static={}", l);
    }
    for l in cfg(&["--link-static", "--libs", "lto", "object", "option", "debuginfodwarf", "all-targets"]).split_whitespace().filter(|l| !l.contains("Polly")) {
        // Polly は -dev に静的ライブラリが無い。lld は使わないので外す。
        println!("cargo:rustc-link-lib=static={}", l.trim_start_matches("-l"));
    }
    for l in cfg(&["--link-static", "--system-libs"]).split_whitespace() {
        println!("cargo:rustc-link-lib={}", l.trim_start_matches("-l"));
    }
    if prefix.is_some() {
        let o = Command::new(std::env::var("CXX").unwrap_or_else(|_| "c++".into())).arg("-print-file-name=libstdc++.a").output().expect("c++ が要る");
        let p = PathBuf::from(String::from_utf8_lossy(&o.stdout).trim());
        if let Some(d) = p.parent().filter(|_| p.is_absolute()) {
            println!("cargo:rustc-link-search=native={}", d.display());
        }
        println!("cargo:rustc-link-lib=static=stdc++");
    } else {
        println!("cargo:rustc-link-lib=stdc++");
    }
}
