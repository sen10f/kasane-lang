#!/usr/bin/env python3
"""`ksn run --engine=tree` と `--engine=vm` (KIR → VM) の、標準出力・終了コード (と panic 行) を比べる (S6: 既定の実行系を VM にする根拠の試験)。
使い方: python3 -I tests/engine_compare.py [--ksn PATH] [--groups linear_ok,check_ok,data,examples,perf,bench] [--jobs 6] [--extra DIR ...]
  `--extra DIR`: DIR の下の `*.ksnl` (入口 `t.main`) と、`kasane.toml` を持つパッケージも対象に足す (生成器の出力など)。
VM が退避した (stderr に `vm engine skipped`) ものは `skip` として数え、理由を集計する。不一致があれば終了コード 1。
"""
import argparse, collections, concurrent.futures as cf, glob, os, shutil, subprocess, sys, tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
TOML = '[package]\nname = "t"\nversion = "0.1.0"\nedition = 1\nentry = "t.main"\n\n[dependencies]\n'
ap = argparse.ArgumentParser()
ap.add_argument('--ksn', default=os.path.join(ROOT, 'stage0/target/release/ksn'))
ap.add_argument('--groups', default='linear_ok,check_ok,data,examples,perf,bench')
ap.add_argument('--jobs', type=int, default=6)
ap.add_argument('--timeout', type=int, default=60)
ap.add_argument('--extra', nargs='*', default=[])
ap.add_argument('--engine-env', default='', help='VM 側にだけ足す環境変数 (K=V,K=V)')
a = ap.parse_args()
env = dict(os.environ)
venv = dict(env)
for kv in filter(None, a.engine_env.split(',')):
    k, v = kv.split('=', 1)
    venv[k] = v


def run(cmd, cwd, e):
    try:
        p = subprocess.run(cmd, cwd=cwd, capture_output=True, timeout=a.timeout, stdin=subprocess.DEVNULL, env=e)
        return p.returncode, p.stdout, p.stderr
    except subprocess.TimeoutExpired:
        return 'timeout', b'', b''


def panic_lines(err):
    return [l for l in err.decode('utf-8', 'replace').splitlines() if l.startswith('panic:') or l.startswith('Error:')]


def job(j):
    grp, name, src = j
    d = tempfile.mkdtemp(prefix='s6kir_cmp_')
    try:
        w = os.path.join(d, 'p')
        if os.path.isdir(src):
            shutil.copytree(src, w, ignore=shutil.ignore_patterns('target', 'results'))
        else:
            os.makedirs(w + '/src')
            open(w + '/kasane.toml', 'w').write(TOML)
            open(w + '/kasane.lock', 'w').write('version = 1\n')
            shutil.copy(src, w + '/src/t.ksnl')
        rt = run([a.ksn, 'run', '--engine=tree'], w, env)
        rv = run([a.ksn, 'run', '--engine=vm'], w, venv)
        if rt[0] == 'timeout' or rv[0] == 'timeout':
            return grp, name, ('skip' if rt[0] == 'timeout' and rv[0] == 'timeout' else 'mismatch'), f'timeout tree={rt[0]} vm={rv[0]}'
        skipped = [l for l in rv[2].decode('utf-8', 'replace').splitlines() if 'vm engine skipped' in l]
        if skipped:
            return grp, name, 'skip', skipped[0].split('skipped:', 1)[-1].strip()[:110]
        if (rt[0], rt[1]) != (rv[0], rv[1]) or panic_lines(rt[2]) != panic_lines(rv[2]):
            return grp, name, 'mismatch', f'rc {rt[0]} vs {rv[0]}; out {rt[1][:80]!r} vs {rv[1][:80]!r}; err {rt[2][-100:]!r} vs {rv[2][-100:]!r}'
        return grp, name, 'ok', ''
    finally:
        shutil.rmtree(d, ignore_errors=True)


def single(g, files):
    return [(g, os.path.basename(f), f) for f in sorted(files) if 'fn t.main' in open(f, errors='replace').read()]


jobs = []
for g in a.groups.split(','):
    if g in ('linear_ok', 'check_ok'):
        jobs += single(g, glob.glob(f'{ROOT}/stage0/tests/data/{g}/*.ksnl'))
    elif g == 'data':
        jobs += single(g, glob.glob(f'{ROOT}/stage0/tests/data/*.ksnl'))
    elif g == 'bench':
        jobs += single(g, glob.glob(f'{ROOT}/bench/tasks/*/reference.ksnl'))
    else:
        for d in sorted(glob.glob(f'{ROOT}/{g}/*/kasane.toml')):
            if 'entry' in open(d).read():
                jobs.append((g, os.path.basename(os.path.dirname(d)), os.path.dirname(d)))
for x in a.extra:
    jobs += single('extra', glob.glob(f'{x}/**/*.ksnl', recursive=True))
    jobs += [('extra', os.path.basename(os.path.dirname(t)), os.path.dirname(t)) for t in sorted(glob.glob(f'{x}/*/kasane.toml'))]

cnt = collections.Counter()
why = collections.Counter()
bad = 0
with cf.ThreadPoolExecutor(a.jobs) as ex:
    for grp, name, st, w in ex.map(job, jobs):
        cnt[(grp, st)] += 1
        if st == 'mismatch':
            bad += 1
            print('MISMATCH', grp, name, w, flush=True)
        elif st == 'skip':
            why[w] += 1
            if os.environ.get('V'):
                print('SKIP', grp, name, w, flush=True)
for k in sorted(cnt):
    print(k, cnt[k])
for w, n in why.most_common(15):
    print(f'  skip {n:4d}  {w}')
print('mismatches:', bad)
sys.exit(1 if bad else 0)
