#!/usr/bin/env python3
"""静かなマシンで測るベンチ走者。1 分の負荷平均が閾値未満の状態が 2 分続くまで待ち (N 時間で諦める)、
xlang の計算ベンチ (Kasane / Rust / C clang / Go / Node) と perf/ の Kasane の課題を、各 7 回の最小値で測って日付つきの結果を書く。

使い方: python3 bench/quiet_run.py [--load 4] [--quiet-min 2] [--give-up-hours 6] [--runs 7] [--out DIR]
                                   [--ksn PATH] [--only vecsum,nbody] [--no-wait] [--no-build]
結果: <out>/quiet_YYYY-MM-DD_HHMM.md と .json (uptime・ksn の commit・CPU 機種つき)。既定の out は bench/results/ (gitignore 済み)。
測っている最中に負荷が閾値の 2 倍を超えたら、その結果に `noisy` と印を付ける (捨てずに残す)。
"""
import argparse, datetime, json, os, re, shutil, subprocess, sys, tempfile, time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
XB = os.environ.get('KSN_XLANG_BENCH', '')
SRC = f'{XB}/src'
GOBIN = f'{XB}/toolchains/go-x/go/bin'
BIN = os.environ.get('KSN_QUIET_BIN', os.path.join(tempfile.gettempdir(), 'ksn_quiet_bin'))
COMPUTE = "vecsum strbuild hashmap soa callchain bytescan mutref itchain closurehot dyncall smallalloc fmtparse enummatch binarytrees nbody fannkuch".split()
ENV = dict(os.environ, KSN_CACHE_DIR=os.path.join(tempfile.gettempdir(), 'ksn_quiet_cache'), GOCACHE=os.path.join(tempfile.gettempdir(), 'ksn_quiet_gocache'), GO111MODULE='off',
           PATH=f'{GOBIN}:{os.path.expanduser("~/.cargo/bin")}:' + os.environ['PATH'])


def sh(cmd, **kw):
    return subprocess.run(cmd, env=ENV, capture_output=True, text=True, **kw)


def load1():
    return os.getloadavg()[0]


def wait_quiet(thr, mins, give_up_h):
    end = time.time() + give_up_h * 3600
    quiet_since = None
    while time.time() < end:
        if load1() < thr:
            quiet_since = quiet_since or time.time()
            if time.time() - quiet_since >= mins * 60:
                return True
        else:
            quiet_since = None
        time.sleep(10)
    return False


def have(tool):
    return shutil.which(tool, path=ENV['PATH']) is not None


def build(ksn, benches, langs):
    os.makedirs(BIN, exist_ok=True)
    for n in benches:
        d = f'{SRC}/{n}'
        if 'rust' in langs: sh(['rustc', '-C', 'opt-level=3', '-o', f'{BIN}/{n}_rust', f'{d}/{n}.rs'])
        if 'c_clang' in langs: sh(['clang', '-O2', '-o', f'{BIN}/{n}_c_clang', f'{d}/{n}.c', '-lm'])
        if 'go' in langs: sh(['go', 'build', '-o', f'{BIN}/{n}_go', f'{n}.go'], cwd=d)
        if 'ksn' in langs:
            sh([ksn, 'build', '--release'], cwd=f'{d}/ksn')
            if os.path.isfile(f'{d}/ksn/target/release/{n}'):
                shutil.copy(f'{d}/ksn/target/release/{n}', f'{BIN}/{n}_ksn')


def perf_pkgs(ksn):
    out = []
    for n in sorted(os.listdir(f'{ROOT}/perf')):
        d = f'{ROOT}/perf/{n}'
        if os.path.isfile(f'{d}/kasane.toml') and re.search(r'^entry\s*=', open(f'{d}/kasane.toml', encoding='utf-8').read(), re.M):
            r = sh([ksn, 'build', '--release'], cwd=d)
            m = re.search(r'native executable: (\S+)', r.stdout)
            if m:
                exe = m.group(1) if os.path.isabs(m.group(1)) else os.path.join(d, m.group(1))
                dst = f'{BIN}/perf_{n}'; shutil.copy(exe, dst); out.append(n)
    return out


