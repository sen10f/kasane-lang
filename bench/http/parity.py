#!/usr/bin/env python3
"""3 つのサーバが同じ要求に同じ応答 (バイト列まで同じ) を返すことを確かめる。標準ライブラリのみ。

使い方: python3 bench/http/parity.py   (run.py と同じビルド成果物を使うので、先に run.py かビルドが要る)
"""
import os
import socket
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
OUT = os.path.join(HERE, "target")
KASANE = os.path.join(ROOT, "examples", "shortener", "target", "release", "shortener.native")

REQUESTS = [
    b"POST /shorten HTTP/1.1\r\nHost: x\r\nContent-Length: 20\r\n\r\nhttp://example.com/a",
    b"POST /shorten HTTP/1.1\r\nContent-Length: 28\r\n\r\nhttps://example.com/b?x=1&y=2",
    b"POST /shorten HTTP/1.1\r\nContent-Length: 4\r\n\r\nnope",
    b"POST /shorten HTTP/1.1\r\nContent-Length: 15\r\n\r\nhttp://a b/path",
    b"GET /1000 HTTP/1.1\r\n\r\n",
    b"GET /1001 HTTP/1.1\r\n\r\n",
    b"GET /1002 HTTP/1.1\r\n\r\n",
    b"GET /zzzz HTTP/1.1\r\n\r\n",
    b"GET /stats HTTP/1.1\r\n\r\n",
    b"GET /shorten HTTP/1.1\r\n\r\n",
    b"DELETE /1000 HTTP/1.1\r\n\r\n",
    b"garbage\r\n\r\n",
    b"POST /shorten HTTP/1.1\r\nContent-Length: 99999\r\n\r\n",
]


def ask(port, req):
    s = socket.create_connection(("127.0.0.1", port), timeout=5)
    s.sendall(req)
    raw = b""
    while True:
        c = s.recv(4096)
        if not c:
            break
        raw += c
    s.close()
    return raw


def serve(cmd, port):
    p = subprocess.Popen(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    for _ in range(100):
        try:
            socket.create_connection(("127.0.0.1", port), timeout=0.2).close()
            return p
        except OSError:
            time.sleep(0.05)
    p.kill()
    sys.exit("start failed: %s" % cmd)


def main():
    servers = {
        "kasane": [KASANE, "127.0.0.1:18201", "0", "", "4"],
        "rust": [os.path.join(OUT, "server_rust"), "127.0.0.1:18202", "4"],
    }
    ports = {"kasane": 18201, "rust": 18202}
    if os.path.exists(os.path.join(OUT, "server_go")):
        servers["go"] = [os.path.join(OUT, "server_go"), "-addr", "127.0.0.1:18203", "-workers", "4"]
        ports["go"] = 18203
    procs = {k: serve(v, ports[k]) for k, v in servers.items()}
    bad = 0
    try:
        for req in REQUESTS:
            got = {k: ask(ports[k], req) for k in servers}
            ref = got["kasane"]
            same = all(v == ref for v in got.values())
            print("%s %r" % ("ok  " if same else "DIFF", req[:40]))
            if not same:
                bad += 1
                for k, v in got.items():
                    print("   ", k, v)
    finally:
        for p in procs.values():
            p.kill()
    print("一致しなかった要求: %d / %d" % (bad, len(REQUESTS)))
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
