#!/usr/bin/env python3
"""評価結果から、書き直し用のフィードバックファイルを作る。使い方: python bench/make_feedback.py <実行名> <次の実行名>

不合格の課題ごとに bench/agent_out/<次の実行名>/<課題>.feedback.txt を書く。内容は run_bench.py が
モデルに返すものと同じ(診断またはテスト結果)。合格した課題は何も作らない。
"""
import os
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(ROOT, 'bench'))
import run_bench as rb  # noqa: E402
from tasks import TASKS  # noqa: E402

run, nxt = sys.argv[1], sys.argv[2]
outdir = os.path.join(ROOT, 'bench', 'agent_out', nxt)
os.makedirs(outdir, exist_ok=True)
nl = chr(10)
failed = []
for t in TASKS:
    p = os.path.join(ROOT, 'bench', 'agent_out', run, t['id'] + '.ksnl')
    if not os.path.exists(p):
        continue
    stage, out, _ = rb.evaluate(open(p, encoding='utf-8').read(), t['tests'])
    if stage == 'pass':
        continue
    what = 'ksn check の診断' if stage == 'check' else 'ksn test の結果(テストは評価側が追記したもの)'
    text = f'{what}:{nl}```{nl}{out[:1800]}{nl}```{nl}修正した src/bench.ksnl の完全な内容を書き直してください。{nl}'
    open(os.path.join(outdir, t['id'] + '.feedback.txt'), 'w', encoding='utf-8', newline=nl).write(text)
    failed.append(t['id'])
print(' '.join(failed))
