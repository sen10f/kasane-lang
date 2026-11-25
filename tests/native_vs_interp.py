#!/usr/bin/env python3
"""`ksn run` (インタプリタ) と `ksn build --release` の実行ファイルの、標準出力と終了コードを比べる (属性の誤コンパイル検出用)。
使い方: python3 tests/native_vs_interp.py [--ksn PATH] [--clang-args=-fsanitize=address] [--groups linear_ok,check_ok,examples,perf]
対象: stage0/tests/data/{linear_ok,check_ok}/*.ksnl (入口 t.main があるものだけ)、examples/*、perf/* (kasane.toml の入口を持つもの)。
"""
import concurrent.futures as cf, glob, os, shutil, subprocess, sys, tempfile, argparse
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
TOML = '[package]\nname = "t"\nversion = "0.1.0"\nedition = 1\nentry = "t.main"\n\n[dependencies]\n'
ap = argparse.ArgumentParser()
ap.add_argument('--ksn', default=os.path.join(ROOT, 'stage0/target/release/ksn'))
ap.add_argument('--groups', default='linear_ok,check_ok,examples,perf')
ap.add_argument('--jobs', type=int, default=6)
ap.add_argument('--clang-args', default='')
a = ap.parse_args()
env = dict(os.environ)
if a.clang_args:
    env['KSN_CLANG_ARGS'] = a.clang_args
T = 120

def run(cmd, cwd, e=env):
    try:
        p = subprocess.run(cmd, cwd=cwd, capture_output=True, timeout=T, stdin=subprocess.DEVNULL, env=e)
        return p.returncode, p.stdout, p.stderr
    except subprocess.TimeoutExpired:
        return 'timeout', b'', b''

def job(j):
    grp, name, src = j
    d = tempfile.mkdtemp(prefix='llvmattrs_cmp_')
    try:
        if os.path.isdir(src):
            w = os.path.join(d, 'p'); shutil.copytree(src, w, ignore=shutil.ignore_patterns('target', 'results'))
        else:
            w = os.path.join(d, 'p'); os.makedirs(w + '/src')
            open(w + '/kasane.toml', 'w').write(TOML); open(w + '/kasane.lock', 'w').write('version = 1\n')
            shutil.copy(src, w + '/src/t.ksnl')
        ri = run([a.ksn, 'run'], w)
        rb = run([a.ksn, 'build', '--release'], w)
        exes = [x for x in glob.glob(w + '/target/release/*') if os.path.isfile(x) and os.access(x, os.X_OK) and not x.endswith('.ll')]
        if rb[0] != 0 or not exes or b'native executable:' not in rb[1]:
            return grp, name, 'skip', 'no native'
        if ri[0] == 'timeout':
            return grp, name, 'skip', 'interp timeout'
        rn = run([exes[0]], w)
        if rn[0] == 'timeout':
            return grp, name, 'mismatch', 'native timeout'
        if (ri[0], ri[1]) != (rn[0], rn[1]):
            return grp, name, 'mismatch', f'rc {ri[0]} vs {rn[0]}; out {ri[1][:80]!r} vs {rn[1][:80]!r}; err {rn[2][-200:]!r}'
        return grp, name, 'ok', ''
    finally:
        shutil.rmtree(d, ignore_errors=True)

jobs = []
for g in a.groups.split(','):
    if g in ('linear_ok', 'check_ok'):
        for f in sorted(glob.glob(f'{ROOT}/stage0/tests/data/{g}/*.ksnl')):
            if 'fn t.main' in open(f).read():
                jobs.append((g, os.path.basename(f), f))
    else:
        for d in sorted(glob.glob(f'{ROOT}/{g}/*/kasane.toml')):
            if 'entry' in open(d).read():
                jobs.append((g, os.path.basename(os.path.dirname(d)), os.path.dirname(d)))
cnt = {}
bad = 0
with cf.ThreadPoolExecutor(a.jobs) as ex:
    for grp, name, st, why in ex.map(job, jobs):
        cnt[(grp, st)] = cnt.get((grp, st), 0) + 1
        if st == 'mismatch':
            bad += 1; print('MISMATCH', grp, name, why)
for k in sorted(cnt): print(k, cnt[k])
print('mismatches:', bad)
sys.exit(1 if bad else 0)
