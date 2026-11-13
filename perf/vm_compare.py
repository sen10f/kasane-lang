#!/usr/bin/env python3
"""木走査と VM (`ksn run --engine=vm`) の実行時間の比較 (設計_KIR.md 5 章 段階 2 の計測)。

使い方: python3 perf/vm_compare.py [--repeat 5] [--only fib,loops] [--ksn <パス>]
各 perf パッケージを既定の入力のまま `ksn run` と `ksn run --engine=vm` で走らせ、壁時計の中央値 (ms) と倍率を表にする。
VM が退避した (stderr に `vm engine skipped`) ケースは n/a にする。標準ライブラリだけを使う。
"""
import argparse
import os
import statistics
import subprocess
import sys
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CASES = ["noop", "fib", "loops", "vecops", "hashmap", "strcat", "structs", "closure"]


def once(ksn, pkg, engine):
    t = time.perf_counter()
    r = subprocess.run([ksn, "run", f"--engine={engine}"], cwd=pkg, capture_output=True, text=True)
    dt = (time.perf_counter() - t) * 1000
    return dt, r.returncode, r.stdout, r.stderr


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--repeat", type=int, default=5)
    ap.add_argument("--only", default="")
    ap.add_argument("--ksn", default=os.path.join(ROOT, "stage0", "target", "release", "ksn"))
    a = ap.parse_args()
    cases = [c for c in CASES if not a.only or c in a.only.split(",")]
    print(f"| ケース | 木走査 (ms) | VM (ms) | 倍率 |")
    print("| --- | ---: | ---: | ---: |")
    for c in cases:
        pkg = os.path.join(ROOT, "perf", c)
        res = {}
        for eng in ("tree", "vm"):
            ts = []
            for _ in range(a.repeat):
                dt, rc, out, err = once(a.ksn, pkg, eng)
                if rc != 0 or (eng == "vm" and "vm engine skipped" in err):
                    ts = None
                    break
                ts.append(dt)
            res[eng] = statistics.median(ts) if ts else None
        t, v = res["tree"], res["vm"]
        if t is None or v is None:
            print(f"| {c} | {'n/a' if t is None else f'{t:.1f}'} | {'n/a' if v is None else f'{v:.1f}'} | n/a |")
        else:
            print(f"| {c} | {t:.1f} | {v:.1f} | {t / v:.1f}x |")


if __name__ == "__main__":
    sys.exit(main())
