#!/usr/bin/env python3
"""二つの ksn の `check --json` の出力が、コーパス全体で 1 バイトも違わないことを確かめる。
使い方: python3 -I tests/check_diff.py OLD_KSN NEW_KSN [--gen DIR ...] [--pkg DIR ...]
 コーパス: stage0/tests/data/{unsound,linear_ok,diag}/*.ksnl (1 ファイル 1 パッケージにして fmt してから check)、
 --gen は単独のパッケージ (kasane.toml のあるディレクトリ) を子に持つ DIR、--pkg は 1 パッケージ。"""
import concurrent.futures as cf, os, shutil, subprocess, sys, tempfile, glob
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
TOML = '[package]\nname = "t"\nversion = "0.1.0"\nedition = 1\nentry = "t.main"\n\n[dependencies]\n'
old, new = sys.argv[1], sys.argv[2]
rest = sys.argv[3:]
pkgs, gens = [], []
while rest:
    k = rest.pop(0)
    (gens if k == "--gen" else pkgs).append(rest.pop(0))
tmp = tempfile.mkdtemp(prefix="check_diff_")

def chk(exe, d, tag):
    env = dict(os.environ, KSN_CACHE_DIR=os.path.join(tmp, "cache_" + tag))
    r = subprocess.run([exe, "check", "--json"], cwd=d, env=env, capture_output=True, text=True, timeout=600)
    return (r.returncode, r.stdout, r.stderr)

def one(item):
    name, d = item
    a, b = chk(old, d, "o"), chk(new, d, "n")
    return name, a == b, a, b

items = []
for sub in ("unsound", "linear_ok", "diag"):
    for f in sorted(glob.glob(f"{ROOT}/stage0/tests/data/{sub}/*.ksnl")):
        d = os.path.join(tmp, "p", sub + "_" + os.path.basename(f)[:-5])
        os.makedirs(d + "/src")
        open(d + "/kasane.toml", "w").write(TOML)
        open(d + "/kasane.lock", "w").write("version = 1\n")
        shutil.copy(f, d + "/src/t.ksnl")
        subprocess.run([old, "fmt"], cwd=d, capture_output=True)
        items.append((sub + "/" + os.path.basename(f), d))
for g in gens:
    for c in sorted(os.listdir(g)):
        if os.path.exists(f"{g}/{c}/kasane.toml"):
            items.append((f"gen/{c}", f"{g}/{c}"))
for p in pkgs:
    items.append((p, p))
bad = 0
with cf.ThreadPoolExecutor(int(os.environ.get("JOBS", "6"))) as ex:
    for name, same, a, b in ex.map(one, items):
        if not same:
            bad += 1
            print("DIFF", name, a[0], b[0])
print(f"{len(items)} cases, {bad} differ")
shutil.rmtree(tmp, ignore_errors=True)
sys.exit(1 if bad else 0)
