#!/usr/bin/env python3
"""selfhost/fmt の突き合わせ。Kasane 版 (fmt) の整形出力と、Rust 版 (ksn fmt) で
整形済みの元ファイルをバイト単位で比べる。

  python3 -I compare.py [--group examples|apps|selfhost|linear_ok|unsound|all] [--files F...] [-v]

- 各ファイルについて、`fmt` (Kasane 版、ネイティブ) の stdout と元ファイルの
  内容をバイト単位で比べる。元ファイルは `ksn fmt --check` で整形済みであることが
  前提 (Rust 版と同等の出力)。
- 結果を 4 行で出力: コミットハッシュ、総行数、一致数/総数、native か否か。
"""
import argparse
import glob
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.normpath(os.path.join(HERE, "..", ".."))
KSN = os.path.join(ROOT, "stage0", "target", "release", "ksn")
NATIVE = os.path.join(HERE, "target", "release", "fmt")


def run(cmd, cwd=None):
    p = subprocess.run(cmd, cwd=cwd, capture_output=True)
    return p.returncode, p.stdout, p.stderr


def corpus(group):
    g = lambda pat: sorted(glob.glob(os.path.join(ROOT, pat), recursive=True))
    if group == "examples":
        return g("examples/*/src/**/*.ksnl")
    if group == "apps":
        return g("apps/*/src/**/*.ksnl")
    if group == "selfhost":
        return g("selfhost/*/src/**/*.ksnl")
    if group == "linear_ok":
        return g("stage0/tests/data/linear_ok/*.ksnl")
    if group == "unsound":
        return g("stage0/tests/data/unsound/*.ksnl")
    raise SystemExit(f"unknown group {group}")


def commit_hash():
    rc, out, _ = run(["git", "-C", ROOT, "rev-parse", "HEAD"])
    if rc == 0:
        return out.decode().strip()
    return "unknown"


def count_lines(paths):
    total = 0
    for p in paths:
        try:
            with open(p, "rb") as f:
                total += f.read().count(b"\n")
        except OSError:
            pass
    return total


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--group", default="all",
                    choices=["examples", "apps", "selfhost", "linear_ok", "unsound", "all"])
    ap.add_argument("--files", nargs="*")
    ap.add_argument("-v", "--verbose", action="store_true")
    a = ap.parse_args()

    if not os.path.exists(NATIVE):
        print(f"fmt がない: cd selfhost/fmt && ksn build --release", file=sys.stderr)
        return 2

    if a.files:
        paths = [os.path.abspath(f) for f in a.files]
        groups = {"files": paths}
    else:
        groups = {}
        for gname in (["examples", "apps", "selfhost", "linear_ok", "unsound"]
                      if a.group == "all" else [a.group]):
            groups[gname] = corpus(gname)

    all_paths = [p for paths in groups.values() for p in paths]
    total_lines = count_lines(all_paths)
    matched = 0
    total = 0

    for gname, paths in groups.items():
        for p in paths:
            total += 1
            rc, out, err = run([NATIVE, p])
            if rc != 0:
                if a.verbose:
                    rel = os.path.relpath(p, ROOT)
                    print(f"ERROR {gname}/{rel}: rc={rc} err={err[:200]}")
                continue
            try:
                with open(p, "rb") as f:
                    original = f.read()
            except OSError:
                if a.verbose:
                    rel = os.path.relpath(p, ROOT)
                    print(f"READERR {gname}/{rel}")
                continue
            if out == original:
                matched += 1
            else:
                if a.verbose:
                    rel = os.path.relpath(p, ROOT)
                    print(f"DIFF {gname}/{rel}")
                    print(f"  native len={len(out)} file len={len(original)}")

    ch = commit_hash()
    native = os.path.exists(NATIVE)
    print(ch)
    print(total_lines)
    print(f"{matched}/{total}")
    print("native" if native else "bundle")
    return 0


if __name__ == "__main__":
    sys.exit(main())