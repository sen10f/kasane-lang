#!/usr/bin/env python3
"""ksn (Rust 版) のテスト。使い方: python tests/run_tests.py

Python 参照実装との比較テストは凍結済みで tests/legacy/ にある (拡張子統一前の仕様向け)。
"""
import os
import re
import shutil
import subprocess
import sys
import tempfile
import threading
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.stdout.reconfigure(encoding='utf-8')
failures = []
NLC = chr(10)
TOML = NLC.join(['[package]', 'name = "t"', 'version = "0.1.0"', 'edition = 1', 'entry = "t.main"', '', '[dependencies]', ''])


def step(name, ok, detail=''):
    print(f'{"ok  " if ok else "FAIL"} {name}')
    if not ok:
        failures.append(name)
        if detail:
            print('     ' + detail.strip().replace(NLC, NLC + '     '))


# 0. Rust 版のビルド (後の段の bench/verify_reference.py などが ksn を使うので最初に。まっさらな checkout・CI でも通るように)
r = subprocess.run(['cargo', 'build', '--release'], cwd=os.path.join(ROOT, 'stage0'), capture_output=True, text=True,
                   encoding='utf-8', errors='replace')
step('stage0: cargo build', r.returncode == 0, r.stderr[-500:])

# 1. 仕様の機械検証
r = subprocess.run([sys.executable, '-I', os.path.join(ROOT, 'spec', 'tools', 'check_all.py')],
                   capture_output=True, text=True, encoding='utf-8')
step('spec/tools/check_all.py', r.returncode == 0, r.stdout[-800:])

# 3. LLM ベンチの参照解 (bench/tasks/*/reference.ksnl) が今の検査器で通ること
r = subprocess.run([sys.executable, '-I', os.path.join(ROOT, 'bench', 'verify_reference.py')],
                   capture_output=True, text=True, encoding='utf-8', errors='replace')
step('bench/verify_reference.py', r.returncode == 0, (r.stdout + r.stderr)[-800:])

# 3a. 旧い検査と新しい検査 (tast_check.rs) の差が台帳 (tests/shadow_baseline.json) の範囲内か (全対象で約 15 秒)
r = subprocess.run([sys.executable, '-I', os.path.join(ROOT, 'tests', 'shadow_diff.py'), '--full'],
                   capture_output=True, text=True, encoding='utf-8', errors='replace')
step('tests/shadow_diff.py (KSN_CHECKER=shadow)', r.returncode == 0, (r.stdout + r.stderr)[-800:])

# 3b. LLM ベンチのハーネス (モック API でリトライ・採点・invalid 判定を検査。外部には接続しない)
r = subprocess.run([sys.executable, '-I', os.path.join(ROOT, 'bench', 'test_harness.py')],
                   capture_output=True, text=True, encoding='utf-8', errors='replace')
step('bench/test_harness.py', r.returncode == 0, (r.stdout + r.stderr)[-800:])

# 2. Rust 版の cargo test (試験のバイナリを並列に回す。ビルドは 0 で先に済ませる)
# 段は既定で full。担当の統合の前の確かめ (kasane-verify/tools/fulltest.sh) は KSN_TEST_TIER=fast で速い段だけにし、
# 遅い段のうち統合で落ちやすい言い換えの試験 (rephrase) は別に回す。
_tier = os.environ.get('KSN_TEST_TIER', 'full')
r = subprocess.run([sys.executable, '-I', os.path.join(ROOT, 'tests', 'par_test.py'), '--tier', _tier],
                   capture_output=True, text=True, encoding='utf-8', errors='replace')
# 落ちたときは、失敗したテストの名前と panic の行を先に出す (末尾は警告で埋まることがある)。
_ct = r.stdout + r.stderr
_fails = '\n'.join(l for l in _ct.splitlines() if ' FAILED' in l or 'panicked at' in l or l.startswith('error: test failed'))[:3000]
step('stage0: cargo test', r.returncode == 0, (_fails + '\n---\n' + _ct[-1500:]) if _fails else _ct[-1500:])

