#!/usr/bin/env python3
"""S5-1/S5-2: KIR のダンプが「TAST 経由」と「旧い経路 (tytab 直引き)」で同じかを、全パッケージで比べる。

使い方: python3 -I tests/kir_dump_diff.py [--ksn PATH]
各パッケージで `KSN_DUMP_KIR=1 ksn check` の stderr (KIR と未対応の一覧) を、
`KSN_S5_TYIDX_TAB=1` と `KSN_S5_PAT_OLD=1` (旧経路) と未設定 (TAST 経路) の 2 回取り、差があれば表示して終了コード 1。
対象: examples/*, apps/*, ui/kasane/*, selfhost/*, perf/ の kasane.toml を持つ所。
"""
import argparse, difflib, os, subprocess, sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ROOTS = ["examples", "apps", os.path.join("ui", "kasane"), "selfhost", "perf"]


def packages():
    out = []
    for r in ROOTS:
        for d, dirs, files in os.walk(os.path.join(REPO, r)):
            dirs[:] = sorted(x for x in dirs if x not in {"target", ".git", "node_modules"})
            if "kasane.toml" in files:
                out.append(d)
                dirs[:] = []
    return sorted(out)


def dump(ksn, pkg, tab):
    env = dict(os.environ, KSN_DUMP_KIR="1")
    for k in ("KSN_S5_TYIDX_TAB", "KSN_S5_PAT_OLD"):
        env.pop(k, None)
        if tab:
            env[k] = "1"
    r = subprocess.run([ksn, "check"], cwd=pkg, env=env, capture_output=True, text=True, timeout=600)
    return r.stderr


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--ksn", default=os.path.join(REPO, "stage0", "target", "release", "ksn"))
    a = ap.parse_args()
    bad, n = 0, 0
    for pkg in packages():
        old, new = dump(a.ksn, pkg, True), dump(a.ksn, pkg, False)
        n += 1
        if old != new:
            bad += 1
            print("DIFF", os.path.relpath(pkg, REPO))
            for l in list(difflib.unified_diff(old.splitlines(), new.splitlines(), lineterm="", n=0))[:20]:
                print("  ", l[:160])
    print(f"{n} パッケージ, 差 {bad}")
    return 1 if bad else 0


sys.exit(main())
