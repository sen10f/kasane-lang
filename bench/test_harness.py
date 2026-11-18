#!/usr/bin/env python3
"""run_bench.py のオフラインテスト。ローカルのモック OpenAI 互換サーバに対してハーネスを動かし、
採点・リトライ・invalid 判定・推論モデルの応答処理・API キーが出力に残らないことを確かめる。
NVIDIA には接続しない。使い方: python -I bench/test_harness.py
"""
import glob
import json
import os
import re
import stat
import subprocess
import sys
import tempfile
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(ROOT, 'bench'))
from tasks import TASKS  # noqa: E402

KEY = 'nvapi-TESTKEY-0123456789abcdef'
failures = []


def check(name, ok, detail=''):
    print(f'{"ok  " if ok else "FAIL"} {name}')
    if not ok:
        failures.append(name)
        if detail:
            print('     ' + str(detail)[-600:].replace('\n', '\n     '))


def ref(task_id):
    return open(os.path.join(ROOT, 'bench', 'tasks', task_id, 'reference.ksnl'), encoding='utf-8').read()


def fence(code):
    return '```kasane\n' + code.rstrip('\n') + '\n```'


def ok(content, **msg):
    return ('ok', dict(content=content, **msg), {'prompt_tokens': 100, 'completion_tokens': 50})


# 課題ごとの応答の列 (使い切ったら最後を繰り返す)。
SCENARIOS = {
    # 推論モデル形式: 別フィールドの推論 + 最終回答。推論トークン数も返す。
    'palindrome': [('ok', {'content': fence(ref('palindrome')), 'reasoning_content': fence('fn bogus ' + 'y' * 500)},
                    {'prompt_tokens': 100, 'completion_tokens': 80,
                     'completion_tokens_details': {'reasoning_tokens': 30}})],
    # 1 回目は誤答、2 回目は <think> 内に長い偽コードブロックを含む正解 (think は無視されるはず)。
    'distinct': [ok(fence('fn broken')),
                 ok('<think>\n' + fence('fn decoy ' + 'x' * 500) + '\n</think>\n' + fence(ref('distinct')))],
    # 429 (Retry-After 付き) のあと 500 のあと正解。
    'digit_sum': [('http', 429, {'Retry-After': '1'}), ('http', 500, {}), ok(fence(ref('digit_sum')))],
    # 常に 500 (リトライ上限まで)。
    'fact': [('http', 500, {})],
}


class Mock(BaseHTTPRequestHandler):
    counts = {}
    auth = []
    lock = threading.Lock()

    def log_message(self, *a):
        pass

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        prompt = body['messages'][1]['content']
        tid = next(t['id'] for t in TASKS if t['decl'] in prompt)
        with Mock.lock:
            n = Mock.counts.get(tid, 0)
            Mock.counts[tid] = n + 1
            Mock.auth.append(self.headers.get('Authorization'))
        seq = SCENARIOS[tid]
        kind, a, b = seq[min(n, len(seq) - 1)]
        if kind == 'http':
            self.send_response(a)
            for k, v in b.items():
                self.send_header(k, v)
            self.send_header('Content-Length', '0')
            self.end_headers()
            return
        msg = {'role': 'assistant', **a}
        out = json.dumps({'choices': [{'message': msg, 'finish_reason': 'stop'}], 'usage': b}).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(out)))
        self.end_headers()
        self.wfile.write(out)


def run(args, out_dir, env_extra=None):
    env = dict(os.environ, TEST_NIM_KEY=KEY)
    env.update(env_extra or {})
    r = subprocess.run([sys.executable, '-I', os.path.join(ROOT, 'bench', 'run_bench.py'), '--out-dir', out_dir,
                        '--retry-base', '0.05'] + args, capture_output=True, text=True, encoding='utf-8',
                       errors='replace', env=env)
    files = sorted(glob.glob(os.path.join(out_dir, '*.json')))
    summary = json.load(open(files[-1], encoding='utf-8')) if files else None
    return r, summary


