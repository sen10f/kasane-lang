#!/usr/bin/env python3
"""ksn (Rust 版) のテスト。使い方: python tests/run_tests.py

Python 参照実装との比較テストは凍結済みで tests/legacy/ にある (拡張子統一前の仕様向け)。
"""
import os
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


# 1. 仕様の機械検証
r = subprocess.run([sys.executable, '-I', os.path.join(ROOT, 'spec', 'tools', 'check_all.py')],
                   capture_output=True, text=True, encoding='utf-8')
step('spec/tools/check_all.py', r.returncode == 0, r.stdout[-800:])

# 3. LLM ベンチの参照解 (bench/reference/*.ksnl) が今の検査器で通ること
r = subprocess.run([sys.executable, '-I', os.path.join(ROOT, 'bench', 'verify_reference.py')],
                   capture_output=True, text=True, encoding='utf-8', errors='replace')
step('bench/verify_reference.py', r.returncode == 0, (r.stdout + r.stderr)[-800:])

# 2. Rust 版のビルドと cargo test
r = subprocess.run(['cargo', 'build', '--release'], cwd=os.path.join(ROOT, 'stage0'), capture_output=True, text=True,
                   encoding='utf-8', errors='replace')
step('stage0: cargo build', r.returncode == 0, r.stderr[-500:])
r = subprocess.run(['cargo', 'test', '--release'], cwd=os.path.join(ROOT, 'stage0'), capture_output=True, text=True,
                   encoding='utf-8', errors='replace')
step('stage0: cargo test', r.returncode == 0, (r.stdout + r.stderr)[-1500:])

HR = """fn hr.main() -> Result<(), std.io.Error> uses [io.console, thread, time] panics may
    let mut i = 0u8
    while i < 25u8 {
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
step('rust ksn: test --both (interpreter and bundle agree)', r.returncode == 0, r.stdout + r.stderr)
shutil.rmtree(os.path.join(d, 'target'), ignore_errors=True)
shutil.rmtree(cache2, ignore_errors=True)

print()
print('ALL TESTS PASSED' if not failures else f'{len(failures)} FAILED: {failures}')
sys.exit(1 if failures else 0)
