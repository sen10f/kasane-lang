#!/usr/bin/env python3
"""bench/reference/<課題>.ksnl の参照解が、課題のテストに通ることを確かめる(課題が言語側で解けるかの検査)。"""
import os
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(ROOT, 'bench'))
import run_bench as rb  # noqa: E402
from tasks import TASKS  # noqa: E402

want = set(sys.argv[1].split(',')) if len(sys.argv) > 1 else None
bad = 0
for t in TASKS:
    if want and t['id'] not in want:
        continue
    p = os.path.join(ROOT, 'bench', 'reference', t['id'] + '.ksnl')
    if not os.path.exists(p):
        continue
    stage, out, _ = rb.evaluate(open(p, encoding='utf-8').read(), t['tests'])
    print(f"{t['id']}: {stage}")
    if stage != 'pass':
        bad += 1
        print('   ' + out[:900].replace('\n', '\n   '))
sys.exit(1 if bad else 0)
