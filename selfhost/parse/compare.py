#!/usr/bin/env python3
"""parse (Kasane 製の構文解析器) と Rust 版 (ksn defs / headers / bodies) の要約を比べる。

  python3 compare.py [--mine native|interp|COMMAND...] [--group NAME] [--list] [--details N]
                     [--files F...]

  --mine native   target/release/parse (既定)
  --mine interp   `ksn run` (木走査)。パッケージ直下で実行する
  --group         examples / stdlib / linear_ok / bench / unsound / self (parse 自身のソース) / broken (複数回指定可。既定は先頭 5 つ)
  --files         グループの代わりにこのファイルだけを比べる
結果: 通った数 (parse がエラーなし) / 一致数 (Rust 版と定義一覧が同じ) / 全体、parse の所要時間。
"""
import glob
import os
import shlex
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.normpath(os.path.join(HERE, "..", ".."))
KSN = os.path.join(ROOT, "stage0", "target", "release", "ksn")
NATIVE = os.path.join(HERE, "target", "release", "parse")

sys.path.insert(0, HERE)
import rust_summary  # noqa: E402


def corpus(group):
    g = lambda pat: sorted(glob.glob(os.path.join(ROOT, pat), recursive=True))
    if group == "examples":
        return g("examples/*/src/**/*.ksnl")
    if group == "stdlib":
        return g("spec/stdlib/**/*.ksnl")
    if group == "linear_ok":
        return g("stage0/tests/data/linear_ok/*.ksnl")
    if group == "bench":
        return g("bench/tasks/*/reference.ksnl")
    if group == "unsound":
        return g("stage0/tests/data/unsound/*.ksnl")
    if group == "self":
        return sorted(glob.glob(os.path.join(HERE, "src", "**", "*.ksnl"), recursive=True))
    if group == "broken":
        return sorted(glob.glob(os.path.join(HERE, "broken", "*.ksnl")))
    raise SystemExit(f"unknown group {group}")


KINDS_ARG = []
ALL_GROUPS = ["examples", "stdlib", "linear_ok", "bench", "unsound"]


def parse_records(text):
    """出力を [(パス, ok, 定義行のリスト or エラー行)] に分ける。"""
    recs = []
    cur = None
    for line in text.splitlines():
        if line.startswith("== "):
            cur = [line[3:], True, []]
            recs.append(cur)
        elif line.startswith("nodes "):
            if cur is not None:
                cur.append(line)
        elif line.startswith("kinds"):
            if cur is not None:
                cur.append(line)
            cur = None
        elif line.startswith("error "):
            body = line[6:]
            path = body.split(":")[0]
            recs.append([path, False, line])
            cur = None
        elif cur is not None:
            cur[2].append(line)
    return recs


def err_loc(line):
    # `error パス:行:桁: 説明`
    parts = line[6:].split(":")
    try:
        return int(parts[1])
    except (IndexError, ValueError):
        return -1


def run_mine(mine, files):
    if mine == ["native"]:
        cmd, cwd = [NATIVE] + KINDS_ARG + files, HERE
    elif mine == ["interp"]:
        cmd, cwd = [KSN, "run"] + KINDS_ARG + files, HERE
    else:
        cmd, cwd = mine + KINDS_ARG + files, HERE
    t0 = time.time()
    r = subprocess.run(cmd, capture_output=True, text=True, cwd=cwd, errors="replace")
    dt = time.time() - t0
    if r.returncode != 0:
        print("mine: exit", r.returncode, r.stderr[:300], file=sys.stderr)
    return r.stdout, dt


