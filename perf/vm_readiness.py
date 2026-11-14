#!/usr/bin/env python3
"""VM を既定にする条件 (設計_KIR.md 7 章 17) の点検。標準ライブラリだけを使う。

条件: cargo test・examples の `test --both`・bench/reference がすべて一致し、perf の中央値で 5 倍以上。
この script は cargo test を除く 4 つを測る (cargo test は `stage0/tests/vm_parity.rs` などが担当)。

  parity : examples の各パッケージの `ksn run` と `ksn run --engine=vm` (出力・終了コード・標準エラーの一致、退避の理由)
  test   : examples の各パッケージの `ksn test --engine=both` (不一致の数、VM で比べたテストの数、退避の理由)
  bench  : bench/vm_reference.py (課題のある参照解を木走査と VM で比べる) の結果と退避の理由
  data   : stage0/tests/data/ の main を持つ単体のプログラム (`ksn run` と `--engine=vm` の一致)
  perf   : perf/vm_compare.py を taskset で固定したコア上で `--repeat` 回走らせた中央値 (実行前後のロードアベレージも記録)

使い方:
  python3 -I perf/vm_readiness.py [--sections parity,test,bench,data,perf] [--ksn PATH] [--jobs 4]
                                  [--perf-cpus 2,3] [--repeat 5] [--json 生データの出力先]

結果は Markdown の表として標準出力に出す。`--json` で生データを書ける (git 管理しない)。
"""
import argparse
import collections
import concurrent.futures as cf
import glob
import json
import os
import re
import signal
import subprocess
import sys
import tempfile
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DEFAULT_KSN = os.path.join(ROOT, 'stage0', 'target', 'release', 'ksn')
TOML = '[package]\nname = "vmready"\nversion = "0.1.0"\nedition = 1\nentry = "{entry}"\n\n[dependencies]\n'
ERR_CODE = re.compile(r'\bE\d{4}\b')
SKIP_RE = re.compile(r'vm engine skipped: (.*?); falling back')
TEST_RE = re.compile(r'^test (.+?) \.\.\. (ok|FAILED)')
DISAGREE_RE = re.compile(r'FAILED: (.*?) disagree:')
REASON_RE = re.compile(r'\bvm skipped: (.*)\)$')  # 「llvm skipped」の末尾の vm に当てない
SUMMARY_VM_RE = re.compile(r'compared tree walker with vm: (\d+) \(vm skipped (\d+)\)')
PERF_ROW_RE = re.compile(r'^\| (\w+) \| ([\d.]+|n/a) \| ([\d.]+|n/a) \| ([\d.]+x|n/a) \|')

# 点検から外すもの。examples/http はサーバ (listen して待つ)、kvlib は entry を持たないライブラリ。
# data の除外は、負の事例 (検査で止まるための入力) と、別の入口 (C の補助・複数ファイル) を持つもの。
PARITY_SKIP = {'http': 'サーバ (listen して待つ)', 'kvlib': 'entry なし (ライブラリ)'}
DATA_SKIP_DIRS = {'diag', 'unsound', 'refactor_pkg', 'ffi'}


def ksn(exe, args, cwd, timeout):
    """`ksn args` を cwd で走らせる。時間切れは進行グループごと殺す (子の実行ファイルも含む)。"""
    p = subprocess.Popen([exe, *args], cwd=cwd, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                         stderr=subprocess.PIPE, start_new_session=True, text=True, encoding='utf-8', errors='replace')
    timed = False
    try:
        out, err = p.communicate(timeout=timeout)
    except subprocess.TimeoutExpired:
        timed = True
        try:
            os.killpg(p.pid, signal.SIGKILL)
        except OSError:
            pass
        out, err = p.communicate()
    return {'code': None if timed else p.returncode, 'out': out.replace('\r\n', '\n'),
            'err': err.replace('\r\n', '\n'), 'timeout': timed}


def reason_category(msg):
    """退避の理由を、テスト名や行番号を除いた種類にする (数えるため)。`pkg.f を KIR に下ろせない (詳細)` → 詳細の種類。"""
    m = re.match(r'^[\w.]+ を KIR に下ろせない \((.*)\)$', msg)
    if m:
        msg = m.group(1)
    msg = re.sub(r'^[\w.]+: 行 \d+: ', '', msg)
    msg = re.sub(r'^[\w.]+: ', '', msg)
    msg = re.sub(r'\s*\([^()]*\)$', '', msg)  # 末尾の (詳細) だけを落とす (型の <...> 内の括弧は残す)
    return msg.strip()


