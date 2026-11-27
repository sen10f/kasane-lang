[日本語](README.md) | English

# Kasane (`ksn`)

Kasane is an experimental systems language, and its toolchain, aimed at letting weak LLMs write code that actually works.
It takes Rust's syntax as a base and borrows (well, steals) an effect system from Koka and a type system from Ada.

There is basically one way to write anything, diagnostics tell you how to fix the problem, and `ksn fix` / `ksn fmt` repair code mechanically. The main use is an LLM writing the code and a machine verifying it.

Honestly, it isn't designed for humans to write by hand, so it is absurdly verbose and hard to read.

The toolchain is written in Rust. A single `ksn` binary with LLVM and lld built in produces native output for Linux (x86_64, aarch64), Windows and WASM (experimental). The source extension is `.ksnl`; the format in which humans write comments is `.ksnh`.

This split puts verifiability of future libraries and packages first: it keeps unintended code from slipping into modules an LLM generated, and keeps third parties from sneaking in malware.
As a rule, only `.ksnl` files can be distributed as packages.

The first alpha is out as `0.1.0-alpha.1`.

## Requirements

To build the alpha from source, you need the following.

| Item | Details |
| --- | --- |
| Host OS | Linux (x86_64) |
| Rust | Cargo and a toolchain that supports the Rust 2021 edition |
| C / C++ build tools | `clang` (or `gcc`), `c++`, `ar` |
| LLVM | LLVM 20 development files, plus zlib and zstd development files, for the default build with embedded LLVM |
| Python | 3.10 or later (for spec verification and integration tests) |
| Git | To fetch the source, and for `git`-type dependency packages |

Running on a macOS host is not supported. Windows binaries can be cross-compiled from Linux, but a development environment on a Windows host is not supported either. See [Known limitations](docs/en/known-limitations.md) for details.

## Installation

### Prebuilt binary

Download the prebuilt binary `ksn-<version>-x86_64-linux-gnu.tar.gz` from the GitHub Release (ALPHA.md, "3. Installation"). The standard library is embedded in the executable, and no `stdlib/` directory is bundled.

### Build from source

```bash
git clone https://github.com/sen10f/kasane-lang.git
cd kasane-lang
bash scripts/setup.sh
```

`scripts/setup.sh` checks the required tools, builds `stage0/target/release/ksn`, and runs a smoke test on `examples/demo`. The default build with embedded LLVM fails unless the LLVM 20 development files and the zlib and zstd development files are installed. Only if you use an API key for the benchmarks, first copy `.env.example` to `.env` and set `NVIDIA_API_KEY`.

## Quick start

Add the built `ksn` to your PATH. Run every `ksn` command in the package root, where `kasane.toml` lives.

```bash
export PATH="$PWD/stage0/target/release:$PATH"
cd docs/tutorial/1
ksn --version
ksn fmt
ksn check
ksn run
```

`ksn fmt` formats the source into Kasane's canonical form, and `ksn check` checks for diagnostics. If there are no diagnostics, `ksn check` prints nothing. The final `ksn run` prints the following.

```text
こんにちは、Kasane!
```

Three worked examples and detailed explanations are in the [tutorial](docs/en/tutorial.md).

## Main commands

| Command | Description |
| --- | --- |
| `new` / `check` / `fix` | Create a package; check types, borrows, effects and more; apply automatic fixes |
| `fmt` / `fmt --check` | Format into the canonical form; check that files are already formatted |
| `run` / `test` / `test --both` | Run and test (default is the VM; `--engine=tree` selects the tree walker); compare results with the LLVM native build |
| `build` | Build a native executable via LLVM, and a portable bundle |
| `debug` / `dap` | Debugger on top of the VM; Debug Adapter Protocol server (`--engine=tree` selects the tree-walker version) |
| `lsp` | Language Server Protocol server |
| `doc` / `index` / `context` / `explain` | API documentation, index, context of a definition, explanation of a diagnostic |
| `refs` / `rename` | List references; rename a definition and its references |
| `get` / `sig` / `update` / `vendor` / `lock` | Manage dependency packages and the lock file |

## Editor support

An official VS Code extension, for what it's worth, is in `editors/vscode/`. Run

```sh
npm install && npm run compile
```

in `editors/vscode/` to build a `.vsix`, then install it from VS Code with "Extensions: Install from VSIX..." (put the `ksn` executable on your `PATH`, or set its path in the `kasane.ksnPath` setting). It provides syntax highlighting for `.ksnl` / `.ksnh`, and, through `ksn lsp`, diagnostics, completion, hover, go to definition, references, rename, formatting, quick fixes, and format on save.

In other editors, you can use the LSP and DAP servers built into the toolchain from any LSP-capable or DAP-capable client.

- `ksn lsp` speaks LSP over standard input/output. It supports diagnostics, hover, go to definition, references, rename, formatting, symbols and quick fixes.
- `ksn dap` speaks DAP over standard input/output. It offers source-level debugging on top of the VM, with breakpoints, stepping, call stacks and local variables. It is not DWARF debugging of native machine code.

For the concrete connection steps on the editor side, follow the documentation of the client you use.

## Documentation

- [Tutorial](docs/en/tutorial.md)
- [Known limitations](docs/en/known-limitations.md)
- [Changelog](CHANGELOG.md)
- [Implementation plan](docs/plan/実装計画.md)
- [Cheat sheet](docs/en/cheatsheet.md)

## Verification during development

```bash
python3 tests/run_tests.py
```

This command verifies, all together, the machine check of the spec, the reference solutions, the Rust tests, builds, hot reload and cache behavior.

## License

Dual-licensed under the MIT License ([LICENSE-MIT](LICENSE-MIT)) and the Apache License 2.0 ([LICENSE-APACHE](LICENSE-APACHE)). Users may choose either one.

Unless stated otherwise, any contribution to this repository is deemed to be provided under the same dual MIT and Apache-2.0 terms.
