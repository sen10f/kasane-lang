#!/usr/bin/env python3
# KIR への下ろしの比較: 自己ホストの lower (Kasane 製、`ksn run` で動く) と Rust 版 `ksn kir FQN` の関数ごとの本文を並べる。
# 比べる前に、関数の中の値 (%N)・ブロック (bN)・集成体 (agg#N) の番号を、出た順の番号に直す (名前の付け方だけの違いを消す)。
# 一致 = 正規化した本文が同じ。未対応 = lower が「unsupported」と出した関数 (本文なし)。
#
# 使い方: python3 -I compare.py [--group probe|examples|linear_ok|all] [--jobs N] [-v]
#   --mine native|interp  既定は interp (`ksn run`)。native は `target/release/lower.native` を使う (作れていれば)。
import argparse
import concurrent.futures as cf
import os
import re
import shutil
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.abspath(os.path.join(HERE, "..", ".."))
KSN = os.path.join(ROOT, "stage0", "target", "release", "ksn")
STD = os.path.join(ROOT, "spec", "stdlib")
TOML = ('[package]\nname = "t"\nversion = "0.1.0"\nedition = 1\nentry = "t.main"\n\n[dependencies]\n')


def run(cmd, cwd=None, timeout=900):
    p = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, timeout=timeout)
    return p.returncode, p.stdout, p.stderr


def defs(pkg):
    """パッケージの関数の FQN の並び (src/ の各 .ksnl を `ksn defs FILE` で読む)。"""
    res = []
    src = os.path.join(pkg, "src")
    for dp, _, fs in os.walk(src):
        for fn in sorted(fs):
            if not fn.endswith(".ksnl"):
                continue
            rc, out, _ = run([KSN, "defs", os.path.join(dp, fn)], cwd=pkg)
            for ln in out.splitlines():
                f = ln.split("\t")
                if len(f) >= 2 and f[0] == "fn":
                    res.append(f[1])
    return sorted(set(res))


def split_fns(text):
    """KIR のテキストから関数の本文 {FQN: 行の並び} を取り出す (`fn FQN ...` から `end FQN` まで)。"""
    out = {}
    cur = None
    buf = []
    for ln in text.splitlines():
        if cur is None:
            m = re.match(r"^fn (\S+) ", ln)
            if m:
                cur = m.group(1)
                buf = [ln]
            continue
        buf.append(ln)
        if ln == "end " + cur:
            if cur not in out:
                out[cur] = buf
            cur = None
    return out


def norm(lines):
    """関数の本文の番号を、出た順に直す (値・ブロック・集成体)。"""
    text = "\n".join(lines)
    maps = {"%": {}, "b": {}, "agg#": {}}

    def rep(prefix, pat):
        def f(m):
            k = m.group(1)
            d = maps[prefix]
            if k not in d:
                d[k] = len(d)
            if prefix == "agg#":
                return "agg#" + str(d[k])
            if prefix == "%":
                return "%v" + str(d[k])
            return "b" + str(d[k])
        return f

    # 出た順に番号を振る (全体の走査の順)。
    text = re.sub(r"%(\d+)", rep("%", None), text)
    text = re.sub(r"\bb(\d+)\b", rep("b", None), text)
    text = re.sub(r"agg#(\d+)", rep("agg#", None), text)
    return text


def mine_fns(pkg, fqns, mode):
    if mode == "native":
        exe = os.path.join(HERE, "target", "release", "lower.native")
        cmd = [exe, pkg, STD] + fqns
        cwd = None
    else:
        cmd = [KSN, "run", pkg, STD] + fqns
        cwd = HERE
    rc, out, err = run(cmd, cwd=cwd)
    unsup = {}
    for ln in out.splitlines():
        m = re.match(r"^unsupported (\S+): (.*)$", ln)
        if m:
            unsup[m.group(1)] = m.group(2)
    kir = "\n".join(ln for ln in out.splitlines() if not ln.startswith("unsupported "))
    return split_fns(kir), unsup, rc, err


def rust_fns(pkg, fqns):
    fns = {}
    fails = {}
    for q in fqns:
        rc, out, err = run([KSN, "kir", q], cwd=pkg)
        if rc != 0:
            fails[q] = err.strip()[:200]
            continue
        for k, v in split_fns(out).items():
            fns.setdefault(k, v)
    return fns, fails


def compare_pkg(pkg, mode, label):
    fqns = defs(pkg)
    if not fqns:
        return label, 0, 0, 0, 0, "定義がない"
    rust, rfail = rust_fns(pkg, fqns)
    mine, unsup, rc, err = mine_fns(pkg, fqns, mode)
    total = 0
    match = 0
    ok = 0
    diffs = []
    for f in fqns:
        if f not in rust:
            continue
        total += 1
        if f in mine:
            ok += 1
            if norm(mine[f]) == norm(rust[f]):
                match += 1
            else:
                diffs.append(f)
    note = ""
    if rc != 0:
        note = "lower が失敗 (rc=%d): %s" % (rc, err.strip()[-200:])
    return label, match, total, ok, len(unsup), note + (" 差: " + ",".join(diffs[:5]) if diffs else "")


def pkgs_probe():
    return [("probe", os.path.join(HERE, "probes", "probe"))]


def pkgs_examples():
    d = os.path.join(ROOT, "examples")
    out = []
    for name in sorted(os.listdir(d)):
        p = os.path.join(d, name)
        if os.path.isfile(os.path.join(p, "kasane.toml")):
            out.append((name, p))
    return out


def pkgs_linear_ok(tmp):
    d = os.path.join(ROOT, "stage0", "tests", "data", "linear_ok")
    out = []
    for name in sorted(os.listdir(d)):
        if not name.endswith(".ksnl"):
            continue
        p = os.path.join(tmp, name[:-5])
        os.makedirs(os.path.join(p, "src"), exist_ok=True)
        with open(os.path.join(p, "kasane.toml"), "w") as f:
            f.write(TOML)
        shutil.copy(os.path.join(d, name), os.path.join(p, "src", "t.ksnl"))
        out.append((name, p))
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--group", default="probe", choices=["probe", "examples", "linear_ok", "all"])
    ap.add_argument("--mine", default="interp", choices=["interp", "native"])
    ap.add_argument("--jobs", type=int, default=12)
    ap.add_argument("-v", action="store_true")
    a = ap.parse_args()
    tmp = tempfile.mkdtemp(prefix="lower-cmp-")
    groups = []
    if a.group in ("probe", "all"):
        groups += pkgs_probe()
    if a.group in ("examples", "all"):
        groups += pkgs_examples()
    if a.group in ("linear_ok", "all"):
        groups += pkgs_linear_ok(tmp)
    T = M = O = U = 0
    rows = []
    with cf.ThreadPoolExecutor(max_workers=a.jobs) as ex:
        futs = [ex.submit(compare_pkg, p, a.mine, n) for n, p in groups]
        for fu in cf.as_completed(futs):
            label, match, total, ok, nu, note = fu.result()
            rows.append((label, match, total, ok, nu, note))
    rows.sort()
    for label, match, total, ok, nu, note in rows:
        if a.v or match != total:
            print("%-32s 一致 %3d / 関数 %3d  (下ろせた %3d, 未対応 %3d) %s" % (label, match, total, ok, nu, note))
        T += total
        M += match
        O += ok
        U += nu
    print("合計: 一致 %d / 全体 %d (下ろせた %d, 未対応の理由 %d)" % (M, T, O, U))
    shutil.rmtree(tmp, ignore_errors=True)


if __name__ == "__main__":
    main()
