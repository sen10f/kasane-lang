#!/usr/bin/env python3
"""Kasane インタプリタ高速化の計測ハーネス（段階 0）。

各ワークロードを次の 3 通りで N 回ずつ計測し、中央値を Markdown 表と JSON に出す。
  1. インタプリタ: `ksn run`
  2. LLVM ネイティブ: `ksn build --release` の生成物 (Cranelift は 2026-11-14 に削除)
  3. C: `cc -O2` で作った、同じ計算をする実行ファイル（perf/c/*.c）

時間は子プロセスの壁時計時間（起動・読み込み・実行を含む）。起動だけの基準
（何もしない noop パッケージ）を表の先頭に出す。
入力は Kasane 側・C 側で同じ値を使う。Kasane の `const <pkg>.N*` と
`const <pkg>.EXPECT*` は、このスクリプトが作業コピー（perf/results/work/）の中で
書き換える。EXPECT は C 版の出力から取るので、両者の結果が一致しないと止まる。

LLM ベンチ（bench/run_bench.py）とは無関係。結果は perf/results/（gitignore 済み）に出す。

使い方:
    python3 perf/run_perf.py                 # 既定の入力、3 回計測
    python3 perf/run_perf.py --quick         # 小さい入力（動作確認用）
    python3 perf/run_perf.py --repeat 5 --only fib,loops
    python3 perf/run_perf.py --ksn /path/to/ksn
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import platform
import re
import shutil
import statistics
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
RESULTS = HERE / "results"
WORK = RESULTS / "work"
BIN = RESULTS / "bin"
C_DIR = HERE / "c"
EXE = ".exe" if os.name == "nt" else ""
DEFAULT_KSN = ROOT / "stage0" / "target" / "release" / ("ksn.exe" if os.name == "nt" else "ksn")

# Kasane 側の定数行: `const <pkg>.<KEY>: <型> = <値><接尾辞>`
# KEY は入力（N, N_*）か期待値（EXPECT, EXPECT_*）。C 側の KEY=値 と名前を揃えてある。
CONST_RE = re.compile(
    r"^const (?P<pkg>\w+)\.(?P<key>N\w*|EXPECT\w*): (?P<ty>\w+) = (?P<val>-?\d+)(?P<suffix>\w+)$",
    re.M,
)
EXPECT_LINE_RE = re.compile(r"^(EXPECT\w*)=(-?\d+)$", re.M)


@dataclass(frozen=True)
class Workload:
    name: str  # パッケージ名。src/<name>.ksnl と perf/c/<name>.c に対応
    title: str  # 表に出す説明
    full: dict  # 既定の入力（KEY -> 値）
    quick: dict  # --quick の入力


WORKLOADS = [
    Workload("fib", "再帰 fib（関数呼び出し）", {"N": 27}, {"N": 20}),
    Workload(
        "loops",
        "整数ループ（総和・素数ふるい）",
        {"N_SUM": 1_000_000, "N_SIEVE": 200_000},
        {"N_SUM": 100_000, "N_SIEVE": 20_000},
    ),
    Workload("vecops", "Vec の push・索引読み・索引書き", {"N": 300_000}, {"N": 30_000}),
    Workload("hashmap", "HashMap の挿入・検索（整数キー）", {"N": 2_000}, {"N": 200}),
    Workload("strcat", "文字列連結（String.push_str）", {"N": 200_000}, {"N": 20_000}),
    Workload("structs", "構造体とメソッド呼び出し（&mut self / &self）", {"N": 200_000}, {"N": 20_000}),
    Workload("closure", "クロージャ（Fn 境界の関数に渡す）", {"N": 300_000}, {"N": 30_000}),
]
BASELINE = Workload("noop", "起動のみ（何もしない）", {}, {})


class HarnessError(RuntimeError):
    pass


def pkg_dir(pkg: str) -> Path:
    return HERE / pkg


def c_build(name: str) -> Path:
    BIN.mkdir(parents=True, exist_ok=True)
    out = BIN / f"c_{name}{EXE}"
    src = C_DIR / f"{name}.c"
    p = subprocess.run(
        ["cc", "-O2", "-o", str(out), str(src)],
        capture_output=True,
        text=True,
    )
    if p.returncode != 0:
        raise HarnessError(f"cc failed for {name}: {p.stderr.strip()}")
    return out


def c_expect(binary: Path, inputs: dict) -> dict:
    args = [str(binary)] + [f"{k}={v}" for k, v in inputs.items()]
    p = subprocess.run(args, capture_output=True, text=True)
    if p.returncode != 0:
        raise HarnessError(f"C reference exited {p.returncode}: {binary.name}")
    return {m.group(1): int(m.group(2)) for m in EXPECT_LINE_RE.finditer(p.stdout)}


def prepare_work(w: Workload, inputs: dict, expect: dict, size: str) -> Path:
    """perf/<pkg>/ を作業コピーへ写し、入力と期待値の定数を書き換える。"""
    src_dir = pkg_dir(w.name)
    dst = WORK / size / w.name
    if dst.exists():
        shutil.rmtree(dst)
    dst.mkdir(parents=True)
    for f in ("kasane.toml", "kasane.lock"):
        shutil.copy2(src_dir / f, dst / f)
    (dst / "src").mkdir()
    text = (src_dir / "src" / f"{w.name}.ksnl").read_text(encoding="utf-8")
    values = {**inputs, **expect}
    originals = {m.group("key"): int(m.group("val")) for m in CONST_RE.finditer(text)}
    for key in values:
        if key not in originals:
            raise HarnessError(f"{w.name}: src にない定数 {key}（入力か期待値の名前を確かめる）")

    def repl(m: re.Match) -> str:
        key = m.group("key")
        if key not in values:
            raise HarnessError(f"{w.name}: 値のない定数 {key}")
        return f"const {m.group('pkg')}.{key}: {m.group('ty')} = {values[key]}{m.group('suffix')}"

    if size == "full":
        # 既定の入力のとき、ソースの定数は C 版の結果と一致していなければならない
        for key, val in values.items():
            if originals[key] != val:
                raise HarnessError(
                    f"{w.name}: src の {key}={originals[key]} が計測値 {val} と違う（ソースか表を直す）"
                )
    rendered = CONST_RE.sub(repl, text)
    (dst / "src" / f"{w.name}.ksnl").write_text(rendered, encoding="utf-8")
    return dst


def time_runs(cmd: list, cwd: Path, repeat: int, timeout: float) -> dict:
    samples = []
    for _ in range(repeat):
        t0 = time.perf_counter()
        try:
            p = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, timeout=timeout)
        except subprocess.TimeoutExpired:
            return {"status": f"timeout {timeout:g}s"}
        elapsed = (time.perf_counter() - t0) * 1000.0
        if p.returncode != 0:
            tail = (p.stderr or p.stdout).strip().splitlines()[-1:] or [""]
            return {"status": f"exit {p.returncode}: {tail[0][:120]}"}
        samples.append(elapsed)
    return {
        "status": "ok",
        "samples_ms": [round(x, 3) for x in samples],
        "median_ms": round(statistics.median(samples), 3),
        "min_ms": round(min(samples), 3),
        "max_ms": round(max(samples), 3),
    }


def native_build(ksn: Path, work: Path, name: str, timeout: float) -> tuple:
    """LLVM ネイティブを作る。作れなければ (None, 理由) を返す。"""
    p = subprocess.run(
        [str(ksn), "build", "--release"],
        cwd=work,
        capture_output=True,
        text=True,
        timeout=timeout,
    )
    out = p.stdout + p.stderr
    binary = work / "target" / "release" / f"{name}.native"
    if p.returncode != 0:
        lines = [l for l in out.splitlines() if l.strip()]
        return None, (lines[-1][:120] if lines else f"build exit {p.returncode}")
    if "native backend skipped" in out or not binary.exists():
        # 作れなかったときはインタプリタ入りのバンドルだけが残る。それを計測しない。
        skipped = [l for l in out.splitlines() if "skipped" in l]
        return None, (skipped[0][:120] if skipped else "native executable not produced")
    return binary, ""


def measure(ksn: Path, w: Workload, size: str, repeat: int, timeout: float, quick: bool) -> dict:
    inputs = w.quick if quick else w.full
    c_bin = c_build(w.name)
    expect = c_expect(c_bin, inputs) if inputs else {}
    if inputs and not expect:
        raise HarnessError(f"{w.name}: C 版が EXPECT 行を出さなかった")
    work = prepare_work(w, inputs, expect, size)

    interp = time_runs([str(ksn), "run"], work, repeat, timeout)
    binary, reason = native_build(ksn, work, w.name, timeout)
    if binary is None:
        native = {"status": f"n/a: {reason}"}
    else:
        native = time_runs([str(binary)], work, repeat, timeout)
    c = time_runs([str(c_bin)] + [f"{k}={v}" for k, v in inputs.items()], HERE, repeat, timeout)
    return {
        "name": w.name,
        "title": w.title,
        "inputs": inputs,
        "expect": expect,
        "interp": interp,
        "native": native,
        "c": c,
    }


def fmt_ms(entry: dict) -> str:
    if entry.get("status") == "ok":
        return f"{entry['median_ms']:.1f}"
    return entry.get("status", "?")


def ratio(a: dict, b: dict) -> str:
    if a.get("status") == "ok" and b.get("status") == "ok" and b["median_ms"] > 0:
        return f"{a['median_ms'] / b['median_ms']:.1f}x"
    return "-"


def render_markdown(rows: list, meta: dict) -> str:
    lines = []
    lines.append(f"計測: {meta['timestamp']} / 入力: {meta['size']} / 各 {meta['repeat']} 回の中央値 / 単位 ms")
    lines.append("")
    lines.append("| ワークロード | 入力 | インタプリタ | LLVM | C (-O2) | インタプリタ / C | LLVM / C |")
    lines.append("| --- | --- | ---: | ---: | ---: | ---: | ---: |")
    for r in rows:
        inputs = ", ".join(f"{k}={v:,}" for k, v in r["inputs"].items()) or "-"
        lines.append(
            "| {title} | {inputs} | {i} | {n} | {c} | {ic} | {nc} |".format(
                title=r["title"] if r["name"] != "noop" else f"**{r['title']}**",
                inputs=inputs,
                i=fmt_ms(r["interp"]),
                n=fmt_ms(r["native"]),
                c=fmt_ms(r["c"]),
                ic=ratio(r["interp"], r["c"]),
                nc=ratio(r["native"], r["c"]),
            )
        )
    lines.append("")
    lines.append("n/a の理由:")
    any_na = False
    for r in rows:
        for key, label in (("interp", "インタプリタ"), ("native", "LLVM"), ("c", "C")):
            st = r[key].get("status", "")
            if st != "ok":
                any_na = True
                lines.append(f"- {r['name']} / {label}: {st}")
    if not any_na:
        lines.append("- なし")
    lines.append("")
    lines.append("環境: " + meta["env"])
    return "\n".join(lines) + "\n"


def environment(ksn: Path) -> str:
    def first_line(cmd: list) -> str:
        try:
            p = subprocess.run(cmd, capture_output=True, text=True, timeout=30)
            return (p.stdout or p.stderr).strip().splitlines()[0]
        except Exception as exc:  # 環境情報は取れなくても計測は続ける
            return f"unavailable ({exc})"

    git = first_line(["git", "-C", str(ROOT), "rev-parse", "--short", "HEAD"])
    return "; ".join(
        [
            f"git {git}",
            f"ksn {ksn}",
            f"cc: {first_line(['cc', '--version'])}",
            f"{platform.system()} {platform.release()} {platform.machine()}",
            f"python {platform.python_version()}",
        ]
    )


def main(argv: list) -> int:
    ap = argparse.ArgumentParser(description="Kasane の実行速度を計測する（インタプリタ / LLVM / C）")
    ap.add_argument("--ksn", type=Path, default=DEFAULT_KSN, help="ksn バイナリ（既定 stage0/target/release/ksn）")
    ap.add_argument("--repeat", type=int, default=3, help="各計測の回数（既定 3。中央値を出す）")
    ap.add_argument("--quick", action="store_true", help="小さい入力で計測する")
    ap.add_argument("--only", default="", help="ワークロード名をカンマ区切りで指定（例 fib,loops）")
    ap.add_argument("--timeout", type=float, default=600.0, help="1 回の実行の上限秒（既定 600）")
    ap.add_argument("--tag", default="", help="結果ファイル名に付ける印（例 baseline）")
    args = ap.parse_args(argv)

    ksn = args.ksn.resolve()
    if not ksn.exists():
        print(f"ksn が見つからない: {ksn}（cd stage0 && cargo build --release）", file=sys.stderr)
        return 2
    if args.repeat < 1:
        print("--repeat は 1 以上", file=sys.stderr)
        return 2
    if shutil.which("cc") is None:
        print("cc が PATH にない（C 版の計測に必要）", file=sys.stderr)
        return 2

    wanted = {s for s in args.only.split(",") if s}
    chosen = [w for w in WORKLOADS if not wanted or w.name in wanted]
    unknown = wanted - {w.name for w in WORKLOADS}
    if unknown:
        print(f"未知のワークロード: {sorted(unknown)}", file=sys.stderr)
        return 2

    size = "quick" if args.quick else "full"
    stamp = dt.datetime.now().strftime("%Y%m%d-%H%M%S")
    rows = []
    for w in [BASELINE] + chosen:
        print(f"[{w.name}] 計測中 ...", file=sys.stderr, flush=True)
        rows.append(measure(ksn, w, size, args.repeat, args.timeout, args.quick))

    meta = {
        "timestamp": dt.datetime.now().isoformat(timespec="seconds"),
        "size": size,
        "repeat": args.repeat,
        "env": environment(ksn),
    }
    RESULTS.mkdir(parents=True, exist_ok=True)
    tag = f"-{args.tag}" if args.tag else ""
    base = RESULTS / f"perf-{stamp}-{size}{tag}"
    base.with_suffix(".json").write_text(
        json.dumps({"meta": meta, "rows": rows}, ensure_ascii=False, indent=2) + "\n",
        encoding="utf-8",
    )
    md = render_markdown(rows, meta)
    base.with_suffix(".md").write_text(md, encoding="utf-8")
    print(md)
    print(f"結果: {base.with_suffix('.json')} / {base.with_suffix('.md')}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv[1:]))
    except HarnessError as exc:
        print(f"error: {exc}", file=sys.stderr)
        sys.exit(1)
