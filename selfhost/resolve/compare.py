#!/usr/bin/env python3
"""selfhost/resolve の突き合わせ。Kasane 版 (resolve / ksn run) と Rust 版 (ksn defs / ksn check --json) を比べる。

  python3 -I compare.py [--mine native|interp] [--group examples|apps|parse|broken|stdlib|all] [-v]

- 定義: 自パッケージの各ファイルについて、Kasane 版の `種別 FQN 行` の並びと `ksn defs` の `種別 名前 行` の並びを比べる。
- 診断: `ksn check --json` の名前解決の診断 (E1101 E1102 E1103 E1104 E1109 E1119) と、Kasane 版の `error CODE パス:行:` を
  (ファイル, 行, コード) の多重集合で比べる。
- 標準ライブラリ (stdlib): spec/stdlib を自パッケージの src として読ませた一時パッケージで、定義だけ比べる。
- 木走査 (--mine interp) のときは ksn run で同じ入力を読ませる (時間の比較用)。
"""
import argparse
import collections
import json
import os
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.abspath(os.path.join(HERE, "..", ".."))
KSN = os.path.join(ROOT, "stage0", "target", "release", "ksn")
NATIVE = os.path.join(HERE, "target", "release", "resolve")
STDLIB = os.path.join(ROOT, "spec", "stdlib")
RESOLVE_CODES = {"E1101", "E1102", "E1103", "E1104", "E1109", "E1119"}


def run(cmd, cwd=None):
    p = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True)
    return p.returncode, p.stdout, p.stderr


def mine_run(mode, pkg, std):
    if mode == "native":
        cmd = [NATIVE, pkg, std]
    else:
        cmd = [KSN, "run", pkg, std]
    t0 = time.perf_counter()
    rc, out, err = run(cmd)
    dt = time.perf_counter() - t0
    if rc != 0 and not out:
        raise RuntimeError(f"{' '.join(cmd)} failed: {err[:400]}")
    return out, dt


def parse_mine(out, base):
    """== 行で区切ったファイルごとの定義と、診断の集合。"""
    defs = collections.OrderedDict()
    diags = collections.Counter()
    cur = None
    summary = ""
    for line in out.splitlines():
        if line.startswith("== "):
            cur = os.path.relpath(line[3:], base)
            defs[cur] = []
        elif line.startswith("error "):
            head, _, rest = line.partition(" ")
            code, _, loc = rest.partition(" ")
            path, _, tail = loc.partition(":")
            lineno = tail.split(":", 1)[0]
            rel = os.path.relpath(path, base)
            if code in RESOLVE_CODES:
                diags[(rel, int(lineno), code)] += 1
        elif line.startswith("summary "):
            summary = line
        elif cur is not None and line.strip():
            parts = line.split(" ")
            if len(parts) >= 3 and parts[-1].isdigit():
                defs[cur].append((parts[0], " ".join(parts[1:-1]), int(parts[-1])))
    return defs, diags, summary


def rust_defs(path):
    """ksn defs (.ksnl) の (種別, 名前, 行)。.sig は ksn defs が読めないので ksn headers の (種別, 名前) を使う。"""
    if path.endswith(".sig"):
        rc, out, _ = run([KSN, "headers", path])
        res = []
        for line in out.splitlines():
            parts = line.split("\t")
            if len(parts) >= 2:
                res.append((parts[0], parts[1], None))
        return res
    rc, out, _ = run([KSN, "defs", path])
    res = []
    for line in out.splitlines():
        parts = line.split("\t")
        if len(parts) >= 3:
            res.append((parts[0], parts[1], int(parts[2])))
    return res


def rust_diags(pkg):
    rc, out, _ = run([KSN, "check", "--json"], cwd=pkg)
    res = collections.Counter()
    for line in out.splitlines():
        line = line.strip()
        if not line.startswith("{"):
            continue
        try:
            d = json.loads(line)
        except ValueError:
            continue
        if d.get("code") in RESOLVE_CODES:
            f = d.get("file", "")
            if not f.startswith("src/"):
                continue
            res[(f, int(d.get("line", 0)), d["code"])] += 1
    return res


