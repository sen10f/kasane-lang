"""ベンチの課題。bench/tasks/<id>/task.json から読み込む。

各課題は bench/tasks/<id>/ に置く:

  task.json    {"id", "title", "spec", "decl", "level": "basic" | "hard", "order"}
               (order は元の TASKS での位置、0 始まり。TASKS はこの順に並ぶ)
  tests.ksnl   今の tests 文字列そのまま (課題があるものだけ)
  reference.ksnl 参照解 (あるものだけ。git mv で bench/reference/ から移した)

回帰用だけの課題 (テストも無く、回帰検査だけの参照解)は
  task.json    {"id", "title", "level": "reference-only"}
  reference.ksnl 参照解

課題の追加の仕方は bench/README.md の「課題の追加」を参照。

TASKS は order 順に並んだ課題(subset)のみ。各辞書のキーは id, title, spec, decl, tests の 5 つ。
REFERENCE_ONLY は回帰用だけの id の一覧。
"""
import json
import os

TASKS_DIR = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'tasks')


def _load():
    entries = []
    reference_only = []
    for name in sorted(os.listdir(TASKS_DIR)):
        d = os.path.join(TASKS_DIR, name)
        tj = os.path.join(d, 'task.json')
        if not os.path.isdir(d) or not os.path.exists(tj):
            continue
        with open(tj, encoding='utf-8') as fh:
            meta = json.load(fh)
        if meta.get('level') == 'reference-only':
            reference_only.append(meta['id'])
            continue
        with open(os.path.join(d, 'tests.ksnl'), encoding='utf-8') as fh:
            tests = fh.read()
        entries.append((meta['order'], {
            'id': meta['id'],
            'title': meta['title'],
            'spec': meta['spec'],
            'decl': meta['decl'],
            'tests': tests,
        }))
    entries.sort(key=lambda x: x[0])
    return [t for _, t in entries], reference_only


TASKS, REFERENCE_ONLY = _load()