def best(cmd, runs):
    ts, lo, hi = [], load1(), load1()
    out = None
    for _ in range(runs):
        t0 = time.time()
        p = subprocess.run(cmd, env=ENV, capture_output=True, stdin=subprocess.DEVNULL, timeout=1800)
        ts.append(time.time() - t0)
        out = p.stdout.decode(errors='replace').strip()[-60:] if p.returncode == 0 else f'rc={p.returncode}'
        hi = max(hi, load1())
        if ts[0] > 40: break
    return {'min': min(ts), 'runs': [round(t, 3) for t in ts], 'out': out, 'max_load1': round(hi, 2)}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--load', type=float, default=4.0)
    ap.add_argument('--quiet-min', type=float, default=2.0)
    ap.add_argument('--give-up-hours', type=float, default=6.0)
    ap.add_argument('--runs', type=int, default=7)
    ap.add_argument('--out', default=f'{ROOT}/bench/results')
    ap.add_argument('--ksn', default=f'{ROOT}/stage0/target/release/ksn')
    ap.add_argument('--only', default=None)
    ap.add_argument('--no-wait', action='store_true')
    ap.add_argument('--no-build', action='store_true')
    a = ap.parse_args()
    benches = a.only.split(',') if a.only else COMPUTE
    langs = ['ksn', 'rust']
    for t, l in (('clang', 'c_clang'), ('go', 'go')):
        if have(t): langs.append(l)
    if not a.no_wait and not wait_quiet(a.load, a.quiet_min, a.give_up_hours):
        print(f'quiet_run: {a.give_up_hours} 時間待っても 1 分負荷平均 < {a.load} が {a.quiet_min} 分続かなかったので諦めた'); return 3
    if not a.no_build:
        build(a.ksn, benches, langs)
    perf = [] if a.no_build and a.only else perf_pkgs(a.ksn)
    cpu = next((l.split(':', 1)[1].strip() for l in open('/proc/cpuinfo') if l.startswith('model name')), '?')
    commit = subprocess.run(['git', 'rev-parse', '--short', 'HEAD'], cwd=ROOT, capture_output=True, text=True).stdout.strip()
    R = {'date': datetime.datetime.now().isoformat(timespec='seconds'), 'uptime': sh(['uptime']).stdout.strip(), 'ksn_commit': commit, 'cpu': cpu,
         'runs': a.runs, 'bench': {}}
    for n in benches:
        for l in langs + (['node'] if have('node') else []):
            cmd = ['node', f'{SRC}/{n}/{n}.js'] if l == 'node' else [f'{BIN}/{n}_{l}']
            if l != 'node' and not os.path.isfile(cmd[0]): continue
            r = best(cmd, a.runs); r['noisy'] = r['max_load1'] > 2 * a.load
            R['bench'].setdefault(n, {})[l] = r
            print(f'{n:12s} {l:8s} min={r["min"]:.3f} load={r["max_load1"]}{" noisy" if r["noisy"] else ""}', flush=True)
    for n in perf:
        r = best([f'{BIN}/perf_{n}'], a.runs); r['noisy'] = r['max_load1'] > 2 * a.load
        R['bench'].setdefault('perf/' + n, {})['ksn'] = r
        print(f'perf/{n:8s} ksn      min={r["min"]:.3f} load={r["max_load1"]}', flush=True)
    os.makedirs(a.out, exist_ok=True)
    stem = f'{a.out}/quiet_{datetime.datetime.now():%Y-%m-%d_%H%M}'
    json.dump(R, open(stem + '.json', 'w'), indent=1)
    L = langs + ['node']
    o = ['# 静かなマシンでのベンチ', '', f'- 日時: {R["date"]}', f'- uptime: `{R["uptime"]}`', f'- ksn commit: {commit}', f'- CPU: {cpu}',
         f'- 各セルは {a.runs} 回の最小 wall 秒 (Rust 比)。`~` は測定中に負荷が閾値の 2 倍を超えた印。', '',
         '| bench | ' + ' | '.join(L) + ' |', '|---|' + '---|' * len(L)]
    for n, row in R['bench'].items():
        base = row.get('rust', {}).get('min')
        cells = []
        for l in L:
            d = row.get(l)
            cells.append('-' if not d else f'{d["min"]:.3f}' + (f' ({d["min"] / base:.2f}x)' if base else '') + ('~' if d['noisy'] else ''))
        o.append(f'| {n} | ' + ' | '.join(cells) + ' |')
    open(stem + '.md', 'w').write('\n'.join(o) + '\n')
    print('wrote', stem + '.md')
    return 0


if __name__ == '__main__':
    sys.exit(main())
