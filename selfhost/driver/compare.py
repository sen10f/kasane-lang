#!/usr/bin/env python3
# ksnc の段の Kasane 版と Rust 版の比較 (`--stages=kasane` と `--stages=rust`)。
#
#   python3 -I compare.py check [--pkgs all|examples|apps|selfhost]   検査: 診断の一致 (examples/apps/selfhost の各パッケージ)
#   python3 -I compare.py fmt   [--pkgs ...]                          整形: 書き換えの要否と出力の一致 (--check で、書き換えはしない)
#   python3 -I compare.py build [--work DIR]                          下ろし・LLVM・clang: linear_ok の各ファイルを作り、実行の出力を Rust 版と比べる
#
# 前提: `KSNC_KSN` (既定 stage0/target/release/ksn) と、ksnc の実行ファイル (既定 selfhost/driver/target/release/ksnc)。
# 作る前に: cd selfhost/driver && ksn build --release
# 一時の作業場所は既定で /tmp/ksnc_stages_work (--work で変える)。リポジトリの中は書かない (fmt も --check のみ)。
import argparse
import collections
import concurrent.futures as cf
import glob
import os
import pathlib
import re
import shutil
import subprocess
import sys

HERE = pathlib.Path(__file__).resolve().parent
ROOT = HERE.parent.parent
KSN = os.environ.get("KSNC_KSN", str(ROOT / "stage0" / "target" / "release" / "ksn"))
KSNC = os.environ.get("KSNC", str(HERE / "target" / "release" / "ksnc"))
LINEAR_OK = ROOT / "stage0" / "tests" / "data" / "linear_ok"
TOML = ('[package]\nname = "t"\nversion = "0.1.0"\nedition = 1\nentry = "t.main"\n\n[dependencies]\n')
ENV = dict(os.environ, KSNC_KSN=KSN, KSNC_ROOT=str(ROOT))


def run(args, cwd=None, timeout=600, env=None):
    # ksnc は作業ディレクトリを PWD から得る (cwd= だけでは PWD が変わらない) ので、PWD も合わせる。
    e = dict(env or ENV)
    if cwd is not None:
        e["PWD"] = str(cwd)
    p = subprocess.run(args, cwd=cwd, capture_output=True, text=True, timeout=timeout, env=e)
    return p.returncode, p.stdout, p.stderr


def packages(group):
    """kasane.toml のあるパッケージ (target/ と broken/ と tests/ の中は除く)。"""
    roots = {"examples": ["examples"], "apps": ["apps"], "selfhost": ["selfhost"]}
    dirs = []
    for g, rs in roots.items():
        if group not in ("all", g):
            continue
        for r in rs:
            for t in glob.glob(str(ROOT / r / "**" / "kasane.toml"), recursive=True):
                d = pathlib.Path(t).parent
                parts = d.relative_to(ROOT).parts
                if "target" in parts or "broken" in parts or "tests" in parts:
                    continue
                dirs.append(d)
    return sorted(set(dirs))


# ---- 検査 ---------------------------------------------------------------

RUST_RE = re.compile(r"^(\S+?):(\d+):(\d+): (E\d{4})\b")
KASANE_RE = re.compile(r"^error (E\d{4}) (.+?):(\d+): ")


def rel_of(pkg, path):
    p = path.replace("\\", "/")
    base = str(pkg).rstrip("/") + "/"
    if p.startswith(base):
        p = p[len(base):]
    while p.startswith("./"):
        p = p[2:]
    return p


def rust_diags(pkg):
    """Rust 版 `ksn check` の診断 (コード・ファイル・行) の多重集合。"""
    _, out, err = run([KSN, "check"], cwd=pkg)
    c = collections.Counter()
    for ln in (out + err).splitlines():
        m = RUST_RE.match(ln)
        if m:
            c[(m.group(4), rel_of(pkg, m.group(1)), int(m.group(2)))] += 1
    return c


def kasane_diags(pkg):
    """`ksnc --stages=kasane check` の診断 (同じ形の多重集合)。ksnc の終了コードと併せる。"""
    _, out, err = run([KSNC, "--stages=kasane", "check"], cwd=pkg)
    c = collections.Counter()
    for ln in (out + err).splitlines():
        m = KASANE_RE.match(ln)
        if m:
            c[(m.group(1), rel_of(pkg, m.group(2)), int(m.group(3)))] += 1
    return c


def one_check(pkg):
    r = rust_diags(pkg)
    k = kasane_diags(pkg)
    same = r == k
    return (pkg, same, sum(r.values()), sum(k.values()), r, k)


def cmd_check(args):
    pk = packages(args.pkgs)
    with cf.ThreadPoolExecutor(max_workers=args.jobs) as ex:
        res = list(ex.map(one_check, pk))
    n_same = 0
    n_rust_zero = 0
    n_both_zero = 0
    for pkg, same, nr, nk, r, k in res:
        if same:
            n_same += 1
        if nr == 0:
            n_rust_zero += 1
            if nk == 0:
                n_both_zero += 1
        if args.v or not same:
            tag = "一致" if same else "不一致"
            print("%-6s %-44s rust=%d kasane=%d" % (tag, str(pkg.relative_to(ROOT)), nr, nk))
            if not same:
                for key in sorted(set(r) | set(k)):
                    if r[key] != k[key]:
                        print("       %s rust=%d kasane=%d %s" % (
                            "" , r[key], k[key], "%s %s:%d" % key))
    print("検査: パッケージ %d / 一致 %d / Rust 版が 0 件 %d (うち Kasane 版も 0 件 %d)" % (
        len(res), n_same, n_rust_zero, n_both_zero))
    return 0 if n_same == len(res) else 1


# ---- 整形 ---------------------------------------------------------------