def main(argv):
    global KINDS_ARG
    mine = ["native"]
    groups = []
    files = []
    details = 25
    show_list = False
    i = 0
    while i < len(argv):
        a = argv[i]
        if a == "--mine":
            i += 1
            mine = [argv[i]] if argv[i] in ("native", "interp") else shlex.split(argv[i])
        elif a == "--group":
            i += 1
            groups.append(argv[i])
        elif a == "--files":
            files = argv[i + 1:]
            break
        elif a == "--details":
            i += 1
            details = int(argv[i])
        elif a == "--kinds":
            KINDS_ARG.append("--kinds")
        elif a == "--list":
            show_list = True
        else:
            raise SystemExit(f"unknown option {a}")
        i += 1
    if not files:
        for g in groups or ALL_GROUPS:
            files += [(g, f) for f in corpus(g)]
    else:
        files = [("files", os.path.abspath(f)) for f in files]
    paths = [f for _g, f in files]
    group_of = {f: g for g, f in files}

    t0 = time.time()
    rust = {}
    for p in paths:
        rust[p] = rust_summary.summarize(p, KSN)
    t_rust = time.time() - t0

    out, t_mine = run_mine(mine, paths)
    recs = parse_records(out)
    if len(recs) != len(paths):
        print(f"warning: {len(recs)} records for {len(paths)} files", file=sys.stderr)
    mine_of = {r[0]: r for r in recs}

    stats = {}
    bad = []
    for p in paths:
        g = group_of[p]
        s = stats.setdefault(g, dict(total=0, mine_ok=0, rust_ok=0, match=0, both_err=0, same_line=0))
        s["total"] += 1
        rok, rlines = rust[p]
        m = mine_of.get(p)
        mok = bool(m and m[1])
        s["mine_ok"] += mok
        s["rust_ok"] += rok
        if rok and mok:
            rdefs = rlines[1:-1]
            if rdefs == m[2]:
                s["match"] += 1
            else:
                bad.append((p, "defs differ", rdefs, m[2]))
        elif not rok and not mok:
            s["both_err"] += 1
            if m and err_loc(rlines[0]) == err_loc(m[2]):
                s["same_line"] += 1
            else:
                bad.append((p, "both error, line differs", rlines[0], m[2] if m else "(no record)"))
        else:
            bad.append((p, "rust ok" if rok else "mine ok", rlines[0] if not rok else "", m[2] if (m and not mok) else ""))

    if KINDS_ARG:
        kstat = [0, 0]
        kbad = []
        for p in paths:
            m = mine_of.get(p)
            if not (m and m[1] and rust[p][0]):
                continue
            kline = [x for x in m[3:] if x.startswith("kinds")]
            mk = {}
            for tok in (kline[0].split()[1:] if kline else []):
                k, v = tok.split("=")
                mk[k] = int(v)
            rk = rust_summary.body_kinds(p, KSN)
            keys = set(rk) | set(rust_summary.BODY_KINDS.values())
            diff = {k: (rk.get(k, 0), mk.get(k, 0)) for k in keys if rk.get(k, 0) != mk.get(k, 0)}
            kstat[0] += 1
            if not diff:
                kstat[1] += 1
            else:
                kbad.append((p, diff))
        print(f"AST kind counts (bodies): {kstat[1]} of {kstat[0]} files identical to ksn bodies' dump")
        for p, diff in kbad[:details]:
            print("   kinds differ:", os.path.relpath(p, ROOT), {k: f"rust {a} / mine {b}" for k, (a, b) in sorted(diff.items())})
    tot = {k: sum(s[k] for s in stats.values()) for k in ("total", "mine_ok", "rust_ok", "match", "both_err", "same_line")}
    print(f"{'group':10} {'total':>5} {'mine_ok':>7} {'rust_ok':>7} {'match':>6} {'both_err':>8} {'same_line':>9}")
    for g, s in stats.items():
        print(f"{g:10} {s['total']:>5} {s['mine_ok']:>7} {s['rust_ok']:>7} {s['match']:>6} {s['both_err']:>8} {s['same_line']:>9}")
    print(f"{'ALL':10} {tot['total']:>5} {tot['mine_ok']:>7} {tot['rust_ok']:>7} {tot['match']:>6} {tot['both_err']:>8} {tot['same_line']:>9}")
    nodes = 0
    lines = 0
    for p in paths:
        try:
            lines += open(p, encoding="utf-8", errors="replace").read().count("\n")
        except OSError:
            pass
    for r in recs:
        if r[1] and len(r) > 3:
            try:
                nodes += int(r[3].split()[1])
            except (IndexError, ValueError):
                pass
    print(f"mine time {t_mine:.2f}s ({' '.join(mine)}), rust-baseline time {t_rust:.2f}s, source lines {lines}, ast nodes {nodes}")
    for p, why, a, b in bad[:details]:
        print(f"--- {os.path.relpath(p, ROOT)}: {why}")
        if isinstance(a, list):
            sa, sb = set(a), set(b)
            for x in [l for l in a if l not in sb][:4]:
                print("   rust only:", x)
            for x in [l for l in b if l not in sa][:4]:
                print("   mine only:", x)
        else:
            print("   rust:", a)
            print("   mine:", b)
    if len(bad) > details:
        print(f"... {len(bad) - details} more")
    if "broken" in stats:
        exp = {}
        for l in open(os.path.join(HERE, "broken", "expected.txt"), encoding="utf-8"):
            if l.strip() and not l.startswith("#"):
                n, ln = l.split()[:2]
                exp[os.path.join(HERE, "broken", n + ".ksnl")] = int(ln)
        hit = 0
        for p in paths:
            m = mine_of.get(p)
            if group_of[p] == "broken" and m and not m[1] and err_loc(m[2]) == exp.get(p):
                hit += 1
        print(f"broken inputs: parse reports the expected line for {hit} of {stats['broken']['total']}")
    if show_list:
        for p in paths:
            print(p)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
