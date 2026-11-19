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

/// `embedded-llvm`: LLVM 20 の静的ライブラリのうち X86 / AArch64 / WebAssembly だけをつなぐ (llvm-sys は `no-llvm-linking`)。
fn link_llvm() {
    let cfg = std::env::var("LLVM_CONFIG").unwrap_or_else(|_| "/usr/lib/llvm-20/bin/llvm-config".to_string());
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
        match n {
            // 静的に取り込む (配布する実行体が libz / libzstd に依存しない)。xml2 は Windows の manifest 用で要らない。
            "z" | "zstd" => println!("cargo:rustc-link-lib=static={}", n),
            "xml2" => {}
            _ => println!("cargo:rustc-link-lib={}", n),
        }
    }
    for d in ["/usr/lib/x86_64-linux-gnu", "/usr/lib/aarch64-linux-gnu", "/usr/lib64"] {
        if Path::new(d).is_dir() {
            println!("cargo:rustc-link-search=native={}", d);
        }
    }
    println!("cargo:rustc-link-lib=static=stdc++");
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

    let id: String = h.finalize().iter().map(|b| format!("{:02x}", b)).collect();
    println!("cargo:rustc-env=KSN_BUILD_ID={}", id);
}
