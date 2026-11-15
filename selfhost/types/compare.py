#!/usr/bin/env python3
# 型検査の段の比較: セルフホストの types.native と Rust 版 `ksn check --json` を並べる。
#
#   python3 -I compare.py [--group examples|apps|self|linear_ok|unsound|diag|all]
#                         [--mine PATH] [--ksn PATH] [--keep]
#
# 数えるもの:
#   受理数: 正しいはずのコード (examples / apps / self / linear_ok) で、型の診断 (E20xx) が 0 件の数。
#   一致数: 型の診断を出すべきコード (diag / unsound の E20xx を含むもの) で、
#           Rust 版と (コード, 行) の多重集合が一致した数。行の不一致は別に数える。
#   E2044 (型が決まらない) は、Rust 版が出した数と自分が出した数を別に出す (取りこぼしの目安)。
# 単一ファイルの例は一時パッケージ (kasane.toml + src/t.ksnl) にして両方で検査する。
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.abspath(os.path.join(HERE, "..", ".."))
STD = os.path.join(ROOT, "spec", "stdlib")
TOML = '[package]\nname = "t"\nversion = "0.1.0"\nedition = 1\nentry = "t.main"\n'
MINE_DEFAULT = os.path.join(HERE, "target", "release", "types.native")
KSN_DEFAULT = os.path.join(ROOT, "stage0", "target", "release", "ksn")

MINE_RE = re.compile(r"^error (E\d+) (.+?):(\d+): ")


def typed(code):
    return code.startswith("E20")


def run_mine(mine, pkg):
    p = subprocess.run([mine, pkg, STD], capture_output=True, text=True, timeout=600)
    out = []
    for line in p.stdout.splitlines():
        m = MINE_RE.match(line)
        if m:
            out.append((m.group(1), int(m.group(3))))
    if p.returncode != 0:
        return None, p.stderr.strip()[:200]
    return out, ""


def run_rust(ksn, pkg):
    p = subprocess.run([ksn, "check", "--json"], cwd=pkg, capture_output=True, text=True,
                       timeout=600)
    out = []
    for line in p.stdout.splitlines():
        if not line.startswith("{"):
            continue
        try:
            d = json.loads(line)
        except ValueError:
            continue
        out.append((d.get("code"), int(d.get("line") or 0)))
    return out


def single(tmp, name, src):
    d = os.path.join(tmp, name)
    os.makedirs(os.path.join(d, "src"), exist_ok=True)
    with open(os.path.join(d, "kasane.toml"), "w") as f:
        f.write(TOML)
    with open(os.path.join(d, "kasane.lock"), "w") as f:
        f.write("version = 1\n")
    with open(os.path.join(d, "src", "t.ksnl"), "w") as f:
        f.write(src)
    return d


def cases(group):
    """(名前, パッケージのパス, 正しいはずか) の並び。単一ファイルは一時パッケージにする。"""
    out = []
    if group in ("examples", "all"):
        base = os.path.join(ROOT, "examples")
        for n in sorted(os.listdir(base)):
            d = os.path.join(base, n)
            if os.path.isfile(os.path.join(d, "kasane.toml")):
                out.append(("examples/" + n, d, True))
    if group in ("apps", "all"):
        base = os.path.join(ROOT, "apps")
        for n in sorted(os.listdir(base)):
            d = os.path.join(base, n)
            if os.path.isfile(os.path.join(d, "kasane.toml")):
                out.append(("apps/" + n, d, True))
    if group in ("self", "all"):
        for n in ("parse", "resolve", "types"):
            d = os.path.join(HERE, "..", n)
            if os.path.isfile(os.path.join(d, "kasane.toml")):
                out.append(("selfhost/" + n, os.path.abspath(d), True))
    tmp = tempfile.mkdtemp(prefix="types_cmp_")
    if group in ("linear_ok", "all"):
        base = os.path.join(ROOT, "stage0", "tests", "data", "linear_ok")
        for n in sorted(os.listdir(base)):
            if n.endswith(".ksnl"):
                with open(os.path.join(base, n)) as f:
                    src = f.read()
                out.append(("linear_ok/" + n, single(tmp, "lo_" + n[:-5], src), True))
    if group in ("diag", "all"):
        base = os.path.join(ROOT, "stage0", "tests", "data", "diag")
        for n in sorted(os.listdir(base)):
            if n.startswith("E20") and n.endswith(".ksnl"):
                with open(os.path.join(base, n)) as f:
                    src = f.read()
                out.append(("diag/" + n, single(tmp, "dg_" + n[:-5], src), False))
    if group in ("unsound", "all"):
        base = os.path.join(ROOT, "stage0", "tests", "data", "unsound")
        for n in sorted(os.listdir(base)):
            if n.endswith(".ksnl"):
                with open(os.path.join(base, n)) as f:
                    src = f.read()
                out.append(("unsound/" + n, single(tmp, "us_" + n[:-5], src), False))
    return out, tmp


