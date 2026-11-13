#!/usr/bin/env python3
"""課題のある参照解 (bench/tasks/*/reference.ksnl) を、木走査と VM の両方で採点して結果を比べる。

run_bench.evaluate と同じ形の一時パッケージ (参照解 + 課題のテスト) で `ksn test --engine=both` を走らせる。
木走査と VM が食い違う (出力に `disagree`) か、テストが落ちたら終了コード 1。VM が退避した課題は数だけ報告する。
使い方: python3 bench/vm_reference.py [id,id,...]
"""
import os
import re
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(ROOT, 'bench'))
import run_bench as rb  # noqa: E402
from tasks import TASKS  # noqa: E402


def compare(code, tests):
    """(成否, 出力, VM で比べた数, VM が退避した数)。"""
    with tempfile.TemporaryDirectory() as td:
        os.makedirs(os.path.join(td, 'src'))
        open(os.path.join(td, 'kasane.toml'), 'w', newline='\n').write(rb.TOML)
        open(os.path.join(td, 'kasane.lock'), 'w', newline='\n').write('version = 1\n')
        nl = chr(10)
        src = code.rstrip(nl) + nl
        if tests:
            src += nl + tests.rstrip(nl) + nl
        open(os.path.join(td, 'src', 'bench.ksnl'), 'w', newline='\n', encoding='utf-8').write(src)
        rc, out = rb.run_ksn_retry(['test', '--engine=both'], td)
        m = re.search(r'compared tree walker with vm: (\d+) \(vm skipped (\d+)\)', out)
        n_vm, n_skip = (int(m.group(1)), int(m.group(2))) if m else (0, 0)
        return rc == 0 and 'disagree' not in out, out, n_vm, n_skip


def main():
    want = set(sys.argv[1].split(',')) if len(sys.argv) > 1 else None
    bad = total = vm = skip = 0
    for t in TASKS:
        if want and t['id'] not in want:
            continue
        ref = os.path.join(ROOT, 'bench', 'tasks', t['id'], 'reference.ksnl')
        if not os.path.exists(ref):
            continue
        total += 1
        ok, out, n_vm, n_skip = compare(open(ref, encoding='utf-8').read(), t['tests'])
        vm += n_vm
        skip += n_skip
        if not ok:
            bad += 1
            print(f"{t['id']}: NG\n   " + out[:900].replace('\n', '\n   '))
        elif n_skip:
            print(f"{t['id']}: ok (VM 退避 {n_skip})")
    print(f'{total} 課題: 一致 {total - bad}、不一致または失敗 {bad}。テスト {vm + skip} 本のうち VM で比べた {vm}、VM が退避 {skip}')
    return 1 if bad else 0


if __name__ == '__main__':
    sys.exit(main())