FMT_RE = re.compile(r"^(?:would reformat|reformatted) (.+)$")


def rust_fmt(pkg):
    """`ksn fmt --check` で書き換えの要るファイル (src からの相対) と、Rust 版の終了コード。"""
    rc, out, err = run([KSN, "fmt", "--check"], cwd=pkg)
    files = set()
    for ln in (out + err).splitlines():
        m = FMT_RE.match(ln)
        if m:
            files.add(rel_of(pkg, m.group(1)))
    return files


def kasane_fmt(pkg):
    rc, out, err = run([KSNC, "--stages=kasane", "fmt", "--check"], cwd=pkg)
    files = set()
    for ln in (out + err).splitlines():
        m = FMT_RE.match(ln)
        if m:
            files.add(rel_of(pkg, m.group(1)))
    return files, rc


def cmd_fmt(args):
    pk = packages(args.pkgs)
    ok = 0
    for pkg in pk:
        rf = rust_fmt(pkg)
        kf, rc = kasane_fmt(pkg)
        same = rf == kf
        ok += 1 if same else 0
        if args.v or not same:
            print("%-6s %-44s rust=%d kasane=%d %s" % (
                "一致" if same else "不一致", str(pkg.relative_to(ROOT)), len(rf), len(kf),
                "" if same else "rust-only=%s kasane-only=%s" % (sorted(rf - kf), sorted(kf - rf))))
    print("整形 (--check): パッケージ %d / 書き換えの要否が一致 %d" % (len(pk), ok))
    return 0


# ---- ビルド ---------------------------------------------------------------

def make_pkg(work, src):
    d = work / src.stem
    if d.exists():
        shutil.rmtree(d)
    (d / "src").mkdir(parents=True)
    (d / "kasane.toml").write_text(TOML)
    shutil.copy(src, d / "src" / "t.ksnl")
    return d


def run_exe(exe):
    try:
        p = subprocess.run([str(exe)], capture_output=True, text=True, timeout=10,
                           stdin=subprocess.DEVNULL)
        return (p.returncode, p.stdout, p.stderr)
    except subprocess.TimeoutExpired:
        return ("timeout", "", "")


def one_build(work, src):
    pkg = make_pkg(work, src)
    # Rust 版 (基準)。
    rc, out, err = run([KSN, "build", "--release"], cwd=pkg, timeout=900)
    ref_exe = pkg / "target" / "release" / "t"
    if rc != 0 or not ref_exe.exists():
        return (src.stem, "ref-fail", None, None)
    ref = run_exe(ref_exe)
    # Kasane 版 (ksnc build --stages=kasane --release)。
    rc2, out2, err2 = run([KSNC, "--stages=kasane", "build", "--release"], cwd=pkg, timeout=900)
    kex = pkg / "target" / "kasane" / "release" / "t.native"
    if rc2 != 0 or not kex.exists():
        lines = [x for x in (out2 + err2).splitlines() if x.startswith("unsupported ")]
        if not lines:
            lines = [x for x in (out2 + err2).strip().splitlines()[-1:]]
        # 理由の分類: 「FQN: 理由」の理由の部分 (括弧の中の補足は除く)。
        cats = sorted({x.split(": ", 1)[1].split(" (")[0] if ": " in x else x for x in lines})
        return (src.stem, "kasane-fail", cats, None)
    mine = run_exe(kex)
    if mine == ref:
        return (src.stem, "same", None, None)
    return (src.stem, "differs", (ref, mine), None)


def cmd_build(args):
    work = pathlib.Path(args.work)
    work.mkdir(parents=True, exist_ok=True)
    names = sorted(LINEAR_OK.glob("*.ksnl"))
    if args.limit:
        names = names[: args.limit]
    res = []
    with cf.ThreadPoolExecutor(max_workers=args.jobs) as ex:
        futs = [ex.submit(one_build, work / "pkg", s) for s in names]
        for f in futs:
            res.append(f.result())
    c = collections.Counter(r[1] for r in res)
    print("linear_ok: 全 %d 件" % len(res))
    print("  Rust 版が作れなかった (基準なし): %d" % c["ref-fail"])
    print("  Kasane 版が作れた: %d" % (c["same"] + c["differs"]))
    print("  Kasane 版が作れた件のうち実行の出力・終了コードが Rust 版と一致: %d" % c["same"])
    print("  作れなかった (Kasane 版): %d" % c["kasane-fail"])
    reasons = collections.Counter()
    for r in res:
        if r[1] == "kasane-fail" and r[2]:
            for cat in r[2]:
                reasons[cat] += 1
        if r[1] == "differs" and args.v:
            print("differs", r[0], r[2])
    if args.v:
        for r in res:
            if r[1] == "kasane-fail":
                print("kasane-fail", r[0], r[2])
    print("  下ろせなかった関数の理由 (件数は、その理由を含むファイルの数):")
    for k, v in reasons.most_common(25):
        print("  %3d: %s" % (v, k))
    return 0


def main():
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    a = sub.add_parser("check")
    a.add_argument("--pkgs", default="all")
    a.add_argument("--jobs", type=int, default=8)
    a.add_argument("-v", action="store_true")
    a.set_defaults(fn=cmd_check)
    b = sub.add_parser("fmt")
    b.add_argument("--pkgs", default="all")
    b.add_argument("-v", action="store_true")
    b.set_defaults(fn=cmd_fmt)
    c = sub.add_parser("build")
    c.add_argument("--work", default="/tmp/ksnc_stages_work")
    c.add_argument("--jobs", type=int, default=8)
    c.add_argument("--limit", type=int, default=0)
    c.add_argument("-v", action="store_true")
    c.set_defaults(fn=cmd_build)
    args = ap.parse_args()
    return args.fn(args)


if __name__ == "__main__":
    sys.exit(main())
