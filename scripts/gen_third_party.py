#!/usr/bin/env python3
"""THIRD_PARTY_LICENSES を作る: stage0 の依存 crate (cargo metadata) のライセンス本文 + LLVM/lld + libstdc++ の注記。
使い方: python3 -I scripts/gen_third_party.py <出力ファイル> [LLVMのソースの根 (LICENSE.TXT のある llvm-project)]"""
import json, os, subprocess, sys

out = sys.argv[1]
llvm_src = sys.argv[2] if len(sys.argv) > 2 else os.path.expanduser("~/llvm-ksn/llvm-project-20.1.2.src")
root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
meta = json.loads(subprocess.check_output(["cargo", "metadata", "--format-version", "1", "--locked"], cwd=os.path.join(root, "stage0")))
names = {"LICENSE", "LICENSE-MIT", "LICENSE-APACHE", "LICENSE.md", "LICENSE.txt", "LICENSE-MIT.md", "LICENSE-APACHE.md", "COPYING", "UNLICENSE", "LICENSE-BSD", "LICENSE-THIRD-PARTY", "LICENSE-ZLIB"}
parts = ["Kasane (ksn) に静的に含まれる第三者のソフトウェアのライセンス\n" + "=" * 60 + "\n"]
for p in sorted(meta["packages"], key=lambda p: (p["name"], p["version"])):
    if p["name"] == "kasane-lang":
        continue
    d = os.path.dirname(p["manifest_path"])
    texts = []
    for f in sorted(os.listdir(d)):
        if f in names or f.upper().startswith(("LICENSE", "COPYING", "UNLICENSE")):
            fp = os.path.join(d, f)
            if os.path.isfile(fp):
                texts.append("--- %s ---\n%s" % (f, open(fp, errors="replace").read().strip()))
    parts.append("\n%s\n%s %s (%s)\n%s\n%s\n" % ("-" * 60, p["name"], p["version"], p.get("license") or "ライセンス表記なし", p.get("repository") or "", "\n\n".join(texts) or "(crate にライセンス本文のファイルが無い。上の SPDX 表記に従う)"))
for tool, rel in [("LLVM / clang 由来のコード生成 (libLLVM*.a)", "llvm/LICENSE.TXT"), ("lld (組み込みのリンカ)", "lld/LICENSE.TXT")]:
    fp = os.path.join(llvm_src, rel)
    parts.append("\n%s\n%s: Apache License v2.0 with LLVM Exceptions (https://llvm.org)\n%s\n%s\n" % ("-" * 60, tool, "-" * 60, open(fp).read().strip() if os.path.isfile(fp) else "(本文は https://llvm.org/LICENSE.txt)"))
parts.append("\n%s\nlibstdc++ / libgcc (静的に取り込み): GNU GPL v3 with GCC Runtime Library Exception\nhttps://www.gnu.org/licenses/gcc-exception-3.1.html\n実行ファイルに取り込まれる部分は上の例外により、結果の実行ファイルを GPL の条件に縛らない。\n\nRust 標準ライブラリ (std/core/alloc): MIT OR Apache-2.0\nzig の compiler-rt (配布用ビルドの cross リンカ経由で取り込み): MIT\n" % ("-" * 60))
open(out, "w").write("".join(parts))
print("wrote", out, len(meta["packages"]) - 1, "crates")
