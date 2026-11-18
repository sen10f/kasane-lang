#!/usr/bin/env python3
"""旧い検査と新しい検査 (stage0/src/tast_check.rs) の差を、(ファイル, 行, コード) の単位で台帳と突き合わせる。

使い方: python3 tests/shadow_diff.py [--full] [--update] [--table]
  既定は unsound と linear_ok だけ。--full で examples・apps・selfhost・perf・bench の参照解・check_ok も。
  各対象を `KSN_CHECKER=shadow ksn check` にかけ、stderr の JSON 行 ({"shadow": "old_only"|"new_only", ...}) を集める。
  台帳 tests/shadow_baseline.json に無い差が出たら失敗する。--update で台帳を書き直す (--full のときだけ全体、
  既定では今回見た対象の分だけを置き換える)。台帳に有って今回出なかった差は「解消」と表示する (失敗にはしない)。
"""
import json
import os
import shutil
import subprocess
import sys
import tempfile
from concurrent.futures import ThreadPoolExecutor

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
EXE = os.path.join(ROOT, 'stage0', 'target', 'release', 'ksn' + ('.exe' if os.name == 'nt' else ''))
BASE = os.path.join(ROOT, 'tests', 'shadow_baseline.json')
DATA = os.path.join(ROOT, 'stage0', 'tests', 'data')
TOML = '[package]\nname = "t"\nversion = "0.1.0"\nedition = 1\nentry = "t.main"\n\n[dependencies]\n'


def rel(p):
    return os.path.relpath(p, ROOT).replace(os.sep, '/')


def packages(top):
    out = []
    for d, dirs, files in os.walk(os.path.join(ROOT, top)):
        dirs[:] = sorted(x for x in dirs if x not in ('target', '.git'))
        if 'kasane.toml' in files:
            out.append(d)
    return sorted(out)


def targets(full):
    """(ラベル, パッケージの dir か単独ファイル) の列。"""
    t = []
    subs = ['unsound', 'linear_ok'] + (['check_ok'] if full else [])
    for s in subs:
        d = os.path.join(DATA, s)
        t += [(f'stage0/tests/data/{s}/{f}', os.path.join(d, f)) for f in sorted(os.listdir(d)) if f.endswith('.ksnl')]
    if full:
        for top in ('examples', 'apps', 'selfhost', 'perf'):
            t += [(rel(p), p) for p in packages(top)]
        d = os.path.join(ROOT, 'bench', 'tasks')
        for n in sorted(os.listdir(d)):
            f = os.path.join(d, n, 'reference.ksnl')
            if os.path.exists(f):
                t.append((rel(f), f))
    return t


def run_one(item):
    label, path = item
    tmp = None
    args = ['check', '--json']
    cwd = path
    if os.path.isfile(path):
        tmp = tempfile.mkdtemp(prefix='shadow_diff_')
        os.makedirs(os.path.join(tmp, 'src'))
        open(os.path.join(tmp, 'kasane.toml'), 'w').write(TOML)
        open(os.path.join(tmp, 'kasane.lock'), 'w').write('version = 1\n')
        shutil.copy(path, os.path.join(tmp, 'src', 't.ksnl'))
        cwd = tmp
        # 印 (`target: X`) は横の `<名前>.expect` にある (`.ksnl` にコメントは置けない)。
        exp = os.path.splitext(path)[0] + '.expect'
        if os.path.exists(exp):
            for l in open(exp, encoding='utf-8'):
                if l.startswith('target:'):
                    args += ['--target', l.split(':', 1)[1].strip()]
    env = dict(os.environ, KSN_CHECKER='shadow')
    r = subprocess.run([EXE] + args, cwd=cwd, env=env, capture_output=True, text=True, encoding='utf-8', errors='replace')
    found = set()
    for l in r.stderr.splitlines():
        if not l.startswith('{"shadow"'):
            continue
        j = json.loads(l)
        f = j['file']
        if tmp and f.startswith(tmp):
            f = label
        elif f.startswith(ROOT + '/'):
            f = rel(f)
        found.add((j['shadow'], f, j['line'], j['code']))
    if tmp:
        shutil.rmtree(tmp, ignore_errors=True)
    return label, found


def main():
    full = '--full' in sys.argv or '--shadow-full' in sys.argv
    update = '--update' in sys.argv
    if not os.path.exists(EXE):
        print('ksn is not built:', EXE)
        return 2
    ts = targets(full)
    with ThreadPoolExecutor(max_workers=min(16, os.cpu_count() or 4)) as ex:
        res = list(ex.map(run_one, ts))
    seen = set()
    per = {}
    for label, found in res:
        for side, f, l, c in found:
            seen.add((side, f, l, c))
            per.setdefault(label, [0, 0])[0 if side == 'old_only' else 1] += 1
    base = set()
    if os.path.exists(BASE):
        base = {tuple(x) for x in json.load(open(BASE, encoding='utf-8'))}
    labels = {l for l, _ in ts}
    if update:
        # 今回見た対象の台帳だけを置き換える (見ていない対象の記録は残す)。
        keep = {b for b in base if not any(b[1] == l or b[1].startswith(l + "/") for l in labels)}
        new = sorted(keep | seen)
        json.dump([list(x) for x in new], open(BASE, 'w', encoding='utf-8'), ensure_ascii=False, indent=0)
        open(BASE, 'a').write('\n')
        print(f'baseline updated: {len(new)} entries ({len(seen)} from this run)')
        return 0
    # 表: ファイルごとの旧だけ / 新だけ (差のあるものだけ)
    print(f'shadow: {len(ts)} targets, old-only {sum(v[0] for v in per.values())}, new-only {sum(v[1] for v in per.values())}')
    if '--table' in sys.argv:
        for k in sorted(per):
            print(f'  {per[k][0]:4d} {per[k][1]:4d}  {k}')
    covered = lambda b: any(b[1] == l or b[1].startswith(l + '/') for l in labels)
    fresh = sorted(seen - base)
    gone = sorted(b for b in base - seen if covered(b))
    for x in gone[:20]:
        print('resolved (no longer differs):', x)
    for x in fresh[:50]:
        print('NEW DIFF:', x)
    if fresh:
        print(f'FAIL: {len(fresh)} differences not in tests/shadow_baseline.json (fix, or --update after review)')
        return 1
    print('ok: no differences beyond the baseline')
    return 0


if __name__ == '__main__':
    sys.exit(main())
