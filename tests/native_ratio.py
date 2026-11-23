#!/usr/bin/env python3
"""ネイティブになった割合を数える (修正プラン 3-5)。使い方: python3 tests/native_ratio.py

対象:
  - stage0/tests/data/linear_ok/*.ksnl … 1 本が 1 パッケージ (src/t.ksnl、入口 t.main)
  - examples/* と perf/* のうち入口 (kasane.toml の entry) を持つパッケージ (kvlib のような库は数えない)
各々を一時ディレクトリへ写して `ksn build` し、標準出力の行で判定する:
  「native executable: <path>」                      → ネイティブ (LLVM)
  「native backend skipped: <理由>; ...」           → 退避 (インタプリタ同梱のバンドル)
  それ以外 (終了コードが 0 でない・成果物が無い等)    → ネイティブではない (理由を出す)
下限は tests/native_ratio_floor.json。下回ったら終了コード 1。上回ったら下限を上げるよう note を出すだけで、
ファイルは書き換えない。前提: `cd stage0 && cargo build --release` 済み (stage0/target/release/ksn)。
"""
import concurrent.futures as cf
import glob
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time

sys.stdout.reconfigure(encoding='utf-8')
sys.stderr.reconfigure(encoding='utf-8')
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
KSN = os.path.join(ROOT, 'stage0', 'target', 'release', 'ksn' + ('.exe' if os.name == 'nt' else ''))
FLOOR_FILE = os.path.join(ROOT, 'tests', 'native_ratio_floor.json')
JOBS = 8          # 同時に走らせる ksn build の数
TIMEOUT = 120     # 1 本あたりの上限秒数
NLC = '\n'
TOML = NLC.join(['[package]', 'name = "t"', 'version = "0.1.0"', 'edition = 1', 'entry = "t.main"', '', '[dependencies]', ''])
ENTRY_RE = re.compile(r'^entry\s*=', re.M)
GROUPS = ['linear_ok', 'examples', 'perf']
OK_PREFIX = 'native executable: '
SKIP_PREFIX = 'native backend skipped: '


def classify(rc, out, err, cwd):
    """(判定, 理由)。判定は 'native' / 'bundle' / 'error'。"""
    lines = out.splitlines()
    if rc == 0:
        for line in lines:
            if line.startswith(OK_PREFIX):
                path = line[len(OK_PREFIX):].strip()
                full = path if os.path.isabs(path) else os.path.join(cwd, path)
                if os.path.isfile(full):
                    return 'native', ''
                return 'error', f'成果物が無い: {path}'
    for line in lines:
        if line.startswith(SKIP_PREFIX):
            return 'bundle', line[len(SKIP_PREFIX):]
    tail = (err.strip() or out.strip()).splitlines()
    return 'error', (tail[-1] if tail else f'rc={rc}')


def build(job):
    group, name, cwd = job
    env = dict(os.environ, KSN_CACHE_DIR=cwd.rstrip(os.sep) + '.cache')
    try:
        r = subprocess.run([KSN, 'build'], cwd=cwd, capture_output=True, text=True,
                           encoding='utf-8', errors='replace', env=env, timeout=TIMEOUT)
        cls, why = classify(r.returncode, r.stdout, r.stderr, cwd)
    except subprocess.TimeoutExpired:
        cls, why = 'error', f'{TIMEOUT} 秒を超えた'
    # 判定が済んだら、パッケージごとのキャッシュ (実行体の写しで 1 本 約 130 MB) をすぐ消す (全部を残すと /tmp が数十 GB 埋まる)。
    shutil.rmtree(env['KSN_CACHE_DIR'], ignore_errors=True)
    # ビルドの成果物 (target/ にバンドルが置かれる。1 本 約 130 MB) も判定の後すぐ消す。全部を残すと 1 回で 25 GB を超え、
    # tmpfs の /tmp が埋まる (2026-11-23)。
    shutil.rmtree(os.path.join(cwd, 'target'), ignore_errors=True)
    return group, name, cls, why


