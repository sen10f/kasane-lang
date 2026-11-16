#!/usr/bin/env python3
"""下ろした KIR を端から端まで走らせる検査: `lower.native` の KIR → `emit.native` → clang → 実行 を、Rust 版 `ksn build` の実行と比べる。

linear_ok の各本について t.main から呼ばれる関数が全部下ろせたものだけを対象にし (下ろせないものは「除外」に数える)、
標準出力・標準エラー・終了コードが一致するかを数える。作業場所は /tmp の一時ディレクトリ (終わりに消す)。

使い方 (先に selfhost/lower と selfhost/emit を `ksn build --release` しておく):
    python3 -I e2e.py [--jobs N]
"""
import argparse
import concurrent.futures as cf
import pathlib
import shutil
import subprocess
import tempfile

HERE = pathlib.Path(__file__).resolve().parent
ROOT = HERE.parent.parent
KSN = str(ROOT / "stage0" / "target" / "release" / "ksn")
LOWER = str(HERE / "target" / "release" / "lower.native")
EMIT = str(ROOT / "selfhost" / "emit" / "target" / "release" / "emit.native")
LINEAR = ROOT / "stage0" / "tests" / "data" / "linear_ok"
RT_C = str(ROOT / "stage0" / "src" / "kir_rt.c")
SYS_C = str(ROOT / "stage0" / "src" / "sysrt.c")
STD = str(ROOT / "spec" / "stdlib")
TOML = '[package]\nname = "t"\nversion = "0.1.0"\nedition = 1\nentry = "t.main"\n\n[dependencies]\n'


def run(args, cwd=None, timeout=120):
    return subprocess.run(args, cwd=cwd, capture_output=True, timeout=timeout)


def one(work, src):
    d = work / src.stem
    (d / "src").mkdir(parents=True)
    (d / "kasane.toml").write_text(TOML)
    (d / "kasane.lock").write_text("version = 1\n")
    shutil.copy(src, d / "src" / "t.ksnl")
    out = run([LOWER, str(d), STD, "t.main"]).stdout.decode()
    if "unsupported" in out:
        return src.stem, "unsup", ""
    run([KSN, "build"], cwd=d)
    exe = d / "target" / "debug" / "t.native"
    if not exe.exists():
        return src.stem, "refskip", ""
    ref = run([str(exe)], cwd=d, timeout=20)
    kir = work / (src.stem + ".kir")
    kir.write_text(out)
    e = run([EMIT, str(kir), "t.main"])
    if e.returncode != 0:
        return src.stem, "fail", "emit: " + e.stderr.decode(errors="replace")[-120:]
    ll = work / (src.stem + ".ll")
    ll.write_bytes(e.stdout)
    x = work / (src.stem + ".exe")
    c = run(["clang", "-O0", "-Wno-override-module", "-x", "ir", str(ll), "-x", "c", RT_C, "-x", "c", SYS_C,
             "-o", str(x), "-lm", "-pthread"])
    if c.returncode != 0:
        return src.stem, "fail", "clang: " + c.stderr.decode(errors="replace")[-120:]
    try:
        r = run([str(x)], cwd=work, timeout=20)
    except subprocess.TimeoutExpired:
        return src.stem, "fail", "timeout"
    if (r.stdout, r.stderr, r.returncode) == (ref.stdout, ref.stderr, ref.returncode):
        return src.stem, "ok", ""
    return src.stem, "fail", "differs rc %d vs %d" % (ref.returncode, r.returncode)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--jobs", type=int, default=8)
    a = ap.parse_args()
    work = pathlib.Path(tempfile.mkdtemp(prefix="lower-e2e-"))
    try:
        files = sorted(LINEAR.glob("*.ksnl"))
        with cf.ThreadPoolExecutor(max_workers=a.jobs) as ex:
            res = list(ex.map(lambda s: one(work, s), files))
    finally:
        shutil.rmtree(work, ignore_errors=True)
    ok = sum(1 for r in res if r[1] == "ok")
    fail = [r for r in res if r[1] == "fail"]
    print("一致 %d / 全部下ろせた %d (未対応で除外 %d, 参照がネイティブでない %d)" % (
        ok, ok + len(fail), sum(1 for r in res if r[1] == "unsup"), sum(1 for r in res if r[1] == "refskip")))
    for r in fail:
        print(" ", r)
    return 0 if not fail else 1


if __name__ == "__main__":
    raise SystemExit(main())
