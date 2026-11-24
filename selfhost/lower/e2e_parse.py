#!/usr/bin/env python3
"""`selfhost/parse` を下ろした KIR で端から端まで走らせる検査 (emit → clang → 実行) を、Rust 版でビルドした同じ本と比べる。

既定は本物の `selfhost/parse` (変形なし。`parse.read_file` の `with` と `std.fs.*` も下ろす)。parse の写しを /tmp に作って
ビルドし、入力は examples・linear_ok・selfhost/parse/broken のファイル名を引数として渡す (`--kinds` の有無で 2 回)。
存在しないファイルの名前でも 1 回走らせる。標準出力と終了コードが Rust 版 (`ksn build` の実行) と一致するかを数える。

`--variant`: 4h の検査。`parse.read_file` を消し、`parse.process` が引数の文字列そのものをソースとして読む形に替える
(入力はファイルの中身を引数として渡す)。

使い方 (先に selfhost/lower と selfhost/emit を `ksn build --release` しておく):
    python3 -I e2e_parse.py [--variant]
"""
import glob
import pathlib
import shutil
import subprocess
import sys
import tempfile

HERE = pathlib.Path(__file__).resolve().parent
ROOT = HERE.parent.parent
KSN = str(ROOT / "stage0" / "target" / "release" / "ksn")
LOWER = str(HERE / "target" / "release" / "lower")
EMIT = str(ROOT / "selfhost" / "emit" / "target" / "release" / "emit")
RT_C = str(ROOT / "stage0" / "src" / "kir_rt.c")
SYS_C = str(ROOT / "stage0" / "src" / "sysrt.c")
STD = str(ROOT / "spec" / "stdlib")


def run(args, cwd=None, timeout=300):
    return subprocess.run(args, cwd=cwd, capture_output=True, timeout=timeout)


def make_variant(work):
    pkg = work / "parse"
    shutil.copytree(ROOT / "selfhost" / "parse", pkg, ignore=shutil.ignore_patterns("target", "broken", "*.py"))
    f = pkg / "src" / "parse.ksnl"
    s = f.read_text()
    a = "    let buf = parse.read_file(path)?\n    let src = buf.as_slice()\n"
    assert a in s
    s = s.replace(a, "    let src = path.as_bytes()\n")
    i = s.index("fn parse.read_file(")
    j = s.index("end parse.read_file") + len("end parse.read_file\n")
    k = s.rfind("\n\n", 0, i) + 2
    s = s[:k] + s[j:].lstrip("\n")
    f.write_text(s)
    return pkg


def main():
    variant = "--variant" in sys.argv[1:]
    work = pathlib.Path(tempfile.mkdtemp(prefix="lower-e2e-parse-"))
    try:
        if variant:
            pkg = make_variant(work)
        else:
            pkg = work / "parse"
            shutil.copytree(ROOT / "selfhost" / "parse", pkg, ignore=shutil.ignore_patterns("target", "broken", "*.py"))
        b = run([KSN, "build"], cwd=pkg)
        ref = pkg / "target" / "debug" / "parse"
        if not ref.exists():
            print("Rust 版のビルドに失敗:", b.stderr.decode(errors="replace")[-300:])
            return 2
        k = run([LOWER, str(pkg), STD, "parse.main"]).stdout.decode()
        unsup = [ln for ln in k.splitlines() if ln.startswith("unsupported")]
        if unsup:
            print("下ろせない関数:", unsup[:5])
            return 2
        (work / "p.kir").write_text(k)
        e = run([EMIT, str(work / "p.kir"), "parse.main"])
        if e.returncode != 0:
            print("emit が失敗:", e.stderr.decode(errors="replace")[-300:])
            return 2
        (work / "p.ll").write_bytes(e.stdout)
        c = run(["clang", "-O0", "-Wno-override-module", "-x", "ir", str(work / "p.ll"), "-x", "c", RT_C,
                 "-x", "c", SYS_C, "-o", str(work / "p.exe"), "-lm", "-pthread"])
        if c.returncode != 0:
            print("clang が失敗:", c.stderr.decode(errors="replace")[-300:])
            return 2
        files = sorted(glob.glob(str(ROOT / "examples" / "*" / "src" / "**" / "*.ksnl"), recursive=True)
                       + glob.glob(str(ROOT / "stage0" / "tests" / "data" / "linear_ok" / "*.ksnl"))
                       + glob.glob(str(ROOT / "selfhost" / "parse" / "broken" / "*.ksnl")))
        ok = bad = 0
        fails = []
        inputs = []
        for fn in files:
            txt = open(fn, encoding="utf-8").read()
            if len(txt.encode()) > 120000:
                continue
            inputs.append((fn, txt if variant else fn))
        if not variant:
            inputs.append(("(存在しないファイル)", str(work / "no_such_file.ksnl")))
        for fn, arg in inputs:
            for extra in ([], ["--kinds"]):
                ra = run([str(ref)] + extra + [arg], timeout=60)
                rb = run([str(work / "p.exe")] + extra + [arg], timeout=60)
                if (ra.stdout, ra.returncode) == (rb.stdout, rb.returncode):
                    ok += 1
                else:
                    bad += 1
                    fails.append((fn, extra, ra.returncode, rb.returncode))
        print("一致 %d / 実行 %d (入力 %d 本 × --kinds の有無)" % (ok, ok + bad, len(files)))
        for x in fails[:10]:
            print(" ", x)
        return 0 if bad == 0 else 1
    finally:
        shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
