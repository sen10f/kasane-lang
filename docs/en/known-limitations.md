[日本語](../known-limitations.md) | English

# Known limitations

This document summarizes the usage and implementation constraints of the alpha `0.1.0-alpha.1`.

## Host and distribution targets

- Running on a macOS host and building for macOS are not supported. The host verified to work is Linux (x86_64).
- Windows executables can be cross-compiled from Linux, but a development environment on a Windows host is not supported.
- The prebuilt binary for distribution is for x86_64 Linux (Arch, Debian 12 or later; glibc 2.30 or later is required). The license is MIT or Apache-2.0 (`LICENSE-MIT` / `LICENSE-APACHE`).

## Package registry

There is no central public package registry. Dependency packages are fetched from a local path written in `kasane.toml`, or from a Git URL and commit hash. There is no version-range syntax. You can publish to a registry (or a path) with `ksn publish --registry <URL>`, and `ksn update` and `ksn vendor` are available, but the real network behavior of `vendor` is unverified.

## S4 checker migration

S4 (the work of moving the checker onto the TAST (typed AST) and lowering to KIR from the TAST) has been integrated (commit `97178d39`). The default of `KSN_CHECKER` is `s4` (the new TAST-based checks). `KSN_CHECKER=old|new|shadow` is accepted for one more release, but the same checker runs and a deprecation notice is printed. The old checker has been removed.

## Scope of execution of a hand-written `Drop`

The `drop` of a hand-written `impl core.ops.Drop` (spec 4.4) is called at the end of a scope (in reverse order of declaration; after the outer `drop`, the fields in declaration order), for an old value discarded on reassignment, on `core.mem.drop`, and for temporaries at the end of a statement. On `return` / `?` / `break` / `continue`, and on exits from the body of `with`, the scopes being left are also dropped. It is not called on a panic or on process exit. Moved-out values are not dropped (a conditional move is decided by a run-time flag). When a value is moved into a `spawn` closure, it is dropped inside the thread.

Execution is carried out by KIR (the VM for `ksn run` / `ksn test`, native for `ksn build`). The tree walker does not track ownership of values, so a package with a hand-written `Drop` always runs on the VM even for `ksn run` / `ksn test`; when the form cannot be lowered to KIR, it does not fall back to the tree walker, and fails with a reason. The `Drop` of a type with type parameters is monomorphized per type argument and called. A value `move`d into an `Fn` / `FnMut` closure is dropped at the end of the scope that created the closure (forms where the closure escapes that scope are out of scope), and the environment of an `FnOnce` that was never called is dropped at the same place. When a form that cannot be lowered to KIR is included (for example a struct that has a `Result` as a field), it fails with a reason.

Taking a field of a `Drop` type out with `let s = g.s` is rejected with E3002. Even if you explicitly pass `--engine=tree`, a package with a hand-written `Drop` runs on the VM. The debugger (`ksn debug` / `ksn dap`) also runs on the VM, so you can debug a package with a hand-written `Drop` (only the `--engine=tree` debugger is refused, because it does not run `Drop`).

## Known holes in the checker

By convention, code that must not pass is placed in `stage0/tests/data/unsound/`, one case per file, and the ones that cannot be closed yet are listed in `KNOWN_HOLES` in `stage0/tests/check_diag.rs`.

## Support per execution engine

Support per feature is summarized in the [engine support matrix](../engine-matrix.md). The key points: `ksn run` / `ksn test` default to the VM, `ksn build` uses LLVM, a form that cannot be lowered falls back to the debug bundle, and a release build fails with E9040.

## Fallback to the interpreter

If you use a feature that cannot be lowered to a native executable, or the required compiler is missing, `ksn build` may fall back to a bundle that includes the interpreter, and it prints the reason. An ordinary debug build allows this fallback, but a release build fails with E9040. To explicitly allow the fallback in a release build, pass `--release --allow-bundle`.

`ksn test --both` also uses the bundle when the comparison through LLVM is not possible. In that case, the comparison target is the runtime bundled with the interpreter, not native machine code.

## spawn failure

When `std.thread.spawn` fails because a thread cannot be created, native and the VM do not return `Err(SpawnError)`; they stop the program with a `spawn failed: ...` panic (exit code 101). Only the tree walker (`--engine=tree`) can return the closure.
