[日本語](ALPHA.md) | English

# ALPHA.md — Guide to the Kasane (ksn) public alpha 0.1.0-alpha.1

> This is an alpha release. The language specification, the standard library and the tools may change in incompatible ways during the alpha (the rule while the version is `0.x`; see the compatibility row in chapter 10 of the spec). Edition 1 will be frozen (5.4.1, "Scope of Edition 1") only after we have heard from alpha users.

## 1. What is Kasane?

Kasane is "a contract-oriented systems language in which an LLM writes and a machine verifies" (Implementation plan 1.1). A human reads only three things: function headers (the `uses` / `panics` contracts), `test` definitions, and the public API. For bodies, the checker guarantees types, borrows and effects. The toolchain is a single binary `ksn` written in Rust, and it runs on a Linux x86_64 host. Source extensions are `.ksnl` (the canonical form, without comments) and `.ksnh` (with comments, the format humans write). A published package ships only `.ksnl`; a dependency's `.ksnh` is neither read nor checked (the `.ksnh` of the root package is checked against its `.ksnl` with E9050).

## 2. Requirements

- Host OS: Linux (x86_64). macOS is not supported. Development on a Windows host is not supported either, but cross-compiling from Linux to Windows is possible.
- This binary release (`ksn-<version>-x86_64-linux-gnu.tar.gz`): runs on x86_64 Linux with glibc 2.30 or later (the targets checked are current Debian 12, Debian 13 and Arch; the required glibc version is written in `REQUIRES.txt` in the tarball). LLVM and the linker (lld) are inside the executable, so clang, ld and LLVM are not needed. The only shared libraries are glibc's `libc` / `libm` / `libpthread` / `libdl`.
- C development files on the user's machine (the crt object files and libgcc), which `ksn build` (native generation) uses:
  - Debian / Ubuntu: `sudo apt install libc6-dev libgcc-12-dev` (Debian 12. On Debian 13 it is `libgcc-14-dev`. Installing `gcc` brings in everything.)
  - Arch: `sudo pacman -S gcc glibc` (`gcc` provides crtbeginS.o and libgcc in `/usr/lib/gcc/x86_64-pc-linux-gnu/<version>/`)
  - If something is missing, `ksn build` fails and shows the missing file and the package to install. `ksn check` / `test` / `run` do not need these.
- To build from source yourself: Rust (cargo), clang or gcc, Python 3.10 or later, and git. The default build with native generation also needs the LLVM 20 development files and the zlib/zstd development files ("Requirements" in the README).

## 3. Installation (steps we actually verified)

Download the tarball (`ksn-<version>-x86_64-linux-gnu.tar.gz`) from the GitHub Release and extract it. The targets are Arch Linux and Debian 12 or later (the required glibc is 2.30 or later).
Extract the tarball and put `ksn` on your PATH. The licenses are `LICENSE-MIT` / `LICENSE-APACHE`, the bundled third-party licenses are in `THIRD_PARTY_LICENSES`, and integrity is checked with `SHA256SUMS`.

```bash
# ksn を PATH に置く（例）
tar xzf ksn-<版>-x86_64-linux-gnu.tar.gz
cd ksn-<版>-x86_64-linux-gnu
# ksn 単体で動作します（標準ライブラリは実行体に埋め込まれています）
mkdir -p "$HOME/.local/opt" && cp -r . "$HOME/.local/opt/ksn"
ln -sf "$HOME/.local/opt/ksn/ksn" "$HOME/.local/bin/ksn"
export PATH="$HOME/.local/bin:$PATH"

# キャッシュは既定で ~/.ksn/cache（自分だけが書ける 0700 のディレクトリ）に置かれます。
# KSN_CACHE_DIR で変える場合も、ほかの利用者が書き込める場所（/tmp など）は避けてください。

ksn --version   # ksn 0.1.0
```

