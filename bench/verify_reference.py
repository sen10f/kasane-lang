#!/usr/bin/env python3
"""bench/tasks/*/reference.ksnl の参照解を検査する。

課題がある参照解は run_bench.evaluate と同じ形の一時パッケージで ksn check +
ksn test を行い pass を期待する。回帰用だけの参照解は、run_bench.py の evaluate と
同じ形の一時パッケージで ksn check が診断 0 件 (と ksn fmt --check が通る) を確かめる。
どれか 1 本でも落ちたら終了コード 1。

--large オプションで bench/tasks_large/* を対象にする。
"""
import os
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(ROOT, 'bench'))
import run_bench as rb  # noqa: E402


def check_reference_only(code):
    """一時パッケージで ksn check (診断0件) と ksn fmt --check を確かめる。(ok, msg)"""
    with tempfile.TemporaryDirectory() as td:
        os.makedirs(os.path.join(td, 'src'))
        open(os.path.join(td, 'kasane.toml'), 'w', newline='\n').write(rb.TOML)
        open(os.path.join(td, 'kasane.lock'), 'w', newline='\n').write('version = 1\n')
        nl = chr(10)
        src = code.rstrip(nl) + nl
        open(os.path.join(td, 'src', 'bench.ksnl'), 'w', newline='\n', encoding='utf-8').write(src)
        rc, out = rb.run_ksn_retry(['check'], td)
        if rc < 0:
            return False, f'ksn check がシグナル {-rc} で落ちた: {out.strip()}'
        if rc != 0:
            return False, f'ksn check 失敗 (rc={rc}):\n{out.strip()}'
        rc, out = rb.run_ksn_retry(['fmt', '--check'], td)
        if rc < 0:
            return False, f'ksn fmt --check がシグナル {-rc} で落ちた: {out.strip()}'
        if rc != 0:
            return False, f'ksn fmt --check 失敗 (rc={rc}):\n{out.strip()}'
        return True, 'ok'


def run_verification(use_large, want):
    if use_large:
        from tasks_large import TASKS_LARGE as TASKS, REFERENCE_ONLY_LARGE as REFERENCE_ONLY  # noqa: E402
        base_dir = os.path.join(ROOT, 'bench', 'tasks_large')
    else:
        from tasks import TASKS, REFERENCE_ONLY  # noqa: E402
        base_dir = os.path.join(ROOT, 'bench', 'tasks')

    bad = 0
    total = 0
    # 課題がある参照解: テスト採点
    for t in TASKS:
        if want and t['id'] not in want:
            continue
        ref_path = os.path.join(base_dir, t['id'], 'reference.ksnl')
        if not os.path.exists(ref_path):
            continue
        total += 1
        code = open(ref_path, encoding='utf-8').read()
        stage, out, _ = rb.evaluate(code, t['tests'])
        print(f"{t['id']}: {stage}")
        if stage != 'pass':
            bad += 1
            print('   ' + out[:900].replace('\n', '\n   '))
    # 回帰用だけの参照解: ksn check (診断0件) + ksn fmt --check
    for tid in REFERENCE_ONLY:
        if want and tid not in want:
            continue
        ref_path = os.path.join(base_dir, tid, 'reference.ksnl')
        if not os.path.exists(ref_path):
            continue
        total += 1
        code = open(ref_path, encoding='utf-8').read()
        ok, msg = check_reference_only(code)
        print(f"{tid}: {'ok' if ok else 'FAIL'} {'' if ok else msg}")
        if not ok:
            bad += 1
            print('   ' + msg.replace('\n', '\n   '))
    print(f'total {total}, bad {bad}')
    sys.exit(1 if bad else 0)


def main():
    args = sys.argv[1:]
    use_large = False
    if '--large' in args:
        use_large = True
        args.remove('--large')
    want = set(args[0].split(',')) if args else None
    run_verification(use_large, want)


if __name__ == '__main__':
    main()