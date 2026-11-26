"""借用・ライフタイムの穴探し。`gen_borrow.py` のプログラムを `ksn check` に通し、ラベルと照合する。

  python -I tools/fuzz/fuzz_borrow.py --ksn stage0/target/release/ksn --seed 1 --count 2000 --jobs 8

- bad なのに診断 0 件 → 穴 (`holes`)。ok なのに診断あり → 誤検出 (`false_pos`)。
- 結果は `--out` (既定 tools/fuzz/found_borrow/) に、タグ (形) ごとに 1 本ずつ書く (gitignore 対象)。
- 診断コードを集計して表示する (ok が別のコード (E2xxx 等) で落ちているのは生成器の誤りなのでここで気付く)。
"""

import argparse
import json
import os
import random
import subprocess
import sys
import tempfile
from collections import Counter
from concurrent.futures import ThreadPoolExecutor

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import gen_borrow  # noqa: E402

TOML = '[package]\nname = "t"\nversion = "0.1.0"\nedition = 1\nentry = "t.main"\n\n[dependencies]\n'


def check(ksn, src):
    with tempfile.TemporaryDirectory(prefix="fzb_") as d:
        os.makedirs(os.path.join(d, "src"))
        open(os.path.join(d, "kasane.toml"), "w").write(TOML)
        open(os.path.join(d, "kasane.lock"), "w").write("version = 1\n")
        open(os.path.join(d, "src", "t.ksnl"), "w").write(src)
        # 生成器の出力を正規形にそろえる (fmt → fix を変化が無くなるまで)。fmt 後の E0020 は生成器の誤り。
        subprocess.run([ksn, "fmt"], cwd=d, capture_output=True, text=True, timeout=60)
        for _ in range(6):
            q = subprocess.run([ksn, "fix"], cwd=d, capture_output=True, text=True, timeout=60)
            subprocess.run([ksn, "fmt"], cwd=d, capture_output=True, text=True, timeout=60)
            if "applied" not in q.stdout + q.stderr:
                break
        p = subprocess.run([ksn, "check", "--json"], cwd=d, capture_output=True, text=True, timeout=60)
        codes = []
        for l in p.stdout.splitlines():
            if l.startswith("{"):
                try:
                    j = json.loads(l)
                    codes.append(j["code"] if not os.environ.get("FZB_MSG") else j["code"] + " " + j.get("message", "")[:90])
                except Exception:
                    pass
        return p.returncode, codes


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--ksn", required=True)
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--count", type=int, default=1000)
    ap.add_argument("--jobs", type=int, default=4)
    ap.add_argument("--out", default=os.path.join(os.path.dirname(os.path.abspath(__file__)), "found_borrow"))
    a = ap.parse_args()
    a.ksn = os.path.abspath(a.ksn)
    jobs = min(a.jobs, 8)
    rng = random.Random(a.seed)
    progs = []
    while len(progs) < a.count:
        b = gen_borrow.build(rng, len(progs))
        if b:
            progs.append(b)
    with ThreadPoolExecutor(jobs) as ex:
        results = list(ex.map(lambda b: check(a.ksn, b[0]), progs))
    os.makedirs(a.out, exist_ok=True)
    for f in os.listdir(a.out):
        if f.endswith(".ksnl"):
            os.remove(os.path.join(a.out, f))
    stat = Counter()
    seen = set()
    codes_ok = Counter()
    for (src, lab, tags), (rc, codes) in zip(progs, results):
        rejected = rc != 0 or bool(codes)
        if lab == "bad":
            kind = "hole" if not rejected else "caught"
        else:
            kind = "false_pos" if rejected else "ok_pass"
            if rejected:
                codes_ok.update(codes)
        stat[kind] += 1
        if kind in ("hole", "false_pos"):
            key = (kind, tuple(tags[:3]) if tags[-1] != "mut" else tuple(tags))
            key = (kind, tuple(t for t in tags))
            if key in seen:
                continue
            seen.add(key)
            name = kind + "_" + "_".join(t.replace("/", "-") for t in tags) + f"_{len(seen)}"
            open(os.path.join(a.out, name + ".ksnl"), "w").write(f"// codes: {codes}\n" + src)
    print(dict(stat))
    if codes_ok:
        print("false-positive codes:", dict(codes_ok))
    print("saved distinct shapes:", len(seen), "->", a.out)


if __name__ == "__main__":
    main()