HR = """fn hr.main() -> Result<(), std.io.Error> uses [io.console, thread, time] panics never
    let mut i = 0u8
    while i < 80u8 {
        let r = hr.msg()
        core.mem.drop(r)
        std.thread.sleep(100u64)
        i = i.wrapping_add(1u8)
    }
    Ok(())
end hr.main

fn hr.msg() -> Result<(), std.io.Error> uses [io.console] panics never
    std.io.print_line("v1")
end hr.msg
"""
# 9. Rust 版 ksn のホットリロード / ビルド / テスト --both
RSEXE = os.path.join(ROOT, 'stage0', 'target', 'release', 'ksn' + ('.exe' if os.name == 'nt' else ''))
with tempfile.TemporaryDirectory() as td:
    os.makedirs(os.path.join(td, 'src'))
    open(os.path.join(td, 'kasane.toml'), 'w', newline=NLC).write(TOML.replace('"t"', '"hr"').replace('t.main', 'hr.main'))
    open(os.path.join(td, 'kasane.lock'), 'w', newline=NLC).write('version = 1' + NLC)
    src = os.path.join(td, 'src', 'hr.ksnl')
    open(src, 'w', newline=NLC).write(HR)

    def edit_rs():
        time.sleep(1.2)
        open(src, 'w', newline=NLC).write(HR.replace('v1', 'v2'))
    threading.Thread(target=edit_rs).start()
    r = subprocess.run([RSEXE, 'run', '--watch'], cwd=td, capture_output=True, text=True, encoding='utf-8')
    step('rust ksn: hot reload applies a body change while running',
         r.returncode == 0 and 'v1' in r.stdout and 'v2' in r.stdout and 'bodies updated' in r.stderr, r.stdout + r.stderr)
cache2 = tempfile.mkdtemp()
envc = dict(os.environ, KSN_CACHE_DIR=cache2)
d = os.path.join(ROOT, 'examples', 'native')
r = subprocess.run([RSEXE, 'build', '--release'], cwd=d, capture_output=True, text=True, encoding='utf-8', env=envc)
exe2 = os.path.join(d, 'target', 'release', 'native.exe' if os.name == 'nt' else 'native.native')
step('rust ksn: build produces a native executable', r.returncode == 0 and os.path.exists(exe2), r.stdout + r.stderr)
if os.path.exists(exe2):
    r = subprocess.run([exe2], capture_output=True, text=True)
    step('rust ksn: native executable output', r.stdout.strip() == 'native ok', r.stdout + r.stderr)
r = subprocess.run([RSEXE, 'build', '--release'], cwd=d, capture_output=True, text=True, encoding='utf-8', env=envc)
step('rust ksn: second build hits the cache', 'cache hit' in r.stdout, r.stdout + r.stderr)
shutil.rmtree(os.path.join(d, 'target'), ignore_errors=True)
d = os.path.join(ROOT, 'examples', 'demo')
r = subprocess.run([RSEXE, 'test', '--both'], cwd=d, capture_output=True, text=True, encoding='utf-8', env=envc)
# clang が無ければ LLVM の比較は退避して バンドルで比べる (`stage0/tests/common/mod.rs` の `clang_available` と同じ条件)。
want_label = '(both: llvm)' if shutil.which('clang') else '(both: bundle'
step('rust ksn: test --both (interpreter and native agree)', r.returncode == 0 and want_label in r.stdout and 'disagree' not in r.stdout, r.stdout + r.stderr)
shutil.rmtree(os.path.join(d, 'target'), ignore_errors=True)
shutil.rmtree(cache2, ignore_errors=True)

# 10. ネイティブになった割合 (修正プラン 3-5)。下限 (tests/native_ratio_floor.json) を下回ったら落とす。
r = subprocess.run([sys.executable, '-I', os.path.join(ROOT, 'tests', 'native_ratio.py')],
                   capture_output=True, text=True, encoding='utf-8', errors='replace')
