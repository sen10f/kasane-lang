#!/usr/bin/env python3
"""Kasane 版の効果・panic・再帰の検査 (selfhost/effects) と Rust 版 `ksn check --json` を比べる。

使い方 (リポジトリのルートから):
  python3 -I selfhost/effects/compare.py [--group clean|diag|unsound|all] [-v]
  （先に `cd selfhost/effects && ksn build --release` で target/release/effects を作る）

比べるのは E4001 E4002 E4003 E4004 E4010 E4013 だけ (効果・panic・再帰・再帰する型の Drop の分野)。
- clean: examples/*・apps/*・selfhost/{parse,resolve,effects}・stage0/tests/data/linear_ok/*.ksnl。
  Kasane 版が診断 0 件であること、Rust 版も 0 件であることを確かめる。
- diag: stage0/tests/data/diag/E40*.ksnl (1 ファイル 1 ケース)。
- unsound: stage0/tests/data/unsound/*.ksnl のうち、Rust 版が E4001〜E4010・E4013 のどれかで拒否するもの。

1 ファイルずつ判定する (`src/t.ksnl` の 1 パッケージに置く。接頭辞は `t`):
  一致   Kasane 版の E40 の集合が Rust 版と同じ
  判断できない  一致せず、Kasane 版が型の要る形 (und_* > 0) を数えたファイル
  取りこぼし  一致せず、判断できない形も無く、何も出なかったファイル
  余分   Rust 版に無いコードを Kasane 版が出したファイル (偽陽性)
"""
import glob
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.abspath(os.path.join(HERE, "..", ".."))
KSN = os.path.join(ROOT, "stage0", "target", "release", "ksn")
EFF = os.path.join(HERE, "target", "release", "effects")
STD = os.path.join(ROOT, "spec", "stdlib")
SET = {"E4001", "E4002", "E4003", "E4004", "E4010", "E4013"}
UND_KEYS = ["und_value", "und_method", "und_infer", "und_var", "und_missing",
            "und_closure", "und_op", "und_rec"]

TOML = '[package]\nname = "t"\nversion = "0.1.0"\nedition = 1\nentry = "t.main"\n'


def rust_codes(pkg):
    """Rust 版 `ksn check --json` の診断コードの一覧 (E40 の分野だけでなく全件)。"""
    r = subprocess.run([KSN, "check", "--json"], cwd=pkg, capture_output=True, text=True, timeout=600)
    codes = []
    for line in (r.stdout or "").splitlines():
        line = line.strip()
        if line.startswith("{"):
            try:
                codes.append(json.loads(line)["code"])
            except (ValueError, KeyError):
                pass
    return codes


def mine(pkg):
    """Kasane 版の結果: (E40 の診断コードの一覧, 集計の辞書, 生の出力)。"""
    r = subprocess.run([EFF, pkg, STD], capture_output=True, text=True, timeout=600)
    out = r.stdout or ""
    codes = []
    summ = {k: 0 for k in UND_KEYS}
    for line in out.splitlines():
        parts = line.split()
        if len(parts) >= 2 and parts[0] == "error" and parts[1].startswith("E40"):
            codes.append(parts[1])
        if parts and parts[0] == "summary":
            vals = dict(zip(parts[1::2], parts[2::2]))
            for k in UND_KEYS:
                summ[k] = int(vals.get(k, "0"))
            summ["errors"] = int(vals.get("errors", "0"))
            summ["ok"] = int(vals.get("ok", "0"))
    if r.returncode != 0 and "summary" not in out:
        raise RuntimeError("effects failed on %s: %s" % (pkg, (r.stderr or out)[-400:]))
    return codes, summ, out


def single_file(path):
    """1 ファイルを `src/t.ksnl` に置いた一時パッケージを作る (戻り値はそのパス)。"""
    d = tempfile.mkdtemp(prefix="effchk-")
    os.makedirs(os.path.join(d, "src"))
    with open(os.path.join(d, "kasane.toml"), "w", encoding="utf-8") as f:
        f.write(TOML)
    with open(os.path.join(d, "kasane.lock"), "w", encoding="utf-8") as f:
        f.write("version = 1\n")
    shutil.copyfile(path, os.path.join(d, "src", "t.ksnl"))
    return d


def check_one(pkg, verbose, label):
    rc = rust_codes(pkg)
    mc, summ, out = mine(pkg)
    r_s = sorted(c for c in rc if c in SET)
    m_s = sorted(mc)
    und = sum(summ[k] for k in UND_KEYS)
    if verbose:
        print("== %s\n   rust=%s\n   mine=%s und=%d" % (label, r_s, m_s, und))
    return r_s, m_s, und, summ, out


