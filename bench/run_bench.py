#!/usr/bin/env python3
"""ローカルの LLM (OpenAI 互換 API) が Kasane を書けるかを測るベンチ。

使い方:
  python bench/run_bench.py [--docs full|none] [--repairs 3] [--tasks gcd,fib] [--model gemma-4-12b-it]
  python bench/run_bench.py --provider nim            # NVIDIA NIM (環境変数 NVIDIA_API_KEY)。詳細は bench/README.md

各課題で、モデルに実装を書かせ、`ksn check` と `ksn test` (評価側が追記したテスト) に通るかを見る。
失敗したら診断をモデルに返し、最大 --repairs 回まで書き直させる。結果は bench/results/ に保存する。
"""
import argparse
import json
import email.utils
import os
import random
import re
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request
from concurrent.futures import ThreadPoolExecutor

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(ROOT, 'bench'))

EXE = os.environ.get('KSN_BENCH_EXE') or os.path.join(ROOT, 'stage0', 'target', 'release', 'ksn' + ('.exe' if os.name == 'nt' else ''))  # KSN_BENCH_EXE はテスト用の差し替え
TOML = ('[package]\nname = "bench"\nversion = "0.1.0"\nedition = 1\n\n[dependencies]\n')

INTRO = """あなたは Kasane というプログラミング言語のコードを書く。Kasane は Rust に似ているが別の言語で、規則が異なる。
以下の資料(チートシートと動く例)の書き方に厳密に従うこと。資料にない Rust の機能や標準ライブラリの名前を推測で使わない。
出力は、`src/bench.ksnl` の完全な内容を ```kasane コードブロック1つだけで返す。テストは書かない(評価側が追記する)。
パッケージ名は bench なので、定義はすべて bench. で始まる完全修飾名で書く。"""


INTRO_NONE = """あなたは Kasane というプログラミング言語のコードを書く。Kasane は Rust に似た言語だが、資料は与えない。
出力は、`src/bench.ksnl` の完全な内容を ```kasane コードブロック1つだけで返す。テストは書かない(評価側が追記する)。
パッケージ名は bench なので、定義はすべて bench. で始まる完全修飾名で書く。"""


PROVIDERS = {
    'local': {'base': 'http://127.0.0.1:1234', 'model': 'gemma-4-12b-it', 'key_env': None, 'jobs': 1},
    'nim': {'base': 'https://integrate.api.nvidia.com', 'model': 'nvidia/nemotron-3.5-lightning-30b-a3b',
            'key_env': 'NVIDIA_API_KEY', 'jobs': 2},
}
RETRY_STATUS = {429, 500, 502, 503, 504}
MAX_BACKOFF = 60.0
MAX_RETRY_AFTER = 120.0


class ApiError(Exception):
    """API 呼び出しの失敗 (リトライ後)。メッセージにリクエストヘッダや鍵は含めない。"""

    def __init__(self, msg, retries=0):
        super().__init__(msg)
        self.retries = retries


def parse_retry_after(v):
    if not v:
        return None
    v = v.strip()
    try:
        return max(0.0, float(v))
    except ValueError:
        pass
    try:
        dt = email.utils.parsedate_to_datetime(v)
        return max(0.0, dt.timestamp() - time.time())
    except (TypeError, ValueError):
        return None


def split_reasoning(msg):
    """応答メッセージから (最終回答, 推論テキスト) を取り出す。
    reasoning_content / reasoning フィールドと、content 内の <think>...</think> (閉じ忘れも) を推論として分ける。"""
    content = msg.get('content') or ''
    reasoning = msg.get('reasoning_content') or msg.get('reasoning') or ''
    if not isinstance(reasoning, str):
        reasoning = ''
    parts = []
    # 開始タグ無しで </think> だけが来る形式にも対応する。
    if '</think>' in content and '<think>' not in content:
        head, content = content.split('</think>', 1)
        parts.append(head)
    while '<think>' in content:
        head, rest = content.split('<think>', 1)
        if '</think>' in rest:
            think, tail = rest.split('</think>', 1)
            parts.append(think)
            content = head + tail
        else:  # 閉じる前に打ち切られた: 以降はすべて推論扱い
            parts.append(rest)
            content = head
    if parts:
        reasoning = (reasoning + '\n' if reasoning else '') + '\n'.join(parts)
    return content.strip(), reasoning


