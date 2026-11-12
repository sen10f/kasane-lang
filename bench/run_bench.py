#!/usr/bin/env python3
"""ローカルの LLM (OpenAI 互換 API) が Kasane を書けるかを測るベンチ。

使い方:
  python bench/run_bench.py [--docs full|none] [--repairs 3] [--tasks gcd,fib] [--model gemma-4-12b-it]
  BENCH_API_KEY (無ければ NVIDIA_API_KEY) があれば Authorization: Bearer で送る (例: --base https://integrate.api.nvidia.com --model nvidia/nemotron-3.5-lightning-30b-a3b)

各課題で、モデルに実装を書かせ、`ksn check` と `ksn test` (評価側が追記したテスト) に通るかを見る。
失敗したら診断をモデルに返し、最大 --repairs 回まで書き直させる。結果は bench/results/ に保存する。
"""
import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(ROOT, 'bench'))
from tasks import TASKS  # noqa: E402

EXE = os.path.join(ROOT, 'stage0', 'target', 'release', 'ksn' + ('.exe' if os.name == 'nt' else ''))
TOML = ('[package]\nname = "bench"\nversion = "0.1.0"\nedition = 1\n\n[dependencies]\n')

INTRO = """あなたは Kasane というプログラミング言語のコードを書く。Kasane は Rust に似ているが別の言語で、規則が異なる。
以下の資料(チートシートと動く例)の書き方に厳密に従うこと。資料にない Rust の機能や標準ライブラリの名前を推測で使わない。
出力は、`src/bench.ksnl` の完全な内容を ```kasane コードブロック1つだけで返す。テストは書かない(評価側が追記する)。
パッケージ名は bench なので、定義はすべて bench. で始まる完全修飾名で書く。"""


INTRO_NONE = """あなたは Kasane というプログラミング言語のコードを書く。Kasane は Rust に似た言語だが、資料は与えない。
出力は、`src/bench.ksnl` の完全な内容を ```kasane コードブロック1つだけで返す。テストは書かない(評価側が追記する)。
パッケージ名は bench なので、定義はすべて bench. で始まる完全修飾名で書く。"""


def chat(base, model, messages, max_tokens, timeout):
    body = json.dumps({'model': model, 'messages': messages, 'temperature': 0.2, 'max_tokens': max_tokens}).encode()
    headers = {'Content-Type': 'application/json'}
    key = os.environ.get('BENCH_API_KEY') or os.environ.get('NVIDIA_API_KEY')
    if key:
        headers['Authorization'] = 'Bearer ' + key
    req = urllib.request.Request(base + '/v1/chat/completions', data=body, headers=headers)
    t = time.time()
    # 接続断・5xx・429 は待って再試行する (リモート API の一時的な失敗を課題の失敗にしない)
    for i in range(5):
        try:
            r = json.load(urllib.request.urlopen(req, timeout=timeout))
            break
        except (urllib.error.URLError, ConnectionError, TimeoutError) as e:
            code = getattr(e, 'code', None)
            if i == 4 or (code is not None and code < 500 and code != 429):
                raise
            time.sleep(15 * 2 ** i)
    msg = r['choices'][0]['message']
    return (msg.get('content') or ''), r.get('usage', {}), time.time() - t, r['choices'][0].get('finish_reason')


def extract_code(text):
    m = re.findall(r'```(?:kasane|ksnl|rust|text)?\s*\n(.*?)```', text, re.S)
    if m:
        return max(m, key=len).rstrip() + '\n'
    return None


def run_ksn(args, cwd, timeout=120):
    r = subprocess.run([EXE] + args, cwd=cwd, capture_output=True, text=True, encoding='utf-8', errors='replace',
                       timeout=timeout)
    return r.returncode, (r.stdout + r.stderr)


def evaluate(code, tests):
    """コード + テストをパッケージにして check と test を実行する。(段階, 出力, コード一覧) を返す。"""
    with tempfile.TemporaryDirectory() as td:
        os.makedirs(os.path.join(td, 'src'))
        open(os.path.join(td, 'kasane.toml'), 'w', newline='\n').write(TOML)
        open(os.path.join(td, 'kasane.lock'), 'w', newline='\n').write('version = 1\n')
        nl = chr(10)
        src = code.rstrip(nl) + nl
        if tests:
            src += nl + tests.rstrip(nl) + nl
        open(os.path.join(td, 'src', 'bench.ksnl'), 'w', newline='\n', encoding='utf-8').write(src)
        rc, out = run_ksn(['check'], td)
        if rc != 0:
            return 'check', out.strip(), re.findall(r'\b(E\d{4})\b', out)
        rc, out = run_ksn(['test'], td)
        if rc != 0:
            return 'test', out.strip(), []
        return 'pass', '', []


def build_docs(mode):
    if mode == 'none':
        return INTRO_NONE
    sheet = open(os.path.join(ROOT, 'チートシート.md'), encoding='utf-8').read()
    example = open(os.path.join(ROOT, 'bench', 'example.ksnl'), encoding='utf-8').read()
    return (INTRO + '\n\n# チートシート\n\n' + sheet + '\n\n# 動く例 (src/bench.ksnl の全体。この形式で書く)\n\n```kasane\n'
            + example + '```\n')