def leaked(out_dir, *texts):
    blob = ''.join(texts)
    for dp, _, fs in os.walk(out_dir):
        for f in fs:
            blob += open(os.path.join(dp, f), encoding='utf-8', errors='replace').read()
    return KEY in blob or 'Bearer' in blob


def main():
    srv = ThreadingHTTPServer(('127.0.0.1', 0), Mock)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    base = f'http://127.0.0.1:{srv.server_address[1]}'
    with tempfile.TemporaryDirectory() as td:
        # 1. 正解 / 誤答→修正 / 429+500→正解 / 500 続き (invalid) を並列 2 で。
        out1 = os.path.join(td, 'r1')
        r, s = run(['--provider', 'nim', '--base', base, '--api-key-env', 'TEST_NIM_KEY', '--jobs', '2',
                    '--tasks', 'palindrome,distinct,digit_sum,fact', '--repairs', '2', '--max-attempts', '3'], out1)
        check('mock: run produced a summary', s is not None, r.stdout + r.stderr)
        if s is None:
            sys.exit(1)
        by = {x['id']: x for x in s['results']}
        check('model name defaults to nim preset', s['model'] == 'nvidia/nemotron-3.5-lightning-30b-a3b', s['model'])
        check('reasoning field: pass@1', by['palindrome']['final'] == 'pass' and len(by['palindrome']['attempts']) == 1,
              by['palindrome'])
        check('reasoning tokens recorded',
              by['palindrome']['attempts'][0]['reasoning_tokens'] == 30 and s['reasoning_tokens'] == 30)
        d = by['distinct']
        check('wrong answer then <think>-wrapped fix: pass at attempt 2 (think block ignored)',
              d['final'] == 'pass' and [x['stage'] for x in d['attempts']][1:] == ['pass']
              and d['attempts'][0]['stage'] != 'pass', d)
        g = by['digit_sum']
        check('429/500 retried then pass', g['final'] == 'pass' and g['attempts'][0]['retries'] == 2, g['attempts'])
        check('retries total (digit_sum 2 + fact 2)', s['retries'] == 4, s['retries'])
        check('request count for digit_sum is 3', Mock.counts['digit_sum'] == 3, Mock.counts)
        f = by['fact']
        check('persistent 500: invalid, not fail', f['final'] == 'invalid' and f['attempts'][0]['stage'] == 'api-error', f)
        check('fact requested exactly --max-attempts times', Mock.counts['fact'] == 3, Mock.counts)
        counts = (s['valid_tasks'], s['invalid_tasks'], s['pass_at_1'], s['pass_at_n'])
        check('invalid excluded from pass counts', counts == (3, 1, 2, 3), counts)
        check('exit code 3 on invalid run', r.returncode == 3, r.returncode)
        check('Authorization header sent as Bearer', set(Mock.auth) == {'Bearer ' + KEY}, set(Mock.auth))
        check('usage tokens recorded', s['prompt_tokens'] > 0 and s['completion_tokens'] > 0)
        check('API key not in stdout/stderr/results', not leaked(out1, r.stdout, r.stderr))
        # 1 回目の答案の正規形: 正解 (palindrome・digit_sum) は true、構文の誤り (fn broken) は fmt が失敗して false。
        check('canonical recorded per first answer',
              by['palindrome']['attempts'][0]['canonical'] is True and d['attempts'][0]['canonical'] is False
              and g['attempts'][-1]['canonical'] is True, d['attempts'][0])
        check('summary: first answers 3, canonical 2', (s['first_answers'], s['first_canonical']) == (3, 2),
              (s['first_answers'], s['first_canonical']))

        # 2. ローカル既定: 鍵なしなら Authorization を送らない。誤答だけの課題は fail (invalid ではない)。
        Mock.auth.clear()
        SCENARIOS['palindrome'] = [ok(fence('fn broken'))]
        out2 = os.path.join(td, 'r2')
        r, s = run(['--base', base, '--tasks', 'palindrome', '--repairs', '1'], out2)
        check('local: no Authorization header', set(Mock.auth) == {None}, set(Mock.auth))
        check('model failure is fail (not invalid), exit 0',
              s is not None and s['results'][0]['final'] == 'fail' and r.returncode == 0, r.stdout + r.stderr)

        # 3. ksn がシグナルで落ちる環境: モデルの失敗ではなく invalid。
        if os.name != 'nt':
            SCENARIOS['palindrome'] = [ok(fence(ref('palindrome')))]
            fake = os.path.join(td, 'fake-ksn')
            open(fake, 'w').write('#!/bin/sh\nkill -SEGV $$\n')
            os.chmod(fake, os.stat(fake).st_mode | stat.S_IXUSR)
            out3 = os.path.join(td, 'r3')
            r, s = run(['--base', base, '--docs', 'none', '--tasks', 'palindrome'], out3, {'KSN_BENCH_EXE': fake})
            res = s['results'][0] if s else {}
            check('ksn killed by signal: invalid (stage infra), exit 3',
                  res.get('final') == 'invalid' and res['attempts'][-1]['stage'] == 'infra' and r.returncode == 3,
                  r.stdout + r.stderr)

        # 4. 鍵の環境変数が未設定なら、実行前に分かりやすく失敗する。
        env = {k: v for k, v in os.environ.items() if k != 'NVIDIA_API_KEY'}
        r = subprocess.run([sys.executable, '-I', os.path.join(ROOT, 'bench', 'run_bench.py'), '--provider', 'nim',
                            '--base', base, '--out-dir', os.path.join(td, 'r4')], capture_output=True, text=True,
                           encoding='utf-8', errors='replace', env=env)
        check('missing key env: exits non-zero naming the variable',
              r.returncode != 0 and 'NVIDIA_API_KEY' in r.stderr, r.stderr)

        # 5. 定義の並びが違う正しいコード: `ksn fmt` を先に通すので pass になり、canonical は false と記録される。
        #    参照解を段落 (空行) ごとに逆順にしたもの。並びの正しい参照解は canonical が true。
        blocks = [b for b in re.split(r'\n\n+', ref('digit_sum').strip('\n')) if b.strip()]
        SCENARIOS['digit_sum'] = [ok(fence('\n\n'.join(reversed(blocks))))]
        out5 = os.path.join(td, 'r5')
        r, s = run(['--base', base, '--docs', 'none', '--tasks', 'digit_sum', '--repairs', '0'], out5)
        res = s['results'][0] if s else {}
        check('out-of-order but correct: pass at attempt 1 (fmt first)',
              res.get('final') == 'pass' and len(res.get('attempts', [])) == 1 and r.returncode == 0,
              r.stdout + r.stderr)
        check('out-of-order: canonical false recorded',
              res.get('attempts', [{}])[0].get('canonical') is False, res)
        check('out-of-order: summary counts it as a non-canonical first answer',
              (s['first_answers'], s['first_canonical']) == (1, 0), s and (s['first_answers'], s['first_canonical']))
        SCENARIOS['digit_sum'] = [ok(fence(ref('digit_sum')))]
        out6 = os.path.join(td, 'r6')
        r, s = run(['--base', base, '--docs', 'none', '--tasks', 'digit_sum', '--repairs', '0'], out6)
        res = s['results'][0] if s else {}
        check('in-order reference: pass, canonical true',
              res.get('final') == 'pass' and res['attempts'][0].get('canonical') is True
              and (s['first_answers'], s['first_canonical']) == (1, 1), res)
    srv.shutdown()
    print('ALL HARNESS TESTS PASSED' if not failures else f'FAILED: {failures}')
    sys.exit(1 if failures else 0)


if __name__ == '__main__':
    main()
