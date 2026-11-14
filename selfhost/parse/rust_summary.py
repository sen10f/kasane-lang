#!/usr/bin/env python3
"""Rust 版 (ksn) の既存コマンドから、parse (Kasane 製) と同じ形の要約を作る。

  python3 rust_summary.py [--ksn PATH] ファイル...

出力 (ファイルごと):
  == パス
  種別 FQN 行番号        (定義ごと。ksn defs の結果)
  nodes -                (Rust 版にはノード数を出す手段がない)
解析に失敗したら 1 行 `error パス:行:桁: 説明` (ksn defs / headers / bodies の最初のエラー)。

基準にしているコマンド:
  ksn defs     行頭の予約語からトップレベル定義を走査 (種別・名前・行・end の行)
  ksn headers  fn / type / enum / trait の頭を構文解析 (失敗するとエラー)
  ksn bodies   fn / test の本体を式・文として構文解析 (失敗するとエラー)
"""
import os
import re
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
DEFAULT_KSN = os.path.normpath(os.path.join(HERE, "..", "..", "stage0", "target", "release", "ksn"))
ERR_RE = re.compile(r"E\d+\|(\d+)\|(\d+)\|(.*)")


def _run(ksn, cmd, path):
    r = subprocess.run([ksn, cmd, path], capture_output=True, text=True, errors="replace")
    return r


def _error(path, text):
    m = ERR_RE.search(text)
    if m:
        return f"error {path}:{m.group(1)}:{m.group(2)}: {m.group(3).strip()}"
    return f"error {path}:0:0: {text.strip()[:120]}"


def summarize(path, ksn=DEFAULT_KSN):
    """(ok, 行のリスト) を返す。ok=False のとき行は 1 行のエラー。"""
    r = _run(ksn, "defs", path)
    if r.returncode != 0 or r.stderr.strip():
        return False, [_error(path, r.stderr or r.stdout)]
    defs = []
    for line in r.stdout.splitlines():
        kw, name, ln, _end = line.split("\t")[:4]
        defs.append(f"{kw} {name} {ln}")
    for cmd in ("headers", "bodies"):
        r2 = _run(ksn, cmd, path)
        if r2.returncode != 0 or r2.stderr.strip():
            return False, [_error(path, r2.stderr or r2.stdout)]
    return True, [f"== {path}"] + defs + ["nodes -"]


# Rust 版の本体ダンプに出るノード名 (parse 側の種別名との対応)。
BODY_KINDS = {
    "Bin": "Bin", "Unary": "Unary", "Call": "Call", "MCall": "MCall", "Field": "Dot", "Index": "Index",
    "Try": "Try", "Lit": "Lit", "Path": "Path", "StructLit": "StructLit", "TupleE": "TupleE",
    "ArrayE": "ArrayE", "ArrayRep": "ArrayRep", "Closure": "Closure", "If": "If", "Match": "Match",
    "Loop": "Loop", "While": "While", "For": "For", "Unsafe": "Unsafe", "With": "With", "Block": "Block",
    "Let": "Let", "Assign": "Assign", "ExprStmt": "ExprStmt", "Break": "Break", "Continue": "Continue",
    "Return": "Return", "PBind": "PBind", "PLit": "PLit", "PWild": "PWild", "PPath": "PPath",
    "PVariant": "PVariant", "PStruct": "PStruct", "PTuple": "PTuple", "PRef": "PRef",
}
NODE_RE = re.compile(r"\((" + "|".join(BODY_KINDS) + r")(?=[ )\]])")


def body_kinds(path, ksn=DEFAULT_KSN):
    """ksn bodies のダンプに現れるノード種別ごとの個数 (parse 側の種別名で返す)。"""
    r = _run(ksn, "bodies", path)
    counts = {}
    for m in NODE_RE.finditer(r.stdout):
        k = BODY_KINDS[m.group(1)]
        counts[k] = counts.get(k, 0) + 1
    # Rust 版は with の束縛名を PBind として数える。parse 側は With ノードに名前を持たせている。
    counts["PBind"] = counts.get("PBind", 0) - counts.get("With", 0)
    return counts


def main(argv):
    ksn = DEFAULT_KSN
    args = []
    it = iter(argv)
    for a in it:
        if a == "--ksn":
            ksn = next(it)
        else:
            args.append(a)
    for p in args:
        ok, lines = summarize(p, ksn)
        print("\n".join(lines))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