def chat(base, model, messages, max_tokens, timeout, api_key=None, max_attempts=6, backoff=2.0, sleep=time.sleep):
    """chat/completions を呼ぶ。429 / 5xx / 接続エラーは指数バックオフ + ジッタ (Retry-After 優先) でリトライする。
    戻り値は dict (content, reasoning, usage, secs, finish, retries)。失敗は ApiError (鍵やヘッダは載せない)。"""
    body = json.dumps({'model': model, 'messages': messages, 'temperature': 0.2, 'max_tokens': max_tokens}).encode()
    headers = {'Content-Type': 'application/json'}
    if api_key:
        headers['Authorization'] = 'Bearer ' + api_key
    last = ''
    for n in range(max_attempts):
        req = urllib.request.Request(base + '/v1/chat/completions', data=body, headers=headers)
        t = time.time()
        wait = None
        try:
            with urllib.request.urlopen(req, timeout=timeout) as resp:
                r = json.load(resp)
            choice = r['choices'][0]
            content, reasoning = split_reasoning(choice['message'])
            usage = r.get('usage') or {}
            details = usage.get('completion_tokens_details') or {}
            return {'content': content, 'reasoning': reasoning, 'usage': usage, 'secs': time.time() - t,
                    'finish': choice.get('finish_reason'), 'retries': n,
                    'reasoning_tokens': details.get('reasoning_tokens') or usage.get('reasoning_tokens')}
        except urllib.error.HTTPError as e:
            last = f'HTTP {e.code}'
            if e.code not in RETRY_STATUS:
                raise ApiError(last, n) from None
            wait = parse_retry_after(e.headers.get('Retry-After') if e.headers else None)
        except (urllib.error.URLError, OSError, TimeoutError) as e:
            last = 'connection error: ' + type(e).__name__
        except (ValueError, KeyError, IndexError, TypeError):
            # 本文が壊れている / choices が無い。一過性の可能性があるのでリトライ対象にする。
            last = 'malformed response'
        if n + 1 >= max_attempts:
            break
        delay = min(MAX_BACKOFF, backoff * 2 ** n) * random.uniform(0.5, 1.0)
        if wait is not None:
            delay = min(MAX_RETRY_AFTER, wait)
        sleep(delay)
    raise ApiError(f'{last} (リトライ {max_attempts - 1} 回後も失敗)', max_attempts - 1)


def extract_code(text):
    m = re.findall(r'```(?:kasane|ksnl|rust|text)?\s*\n(.*?)```', text, re.S)
    if m:
        return max(m, key=len).rstrip() + '\n'
    return None


def run_ksn(args, cwd, timeout=120):
    try:
        r = subprocess.run([EXE] + args, cwd=cwd, capture_output=True, text=True, encoding='utf-8', errors='replace',
                           timeout=timeout)
    except subprocess.TimeoutExpired:
        return 124, f'ksn {args[0]} が {timeout} 秒でタイムアウトした'
    return r.returncode, (r.stdout + r.stderr)


def run_ksn_retry(args, cwd, tries=3):
    """シグナルで落ちた (returncode < 0) ときは同じ呼び出しを数回やり直す。不安定な CPU コアの対策。"""
    rc, out = run_ksn(args, cwd)
    for _ in range(tries - 1):
        if rc >= 0:
            break
        rc, out = run_ksn(args, cwd)
    return rc, out


