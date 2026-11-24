#!/usr/bin/env python3
"""Kasane 版の LLVM 出力 (`selfhost/emit`) と Rust 版のネイティブの比較。

各 `stage0/tests/data/linear_ok/*.ksnl` について:
  1. Rust 版: 一時パッケージで `ksn build` し、できた実行ファイルを走らせる (出力・終了コードを基準にする)。
  2. Kasane 版: `ksn kir t.main` の KIR を `emit` (Kasane で書いた emitter のネイティブ) に通して `.ll` を作り、
     Rust 版と同じランタイム (`stage0/src/kir_rt.c`・`sysrt.c`) と clang でリンクして走らせる。
  3. 標準出力・標準エラー・終了コードが一致するかを数える。
  4. (任意) `ksn kir --llvm` の出力と、Kasane 版の `.ll` が文字として一致するか (参考。ヘッダや main の包みが違うので通常は不一致)。

使い方: python3 -I compare.py [--only NAME,...] [--work DIR] [--keep]
作業場所は既定で /tmp/emit-work (リポジトリの外)。`-I` を付けて実行する。
"""

import argparse
import os
import pathlib
import shutil
import subprocess
import sys

HERE = pathlib.Path(__file__).resolve().parent
ROOT = HERE.parent.parent  # selfhost/emit の 2 つ上 = ワークツリーのルート
KSN = os.environ.get("KSN", str(ROOT / "stage0" / "target" / "release" / "ksn"))
EMIT = os.environ.get("KSN_EMIT", str(HERE / "target" / "debug" / "emit"))
LINEAR = ROOT / "stage0" / "tests" / "data" / "linear_ok"
RT_C = ROOT / "stage0" / "src" / "kir_rt.c"
SYS_C = ROOT / "stage0" / "src" / "sysrt.c"
TOML = ('[package]\nname = "t"\nversion = "0.1.0"\nedition = 1\nentry = "t.main"\n\n[dependencies]\n')


def run(args, cwd=None, timeout=120, stdin=None):
    return subprocess.run(args, cwd=cwd, capture_output=True, timeout=timeout, input=stdin)


def make_pkg(work: pathlib.Path, src: pathlib.Path) -> pathlib.Path:
    d = work / src.stem
    shutil.rmtree(d, ignore_errors=True)
    (d / "src").mkdir(parents=True)
    (d / "kasane.toml").write_text(TOML)
    (d / "kasane.lock").write_text("version = 1\n")
    shutil.copy(src, d / "src" / "t.ksnl")
    return d


def rust_reference(pkg: pathlib.Path):
    b = run([KSN, "build"], cwd=pkg)
    exe = pkg / "target" / "debug" / "t"
    if b"native backend skipped" in b.stdout + b.stderr or not exe.exists():
        return None, "skip: the Rust reference is not native (" + (b.stdout + b.stderr).decode(errors="replace").strip().splitlines()[-1][:120] + ")"
    if b.returncode != 0:
        return None, "rust build failed: " + b.stderr.decode(errors="replace")[-200:]
    try:
        r = run([str(exe)], cwd=pkg, timeout=20)
    except subprocess.TimeoutExpired:
        return None, "rust run timed out"
    return (r.stdout, r.stderr, r.returncode), None


def kasane_ir(pkg: pathlib.Path, work: pathlib.Path):
    k = run([KSN, "kir", "t.main"], cwd=pkg)
    if k.returncode != 0:
        return None, "rust kir failed: " + k.stderr.decode(errors="replace")[-200:]
    kir = work / (pkg.name + ".kir")
    kir.write_bytes(k.stdout)
    e = run([EMIT, str(kir), "t.main"], timeout=120)
    if e.returncode != 0:
        return None, "emit failed: " + e.stderr.decode(errors="replace")[-300:]
    ll = work / (pkg.name + ".ll")
    ll.write_bytes(e.stdout)
    return ll, None


def link_and_run(ll: pathlib.Path, work: pathlib.Path, name: str):
    exe = work / (name + ".exe")
    c = run(["clang", "-O0", "-Wno-override-module", "-x", "ir", str(ll),
             "-x", "c", str(RT_C), "-x", "c", str(SYS_C), "-o", str(exe), "-lm", "-pthread"])
    if c.returncode != 0 or not exe.exists():
        return None, "clang failed: " + c.stderr.decode(errors="replace")[-300:]
    try:
        r = run([str(exe)], cwd=work, timeout=20)
    except subprocess.TimeoutExpired:
        return None, "emitted run timed out"
    return (r.stdout, r.stderr, r.returncode), None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--only", default="")
    ap.add_argument("--work", default="/tmp/emit-work")
    ap.add_argument("--keep", action="store_true")
    ap.add_argument("--llvm-text", action="store_true", help="also compare with ksn kir --llvm (reference only)")
    args = ap.parse_args()
    work = pathlib.Path(args.work)
    shutil.rmtree(work, ignore_errors=True)
    (work / "pkg").mkdir(parents=True)
    names = sorted(p for p in LINEAR.glob("*.ksnl"))
    if args.only:
        want = set(args.only.split(","))
        names = [p for p in names if p.stem in want]
    ok = 0
    failures = []
    skipped = []
    for src in names:
        pkg = make_pkg(work / "pkg", src)
        ref, why = rust_reference(pkg)
        if ref is None:
            if why.startswith("skip:"):
                skipped.append((src.stem, why))
            else:
                failures.append((src.stem, "ref: " + why))
            continue
        ll, why = kasane_ir(pkg, work)
        if ll is None:
            failures.append((src.stem, why))
            continue
        mine, why = link_and_run(ll, work, src.stem)
        if mine is None:
            failures.append((src.stem, why))
            continue
        if mine == ref or (ref is None):
            ok += 1
        else:
            failures.append((src.stem, "differs: rust=%r mine=%r" % (ref[2], mine[2])))
    total = len(names) - len(skipped)
    print(f"output equal: {ok} / {total}  (excluded, reference not native: {len(skipped)})")
    for name, why in skipped:
        print(f"  skipped {name}: {why[:200]}")
    for name, why in failures[:200]:
        print(f"  {name}: {why[:240]}")
    if not args.keep:
        shutil.rmtree(work, ignore_errors=True)
    return 0 if ok == total else 1


if __name__ == "__main__":
    sys.exit(main())
