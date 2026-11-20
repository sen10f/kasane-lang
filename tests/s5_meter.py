#!/usr/bin/env python3
"""S5-0 / S6-0 の計器を、リポジトリの ksn パッケージ全部に回して要約を出す (Python 3 標準のみ)。

使い方: python3 -I tests/s5_meter.py [--ksn PATH] [--json OUT] [--show N]

- S5-0: 下ろしが型・種類・名前を読み直した箇所の回数と、TAST と食い違った式の数 (`ksn s5-meter` の報告)。
- S6-0: KIR に下ろせなかった関数の数と理由 (木走査・バンドルへ退避する対象)。標準ライブラリは別に数える。

対象: examples/*, apps/*, ui/kasane/*, selfhost/*, perf/ の下の kasane.toml を持つディレクトリ。
設計: 設計_S5S6_型付きIRの一本化.md 4 章・7 章。計測の記録: 設計_S5S6_計測.md。
"""

import argparse
import collections
import json
import os
import re
import subprocess
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ROOTS = ["examples", "apps", os.path.join("ui", "kasane"), "selfhost", "perf"]
SKIP_DIRS = {"target", ".git", "node_modules"}


def find_packages():
    pkgs = []
    for r in ROOTS:
        base = os.path.join(REPO, r)
        if not os.path.isdir(base):
            continue
        for dirpath, dirnames, filenames in os.walk(base):
            dirnames[:] = sorted(d for d in dirnames if d not in SKIP_DIRS)
            if "kasane.toml" in filenames:
                pkgs.append(dirpath)
                dirnames[:] = []  # パッケージの中の下位のパッケージは、そのパッケージ自身の下ろしに入る
    return pkgs


def reason_key(reason):
    """理由を型名・行番号・詳細を除いた形で数える (例: 「引数の型 〈型〉 は段階 1 の対象外」)。"""
    s = re.sub(r"^行 \d+: ", "", reason)
    s = re.sub(r"(の型|メソッド|定数|呼び先|の対象|の型) .*? (は|が)", r"\1 〈型〉 \2", s)
    s = re.split(r"[({<\[]", s, maxsplit=1)[0].strip()
    s = re.sub(r"[A-Za-z_][A-Za-z0-9_.]*", "N", s)
    return s or reason


def run_one(ksn, pkg):
    try:
        cp = subprocess.run([ksn, "s5-meter"], cwd=pkg, capture_output=True, text=True, timeout=600)
    except subprocess.TimeoutExpired:
        return None, "timeout"
    rows, bad = [], []
    for line in cp.stdout.splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            rows.append(json.loads(line))
        except json.JSONDecodeError:
            bad.append(line[:200])
    if bad:
        return rows, "invalid JSON line: " + bad[0]
    if not rows:
        return None, "no output (rc=%d) %s" % (cp.returncode, cp.stderr.strip()[-200:])
    return rows, None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--ksn", default=os.path.join(REPO, "stage0", "target", "release", "ksn"))
    ap.add_argument("--json", help="要約を JSON で書く先")
    ap.add_argument("--show", type=int, default=10, help="食い違いの一覧を何件まで出すか")
    args = ap.parse_args()

    pkgs = find_packages()
    sites = collections.OrderedDict()
    dis = []
    reasons = collections.Counter()
    reason_ex = {}
    std_seen = {}
    lowered = total = 0
    errors = []
    for pkg in pkgs:
        rel = os.path.relpath(pkg, REPO)
        rows, err = run_one(args.ksn, pkg)
        if err:
            errors.append((rel, err))
        for r in rows or []:
            k = r.get("kind")
            if k == "site":
                s = sites.setdefault(r["site"], collections.Counter())
                for f in ("count", "no_tast", "tast_unknown", "disagree"):
                    s[f] += r.get(f, 0)
            elif k == "disagree":
                dis.append(dict(r, pkg=rel))
            elif k == "coverage":
                lowered += r.get("lowered", 0)
                total += r.get("total", 0)
            elif k == "error":
                errors.append((rel, "検査が通らない: " + r.get("first", r.get("msg", ""))))
            elif k == "unsupported":
                if r.get("origin") == "std":
                    std_seen.setdefault(r["fn"], reason_key(r.get("reason", "")))
                    continue
                key = reason_key(r.get("reason", ""))
                reasons[key] += 1
                reason_ex.setdefault(key, "%s:%s" % (rel, r.get("fn", "")))

    print("# S5-0 / S6-0 計器の要約 (%d パッケージ)" % len(pkgs))
    print()
    print("## S5-0: 下ろしの型・種類・名前の読み直し (箇所ごと、全パッケージ合計)")
    print()
    print("| 箇所 | 回数 | TAST に無い | TAST が Unknown | 食い違い |")
    print("| --- | ---: | ---: | ---: | ---: |")
    for name, c in sites.items():
        print("| %s | %d | %d | %d | %d |" % (name, c["count"], c["no_tast"], c["tast_unknown"], c["disagree"]))
    print()
    print("食い違い: %d 件" % len(dis))
    for d in dis[: args.show]:
        print("- %s %s:%s %s (行 %s): TAST=%s / 下ろし=%s" % (d["pkg"], d["def"], d["id"], d["site"], d["line"], d["tast"], d["lowered"]))
    print()
    print("## S6-0: KIR に下ろせなかった関数 (own = 利用者のパッケージ)")
    print()
    print("- 下ろせた関数: %d / %d (own)" % (lowered, total))
    print("- 下ろせなかった関数: %d 件 (理由ごと)" % sum(reasons.values()))
    print()
    print("| 理由 | 件数 | 例 |")
    print("| --- | ---: | --- |")
    for why, n in reasons.most_common():
        print("| %s | %d | %s |" % (why, n, reason_ex[why]))
    print()
    print("標準ライブラリ (`spec/stdlib`) で下ろせなかった関数: %d 件 (重複を除く)" % len(std_seen))
    if errors:
        print()
        print("## 実行の失敗 (%d)" % len(errors))
        for rel, err in errors:
            print("- %s: %s" % (rel, err))

    if args.json:
        out = {
            "packages": len(pkgs),
            "sites": {k: dict(v) for k, v in sites.items()},
            "disagreements": dis,
            "lowered": lowered,
            "total": total,
            "unsupported_reasons": dict(reasons),
            "std_unsupported": std_seen,
            "errors": errors,
        }
        with open(args.json, "w", encoding="utf-8") as f:
            json.dump(out, f, ensure_ascii=False, indent=1)
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