def evaluate_full(code, tests):
    """コード + テストをパッケージにして採点する。(段階, 出力, コード一覧, 正規形か) を返す。
    段階: pass / check / test / infra (ksn がシグナルで落ち続けた: モデルの失敗ではない)。
    正規形か: モデルのコードを `ksn fmt` に通す前後でファイルが変わらなければ True。fmt が構文の誤りで
    失敗したときは False、infra のときは None。
    書き方は 1 通り (E0020) だが、LLM の作業の流れでは道具が先に `ksn fmt` を通す (人がエディタで保存するときと同じ)。
    だから採点は整形後の形で行い、整形の前後の差は記録だけする。"""
    with tempfile.TemporaryDirectory() as td:
        os.makedirs(os.path.join(td, 'src'))
        open(os.path.join(td, 'kasane.toml'), 'w', newline='\n').write(TOML)
        open(os.path.join(td, 'kasane.lock'), 'w', newline='\n').write('version = 1\n')
        nl = chr(10)
        src = code.rstrip(nl) + nl
        path = os.path.join(td, 'src', 'bench.ksnl')
        open(path, 'w', newline='\n', encoding='utf-8').write(src)
        # 1. モデルのコードを先に `ksn fmt` に通す。前後で変わったか = 最初から正規形だったか。
        rc, out = run_ksn_retry(['fmt'], td)
        if rc < 0:
            return 'infra', f'ksn fmt がシグナル {-rc} で落ちた', [], None
        if rc != 0:  # 構文の誤り。今と同じ形で check の段の失敗として返す。
            return 'check', out.strip(), re.findall(r'\b(E\d{4})\b', out), False
        after = open(path, encoding='utf-8').read()
        canonical = after == src
        src = after
        # 2. 整形後のコードを検査する。
        rc, out = run_ksn_retry(['check'], td)
        if rc < 0:
            return 'infra', f'ksn check がシグナル {-rc} で落ちた', [], None
        if rc != 0:
            return 'check', out.strip(), re.findall(r'\b(E\d{4})\b', out), canonical
        if tests:
            # 3. テストを追記し、並びは `ksn fmt` に任せてから検査する (テストは採点側の組み立て)。
            src += nl + tests.rstrip(nl) + nl
            open(path, 'w', newline='\n', encoding='utf-8').write(src)
            rc, out = run_ksn_retry(['fmt'], td)
            if rc < 0:
                return 'infra', f'ksn fmt がシグナル {-rc} で落ちた', [], None
            if rc != 0:
                return 'check', out.strip(), re.findall(r'\b(E\d{4})\b', out), canonical
            rc, out = run_ksn_retry(['check'], td)
            if rc < 0:
                return 'infra', f'ksn check がシグナル {-rc} で落ちた', [], None
            if rc != 0:
                return 'check', out.strip(), re.findall(r'\b(E\d{4})\b', out), canonical
        # 4. テストを走らせる。
        rc, out = run_ksn_retry(['test'], td)
        if rc < 0:
            return 'infra', f'ksn test がシグナル {-rc} で落ちた', [], None
        if rc != 0:
            return 'test', out.strip(), [], canonical
        return 'pass', '', [], canonical


def evaluate(code, tests):
    """evaluate_full の段階・出力・コード一覧だけを返す (正規形の記録は要らない呼び出し側向け)。"""
    stage, out, codes, _ = evaluate_full(code, tests)
    return stage, out, codes


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
        run_dir.path = os.path.join(a.out_dir, name)
        os.makedirs(run_dir.path, exist_ok=True)
    return run_dir.path


def dump_attempt(a, task_id, n, code, stage, out, canonical=None):
    nl = chr(10)
    d = run_dir(a)
    open(os.path.join(d, f'{task_id}-{n}.ksnl'), 'w', encoding='utf-8', newline=nl).write(code)
    note = f'// stage: {stage}{nl}// canonical: {canonical}{nl}' + ''.join(f'// {l}{nl}' for l in out.splitlines()[:20])
    open(os.path.join(d, f'{task_id}-{n}.log'), 'w', encoding='utf-8', newline=nl).write(note)


