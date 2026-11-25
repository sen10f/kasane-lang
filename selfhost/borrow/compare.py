#!/usr/bin/env python3
# 借用・線形性の段の比較: セルフホストの borrow と Rust 版 `ksn check --json` を並べる。
#
#   python3 -I compare.py [--group examples|apps|self|linear_ok|unsound|diag|all]
#                         [--mine PATH] [--ksn PATH] [--keep]
#
# 数えるもの (E30xx = 借用・線形性の診断。E3001〜E3050):
#   正しいコード (examples / apps / self / linear_ok):
#     誤検出: 自分が E30xx を出した件数 (0 でなければならない)。
#     受理: 誤検出 0 かつ判断保留 (undec) 0 の件数。
#   拒否すべきコード (unsound / diag で Rust 版が E30xx を出すもの):
#     拒否: 自分が E30xx を一つでも出した件数。
#     一致: 自分の E30xx の (コード) の多重集合が Rust 版と同じ件数。
#     余分: Rust 版が E30xx を出さないのに自分が出した件数 (誤検出の目安)。
# 単一ファイルの例は一時パッケージ (kasane.toml + src/t.ksnl) にして両方で検査する。
# 自分の検査器は `ksn build --release` の成果物 (target/release/borrow) を使う。
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
MINE_DEFAULT = os.path.join(HERE, "target", "release", "borrow")
KSN_DEFAULT = os.path.join(ROOT, "stage0", "target", "release", "ksn")
SUM_RE = re.compile(r"^summary errors (\d+) undec (\d+) files (\d+)")
DIAG_RE = re.compile(r"^error (E\d+) (.+?):(\d+): ")


def e30(code):
    return bool(code) and code.startswith("E30")


def run_mine(mine, pkg):
    p = subprocess.run([mine, pkg, STD], capture_output=True, text=True, timeout=600)
    if p.returncode != 0:
        return None, 0, p.stderr.strip()[:200]
    out = []
    undec = 0
    for line in p.stdout.splitlines():
        m = DIAG_RE.match(line)
        if m:
            out.append((m.group(1), int(m.group(3))))
        m = SUM_RE.match(line)
        if m:
            undec = int(m.group(2))
    return out, undec, ""


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


def read_text(path):
    with open(path, encoding="utf-8") as f:
        return f.read()


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
        for n in ("parse", "resolve", "types", "effects", "borrow"):
            d = os.path.join(HERE, "..", n)
            if os.path.isfile(os.path.join(d, "kasane.toml")):
                out.append(("selfhost/" + n, os.path.abspath(d), True))
    tmp = tempfile.mkdtemp(prefix="borrow_cmp_")
    data = os.path.join(ROOT, "stage0", "tests", "data")
    if group in ("linear_ok", "all"):
        base = os.path.join(data, "linear_ok")
        for n in sorted(os.listdir(base)):
            if n.endswith(".ksnl"):
                out.append(("linear_ok/" + n, single(tmp, "lo_" + n[:-5], read_text(os.path.join(base, n))), True))
    if group in ("diag", "all"):
        base = os.path.join(data, "diag")
        for n in sorted(os.listdir(base)):
            if n.startswith("E30") and n.endswith(".ksnl"):
                out.append(("diag/" + n, single(tmp, "dg_" + n[:-5], read_text(os.path.join(base, n))), False))
    if group in ("unsound", "all"):
        base = os.path.join(data, "unsound")
        for n in sorted(os.listdir(base)):
            if n.endswith(".ksnl"):
                out.append(("unsound/" + n, single(tmp, "us_" + n[:-5], read_text(os.path.join(base, n))), False))
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
    st = {"n": 0, "correct": 0, "false_rej": 0, "accepted": 0, "undec_total": 0,
          "rej_rust": 0, "rej_mine": 0, "exact": 0, "over": 0, "missed": 0, "crash": 0}
    detail = []
    for name, pkg, correct in items:
        st["n"] += 1
        rust = [(c, l) for c, l in run_rust(ksn, pkg) if e30(c)]
        mine_out, undec, err = run_mine(mine, pkg)
        if mine_out is None:
            st["crash"] += 1
            detail.append(("CRASH", name, err))
            continue
        mine_e = [(c, l) for c, l in mine_out if e30(c)]
        if correct:
            st["correct"] += 1
            st["undec_total"] += undec
            if mine_e:
                st["false_rej"] += 1
                detail.append(("FALSE-REJECT", name, mine_e[:3]))
            elif undec == 0:
                st["accepted"] += 1
            else:
                detail.append(("UNDECIDED", name, undec))
            continue
        if rust:
            st["rej_rust"] += 1
            if mine_e:
                st["rej_mine"] += 1
            if sorted(c for c, _ in rust) == sorted(c for c, _ in mine_e):
                st["exact"] += 1
            else:
                st["missed"] += 1
                detail.append(("DIFF", name, (sorted(c for c, _ in rust)[:4],
                                              sorted(c for c, _ in mine_e)[:4])))
        elif mine_e:
            st["over"] += 1
            detail.append(("OVER-REJECT", name, mine_e[:3]))
    secs = time.time() - t0
    if not keep:
        shutil.rmtree(tmp, ignore_errors=True)
    for d in detail[:300]:
        print("  ".join(str(x) for x in d))
    print(f"cases {st['n']}  correct-set {st['correct']}  false-reject {st['false_rej']}  "
          f"accepted (no E30, no undec) {st['accepted']}/{st['correct']}  "
          f"undec total {st['undec_total']}")
    print(f"rust-E30-cases {st['rej_rust']}  mine-rejects {st['rej_mine']}  "
          f"exact (code multiset) {st['exact']}  diff {st['missed']}  over-reject {st['over']}")
    print(f"crashes {st['crash']}  time {secs:.1f}s")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