def main(argv):
    group = "all"
    mine = MINE_DEFAULT
    ksn = KSN_DEFAULT
    keep = False
    i = 0
    while i < len(argv):
        a = argv[i]
        if a == "--group":
            group = argv[i + 1]
            i += 2
            continue
        if a == "--mine":
            mine = argv[i + 1]
            i += 2
            continue
        if a == "--ksn":
            ksn = argv[i + 1]
            i += 2
            continue
        if a == "--keep":
            keep = True
        i += 1
    items, tmp = cases(group)
    t0 = time.time()
    stats = {"n": 0, "accepted_ok": 0, "correct_n": 0, "rust_rej": 0, "mine_rej": 0,
             "typed_match": 0, "typed_code_match": 0, "typed_total": 0,
             "e2044_rust": 0, "e2044_mine": 0, "errors": 0}
    detail = []
    for name, pkg, correct in items:
        stats["n"] += 1
        rust = run_rust(ksn, pkg)
        mine_out, err = run_mine(mine, pkg)
        if mine_out is None:
            stats["errors"] += 1
            detail.append(("CRASH", name, err))
            continue
        r_typed = sorted((c, l) for c, l in rust if c and typed(c))
        m_typed = sorted((c, l) for c, l in mine_out if typed(c))
        r_e44 = sum(1 for c, _ in rust if c == "E2044")
        m_e44 = sum(1 for c, _ in mine_out if c == "E2044")
        stats["e2044_rust"] += r_e44
        stats["e2044_mine"] += m_e44
        if correct:
            stats["correct_n"] += 1
            if not m_typed:
                stats["accepted_ok"] += 1
            else:
                detail.append(("FALSE-REJECT", name, m_typed[:3]))
        if r_typed:
            stats["typed_total"] += 1
            if r_typed == m_typed:
                stats["typed_match"] += 1
            if sorted(c for c, _ in r_typed) == sorted(c for c, _ in m_typed):
                stats["typed_code_match"] += 1
            else:
                detail.append(("TYPED-DIFF", name, (r_typed[:4], m_typed[:4])))
        if m_typed:
            stats["mine_rej"] += 1
        if not keep and tmp and name.startswith(("diag/", "linear_ok/", "unsound/")):
            pass
    secs = time.time() - t0
    if not keep:
        shutil.rmtree(tmp, ignore_errors=True)
    for d in detail[:200]:
        print("  ".join(str(x) for x in d))
    print(f"cases {stats['n']}  correct-set {stats['correct_n']}  "
          f"accepted {stats['accepted_ok']}/{stats['correct_n']}")
    print(f"rust-typed-cases {stats['typed_total']}  exact (code,line) match "
          f"{stats['typed_match']}  code-only match {stats['typed_code_match']}")
    print(f"mine-rejected {stats['mine_rej']}  rust-E2044 {stats['e2044_rust']}  "
          f"mine-E2044 {stats['e2044_mine']}  crashes {stats['errors']}  "
          f"time {secs:.1f}s")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
