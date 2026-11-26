#!/usr/bin/env python3
"""属性の差分試験: 同じ Kasane プログラムを release で 2 回ビルドし (通常 / `KSN_LLVM_NO_ATTRS=1`)、標準出力と終了コードを比べる。
`KSN_LLVM_NO_ATTRS` は feature `test-hooks` つきのビルドだけが認める (kir_llvm.rs の strip_attrs)。外した版は noalias・readonly・memory(..)・
nonnull・dereferenceable・align>1・nounwind・alwaysinline を持たない IR になるので、差があれば誤った約束による誤コンパイルである。

使い方: python3 tests/attr_diff.py [--fast] [--ksn PATH_通常 --hooks-ksn PATH_test-hooks] [--only linear_ok,examples,perf,xlang,gen]
                                   [--gen N] [--seed S] [--limit N] [--jobs J] [--keep]
  --fast  … 数百本・3 分以内 (linear_ok 全部 + examples + perf、gen 40 本、xlang は含めない)。
  既定の --ksn は stage0/target/release/ksn、--hooks-ksn は --ksn と同じ (feature 付きでビルドしたものを渡す)。無ければ
  CARGO_TARGET_DIR=<tmp> cargo build --release --features test-hooks で作る (--build-hooks)。
  --ksn に修正前のバイナリ (feature が無いなら、test-hooks 付きで古い commit を作る) を渡すと、Mythos3 L1 の再現
  (stage0/tests/data/llvm_attrs/argmem_tuple_ref.ksnl) の差が出ることを確かめられる。--repro-only はそれだけを走らせる。
終了コード: 差があれば 1。ビルドの失敗は両方で同じなら差と見なさない (数だけ出す)。
"""
import argparse, concurrent.futures as cf, glob, hashlib, os, re, shutil, subprocess, sys, tempfile, time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
XL = os.environ.get('KSN_XLANG_SRC', '')
GEN = os.environ.get('KSN_HUNT_GEN', '')
NL = '\n'
TOML = NL.join(['[package]', 'name = "t"', 'version = "0.1.0"', 'edition = 1', 'entry = "t.main"', '', '[dependencies]', ''])
ENTRY_RE = re.compile(r'^entry\s*=', re.M)
RUN_TIMEOUT = 20
BUILD_TIMEOUT = 300


def wrap_single(path, dst):
    os.makedirs(os.path.join(dst, 'src'))
    open(os.path.join(dst, 'kasane.toml'), 'w', newline=NL).write(TOML)
    open(os.path.join(dst, 'kasane.lock'), 'w', newline=NL).write('version = 1' + NL)
    shutil.copyfile(path, os.path.join(dst, 'src', 't.ksnl'))


def collect(work, groups, gen_n, seed, limit):
    jobs = []
    if 'linear_ok' in groups:
        for p in sorted(glob.glob(os.path.join(ROOT, 'stage0/tests/data/linear_ok/*.ksnl'))):
            d = os.path.join(work, 'linear_ok', os.path.basename(p)[:-5]); wrap_single(p, d); jobs.append(('linear_ok/' + os.path.basename(p), d))
    if 'check_ok' in groups:
        for p in sorted(glob.glob(os.path.join(ROOT, 'stage0/tests/data/check_ok/*.ksnl'))):
            d = os.path.join(work, 'check_ok', os.path.basename(p)[:-5]); wrap_single(p, d); jobs.append(('check_ok/' + os.path.basename(p), d))
    if 'llvm_attrs' in groups:
        for p in sorted(glob.glob(os.path.join(ROOT, 'stage0/tests/data/llvm_attrs/*.ksnl'))):
            d = os.path.join(work, 'llvm_attrs', os.path.basename(p)[:-5]); wrap_single(p, d); jobs.append(('llvm_attrs/' + os.path.basename(p), d))
    for g in ('examples', 'perf'):
        if g in groups:
            if g == 'examples' and os.path.isdir(os.path.join(ROOT, 'ui')) and not os.path.isdir(os.path.join(work, 'ui')):
                shutil.copytree(os.path.join(ROOT, 'ui'), os.path.join(work, 'ui'), ignore=shutil.ignore_patterns('target', '__pycache__'))
            dst = os.path.join(work, g)
            shutil.copytree(os.path.join(ROOT, g), dst, ignore=shutil.ignore_patterns('target', '__pycache__'))
            for n in sorted(os.listdir(dst)):
                t = os.path.join(dst, n, 'kasane.toml')
                if os.path.isfile(t) and ENTRY_RE.search(open(t, encoding='utf-8').read()):
                    jobs.append((f'{g}/{n}', os.path.join(dst, n)))
    if 'xlang' in groups and os.path.isdir(XL):
        for n in sorted(os.listdir(XL)):
            s = os.path.join(XL, n, 'ksn')
            if os.path.isfile(os.path.join(s, 'kasane.toml')):
                d = os.path.join(work, 'xlang', n); shutil.copytree(s, d, ignore=shutil.ignore_patterns('target')); jobs.append((f'xlang/{n}', d))
    if 'gen' in groups and gen_n and os.path.isfile(GEN):
        gd = os.path.join(work, 'gen_work'); os.makedirs(gd)
        src = open(GEN, encoding='utf-8').read()
        src = src.replace('/home/kasane/kasane-verify/agents/hunt3/native', gd).replace('/tmp/hunt3nat_pkgs', gd + '/pkgs')
        src = src.replace('/tmp/hunt3nat_cache', gd + '/cache').replace('RANDOM_SEED = 20261124', f'RANDOM_SEED = {seed}')
        sp = os.path.join(gd, 'gen_copy.py'); open(sp, 'w', encoding='utf-8').write(src)
        subprocess.run([sys.executable, '-I', sp, 'gen', str(gen_n)], capture_output=True, timeout=600)
        for d in sorted(glob.glob(gd + '/pkgs/prog*')):
            jobs.append(('gen/' + os.path.basename(d), d))
    if limit:
        jobs = jobs[:limit]
    return jobs