summary = next((ln for ln in r.stdout.splitlines() if ln.startswith('native ratio:')), 'native ratio: (集計が出力されなかった)')
step(summary, r.returncode == 0, r.stdout + r.stderr)
if r.returncode == 0:
    for ln in r.stdout.splitlines():
        if ln.startswith('note: '):
            print('     ' + ln)

# 11. selfhost/* の全パッケージ (selfhost/probes/*・selfhost/lower/probes/* を含む) で Rust 版 `ksn check` の診断が
# 期待どおりであること。既定は 0 件。期待で診断を出すパッケージだけを下の表に書く (コード → 件数)。
# target/・broken/・tests/ は対象外 (`selfhost/driver/compare.py` の packages() と同じ)。
# 表にない診断・表の件数の違い・表にあるのに無いパッケージ (書き間違い) は失敗にする。
SELFHOST_EXPECT = {
    # 再帰する型 boxtree.Expr に Drop の明示実装が無い (効果の段 E4013 の確かめ用の probe)。
    'selfhost/probes/boxtree': {'E4013': 1},
}
DIAG_RE = re.compile(r'^\S+?:\d+:\d+: (E\d{4})\b')
t0 = time.monotonic()
sh_pkgs = []
for dp, dns, fns in os.walk(os.path.join(ROOT, 'selfhost')):
    dns[:] = sorted(x for x in dns if x not in ('target', 'broken', 'tests'))
    if 'kasane.toml' in fns:
        sh_pkgs.append(os.path.relpath(dp, ROOT).replace(os.sep, '/'))
sh_pkgs.sort()
for k in SELFHOST_EXPECT:
    if k not in sh_pkgs:
        step(f'selfhost: expected package exists: {k}', False, 'SELFHOST_EXPECT に書いたパッケージが無い')
for rel in sh_pkgs:
    r = subprocess.run([RSEXE, 'check'], cwd=os.path.join(ROOT, rel), capture_output=True, text=True,
                       encoding='utf-8', errors='replace')
    got = {}
    for ln in (r.stdout + r.stderr).splitlines():
        m = DIAG_RE.match(ln)
        if m:
            got[m.group(1)] = got.get(m.group(1), 0) + 1
    want = SELFHOST_EXPECT.get(rel, {})
    step(f'selfhost: ksn check {rel}', got == want and (r.returncode == 0) == (not want),
         f'got={got} want={want} rc={r.returncode}\n' + (r.stdout + r.stderr)[-800:])
print(f'     selfhost の ksn check: {len(sh_pkgs)} パッケージ, {time.monotonic() - t0:.1f} 秒')

# selfhost の本体のパッケージは整形済み (試しの probes/ は除く)。
for rel in sh_pkgs:
    if '/probes' in rel:
        continue
    r = subprocess.run([RSEXE, 'fmt', '--check'], cwd=os.path.join(ROOT, rel), capture_output=True, text=True,
                       encoding='utf-8', errors='replace')
    step(f'selfhost: ksn fmt --check {rel}', r.returncode == 0, (r.stdout + r.stderr)[-800:])

# apps/ の各パッケージは診断 0 件で、整形済み (回帰の基準。AGENTS.md)。
t0 = time.monotonic()
app_pkgs = sorted(d for d in os.listdir(os.path.join(ROOT, 'apps'))
                  if os.path.isfile(os.path.join(ROOT, 'apps', d, 'kasane.toml')))
for name in app_pkgs:
    cwd = os.path.join(ROOT, 'apps', name)
    for args in (['check'], ['fmt', '--check']):
        r = subprocess.run([RSEXE] + args, cwd=cwd, capture_output=True, text=True, encoding='utf-8', errors='replace')
        step(f'apps: ksn {" ".join(args)} apps/{name}', r.returncode == 0, (r.stdout + r.stderr)[-800:])
print(f'     apps の ksn check / fmt --check: {len(app_pkgs)} パッケージ, {time.monotonic() - t0:.1f} 秒')

print()
print('ALL TESTS PASSED' if not failures else f'{len(failures)} FAILED: {failures}')
sys.exit(1 if failures else 0)