def collect(work):
    jobs = []
    # linear_ok: 1 本ずつ一時パッケージにする (中身は入力として読むだけ)
    src_dir = os.path.join(ROOT, 'stage0', 'tests', 'data', 'linear_ok')
    for p in sorted(glob.glob(os.path.join(src_dir, '*.ksnl'))):
        stem = os.path.splitext(os.path.basename(p))[0]
        d = os.path.join(work, 'linear_ok', stem)
        os.makedirs(os.path.join(d, 'src'))
        with open(os.path.join(d, 'kasane.toml'), 'w', encoding='utf-8', newline=NLC) as f:
            f.write(TOML)
        with open(os.path.join(d, 'kasane.lock'), 'w', encoding='utf-8', newline=NLC) as f:
            f.write('version = 1' + NLC)
        shutil.copyfile(p, os.path.join(d, 'src', 't.ksnl'))
        jobs.append(('linear_ok', os.path.basename(p), d))
    # 依存の `path` は相対パス (`../../ui/kasane` など) なので、examples と同じ位置関係で ui/ も写す (examples/ui_demo・tutorial_ui が要る)。
    if os.path.isdir(os.path.join(ROOT, 'ui')):
        shutil.copytree(os.path.join(ROOT, 'ui'), os.path.join(work, 'ui'), ignore=shutil.ignore_patterns('target', '__pycache__'))
    # examples / perf: 木ごと写す (examples/kvapp は ../kvlib に依存する)。target/ は写さない。
    for group in ('examples', 'perf'):
        dst = os.path.join(work, group)
        shutil.copytree(os.path.join(ROOT, group), dst, ignore=shutil.ignore_patterns('target', '__pycache__'))
        for name in sorted(os.listdir(dst)):
            d = os.path.join(dst, name)
            toml = os.path.join(d, 'kasane.toml')
            if not os.path.isfile(toml):
                continue
            with open(toml, encoding='utf-8') as f:
                if ENTRY_RE.search(f.read()):
                    jobs.append((group, name, d))
    return jobs


def main():
    if not os.path.isfile(KSN):
        print(f'native ratio: ksn が無い ({KSN})。先に cd stage0 && cargo build --release')
        return 2
    t0 = time.monotonic()
    work = tempfile.mkdtemp(prefix='ksn_native_ratio_')
    # `timeout` などの SIGTERM でも finally の片付けが走るように、SystemExit に変える。
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(143))
    try:
        jobs = collect(work)
        with cf.ThreadPoolExecutor(max_workers=JOBS) as ex:
            results = list(ex.map(build, jobs))
    finally:
        shutil.rmtree(work, ignore_errors=True)
    elapsed = time.monotonic() - t0

    groups = {g: {'total': 0, 'native': 0, 'miss': []} for g in GROUPS}
    for group, name, cls, why in results:
        g = groups[group]
        g['total'] += 1
        if cls == 'native':
            g['native'] += 1
        else:
            g['miss'].append(f'{name}: {cls}: {why}')
    fails, notes = [], []
    floor = {}
    if os.path.isfile(FLOOR_FILE):
        with open(FLOOR_FILE, encoding='utf-8') as f:
            floor = json.load(f)
    else:
        notes.append('tests/native_ratio_floor.json が無い: 下限を 0 として数えた')
    for g in GROUPS:
        n, total, fl = groups[g]['native'], groups[g]['total'], floor.get(g, 0)
        if n < fl:
            fails.append(f'{g}: {n}/{total} は下限 {fl} を下回った')
        elif n > fl:
            notes.append(f'{g} が {n} に上がった (下限 {fl})。tests/native_ratio_floor.json の {g} を {n} に上げる')
        for m in groups[g]['miss']:
            print(f'  not native: {g}/{m}')

    summary = ', '.join(f"{g} {groups[g]['native']}/{groups[g]['total']}" for g in GROUPS)
    print(f'native ratio: {summary} ({elapsed:.1f} s)')
    for x in fails:
        print('below floor: ' + x)
    for x in notes:
        print('note: ' + x)
    return 1 if fails else 0


if __name__ == '__main__':
    sys.exit(main())