def run_task(a, task, system, api_key, lock):
    msgs = [{'role': 'system', 'content': system}, {'role': 'user', 'content': task_prompt(task)}]
    attempts = []
    final = 'fail'

    def say(text):
        with lock:
            print(text, flush=True)

    for k in range(a.repairs + 1):
        try:
            r = chat(a.base, a.model, msgs, a.max_tokens, a.timeout, api_key, a.max_attempts, a.retry_base)
        except ApiError as e:
            attempts.append({'stage': 'api-error', 'detail': str(e)[:200], 'retries': e.retries})
            say(f"[{task['id']}] attempt {k + 1}: api-error ({e}) -> invalid")
            break
        content, usage = r['content'], r['usage']
        rec = {'secs': round(r['secs'], 1), 'completion_tokens': usage.get('completion_tokens'),
               'prompt_tokens': usage.get('prompt_tokens'), 'reasoning_tokens': r['reasoning_tokens'],
               'finish': r['finish'], 'retries': r['retries']}
        code = extract_code(content)
        if code is None:
            rec.update(stage='no-code', detail=content[:300])
            attempts.append(rec)
            msgs.append({'role': 'assistant', 'content': content or '(empty)'})
            msgs.append({'role': 'user', 'content': '```kasane コードブロックで src/bench.ksnl の完全な内容を返してください。'})
            continue
        stage, out, codes, canonical = evaluate_full(code, task['tests'])
        rec.update(stage=stage, codes=codes, canonical=canonical, detail=out[:600], code=code)
        attempts.append(rec)
        dump_attempt(a, task['id'], k + 1, code, stage, out, canonical)
        form = {True: '正規形', False: '非正規形', None: '整形なし'}[canonical]
        say(f"[{task['id']}] attempt {k + 1}: {stage} {sorted(set(codes))} ({r['secs']:.0f}s, "
            f"{usage.get('completion_tokens')} tok, retries {r['retries']}, {form})")
        if stage == 'pass':
            final = 'pass'
            break
        if stage == 'infra':
            break
        msgs.append({'role': 'assistant', 'content': '```kasane\n' + code + '```'})
        what = 'ksn check の診断' if stage == 'check' else 'ksn test の結果(テストは評価側が追記したもの)'
        msgs.append({'role': 'user', 'content': f'{what}:\n```\n{out[:1800]}\n```\n修正した src/bench.ksnl の完全な内容を返してください。'})
    # インフラの失敗 (API エラー / ksn の異常終了) で結論が出なかった課題は、モデルの失敗に数えず invalid とする。
    if final != 'pass' and any(x.get('stage') in ('api-error', 'infra') for x in attempts):
        final = 'invalid'
    return {'id': task['id'], 'final': final, 'attempts': attempts}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--provider', default='local', choices=sorted(PROVIDERS))
    ap.add_argument('--base', default=None, help='API のベース URL (既定は --provider による)')
    ap.add_argument('--model', default=None, help='モデル名 (既定は --provider による)')
    ap.add_argument('--api-key-env', default=None, metavar='NAME',
                    help='API キーを読む環境変数の名前 (キー自体は引数にしない。nim の既定は NVIDIA_API_KEY)')
    ap.add_argument('--jobs', type=int, default=None, help='課題の並列数 (local の既定 1, nim の既定 2)')
    ap.add_argument('--max-attempts', type=int, default=6, help='1 リクエストあたりの最大試行回数 (リトライ込み)')
    ap.add_argument('--retry-base', type=float, default=2.0, help='指数バックオフの基準秒')
    ap.add_argument('--out-dir', default=os.path.join(ROOT, 'bench', 'results'))
    ap.add_argument('--docs', default='full', choices=['full', 'none'])
    ap.add_argument('--repairs', type=int, default=3)
    ap.add_argument('--tasks', default='')
    ap.add_argument('--max-tokens', type=int, default=12000)
    ap.add_argument('--timeout', type=int, default=900)
    ap.add_argument('--tag', default='')
    ap.add_argument('--large', action='store_true', help='bench/tasks_large の大規模課題を対象にする')
    a = ap.parse_args()
    sys.stdout.reconfigure(encoding='utf-8')

    # 大規模課題か通常課題かで TASKS を切り替える
    if a.large:
        from tasks_large import TASKS_LARGE as TASKS  # noqa: E402
    else:
        from tasks import TASKS  # noqa: E402

    p = PROVIDERS[a.provider]
    a.base = (a.base or p['base']).rstrip('/')
    a.model = a.model or p['model']
    key_env = a.api_key_env or p['key_env']
    a.jobs = max(1, a.jobs if a.jobs is not None else p['jobs'])
    api_key = None
    if key_env:
        api_key = os.environ.get(key_env)
        if not api_key:
            sys.exit(f'環境変数 {key_env} が設定されていない (API キーはシークレットの環境変数で渡す)')
    a.api_key_env = key_env  # 結果に残すのは変数名だけ

    # 資料の例が実際に通ることを確認する。
    if a.docs == 'full':
        ex = open(os.path.join(ROOT, 'bench', 'example.ksnl'), encoding='utf-8').read()
        st, out, _ = evaluate(ex, '')
        assert st == 'pass', f'example.ksnl does not pass: {st}\n{out}'

    want = set(a.tasks.split(',')) if a.tasks else None
    system = build_docs(a.docs)
    tasks = [t for t in TASKS if not want or t['id'] in want]
    run_dir(a)  # 並列実行の前に作っておく
    t0 = time.time()
    lock = threading.Lock()
    with ThreadPoolExecutor(max_workers=a.jobs) as ex:
        results = list(ex.map(lambda t: run_task(a, t, system, api_key, lock), tasks))
    valid = [r for r in results if r['final'] != 'invalid']
    atts = [x for r in results for x in r['attempts']]
    # 1 回目の答案 (コードブロックのあるもの) のうち、`ksn fmt` を通す前から正規形だった数。
    first = [r['attempts'][0] for r in results if r['attempts'] and isinstance(r['attempts'][0].get('canonical'), bool)]
    summary = {
        'model': a.model, 'provider': a.provider, 'base': a.base, 'api_key_env': a.api_key_env,
        'docs': a.docs, 'repairs': a.repairs, 'jobs': a.jobs, 'elapsed_s': round(time.time() - t0),
        'tasks': len(results),
        'valid_tasks': len(valid),
        'invalid_tasks': len(results) - len(valid),
        'invalid_ids': [r['id'] for r in results if r['final'] == 'invalid'],
        'pass_at_1': sum(1 for r in valid if r['attempts'] and r['attempts'][0].get('stage') == 'pass'),
        'pass_at_n': sum(1 for r in valid if r['final'] == 'pass'),
        'first_answers': len(first),
        'first_canonical': sum(1 for x in first if x['canonical']),
        'requests': sum(1 for x in atts if x.get('stage') != 'api-error'),
        'retries': sum(x.get('retries') or 0 for x in atts),
        'prompt_tokens': sum(x.get('prompt_tokens') or 0 for x in atts),
        'completion_tokens': sum(x.get('completion_tokens') or 0 for x in atts),
        'reasoning_tokens': sum(x.get('reasoning_tokens') or 0 for x in atts),
        'results': results,
    }
    os.makedirs(a.out_dir, exist_ok=True)
    name = time.strftime('%Y%m%d-%H%M%S') + f"-{a.docs}{('-' + a.tag) if a.tag else ''}.json"
    path = os.path.join(a.out_dir, name)
    json.dump(summary, open(path, 'w', encoding='utf-8'), ensure_ascii=False, indent=1)
    print(f"\npass@1 {summary['pass_at_1']}/{summary['valid_tasks']}  pass@{a.repairs + 1} {summary['pass_at_n']}/{summary['valid_tasks']}"
          f"  1回目の答案が正規形 {summary['first_canonical']}/{summary['first_answers']}"
          f"  invalid {summary['invalid_tasks']}  retries {summary['retries']}  ({summary['elapsed_s']}s)  -> {path}")
    if summary['invalid_tasks']:
        print('INVALID RUN: インフラの失敗で結論が出なかった課題がある (モデルの失敗ではない): '
              + ','.join(summary['invalid_ids']))
        sys.exit(3)


if __name__ == '__main__':
    main()
