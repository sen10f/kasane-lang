#!/usr/bin/env python3
"""bench/agent_out/<実行名>/<課題>.ksnl を課題のテストで評価する。使い方: python bench/eval_files.py <実行名> [課題,...]"""
import os
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(ROOT, 'bench'))
import run_bench as rb  # noqa: E402
from tasks import TASKS  # noqa: E402

run = sys.argv[1]
want = set(sys.argv[2].split(',')) if len(sys.argv) > 2 else None
ok = 0
n = 0
for t in TASKS:
    if want and t['id'] not in want:
        continue
    p = os.path.join(ROOT, 'bench', 'agent_out', run, t['id'] + '.ksnl')
    if not os.path.exists(p):
        print(f"{t['id']}: (no file)")
        continue
    n += 1
    code = open(p, encoding='utf-8').read()
    stage, out, codes = rb.evaluate(code, t['tests'])
    ok += stage == 'pass'
    print(f"{t['id']}: {stage} {sorted(set(codes))}")
    if stage != 'pass':
        print('   ' + out[:700].replace('\n', '\n   '))
print(f'pass {ok}/{n}')
