"""ベンチの大規模課題。bench/tasks_large/<id>/task.json から読み込む。

各課題は bench/tasks_large/<id>/ に置く:
  task.json    {"id", "title", "spec", "decl", "level": "large", "order"}
               (order は TASKS_LARGE での位置、0 始まり。TASKS_LARGE はこの順に並ぶ)
  tests.ksnl   課題のテスト文字列
  reference.ksnl 参照解

TASKS_LARGE は order 順に並んだ課題のみ。各辞書のキーは id, title, spec, decl, tests の 5 つ。
REFERENCE_ONLY_LARGE は回帰用だけの id の一覧（現状は空）。
"""
import json
import os

TASKS_DIR = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'tasks_large')


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


TASKS_LARGE, REFERENCE_ONLY_LARGE = _load()