def first_reason(err):
    m = SKIP_RE.search(err)
    return m.group(1).strip() if m else None


def strip_skip(err):
    return '\n'.join(l for l in err.split('\n') if 'vm engine skipped' not in l)


def compare_run(exe, pkgdir, timeout=120):
    """`ksn run` と `ksn run --engine=vm` を比べる。status: VM / 退避 / 不一致 / 検査 / 時間切れ。"""
    t = ksn(exe, ['run'], pkgdir, timeout)
    v = ksn(exe, ['run', '--engine=vm'], pkgdir, timeout)
    d = {'status': None, 'reason': None, 'detail': ''}
    if t['timeout'] or v['timeout']:
        d['status'] = '時間切れ'
        return d
    if t['code'] != 0 and ERR_CODE.search(t['err'] + t['out']) and not t['out']:
        d['status'] = '検査'
        return d
    d['reason'] = first_reason(v['err'])
    if (v['code'], v['out']) != (t['code'], t['out']):
        d['status'] = '不一致'
        d['detail'] = f"終了コード tree={t['code']} vm={v['code']}; 標準出力の一致={t['out'] == v['out']}"
    elif strip_skip(v['err']) != strip_skip(t['err']):
        d['status'] = '不一致'
        d['detail'] = 'stderr が違う'
    else:
        d['status'] = '退避' if d['reason'] else 'VM'
    return d


def pkg_entry(pkgdir):
    try:
        m = re.search(r'^entry\s*=\s*"([^"]+)"', open(os.path.join(pkgdir, 'kasane.toml'), encoding='utf-8').read(), re.M)
        return m.group(1) if m else None
    except OSError:
        return None


def example_pkgs():
    out = []
    for d in sorted(glob.glob(os.path.join(ROOT, 'examples', '*'))):
        if os.path.isfile(os.path.join(d, 'kasane.toml')):
            out.append(os.path.basename(d))
    return out


def section_parity(exe, jobs):
    rows = []
    for name in example_pkgs():
        pkg = os.path.join(ROOT, 'examples', name)
        if name in PARITY_SKIP or pkg_entry(pkg) is None:
            rows.append({'pkg': name, 'status': '飛ばす', 'reason': PARITY_SKIP.get(name, 'entry なし'), 'detail': ''})
            continue
        d = compare_run(exe, pkg)
        d['pkg'] = name
        rows.append(d)
    return rows


def run_data_file(exe, path):
    """単体ファイルを一時パッケージに入れ、main の FQN を入口にして比べる。"""
    src = open(path, encoding='utf-8').read()
    m = re.search(r'^\s*fn\s+([A-Za-z_]\w*)\.main\b', src, re.M)
    rel = os.path.relpath(path, ROOT)
    if not m:
        return {'pkg': rel, 'status': '飛ばす', 'reason': 'main なし', 'detail': ''}
    with tempfile.TemporaryDirectory(prefix='vmready_') as td:
        os.makedirs(os.path.join(td, 'src'))
        with open(os.path.join(td, 'kasane.toml'), 'w', newline='\n') as f:
            f.write(TOML.format(entry=f'{m.group(1)}.main'))
        with open(os.path.join(td, 'kasane.lock'), 'w', newline='\n') as f:
            f.write('version = 1\n')
        with open(os.path.join(td, 'src', 't.ksnl'), 'w', newline='\n', encoding='utf-8') as f:
            f.write(src)
        d = compare_run(exe, td, timeout=60)
    d['pkg'] = rel
    return d


def section_data(exe, jobs):
    base = os.path.join(ROOT, 'stage0', 'tests', 'data')
    files = []
    for p in sorted(glob.glob(os.path.join(base, '**', '*.ksnl'), recursive=True)):
        parts = set(os.path.relpath(p, base).split(os.sep))
        if parts & DATA_SKIP_DIRS:
            continue
        files.append(p)
    with cf.ThreadPoolExecutor(max_workers=jobs) as ex:
        return list(ex.map(lambda p: run_data_file(exe, p), files))


