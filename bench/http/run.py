#!/usr/bin/env python3
"""URL 短縮サーバの比較ベンチマーク (Kasane / Rust / Go)。標準ライブラリのみ。

使い方 (リポジトリ直下から):
    python3 bench/http/run.py [--seconds 10] [--conns 64] [--workers 4] [--server-cpus 0-9] [--client-cpus 12-31]

やること:
  1. 3 つのサーバと負荷生成器をリリースビルドして、ビルド時間 (キャッシュなし) と実行ファイルの大きさを測る。
  2. 実装ごとにサーバを起動し、負荷生成器で RPS と p50 / p99 を測る (サーバ・生成器とも taskset で CPU を固定)。
  3. Markdown の表を標準出力に出す。

CPU の固定: この環境は CPU 10 と 11 が不安定 (実装計画.md 7 章) なので、既定ではサーバを 0-9、負荷生成器を 12-31 に固定し、
10 と 11 は使わない (taskset -c 0-9,12-31 の範囲を、サーバ側と生成器側に分けたもの)。
"""
import argparse
import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
KSN = os.path.join(ROOT, "stage0", "target", "release", "ksn")
SHORTENER = os.path.join(ROOT, "examples", "shortener")
OUT = os.path.join(HERE, "target")


def run(cmd, **kw):
    r = subprocess.run(cmd, capture_output=True, text=True, **kw)
    if r.returncode != 0:
        sys.exit("command failed: %s\n%s%s" % (" ".join(cmd), r.stdout, r.stderr))
    return r


def timed(cmd, **kw):
    t0 = time.monotonic()
    run(cmd, **kw)
    return time.monotonic() - t0


def build_all():
    os.makedirs(OUT, exist_ok=True)
    info = {}
    # Rust (rustc 直接。外部クレートなし)
    exe = os.path.join(OUT, "server_rust")
    t = timed(["rustc", "-C", "opt-level=3", "-C", "codegen-units=1", "-o", exe, os.path.join(HERE, "rust", "server.rs")])
    info["Rust (std, thread pool)"] = {"exe": exe, "build_s": t, "size": os.path.getsize(exe)}
    # 負荷生成器
    lg = os.path.join(OUT, "loadgen")
    run(["rustc", "-C", "opt-level=3", "-o", lg, os.path.join(HERE, "loadgen", "loadgen.rs")])
    # Go (あれば。ビルドキャッシュは使わない)
    if shutil.which("go"):
        exe = os.path.join(OUT, "server_go")
        env = dict(os.environ, GOCACHE=tempfile.mkdtemp(prefix="gocache_"), GO111MODULE="off")
        t = timed(["go", "build", "-o", exe, os.path.join(HERE, "go", "server.go")], env=env)
        shutil.rmtree(env["GOCACHE"], ignore_errors=True)
        info["Go (net, worker pool)"] = {"exe": exe, "build_s": t, "size": os.path.getsize(exe), "go": True}
    else:
        print("go が見つからないので Go は飛ばす", file=sys.stderr)
    # Kasane (LLVM が既定の経路。コンパイラのキャッシュは使わない)
    if not os.path.exists(KSN):
        sys.exit("ksn が未ビルド: cd stage0 && cargo build --release")
    cache = tempfile.mkdtemp(prefix="ksncache_")
    t = timed([KSN, "build", "--release"], cwd=SHORTENER, env=dict(os.environ, KSN_CACHE_DIR=cache))
    shutil.rmtree(cache, ignore_errors=True)
    exe = os.path.join(SHORTENER, "target", "release", "shortener.native")
    info["Kasane (native, worker pool)"] = {"exe": exe, "build_s": t, "size": os.path.getsize(exe), "kasane": True}
    return info


def wait_port(port, proc):
    for _ in range(200):
        if proc.poll() is not None:
            sys.exit("server exited early")
        try:
            socket.create_connection(("127.0.0.1", port), timeout=0.2).close()
            return
        except OSError:
            time.sleep(0.05)
    sys.exit("server did not start")


def start(name, spec, port, workers, cpus, pool=True):
    addr = "127.0.0.1:%d" % port
    if spec.get("kasane"):
        # 引数: <アドレス> <処理する接続数> <保存先 ("" = メモリのみ)> [ワーカー数 (0 または省略 = 1 接続ずつ)]
        # 接続数は、ワーカーの形では 0 = 無制限、1 接続ずつの形では上限 (大きな値) を渡す。
        cmd = [spec["exe"], addr, "0" if pool else "1000000000", "", str(workers if pool else 0)]
    elif spec.get("go"):
        cmd = [spec["exe"], "-addr", addr, "-workers", str(workers)]
        if not pool:
            cmd += ["-mode", "spawn"]
    else:
        cmd = [spec["exe"], addr, str(workers)]
    p = subprocess.Popen(["taskset", "-c", cpus] + cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    wait_port(port, p)
    return p


def measure(port, conns, seconds, cpus):
    r = run(["taskset", "-c", cpus, os.path.join(OUT, "loadgen"), "127.0.0.1:%d" % port, str(conns), str(seconds)])
    return dict(kv.split("=") for kv in r.stdout.split())


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--seconds", type=float, default=10)
    ap.add_argument("--conns", type=int, default=64)
    ap.add_argument("--workers", type=int, default=4)
    ap.add_argument("--server-cpus", default="0-9")
    ap.add_argument("--client-cpus", default="12-31")
    ap.add_argument("--repeat", type=int, default=3)
    ap.add_argument("--json", help="結果を JSON でも書き出す")
    a = ap.parse_args()
    info = build_all()
    rows = []
    port = 18100
    cases = []
    for name, spec in info.items():
        cases.append((name, spec, True))
    # 参考: 1 接続ずつ (Kasane の従来の動作) と、Go の接続ごとの goroutine
    for name, spec in info.items():
        if spec.get("kasane"):
            cases.append(("Kasane (native, sequential)", spec, False))
        if spec.get("go"):
            cases.append(("Go (net, goroutine per connection)", spec, False))
    for name, spec, pool in cases:
        runs = []
        for _ in range(a.repeat):
            port += 1
            p = start(name, spec, port, a.workers, a.server_cpus, pool)
            try:
                runs.append(measure(port, a.conns, a.seconds, a.client_cpus))
            finally:
                p.kill()
                p.wait()
        runs.sort(key=lambda r: float(r["rps"]))
        mid = runs[len(runs) // 2]  # RPS が中央値の回を採る
        rows.append((name, spec, mid, runs))
    print("\n条件: 並行接続 %d / 計測 %.0f 秒 (ウォームアップ 1 秒) / サーバのワーカー %d / %d 回の中央値 / サーバ CPU %s、生成器 CPU %s" % (a.conns, a.seconds, a.workers, a.repeat, a.server_cpus, a.client_cpus))
    print("\n| 実装 | RPS | p50 (ms) | p99 (ms) | エラー | 実行ファイル (KiB) | ビルド (秒) |")
    print("| --- | ---: | ---: | ---: | ---: | ---: | ---: |")
    for name, spec, m, runs in rows:
        print("| %s | %s | %.2f | %.2f | %s | %d | %.1f |" % (name, m["rps"], int(m["p50_us"]) / 1000, int(m["p99_us"]) / 1000, m["errors"], spec["size"] // 1024, spec["build_s"]))
    if a.json:
        with open(a.json, "w") as f:
            json.dump([{"name": n, "median": m, "runs": r, "size": s["size"], "build_s": s["build_s"]} for n, s, m, r in rows], f, indent=1)


if __name__ == "__main__":
    main()
