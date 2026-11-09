#!/usr/bin/env python3
"""自走型の計測: 大学アカウントの claude (Haiku, effort low) に、ksn ツールを自分で回させて直させる。

課題ごとに作業用パッケージ bench/work/<実行名>/<課題>/ を作り、claude -p をそこで走らせる。
モデルには資料(チートシート)は与えないが、ksn の最低限の使い方 (check / explain) は教える。
claude-uni は管理設定で Bash を事前許可できないので、ドライバが ksn check を実行して --resume で結果を返す。
隠しテストは見せない。終わったら src/bench.ksnl を run_bench.evaluate で採点する。

使い方: python bench/run_selfdriven.py <実行名> [課題,...] [--jobs N] [--turns N]
"""
import argparse
import concurrent.futures as cf
import os
import re
import shutil
import subprocess
import sys
import uuid

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(ROOT, 'bench'))
import run_bench as rb  # noqa: E402
from tasks import TASKS  # noqa: E402

NL = chr(10)
KSN_DIR = os.path.join(ROOT, 'stage0', 'target', 'release')

GUIDE = """あなたは Kasane というプログラミング言語のコードを書く。Kasane は Rust に似た言語だが、規則が違う。資料は与えない。
作業ディレクトリは Kasane のパッケージ (パッケージ名 bench)。`src/bench.ksnl` を Write / Edit で書く。
定義はすべて bench. で始まる完全修飾名で書く。Bash は使えない。

# 検査の仕組み (ksn コマンド)

書き終えて返信すると、こちらが `ksn check` を実行して結果を次のメッセージで返す。
- `ksn check` : 検査。エラーは `ファイル:行:列: コード: 説明` で出る。説明は具体的な直し方を言っているので、そのとおりに直す。
- エラーコードごとの詳しい説明 (`ksn explain <コード>` の出力) も一緒に付ける。
- エラーがなければ `OK` と返す。そうなったら `done` とだけ返信して終わる。
Rust の癖を持ち込まず、診断が言うとおりの書き方にする。分からない書き方は推測せず、診断に従う。

最初の返信では、`src/bench.ksnl` を書いたら `written` とだけ返信する。テストは書かない (評価側が追記する)。
ほかのファイル、リポジトリの他の場所、ネットは見ない。"""


def task_text(t):
    return (f"# 課題\n\n課題: {t['title']}\n\n{t['spec']}\n\n"
            f"次の宣言(名前と型)で実装すること。ヘッダの uses / panics は自分で正しく決める:\n```\n{t['decl']}\n```\n"
            "関数は pub にしなくてよい(テストは同じファイルに追記される)。")


def prepare(run, t):
    d = os.path.join(ROOT, 'bench', 'work', run, t['id'])
    os.makedirs(os.path.join(d, 'src'), exist_ok=True)
    open(os.path.join(d, 'kasane.toml'), 'w', newline='\n').write(rb.TOML)
    open(os.path.join(d, 'kasane.lock'), 'w', newline='\n').write('version = 1\n')
    p = os.path.join(d, 'src', 'bench.ksnl')
    if os.path.exists(p):
        os.remove(p)
    return d


def run_claude(d, env, sid, first, prompt, turns):
    cmd = [shutil.which('claude') or 'claude', '-p', '--model', 'haiku', '--effort', 'low',
           '--permission-mode', 'acceptEdits', '--max-turns', str(turns), '--output-format', 'text']
    cmd += ['--session-id', sid] if first else ['--resume', sid]
    try:
        r = subprocess.run(cmd, cwd=d, env=env, input=prompt, capture_output=True, text=True,
                           encoding='utf-8', errors='replace', timeout=600)
        return (r.stdout or '') + (r.stderr or '')
    except subprocess.TimeoutExpired:
        return 'timeout'


def ksn(args, d):
    env = dict(os.environ)
    env['PATH'] = KSN_DIR + os.pathsep + env.get('PATH', '')
    r = subprocess.run([os.path.join(KSN_DIR, 'ksn.exe')] + args, cwd=d, env=env, capture_output=True, text=True, encoding='utf-8', errors='replace')
    return r.returncode, ((r.stdout or '') + (r.stderr or '')).strip()


def feedback(d, tests):
    rc, out = ksn(['check'], d)
    if rc == 0:
        # check は通った。評価側のテストを追記して実行し、失敗したらその文面を返す。
        p = os.path.join(d, 'src', 'bench.ksnl')
        stage, tout, _ = rb.evaluate(open(p, encoding='utf-8').read(), tests)
        if stage == 'pass':
            return None
        if stage == 'test':
            return ('`ksn check` は通りましたが、評価側のテスト (`ksn test`) が失敗しました:' + NL + '```' + NL + tout + NL + '```' + NL
                    + NL + '課題の仕様と照らして直し、`src/bench.ksnl` を書き直し、`written` とだけ返信してください。')
        out = tout
    codes = sorted(set(re.findall(r'(E[0-9]{4})', out)))
    text = '`ksn check` の結果:' + NL + '```' + NL + out + NL + '```' + NL
    for c in codes:
        rc2, ex = ksn(['explain', c], d)
        if ex:
            text += NL + f'`ksn explain {c}`:' + NL + ex + NL
    return text + NL + '直して、`src/bench.ksnl` を書き直し、`written` とだけ返信してください。'


def drive(run, t, turns, rounds):
    d = prepare(run, t)
    env = dict(os.environ)
    env['CLAUDE_CONFIG_DIR'] = os.path.join(os.path.expanduser('~'), '.claude-uni')
    sid = str(uuid.uuid4())
    p = os.path.join(d, 'src', 'bench.ksnl')
    log = ''
    used = 0
    prompt = GUIDE + NL + NL + '---' + NL + NL + task_text(t)
    for i in range(rounds + 1):
        log += NL + f'=== round {i} ===' + NL + run_claude(d, env, sid, i == 0, prompt, turns)
        if not os.path.exists(p):
            break
        fb = feedback(d, t['tests'])
        if fb is None:
            break
        used = i + 1
        prompt = fb
        log += NL + '--- feedback ---' + NL + fb
    open(os.path.join(d, 'agent.log'), 'w', encoding='utf-8').write(log)
    if not os.path.exists(p):
        return t['id'], 'nofile', '', log, used
    stage, out, _ = rb.evaluate(open(p, encoding='utf-8').read(), t['tests'])
    return t['id'], stage, out, log, used


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('run')
    ap.add_argument('tasks', nargs='?')
    ap.add_argument('--jobs', type=int, default=5)
    ap.add_argument('--turns', type=int, default=10)
    ap.add_argument('--rounds', type=int, default=5)
    a = ap.parse_args()
    want = set(a.tasks.split(',')) if a.tasks else None
    ts = [t for t in TASKS if not want or t['id'] in want]
    ok = 0
    with cf.ThreadPoolExecutor(a.jobs) as ex:
        for tid, stage, out, log, used in ex.map(lambda t: drive(a.run, t, a.turns, a.rounds), ts):
            ok += stage == 'pass'
            print(f'{tid}: {stage} (書き直し {used} 回)', flush=True)
            if stage not in ('pass', 'nofile'):
                print('   ' + out[:500].replace('\n', '\n   '), flush=True)
            if stage == 'nofile':
                print('   ' + log[:300].replace('\n', '\n   '), flush=True)
    print(f'pass {ok}/{len(ts)}')


if __name__ == '__main__':
    main()
