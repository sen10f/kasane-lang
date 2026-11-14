#!/usr/bin/env python3
"""合成プロジェクトを作る: gen.py <出力dir> <モジュール数> <モジュールあたりの関数数>
モジュール m<i> の関数 f0 は親 m<(i-1)//2>.f0 を呼ぶ (2 分木の依存)。f1.. は同じモジュール内の f0 を呼ぶ。"""
import os, sys
out, nmod, nfn = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
os.makedirs(out + "/src/syn", exist_ok=True)
open(out + "/kasane.toml", "w").write('[package]\nname = "syn"\nversion = "0.1.0"\nedition = 1\nentry = "syn.main"\n')
open(out + "/kasane.lock", "w").write("version = 1\n")
imports = f"import syn.m{nmod-1}\n"
open(out + "/src/syn.ksnl", "w").write(imports + "\nfn syn.main() -> i32 uses [] panics never pub\n    syn.m%d.f0(1i32)\nend syn.main\n" % (nmod - 1))
for i in range(nmod):
    s = ""
    if i > 0:
        s += f"import syn.m{(i-1)//2}\n\n"
    for j in range(nfn):
        if j == 0:
            body = f"syn.m{(i-1)//2}.f0(x).wrapping_add(1i32)" if i > 0 else "x.wrapping_add(1i32)"
        else:
            body = f"let a = syn.m{i}.f0(x)\n    a.wrapping_add({j}i32)"
        s += f"fn syn.m{i}.f{j}(x: i32) -> i32 uses [] panics never pub\n    {body}\nend syn.m{i}.f{j}\n"
        if j < nfn - 1:
            s += "\n"
    open(f"{out}/src/syn/m{i}.ksnl", "w").write(s)