(In the block above, `<版>` stands for `<version>`. By default the cache is placed in `~/.ksn/cache`, a 0700 directory writable only by you. If you change it with `KSN_CACHE_DIR`, avoid places other users can write to, such as `/tmp`.)

Run the `ksn` command inside a package (a place with `kasane.toml`). It also finds the package by walking up from a subdirectory.

## 4. 10-minute tutorial

### 4.1 Create a package

```bash
mkdir -p /tmp/alpha_try && cd /tmp/alpha_try
ksn new hello && cd hello
```

Generated files: `kasane.toml`, `kasane.lock`, `src/hello.ksnl`.

### 4.2 Write code (a function using `uses` / `panics`, and a test)

Rewrite `src/hello.ksnl` as follows.

```kasane
fn hello.double(n: i32) -> i32 uses [] panics never
    n.wrapping_add(n)
end hello.double

fn hello.main() -> Result<(), std.io.Error> uses [io.console] panics never
    std.io.print_line("double: 4")?
    Ok(())
end hello.main

test hello.double.test_basic
    core.check.eq(hello.double(2i32), 4i32)
end hello.double.test_basic
```

- `uses [...]` declares side effects (console output, allocation, and so on).
- `panics never` guarantees that the function does not panic (expressions that can panic, such as the index `a[i]` or integer `+`, are not allowed). Deadlock detection (exit code 101), resource exhaustion, and the call-depth limit (exit code 102) are outside this guarantee.
- `?` propagates a `Result` failure to the caller.

### 4.3 Check, format, run

```bash
ksn fmt        # 正規形へ整える（書き方は 1 通り）
ksn check      # 診断 0 件なら何も出力しない
ksn test       # テストを実行
ksn run        # 実行 (既定は VM。--engine=tree で木走査)
```

(The comments above say: `ksn fmt` formats into the canonical form (one canonical form); `ksn check` prints nothing when there are 0 diagnostics; `ksn test` runs the tests; `ksn run` runs the program (the default is the VM; `--engine=tree` selects the tree walker).)

Output of `ksn test`: `test hello.double.test_basic ... ok` / `1 passed; 0 failed`.
Output of `ksn run`: `double: 4`.

A file that differs from the output of `ksn fmt` gets E0020 (not formatted). Other errors are reported at the same time, so format with `ksn fmt` first and then fix the rest.

### 4.4 Build and run a native executable

```bash
ksn build --release
./target/release/hello
```

Output: `double: 4`. At the end of the build, `built: target/release/hello (native)` shows where the executable is.

## 5. Where the documentation is

- Cheat sheet (a list of how to write things, with error examples such as E0042 and the correct forms): `docs/en/cheatsheet.md` (Japanese: `チートシート.md`)
- Language specification: `Kasane 簡易仕様書.md` (the grammar is in `spec/grammar.ebnf`, the standard library API in `spec/stdlib/`)
- Tutorial (three worked examples): `docs/en/tutorial.md`
- Known limitations: `docs/en/known-limitations.md`
- Explanation of diagnostic codes: `ksn explain <code>` (for example `ksn explain E0042`)
- API documentation of a package: `ksn doc` (`target/doc/index.html`); standard library: `ksn doc --std`
- Current state and limits of the implementation: `docs/plan/実装計画.md`

## 6. Known limitations (as of the alpha)