def compare_pkg(name, pkg, std, mode, verbose, stdlib_mode=False, base=None):
    out, dt = mine_run(mode, pkg, std)
    base = base or pkg
    defs, diags, summary = parse_mine(out, base)
    bad_defs = 0
    total_defs = 0
    for rel, mine_list in defs.items():
        path = os.path.join(base, rel)
        rust = rust_defs(path)
        if path.endswith(".sig"):
            mine_cmp = [(k, n, None) for (k, n, _l) in mine_list]
        else:
            mine_cmp = mine_list
        total_defs += max(len(rust), len(mine_cmp))
        if rust != mine_cmp:
            bad_defs += 1
            if verbose:
                print(f"  DEFS DIFF {name}:{rel}")
                print("    rust:", rust[:6])
                print("    mine:", mine_cmp[:6])
    result = {
        "name": name,
        "files": len(defs),
        "def_files_ok": len(defs) - bad_defs,
        "defs": total_defs,
        "time": dt,
        "summary": summary,
    }
    if not stdlib_mode:
        rd = rust_diags(pkg)
        md = diags
        keys = set(rd) | set(md)
        match = sum(min(rd[k], md[k]) for k in keys)
        total = sum(max(rd[k], md[k]) for k in keys)
        result.update(diag_match=match, diag_total=total)
        if verbose and match != total:
            for k in sorted(keys):
                if rd[k] != md[k]:
                    print(f"  DIAG DIFF {name} {k}: rust={rd[k]} mine={md[k]}")
    return result


def packages(group):
    out = []
    if group in ("examples", "all"):
        ex = os.path.join(ROOT, "examples")
        for d in sorted(os.listdir(ex)):
            p = os.path.join(ex, d)
            if os.path.exists(os.path.join(p, "kasane.toml")):
                out.append(("examples/" + d, p))
    if group in ("apps", "all"):
        ap = os.path.join(ROOT, "apps")
        for d in sorted(os.listdir(ap)):
            p = os.path.join(ap, d)
            if os.path.exists(os.path.join(p, "kasane.toml")):
                out.append(("apps/" + d, p))
    if group in ("parse", "all"):
        out.append(("selfhost/parse", os.path.join(ROOT, "selfhost", "parse")))
    if group in ("broken", "all"):
        out.append(("selfhost/resolve/broken/undef", os.path.join(HERE, "broken", "undef")))
    return out


def stdlib_pkg(tmp):
    """spec/stdlib を src とする一時パッケージ。"""
    os.makedirs(tmp, exist_ok=True)
    with open(os.path.join(tmp, "kasane.toml"), "w") as f:
        f.write('[package]\nname = "core"\nversion = "0.1.0"\nedition = 1\n\n[dependencies]\n')
    link = os.path.join(tmp, "src")
    if not os.path.lexists(link):
        os.symlink(STDLIB, link)
    return tmp


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--mine", choices=["native", "interp"], default="native")
    ap.add_argument("--group", default="all",
                    choices=["examples", "apps", "parse", "broken", "stdlib", "all"])
    ap.add_argument("-v", "--verbose", action="store_true")
    a = ap.parse_args()
    if a.mine == "native" and not os.path.exists(NATIVE):
        print("resolve がない: cd selfhost/resolve && ksn build --release", file=sys.stderr)
        return 2
    empty = tempfile.mkdtemp(prefix="resolve-empty-")
    rows = []
    for name, pkg in packages(a.group):
        # 標準ライブラリは各パッケージの読み込みに含める (3.2 の core/alloc/std)。
        rows.append(compare_pkg(name, pkg, STDLIB, a.mine, a.verbose))
    if a.group in ("stdlib", "all"):
        tmp = stdlib_pkg(os.path.join(tempfile.gettempdir(), "resolve-stdlib-pkg"))
        rows.append(compare_pkg("spec/stdlib", tmp, empty, a.mine, a.verbose,
                                stdlib_mode=True, base=tmp))
    tot = collections.Counter()
    for r in rows:
        print(f"{r['name']:<28} files {r['files']:>3}  defs-files-ok {r['def_files_ok']:>3}/{r['files']:<3}"
              f"  diag {r.get('diag_match', '-')}/{r.get('diag_total', '-')}  {r['time']:.3f}s  {r['summary']}")
        tot["files"] += r["files"]
        tot["ok"] += r["def_files_ok"]
        tot["defs"] += r["defs"]
        tot["dm"] += r.get("diag_match", 0)
        tot["dt"] += r.get("diag_total", 0)
    print(f"TOTAL files {tot['ok']}/{tot['files']} (defs equal, {tot['defs']} defs compared), "
          f"diags {tot['dm']}/{tot['dt']}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
