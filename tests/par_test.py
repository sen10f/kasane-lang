#!/usr/bin/env python3
"""Rust 版 ksn の cargo の試験を、試験のバイナリ単位で並列に回す (`cargo test --release` の代わり)。

使い方 (stage0 の中でも、リポジトリのどこからでも可):
  python3 tests/par_test.py [--tier fast|full] [--jobs N]

- 試験のバイナリの一覧は `cargo test --release --no-run --message-format=json` で取る。
- `--jobs N` 本ずつ並列に走らせ、各バイナリには `--test-threads = CPU の数 / N` を渡す。
  既定の N は CPU の数の 1/4 (最小 2)。cargo-nextest は使わない。
- `--tier fast` は tests/slow_tests.txt の遅いバイナリを除く。`--tier full` は全部 (既定)。
- 各バイナリの時間は stage0/target/par_test_times.json に書き、遅い順の上位 10 を出す。
- 失敗があれば、失敗したバイナリと試験の名前を最後に出し、終了コード 1。
"""
import argparse
import json
import os
import re
import subprocess
import sys
import time
from concurrent.futures import ThreadPoolExecutor

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
STAGE0 = os.path.join(ROOT, 'stage0')
SLOW_LIST = os.path.join(ROOT, 'tests', 'slow_tests.txt')
TIMES = os.path.join(STAGE0, 'target', 'par_test_times.json')
sys.stdout.reconfigure(encoding='utf-8')

RESULT_RE = re.compile(r'test result: \w+\. (\d+) passed; (\d+) failed; (\d+) ignored')
FAILED_RE = re.compile(r'^test (.+) \.\.\. FAILED$')


def read_slow():
    names = []
    if not os.path.exists(SLOW_LIST):
        return names
    for ln in open(SLOW_LIST, encoding='utf-8'):
        ln = ln.split('#', 1)[0].strip()
        if ln:
            names.append(ln)
    return names


def list_binaries():
    """試験のバイナリ (name, exe) の一覧。ビルドに失敗したら (None, 出力)。"""
    r = subprocess.run(['cargo', 'test', '--release', '--no-run', '--message-format=json'],
                       cwd=STAGE0, capture_output=True, text=True, encoding='utf-8', errors='replace')
    if r.returncode != 0:
        return None, (r.stdout + r.stderr)[-3000:]
    bins = {}
    for ln in r.stdout.splitlines():
        try:
            m = json.loads(ln)
        except ValueError:
            continue
        if m.get('reason') != 'compiler-artifact' or not m.get('executable'):
            continue
        if not m.get('profile', {}).get('test'):
            continue
        bins[m['target']['name']] = m['executable']
    return bins, ''


def run_one(name, exe, threads):
    t0 = time.monotonic()
    r = subprocess.run([exe, f'--test-threads={threads}'], cwd=STAGE0, capture_output=True,
                       text=True, encoding='utf-8', errors='replace')
    dt = time.monotonic() - t0
    out = r.stdout + r.stderr
    passed = failed = ignored = 0
    for m in RESULT_RE.finditer(out):
        passed += int(m.group(1))
        failed += int(m.group(2))
        ignored += int(m.group(3))
    fails = [m.group(1) for m in (FAILED_RE.match(l) for l in out.splitlines()) if m]
    ok = r.returncode == 0 and failed == 0
    return {'name': name, 'ok': ok, 'rc': r.returncode, 'secs': dt, 'passed': passed,
            'failed': failed, 'ignored': ignored, 'fails': fails, 'out': out}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--tier', choices=['fast', 'full'], default='full')
    ap.add_argument('--jobs', type=int, default=None)
    args = ap.parse_args()

    cpu = os.cpu_count() or 4
    jobs = args.jobs if args.jobs else max(2, cpu // 4)
    threads = max(1, cpu // jobs)

    t_all = time.monotonic()
    bins, err = list_binaries()
    if bins is None:
        print('FAIL cargo test --no-run (ビルド)')
        print(err)
        return 1

    slow = read_slow()
    bad_slow = [n for n in slow if n not in bins]
    if bad_slow:
        print(f'FAIL tests/slow_tests.txt に試験のバイナリが無い名前: {bad_slow}')
        return 1
    if args.tier == 'fast':
        skipped = sorted(n for n in bins if n in slow)
        todo = {n: e for n, e in bins.items() if n not in slow}
        print(f'tier fast: {len(todo)} バイナリ (除外 {len(skipped)}: {", ".join(skipped)})')
    else:
        todo = bins
        print(f'tier full: {len(todo)} バイナリ')
    print(f'jobs={jobs} test-threads={threads}')

    results = []
    with ThreadPoolExecutor(max_workers=jobs) as ex:
        futs = [ex.submit(run_one, n, e, threads) for n, e in sorted(todo.items())]
        for f in futs:
            res = f.result()
            results.append(res)
            mark = 'ok  ' if res['ok'] else 'FAIL'
            print(f'{mark} {res["name"]:<28} {res["secs"]:7.1f}s  {res["passed"]} passed'
                  + (f', {res["failed"]} failed' if res['failed'] else '')
                  + (f' (rc={res["rc"]})' if not res['ok'] and not res['failed'] else ''))

    os.makedirs(os.path.dirname(TIMES), exist_ok=True)
    with open(TIMES, 'w', encoding='utf-8') as fh:
        json.dump({r['name']: round(r['secs'], 2) for r in results}, fh, ensure_ascii=False, indent=1)

    wall = time.monotonic() - t_all
    tot = {k: sum(r[k] for r in results) for k in ('passed', 'failed', 'ignored')}
    print()
    print('遅い順の上位 10:')
    for r in sorted(results, key=lambda r: -r['secs'])[:10]:
        print(f'  {r["name"]:<28} {r["secs"]:7.1f}s')
    print(f'合計: {tot["passed"]} passed, {tot["failed"]} failed, {tot["ignored"]} ignored'
          f' ({len(results)} バイナリ), 壁の時間 {wall:.1f} 秒')

    failed_bins = [r for r in results if not r['ok']]
    if not failed_bins:
        print('ALL TESTS PASSED (par_test)')
        return 0
    print()
    print(f'失敗したバイナリ: {[r["name"] for r in failed_bins]}')
    for r in failed_bins:
        print(f'--- {r["name"]} (rc={r["rc"]})')
        for n in r['fails']:
            print(f'test {n} ... FAILED')
        for ln in r['out'].splitlines():
            if 'panicked at' in ln:
                print('     ' + ln)
        if not r['fails'] and not any('panicked at' in ln for ln in r['out'].splitlines()):
            print('\n'.join('     ' + ln for ln in r['out'].splitlines()[-15:]))
    return 1


if __name__ == '__main__':
    sys.exit(main())