def section_test(exe):
    rows = []
    for name in example_pkgs():
        pkg = os.path.join(ROOT, 'examples', name)
        # `--both` (木走査と LLVM) と `--engine=both` (木走査と VM) を同時に使い、3 者を比べる。
        r = ksn(exe, ['test', '--both', '--engine=both'], pkg, 900)
        row = {'pkg': name, 'timeout': r['timeout'], 'tests': 0, 'ok': 0, 'failed': 0, 'vm_dis': [],
               'llvm_dis': 0, 'both_fail': 0, 'compared': None, 'skipped': None, 'reasons': [], 'llvm_fb': 0}
        if r['timeout']:
            rows.append(row)
            continue
        for line in r['out'].split('\n'):
            m = TEST_RE.match(line)
            if not m:
                continue
            row['tests'] += 1
            if 'both: native' in line or 'both: bundle' in line:
                row['llvm_fb'] += 1  # LLVM で下ろせず Cranelift / バンドルで比べた (情報)
            rm = REASON_RE.search(line)
            if rm:
                row['reasons'].append(rm.group(1).strip())
            if m.group(2) == 'ok':
                row['ok'] += 1
                continue
            row['failed'] += 1
            dm = DISAGREE_RE.search(line)
            if dm:
                who = [w.strip() for w in dm.group(1).split(',')]
                if 'vm' in who:
                    row['vm_dis'].append(m.group(1))
                else:
                    row['llvm_dis'] += 1
            else:
                row['both_fail'] += 1
        sm = SUMMARY_VM_RE.search(r['out'])
        if sm:
            row['compared'], row['skipped'] = int(sm.group(1)), int(sm.group(2))
        row['exit'] = r['code']
        rows.append(row)
    return rows


def section_bench():
    sys.path.insert(0, os.path.join(ROOT, 'bench'))
    import vm_reference as vr  # noqa: E402
    from tasks import TASKS  # noqa: E402
    rows = []
    for t in TASKS:
        ref = os.path.join(ROOT, 'bench', 'tasks', t['id'], 'reference.ksnl')
        if not os.path.exists(ref):
            continue
        ok, out, n_vm, n_skip = vr.compare(open(ref, encoding='utf-8').read(), t['tests'])
        reasons = [m.group(1).strip() for m in (REASON_RE.search(l) for l in out.split('\n')) if m]
        rows.append({'id': t['id'], 'ok': ok, 'vm': n_vm, 'skip': n_skip, 'reasons': reasons,
                     'out': '' if ok else out[:600]})
    return rows


def section_perf(cpus, repeat, exe):
    before = os.getloadavg()
    cmd = ['taskset', '-c', cpus, sys.executable, '-I', os.path.join(ROOT, 'perf', 'vm_compare.py'),
           '--repeat', str(repeat), '--ksn', exe]
    r = subprocess.run(cmd, cwd=ROOT, capture_output=True, text=True, timeout=7200)
    after = os.getloadavg()
    rows = []
    for line in r.stdout.split('\n'):
        m = PERF_ROW_RE.match(line)
        if m:
            rows.append({'case': m.group(1), 'tree': m.group(2), 'vm': m.group(3), 'ratio': m.group(4)})
    return {'cpus': cpus, 'repeat': repeat, 'load_before': before, 'load_after': after,
            'rc': r.returncode, 'rows': rows, 'stderr': r.stderr[-400:]}


# ---- 表示 ----

def md_parity(rows):
    print('### 1. examples の `ksn run` と `--engine=vm` の一致\n')
    print('| パッケージ | 判定 | 退避の理由 / 詳細 |')
    print('| --- | --- | --- |')
    for r in rows:
        print(f"| {r['pkg']} | {r['status']} | {r.get('reason') or r.get('detail') or ''} |")
    c = collections.Counter(r['status'] for r in rows)
    print(f"\n集計: VM で走った {c['VM']}、退避 {c['退避']}、不一致 {c['不一致']}、時間切れ {c['時間切れ']}、検査で止まった {c['検査']}、飛ばした {c['飛ばす']}。\n")
    rs = collections.Counter(reason_category(r['reason']) for r in rows if r['status'] == '退避')
    if rs:
        print('退避の理由 (上位):\n')
        for k, v in rs.most_common(10):
            print(f'- {v} 件: {k}')
        print()


