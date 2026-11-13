#!/usr/bin/env python3
"""spec/ の全検査を一括実行する。使い方: python -I spec/tools/check_all.py (どのディレクトリからでも可)。

1. 文法の規則参照 (未定義・未使用ルール)
2. 文法の実パース (examples/ok と stdlib の .ksnl/.sig は導出できる / examples/fail は拒否される)
3. lint (ok は違反なし / examples/lint_fail は期待コードの違反を報告)
4. 相互参照 (stdlib は整合 / examples/xref_fail は期待コードを報告)
"""
import glob
import os
import re
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
SPEC = os.path.dirname(HERE)
os.chdir(SPEC)
sys.path.insert(0, HERE)
PY = [sys.executable, '-I']
sys.stdout.reconfigure(encoding='utf-8')


def run(args):
    r = subprocess.run(PY + args, capture_output=True, text=True, encoding='utf-8')
    return r.returncode, r.stdout + r.stderr


def files(*pats):
    out = []
    for p in pats:
        out += sorted(glob.glob(p, recursive=True))
    return out


failures = []


def step(name, ok, detail=''):
    print(f'{"ok  " if ok else "FAIL"} {name}')
    if not ok:
        failures.append(name)
        if detail:
            print('     ' + detail.strip().replace('\n', '\n     '))


def only(prefix, out, rc=None):
    hit = '\n'.join(l for l in out.splitlines() if l.startswith(prefix))
    # 失敗したのに FAIL 行がない (下位のツールが異常終了した) ときは、終了コードと出力の末尾を見せる。
    if not hit and rc not in (None, 0):
        tail = '\n'.join(out.splitlines()[-15:])
        return f'exit code {rc}, no {prefix} line; last output:\n{tail}'
    return hit


stdlib_src = files('stdlib/**/*.ksnl', 'stdlib/**/*.sig')
ok_src = files('examples/ok/**/*.ksnl')
fail_src = files('examples/fail/*.ksnl')
lint_fail = files('examples/lint_fail/**/*.ksnl')
xref_fail = files('examples/xref_fail/*.ksnl')

# 1. 文法の規則参照
import ebnfcheck  # noqa: E402

rules = ebnfcheck.load_grammar('grammar.ebnf')
used = set()
for r in rules.values():
    stack = [r]
    while stack:
        x = stack.pop()
        if x[0] == 'ref':
            used.add(x[1])
        elif x[0] in ('seq', 'alt'):
            stack += x[1]
        elif x[0] in ('opt', 'rep'):
            stack.append(x[1])
lex = {'NL', 'LF', 'BLANK', 'INDENT', 'INDENT_BACK', 'CONT_INDENT', 'CLOSE_INDENT', 'text', 'char_any', 'ascii',
       'lower', 'upper', 'letter', 'digit', 'hex', 'bin', 'oct'}
undefined = sorted(w for w in used if w not in rules and w not in lex)
unused = sorted(n for n in rules if n not in used and n not in ('file', 'sig_file'))
step(f'grammar references ({len(rules)} rules)', not undefined and not unused, f'undefined={undefined} unused={unused}')

# 2. 実パース
srcs = stdlib_src + ok_src
rc, out = run(['tools/ebnfcheck.py', 'grammar.ebnf', 'ok'] + srcs)
step(f'parse: {len(srcs)} valid sources derive', rc == 0, only('FAIL', out, rc))
rc, out = run(['tools/ebnfcheck.py', 'grammar.ebnf', 'fail'] + fail_src)
step(f'parse: {len(fail_src)} invalid sources rejected for the intended reason', rc == 0, only('FAIL', out, rc))
neg = lint_fail + xref_fail
rc, out = run(['tools/ebnfcheck.py', 'grammar.ebnf', 'ok'] + neg)
step(f'parse: {len(neg)} lint/xref negative examples are grammatical', rc == 0, only('FAIL', out, rc))

# 3. lint
for root, pats in (('stdlib', stdlib_src), ('examples/ok', ok_src)):
    rc, out = run(['tools/ksnlint.py', root, 'ok'] + pats)
    step(f'lint: {len(pats)} sources under {root} are clean', rc == 0, '\n'.join(l for l in out.splitlines() if not l.startswith('PASS')))
rc, out = run(['tools/ksnlint.py', 'examples/lint_fail', 'fail'] + lint_fail)
step(f'lint: {len(lint_fail)} violations each reported with the expected code', rc == 0, only('FAIL', out, rc))

# 4. xref
rc, out = run(['tools/xref.py', '.'] + stdlib_src)
step('xref: standard library is consistent', rc == 0, out)
bad = []
for f in xref_fail:
    key = os.path.basename(f).split('_')[0]
    rc, out = run(['tools/xref.py', '.'] + stdlib_src + [f])
    if not re.search(r'\[' + key + r'\] ' + re.escape(f.replace(os.sep, '/')), out.replace(chr(92), '/')):
        bad.append(f'{f}: expected [{key}] not reported:\n{out}')
step(f'xref: {len(xref_fail)} violations each reported with the expected code', not bad, '\n'.join(bad))

# 4b. .sig の整形が冪等 (fmtsig を一時コピーに適用して差分が無い)
import shutil
import tempfile
sigs = files('stdlib/**/*.sig')
drift = []
with tempfile.TemporaryDirectory() as tmp:
    for sg in sigs:
        dst = os.path.join(tmp, sg.replace('/', '__').replace(os.sep, '__'))
        shutil.copyfile(sg, dst)
        rc, out = run(['tools/fmtsig.py', dst])
        if open(dst, encoding='utf8', newline='').read() != open(sg, encoding='utf8', newline='').read():
            drift.append(sg)
step(f'fmtsig: {len(sigs)} .sig files are already formatted', not drift, ', '.join(drift))

# 5. 生成物のドリフト (core/num.sig は gen_num_sig.py の出力と一致する)
rc, out = run(['tools/gen_num_sig.py'])
cur = open('stdlib/core/num.sig', encoding='utf8', newline='').read()
step('generated: stdlib/core/num.sig matches tools/gen_num_sig.py output', rc == 0 and out.replace('\r\n', '\n') == cur)

print()
print('ALL CHECKS PASSED' if not failures else f'{len(failures)} CHECK(S) FAILED')
sys.exit(1 if failures else 0)
