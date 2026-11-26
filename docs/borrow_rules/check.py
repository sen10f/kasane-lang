#!/usr/bin/env python3
"""docs/borrow_rules の全サンプルを ksn check で検証する。

各 `<rule>_{ok,ng}.ksnl` に対して、同ディレクトリの `<rule>_{ok,ng}.expect`
の 1 行目を読む：
  - `ok`      : `ksn check --json` が診断なしで成功することを期待する
  - `Exxxx`   : `ksn check --json` が失敗し、コード `Exxxx` の診断を 1 件以上出すことを期待する
  - 2 行目が `unsafe` のとき：`kasane.toml` に `[unsafe] modules = ["t"]` を追加する

Python 3 標準ライブラリのみを使用し、`python3 -I check.py` でも動く。
"""
import glob
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path

KSN = os.environ.get("KSN", str(__import__("pathlib").Path(__file__).resolve().parents[2] / "stage0/target/release/ksn"))

BASE = Path(__file__).resolve().parent

TOML_STANDARD = """[package]
name = "t"
version = "0.1.0"
edition = 1
entry = "t.main"
[dependencies]
"""

TOML_UNSAFE = """[package]
name = "t"
version = "0.1.0"
edition = 1
entry = "t.main"

[unsafe]
modules = ["t"]

[dependencies]
"""

LOCK = "version = 1\n"


def write_package(td: Path, src_text: str, unsafe: bool) -> None:
    (td / "kasane.toml").write_text(TOML_UNSAFE if unsafe else TOML_STANDARD, encoding="utf-8", newline="\n")
    (td / "kasane.lock").write_text(LOCK, encoding="utf-8", newline="\n")
    src_dir = td / "src"
    src_dir.mkdir()
    (src_dir / "t.ksnl").write_text(src_text, encoding="utf-8", newline="\n")


def fmt_package(td: Path) -> None:
    # 正規形でないと ksn check が E0020 で止めるため、まず ksn fmt で正規化する。
    # 構文エラーのあるサンプルはそのまま残されるが、check は後述で回す。
    subprocess.run([KSN, "fmt"], cwd=td, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def run_check(td: Path):
    fmt_package(td)
    proc = subprocess.run(
        [KSN, "check", "--json"],
        cwd=td,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    codes = []
    for line in proc.stdout.splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            obj = json.loads(line)
        except json.JSONDecodeError:
            continue
        code = obj.get("code")
        if code:
            codes.append(code)
    return proc.returncode, codes, proc.stderr


def main():
    files = sorted(glob.glob(str(BASE / "*_ok.ksnl")) + glob.glob(str(BASE / "*_ng.ksnl")))
    if not files:
        print("no sample files found", file=sys.stderr)
        return 1

    failed = 0
    checked = 0
    for path in files:
        path = Path(path)
        expect_path = path.with_suffix(".expect")
        lines = expect_path.read_text(encoding="utf-8").splitlines()
        expected = lines[0].strip() if lines else ""
        unsafe = len(lines) > 1 and lines[1].strip() == "unsafe"

        src_text = path.read_text(encoding="utf-8")
        with tempfile.TemporaryDirectory() as td:
            write_package(Path(td), src_text, unsafe)
            returncode, codes, stderr = run_check(Path(td))

        checked += 1
        ok = False
        if expected == "ok":
            ok = returncode == 0 and not codes
        else:
            ok = returncode != 0 and expected in codes

        status = "PASS" if ok else "FAIL"
        if not ok:
            failed += 1
        print(f"{status:4} {path.name:20} expected={expected:6} rc={returncode} codes={codes[:3]}")
        if not ok and stderr.strip():
            print(f"      stderr: {stderr.strip()[:200]}", file=sys.stderr)

    print(f"\nchecked={checked} failed={failed}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
