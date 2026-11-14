#!/usr/bin/env python3
"""LSP (ksn lsp) を JSON-RPC で駆動し、1 ファイルを編集してから publishDiagnostics が来るまでを測る。
使い方: drive.py <ksn> <プロジェクト> <編集するモジュール名 (例 m49)> [回数]
編集は関数 f1 の本体の定数を変えるだけ (診断は変わらない)。最後に check --json との一致も確かめる。"""
import json, os, subprocess, sys, time, re
ksn, proj, mod = sys.argv[1], os.path.abspath(sys.argv[2]), sys.argv[3]
n = int(sys.argv[4]) if len(sys.argv) > 4 else 3
path = f"{proj}/src/syn/{mod}.ksnl"
uri = "file://" + path
p = subprocess.Popen([ksn, "lsp"], cwd=proj, stdin=subprocess.PIPE, stdout=subprocess.PIPE, env=os.environ)
def send(m):
    b = json.dumps(m).encode(); p.stdin.write(b"Content-Length: %d\r\n\r\n" % len(b) + b); p.stdin.flush()
def recv():
    h = b""
    while not h.endswith(b"\r\n\r\n"): h += p.stdout.read(1)
    ln = int(re.search(rb"Content-Length: (\d+)", h).group(1))
    return json.loads(p.stdout.read(ln))
def wait_diag(target):
    while True:
        m = recv()
        if os.environ.get("DBG"): print(str(m)[:200])
        if m.get("method") == "textDocument/publishDiagnostics" and m["params"]["uri"] == target: return m
send({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"rootUri": "file://" + proj, "capabilities": {}}}); recv()
send({"jsonrpc": "2.0", "method": "initialized", "params": {}})
text = open(path).read()
t0 = time.time()
send({"jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {"textDocument": {"uri": uri, "languageId": "ksn", "version": 1, "text": text}}})
wait_diag(uri); print(f"open (初回の全体検査): {time.time()-t0:.2f}s")
for k in range(n):
    new = text.replace("wrapping_add(1i32)", f"wrapping_add({k+2}i32)", 1) if k == 0 else text.replace("wrapping_add(1i32)", f"wrapping_add({k+2}i32)", 1)
    t0 = time.time()
    send({"jsonrpc": "2.0", "method": "textDocument/didChange", "params": {"textDocument": {"uri": uri, "version": k + 2}, "contentChanges": [{"text": new}]}})
    wait_diag(uri); print(f"didChange #{k+1} → publishDiagnostics: {time.time()-t0:.2f}s")
send({"jsonrpc": "2.0", "id": 9, "method": "shutdown"}); recv(); send({"jsonrpc": "2.0", "method": "exit"})