def task_prompt(task):
    return (f"課題: {task['title']}\n\n{task['spec']}\n\n"
            f"次の宣言(名前と型)で実装すること。ヘッダの uses / panics は自分で正しく決める:\n```\n{task['decl']}\n```\n"
            "関数は pub にしなくてよい(テストは同じファイルに追記される)。")


def run_dir(a):
    """この実行の生成物を試行ごとに保存するディレクトリ。実行中でも中身を読める。"""
    if not hasattr(run_dir, 'path'):
        name = time.strftime('%Y%m%d-%H%M%S') + f"-{a.docs}{('-' + a.tag) if a.tag else ''}"
        run_dir.path = os.path.join(ROOT, 'bench', 'results', name)
        os.makedirs(run_dir.path, exist_ok=True)
    return run_dir.path


def dump_attempt(a, task_id, n, code, stage, out):
    nl = chr(10)
    d = run_dir(a)
    open(os.path.join(d, f'{task_id}-{n}.ksnl'), 'w', encoding='utf-8', newline=nl).write(code)
    note = f'// stage: {stage}{nl}' + ''.join(f'// {l}{nl}' for l in out.splitlines()[:20])
    open(os.path.join(d, f'{task_id}-{n}.log'), 'w', encoding='utf-8', newline=nl).write(note)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--base', default='http://127.0.0.1:1234')
    ap.add_argument('--model', default='gemma-4-12b-it')
    ap.add_argument('--docs', default='full', choices=['full', 'none'])
    ap.add_argument('--repairs', type=int, default=3)
    ap.add_argument('--tasks', default='')
    ap.add_argument('--max-tokens', type=int, default=12000)
    ap.add_argument('--timeout', type=int, default=900)
    ap.add_argument('--tag', default='')
    a = ap.parse_args()
    sys.stdout.reconfigure(encoding='utf-8')

    # 資料の例が実際に通ることを確認する。
    if a.docs == 'full':
        ex = open(os.path.join(ROOT, 'bench', 'example.ksnl'), encoding='utf-8').read()
        st, out, _ = evaluate(ex, '')
        assert st == 'pass', f'example.ksnl does not pass: {st}\n{out}'

    want = set(a.tasks.split(',')) if a.tasks else None
    system = build_docs(a.docs)
    results = []
    t0 = time.time()
    for task in TASKS:
        if want and task['id'] not in want:
            continue
        msgs = [{'role': 'system', 'content': system}, {'role': 'user', 'content': task_prompt(task)}]
        attempts = []
        final = 'fail'
        for k in range(a.repairs + 1):
            try:
                content, usage, secs, fin = chat(a.base, a.model, msgs, a.max_tokens, a.timeout)
            except Exception as e:  # noqa: BLE001
                attempts.append({'stage': 'api-error', 'detail': str(e)[:200]})
                break
            code = extract_code(content)
            rec = {'secs': round(secs, 1), 'completion_tokens': usage.get('completion_tokens'),
                   'prompt_tokens': usage.get('prompt_tokens'), 'finish': fin}
            if code is None:
                rec.update(stage='no-code', detail=content[:300])
                attempts.append(rec)
                msgs.append({'role': 'assistant', 'content': content or '(empty)'})
                msgs.append({'role': 'user', 'content': '```kasane コードブロックで src/bench.ksnl の完全な内容を返してください。'})
                continue
            stage, out, codes = evaluate(code, task['tests'])
            rec.update(stage=stage, codes=codes, detail=out[:600], code=code)
            attempts.append(rec)
            dump_attempt(a, task['id'], k + 1, code, stage, out)
            print(f"[{task['id']}] attempt {k + 1}: {stage} {sorted(set(codes))} ({secs:.0f}s, {usage.get('completion_tokens')} tok)",
                  flush=True)
            if stage == 'pass':
                final = 'pass'
                break
            msgs.append({'role': 'assistant', 'content': '```kasane\n' + code + '```'})
            what = 'ksn check の診断' if stage == 'check' else 'ksn test の結果(テストは評価側が追記したもの)'
            msgs.append({'role': 'user', 'content': f'{what}:\n```\n{out[:1800]}\n```\n修正した src/bench.ksnl の完全な内容を返してください。'})
        results.append({'id': task['id'], 'final': final, 'attempts': attempts})
    summary = {
        'model': a.model, 'docs': a.docs, 'repairs': a.repairs, 'elapsed_s': round(time.time() - t0),
        'tasks': len(results),
        'pass_at_1': sum(1 for r in results if r['attempts'] and r['attempts'][0].get('stage') == 'pass'),
        'pass_at_n': sum(1 for r in results if r['final'] == 'pass'),
        'results': results,
    }
    os.makedirs(os.path.join(ROOT, 'bench', 'results'), exist_ok=True)
    name = time.strftime('%Y%m%d-%H%M%S') + f"-{a.docs}{('-' + a.tag) if a.tag else ''}.json"
    path = os.path.join(ROOT, 'bench', 'results', name)
    json.dump(summary, open(path, 'w', encoding='utf-8'), ensure_ascii=False, indent=1)
    print(f"\npass@1 {summary['pass_at_1']}/{summary['tasks']}  pass@{a.repairs + 1} {summary['pass_at_n']}/{summary['tasks']}"
          f"  ({summary['elapsed_s']}s)  -> {path}")


if __name__ == '__main__':
    main()