def run_single(files, verbose, tag):
    res = {"一致": 0, "判断できない": 0, "取りこぼし": 0, "余分": 0}
    miss, extra, undec = [], [], []
    for f in files:
        d = single_file(f)
        try:
            r_s, m_s, und, _s, _o = check_one(d, verbose, os.path.relpath(f, ROOT))
        finally:
            shutil.rmtree(d, ignore_errors=True)
        name = os.path.relpath(f, ROOT)
        if set(m_s) - set(r_s):
            res["余分"] += 1
            extra.append((name, r_s, m_s))
        if sorted(m_s) == r_s and not (set(m_s) - set(r_s)):
            res["一致"] += 1
        elif not (set(m_s) - set(r_s)):
            if und > 0:
                res["判断できない"] += 1
                undec.append((name, r_s, m_s, und))
            else:
                res["取りこぼし"] += 1
                miss.append((name, r_s, m_s))
    n = len(files)
    print("[%s] %d ファイル: 一致 %d / 判断できない %d / 取りこぼし %d / 余分 %d" % (
        tag, n, res["一致"], res["判断できない"], res["取りこぼし"], res["余分"]))
    for name, r, m in miss:
        print("  取りこぼし %s rust=%s mine=%s" % (name, r, m))
    for name, r, m in extra:
        print("  余分 %s rust=%s mine=%s" % (name, r, m))
    if verbose:
        for name, r, m, u in undec:
            print("  判断できない %s rust=%s mine=%s und=%d" % (name, r, m, u))
    return res, n


def run_clean(verbose):
    """examples・apps・selfhost のパッケージと linear_ok: Kasane 版も Rust 版も診断 0 件。"""
    pkgs = []
    for pat in ["examples/*", "apps/*", "selfhost/parse", "selfhost/resolve", "selfhost/effects"]:
        for p in sorted(glob.glob(os.path.join(ROOT, pat))):
            if os.path.isfile(os.path.join(p, "kasane.toml")):
                pkgs.append(p)
    bad = []
    und_total = 0
    for p in pkgs:
        rc = rust_codes(p)
        mc, summ, _o = mine(p)
        und = sum(summ[k] for k in UND_KEYS)
        und_total += und
        rs = [c for c in rc if c in SET]
        if mc or rs:
            bad.append((os.path.relpath(p, ROOT), rs, mc))
        if verbose:
            print("pkg %s rust=%s mine=%s und=%d" % (os.path.relpath(p, ROOT), rs, mc, und))
    print("[clean] パッケージ %d: 診断の不一致 %d (Kasane 版の判断できない形 %d)" % (len(pkgs), len(bad), und_total))
    for b in bad:
        print("  %s rust=%s mine=%s" % b)
    lo = sorted(glob.glob(os.path.join(ROOT, "stage0", "tests", "data", "linear_ok", "*.ksnl")))
    res, n = run_single(lo, verbose, "linear_ok")
    return len(bad), res, n


def main(argv):
    group = "all"
    verbose = "-v" in argv
    if "--group" in argv:
        group = argv[argv.index("--group") + 1]
    if not os.path.isfile(EFF):
        print("effects が無い: cd selfhost/effects && ksn build --release", file=sys.stderr)
        return 2
    t0 = time.time()
    if group in ("clean", "all"):
        run_clean(verbose)
    if group in ("diag", "all"):
        files = sorted(glob.glob(os.path.join(ROOT, "stage0", "tests", "data", "diag", "E40*.ksnl")))
        # 対象は E4001〜E4010 と E4013 (効果・panic・再帰・再帰する型の Drop)。E4011・E4012・E4021 は別の検査。
        files = [f for f in files if os.path.basename(f)[:5] in SET]
        run_single(files, verbose, "diag E40")
    if group in ("unsound", "all"):
        allf = sorted(glob.glob(os.path.join(ROOT, "stage0", "tests", "data", "unsound", "*.ksnl")))
        sel = []
        for f in allf:
            d = single_file(f)
            try:
                rc = rust_codes(d)
            finally:
                shutil.rmtree(d, ignore_errors=True)
            if any(c in SET for c in rc):
                sel.append(f)
        print("[unsound] Rust 版が E4001〜E4010・E4013 で拒否するもの: %d / %d" % (len(sel), len(allf)))
        run_single(sel, verbose, "unsound 効果・panic")
    print("elapsed %.1f s" % (time.time() - t0))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