def md_test(rows):
    print('### 2. examples の `ksn test --engine=both`\n')
    print('| パッケージ | テスト | ok | VM と不一致 | LLVM だけ不一致 | 両方の失敗 | VM で比べた | VM が退避 | LLVM が退避 (情報) | 終了 |')
    print('| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |')
    tot = collections.Counter()
    reasons = collections.Counter()
    for r in rows:
        if r['timeout']:
            print(f"| {r['pkg']} | 時間切れ | | | | | | | | |")
            continue
        print(f"| {r['pkg']} | {r['tests']} | {r['ok']} | {len(r['vm_dis'])} | {r['llvm_dis']} | {r['both_fail']} | "
              f"{r['compared'] if r['compared'] is not None else '-'} | {r['skipped'] if r['skipped'] is not None else '-'} | "
              f"{r['llvm_fb']} | {r['exit']} |")
        tot['llvm_fb'] += r['llvm_fb']
        for k in ('tests', 'ok', 'llvm_dis', 'both_fail'):
            tot[k] += r[k]
        tot['vm_dis'] += len(r['vm_dis'])
        tot['compared'] += r['compared'] or 0
        tot['skipped'] += r['skipped'] or 0
        reasons.update(reason_category(x) for x in r['reasons'])
    print(f"| 合計 | {tot['tests']} | {tot['ok']} | {tot['vm_dis']} | {tot['llvm_dis']} | {tot['both_fail']} | {tot['compared']} | {tot['skipped']} | {tot['llvm_fb']} | |\n")
    for r in rows:
        for t in r.get('vm_dis', []):
            print(f"- 不一致 (VM): {r['pkg']} / {t}")
    if reasons:
        print('\n退避の理由 (上位):\n')
        for k, v in reasons.most_common(10):
            print(f'- {v} 件: {k}')
    print()


def md_bench(rows):
    print('### 3. bench/reference (課題のある参照解)\n')
    print('| 課題 | 判定 | VM で比べた | VM が退避 | 退避の理由 |')
    print('| --- | --- | ---: | ---: | --- |')
    for r in rows:
        print(f"| {r['id']} | {'一致' if r['ok'] else '不一致'} | {r['vm']} | {r['skip']} | {'; '.join(sorted({reason_category(x) for x in r['reasons']}))} |")
    print(f"\n一致 {sum(r['ok'] for r in rows)} / {len(rows)} 課題。\n")
    for r in rows:
        if not r['ok']:
            print(f"```\n{r['id']}:\n{r['out']}\n```")


def md_data(rows):
    print('### 4. stage0/tests/data の単体プログラム (main を持つもの)\n')
    c = collections.Counter(r['status'] for r in rows)
    print(f"対象 {len(rows)} 本: VM で走った {c['VM']}、退避 {c['退避']}、不一致 {c['不一致']}、検査で止まった {c['検査']}、時間切れ {c['時間切れ']}、飛ばした {c['飛ばす']}。\n")
    bad = [r for r in rows if r['status'] in ('不一致', '時間切れ')]
    for r in bad:
        print(f"- {r['status']}: {r['pkg']} {r['detail']}")
    rs = collections.Counter(reason_category(r['reason']) for r in rows if r['status'] == '退避')
    if rs:
        print('\n退避の理由 (上位):\n')
        for k, v in rs.most_common(10):
            print(f'- {v} 件: {k}')
    print()


def md_perf(p):
    print('### 5. perf (木走査 / VM の中央値)\n')
    print(f"taskset -c {p['cpus']}、--repeat {p['repeat']}。ロードアベレージ 実行前 {p['load_before'][0]:.2f} / 実行後 {p['load_after'][0]:.2f} (1 分)。\n")
    print('| ケース | 木走査 (ms) | VM (ms) | 倍率 |')
    print('| --- | ---: | ---: | ---: |')
    for r in p['rows']:
        print(f"| {r['case']} | {r['tree']} | {r['vm']} | {r['ratio']} |")
    if p['rc'] != 0:
        print(f"\n(vm_compare.py が終了コード {p['rc']}: {p['stderr']})")
    print()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--sections', default='parity,test,bench,data,perf')
    ap.add_argument('--ksn', default=DEFAULT_KSN)
    ap.add_argument('--jobs', type=int, default=4)
    ap.add_argument('--perf-cpus', default='2,3')
    ap.add_argument('--repeat', type=int, default=5)
    ap.add_argument('--json', default='')
    a = ap.parse_args()
    secs = a.sections.split(',')
    res = {}
    t0 = time.time()
    if 'parity' in secs:
        res['parity'] = section_parity(a.ksn, a.jobs)
        md_parity(res['parity'])
    if 'test' in secs:
        res['test'] = section_test(a.ksn)
        md_test(res['test'])
    if 'bench' in secs:
        res['bench'] = section_bench()
        md_bench(res['bench'])
    if 'data' in secs:
        res['data'] = section_data(a.ksn, a.jobs)
        md_data(res['data'])
    if 'perf' in secs:
        res['perf'] = section_perf(a.perf_cpus, a.repeat, a.ksn)
        md_perf(res['perf'])
    print(f'(計測の所要時間 {time.time() - t0:.0f} 秒)')
    if a.json:
        with open(a.json, 'w', encoding='utf-8') as f:
            json.dump(res, f, ensure_ascii=False, indent=1)
    return 0


if __name__ == '__main__':
    sys.exit(main())