def build_run(ksn, base, noattrs):
    """base を写して release ビルド → 実行。(状態, 標準出力のハッシュ, rc)。"""
    d = base + ('.na' if noattrs else '.at')
    shutil.copytree(base, d, ignore=shutil.ignore_patterns('target'), dirs_exist_ok=False)
    env = dict(os.environ, KSN_CACHE_DIR=d + '.cache')
    if noattrs:
        env['KSN_LLVM_NO_ATTRS'] = '1'
    else:
        env.pop('KSN_LLVM_NO_ATTRS', None)
    try:
        r = subprocess.run([ksn, 'build', '--release'], cwd=d, env=env, capture_output=True, timeout=BUILD_TIMEOUT)
        if r.returncode != 0:
            return ('buildfail', '', r.returncode)
        exe = None
        m = re.search(rb'native executable: (\S+)', r.stdout)
        if m:
            exe = m.group(1).decode()
            exe = exe if os.path.isabs(exe) else os.path.join(d, exe)
        if not exe or not os.path.isfile(exe):
            return ('nonative', '', 0)  # バンドルへ退避した: 属性の差が無い
        try:
            p = subprocess.run([exe], cwd=d, env=env, stdin=subprocess.DEVNULL, capture_output=True, timeout=RUN_TIMEOUT)
        except subprocess.TimeoutExpired:
            return ('timeout', '', 0)
        return ('ran', hashlib.md5(p.stdout).hexdigest(), p.returncode)
    except subprocess.TimeoutExpired:
        return ('buildtimeout', '', 0)
    finally:
        shutil.rmtree(d + '.cache', ignore_errors=True)
        shutil.rmtree(d, ignore_errors=True)


def one(args):
    name, base, ksn, hooks = args
    a = build_run(ksn, base, False)
    b = build_run(hooks, base, True)
    return name, a, b


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--fast', action='store_true')
    ap.add_argument('--ksn', default=os.path.join(ROOT, 'stage0/target/release/ksn'))
    ap.add_argument('--hooks-ksn', default=None)
    ap.add_argument('--only', default=None)
    ap.add_argument('--gen', type=int, default=None)
    ap.add_argument('--seed', type=int, default=20261124)
    ap.add_argument('--limit', type=int, default=0)
    ap.add_argument('--jobs', type=int, default=8)
    ap.add_argument('--repro-only', action='store_true')
    a = ap.parse_args()
    hooks = a.hooks_ksn
    if not hooks:
        # feature test-hooks つきを別の target ディレクトリへ作る (通常の stage0/target の ksn は触らない)。
        td = os.path.join(ROOT, 'stage0', 'target_hooks')
        r = subprocess.run(['cargo', 'build', '--release', '--features', 'test-hooks'], cwd=os.path.join(ROOT, 'stage0'),
                           env=dict(os.environ, CARGO_TARGET_DIR=td), capture_output=True, text=True)
        if r.returncode != 0:
            print('attr_diff: test-hooks のビルドに失敗\n' + r.stderr[-800:]); return 2
        hooks = os.path.join(td, 'release', 'ksn')
    if a.repro_only:
        groups = {'llvm_attrs'}
    elif a.only:
        groups = set(a.only.split(','))
    elif a.fast:
        groups = {'linear_ok', 'llvm_attrs', 'examples', 'perf', 'gen'}
    else:
        groups = {'linear_ok', 'check_ok', 'llvm_attrs', 'examples', 'perf', 'xlang', 'gen'}
    gen_n = a.gen if a.gen is not None else (40 if a.fast else 200)
    if not os.path.isfile(a.ksn) or not os.path.isfile(hooks):
        print(f'attr_diff: ksn が無い ({a.ksn} / {hooks})'); return 2
    t0 = time.monotonic()
    work = tempfile.mkdtemp(prefix='attr_diff_')
    try:
        jobs = collect(work, groups, gen_n, a.seed, a.limit)
        with cf.ThreadPoolExecutor(max_workers=a.jobs) as ex:
            res = list(ex.map(one, [(n, d, a.ksn, hooks) for n, d in jobs]))
    finally:
        shutil.rmtree(work, ignore_errors=True)
    diffs = [(n, x, y) for n, x, y in res if x != y]
    cnt = {}
    for _, x, _ in res:
        cnt[x[0]] = cnt.get(x[0], 0) + 1
    print(f'attr_diff: {len(res)} programs, {cnt}, {len(diffs)} differ ({time.monotonic() - t0:.1f} s)')
    for n, x, y in diffs:
        print(f'  DIFF {n}: attrs={x} no-attrs={y}')
    return 1 if diffs else 0


if __name__ == '__main__':
    sys.exit(main())