- The default execution engine for `ksn run` / `ksn test` is the VM (KIR → bytecode). A form that cannot be lowered to the VM silently falls back to the tree walker. If the output looks wrong, compare with `--engine=tree` (or `KSN_ENGINE=tree`) and let us know. `--watch` uses the tree walker. The VM reserves a large virtual region at startup, so if you set `ulimit -v`, use 8 GB or more (with a smaller limit, large I/O causes an `AllocError` panic or a fallback to the tree walker). Standard output is written line by line in the tree walker, the VM and native alike (when printing a lot, it is faster to gather the text into one string and print it once).
- Running and building on a macOS host is not supported. Development on a Windows host is not supported either (cross-building from Linux is possible).
- Distributions are for x86_64 Linux only (Arch, Debian 12 or later). The license is MIT or Apache-2.0 (`LICENSE-MIT` / `LICENSE-APACHE`; the bundled LLVM and others are in `THIRD_PARTY_LICENSES`).
- The checker (`ksn check`) aims to reject code that must not pass, but holes are still being found in hole-hunting (fixed ones are in `stage0/tests/data/unsound/`). For code where safety matters, also verify native builds with ASan or similar. If you find a wrong acceptance, please tell us.
- A package that uses `[native]` in `kasane.toml` (linking C libraries), including through its dependencies, must write `native = true` in `[unsafe]` (otherwise E9024). This is because a C library can run code at startup; you can list them with `ksn unsafe`.
- There is no central public package registry. You can publish to a registry (or a path) with `ksn publish --registry <URL>`. Dependencies are specified in `kasane.toml` by a local path or a Git URL + commit hash.
- The async API of `std.net` (`accept_async` / `read_async` / `write_async`) is Linux only.
- Features that cannot be lowered to native generation (some of `std.tls`, FFI, and others) fall back to the interpreter-bundled bundle, and `ksn build --release` fails with E9040 (`--release --allow-bundle` allows the fallback).
- The `drop` of a hand-written `impl core.ops.Drop` is called at the end of a scope (in reverse order of declaration), on reassignment, on `core.mem.drop`, and for temporaries at the end of a statement (the same in `ksn run` / `ksn test` / native. Execution goes through KIR; a package with a hand-written `Drop` does not fall back to the tree walker, and if it cannot be lowered to KIR it fails with a reason). For failures when a form that cannot be lowered to KIR is included, see `docs/en/known-limitations.md`. `ksn debug` / `ksn dap` also run on the VM, so packages with a hand-written `Drop` can be debugged too.
- `ksn fix` can automatically fix only some diagnostics (E1108 / E1020 / E2041 / E2042).
- When `std.thread.spawn` fails because a thread cannot be created, native and the VM do not return `Err(SpawnError)`; they stop the program with a `spawn failed: ...` panic (exit code 101). Only the tree walker (`--engine=tree`) can return the closure.
- Some features are unsupported or limited, such as `dyn` types and passing closures as C function pointers (which needs `core.ffi.trampoline`).

## 7. How to report problems

For bugs or anything unclear, please tell us through GitHub Issues: https://github.com/sen10f/kasane-lang/issues

Please attach the following to your report.

- The output of `ksn --version`, and your OS (distribution and version)
- A minimal reproducible program (`kasane.toml` and the `.ksnl` files in `src/`), and the commands you ran
- The expected result, and the actual output and exit code (for diagnostics, the output of `ksn check --json` helps)
- If you suspect a difference between execution engines, also the result of running with `--engine=tree`

If you think code that must not pass the checker did pass (a soundness hole), say so in the title.

If you can send a reproduction, please include the following.
- The output of `ksn --version`
- The host OS and architecture (`uname -a`)
- A minimal `.ksnl` source and `kasane.toml` that reproduce it
- The error message of `ksn check` / `ksn run` / `ksn build` (copy and paste is fine)

## 8. Scope of Edition 1 (what is in, what is experimental)

Edition 1 freezes only the core of the toolchain.

- Frozen (language, core standard library, tools): the checker (`ksn check`), the execution engines (VM + LLVM native), `check` / `run` / `test` / `build` / `fmt` / `fix`, the LSP, and the language specification and core standard library these use (the range of `core` / `alloc` / `std` that is settled for Edition 1).
- Experimental and outside Edition 1: `ui/` (UI), the WASM SPA, the package registry / `publish`, self-hosting (stage1/stage2), and bindgen. These are not part of the Edition 1 contract and will be considered for v1 and later.
