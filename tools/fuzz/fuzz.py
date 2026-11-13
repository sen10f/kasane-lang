#!/usr/bin/env python3
"""Kasane 差分ファジング道具 (標準ライブラリのみ)。

使い方:
    python -I tools/fuzz/fuzz.py --ksn <ksn パス> --seed <n> --count <n> --jobs <n>

子プロセスは taskset -c 0-9,12-31 で起動する (CPU 10/11 は不安定のため)。
"""
import argparse
import hashlib
import json
import os
import random
import re
import subprocess
import sys
import tempfile
from concurrent.futures import ThreadPoolExecutor, as_completed

TASKSET = "taskset -c 0-9,12-31".split()


def die(msg):
    sys.stderr.write("fuzz: " + msg + "\n")
    sys.exit(1)


def sh(cmd, cwd=None, env=None, timeout=120):
    """taskset 付きでコマンドを実行し (rc, stdout, stderr) を返す。"""
    full = list(TASKSET) + cmd
    try:
        p = subprocess.run(full, cwd=cwd, env=env, capture_output=True, timeout=timeout)
    except FileNotFoundError:
        return 127, b"", b"fuzz: command not found: " + full[0].encode()
    except subprocess.TimeoutExpired:
        return 124, b"", b"fuzz: timeout\n"
    return p.returncode, p.stdout, p.stderr


INT_TYPES = ["u8", "u16", "u32", "u64", "i8", "i16", "i32", "i64"]
UNSIGNED = ["u8", "u16", "u32", "u64"]
SIGNED = ["i8", "i16", "i32", "i64"]
BOOL = ["bool"]
FLOAT = ["f64"]
SCALAR = INT_TYPES + BOOL + FLOAT
RET_TYPES = ["u64", "i64"]
MAX_LINE = 100

# 構造体・列挙体の定義（生成器が出す Kasane ソースの先頭に並べる）
STRUCTS = [
    ("Pair", [("a", "u64"), ("b", "i64")]),
    ("Triple", [("x", "u32"), ("y", "i32"), ("z", "u64")]),
]
ENUMS = [
    ("Flag", [("A", None), ("B", None)]),
    ("Pick", [("One", "u64"), ("Two", "i64"), ("None_", None)]),
]


def lit(rng, t):
    if t == "bool":
        return rng.choice(["true", "false"])
    if t == "&str":
        return "\"hi\""
    if t in UNSIGNED:
        return "%du%s" % (rng.randint(0, 200), t[1:])
    if t in SIGNED:
        return "%d%s" % (rng.randint(-100, 100), t)
    if t in FLOAT:
        return "%.1f%s" % (rng.uniform(-10, 10), t)
    die("unknown type " + t)


def decl_block(rng):
    """構造体・列挙体の定義ブロック（0～1 個）。(lines, 宣言された構造体名リスト)"""
    lines = []
    structs = []
    if rng.random() < 0.6:
        sname, fields = rng.choice(STRUCTS)
        structs.append(sname)
        lines.append("type fuzz.%s" % sname)
        for fname, ft in fields:
            lines.append("    %s: %s" % (fname, ft))
        lines.append("end fuzz.%s" % sname)
        lines.append("")
    if rng.random() < 0.6:
        ename, variants = rng.choice(ENUMS)
        lines.append("enum fuzz.%s" % ename)
        for vname, pt in variants:
            if pt is None:
                lines.append("    %s" % vname)
            else:
                lines.append("    %s(%s)" % (vname, pt))
        lines.append("end fuzz.%s" % ename)
        lines.append("")
    return lines, structs


def int_binop(rng, a, b, t):
    op = rng.choice(["+", "-", "*", "/", "%", "<", ">", "<=", ">=", "==", "!="])
    if op in ["<", ">", "<=", ">=", "==", "!="]:
        return "(%s %s %s)" % (a, op, b), "bool"
    return "(%s.wrapping_%s(%s))" % (a, {"+": "add", "-": "sub", "*": "mul",
                                         "/": "div", "%": "rem"}[op], b), t


def int_method(rng, a, t):
    m = rng.choice(["wrapping_add", "wrapping_sub", "wrapping_mul",
                    "wrapping_neg", "checked_add", "checked_sub", "checked_mul",
                    "saturating_add", "saturating_sub", "saturating_mul"])
    if m in ["checked_add", "checked_sub", "checked_mul"]:
        b = lit(rng, t)
        return "(match %s.%s(%s) { Some(c) => c None => 0%s })" % (a, m, b, "" if t in SIGNED else "u64"), t
    if m == "wrapping_neg":
        return "(%s.wrapping_neg())" % a, t
    b = lit(rng, t)
    return "(%s.%s(%s))" % (a, m, b), t


def shift_expr(rng, a, t):
    if t in UNSIGNED:
        amt = lit(rng, "u32")
        op = rng.choice(["wrapping_shl", "wrapping_shr"])
        return "(%s.%s(%s))" % (a, op, amt), t
    return a, t


def float_expr(rng, a, b):
    op = rng.choice(["+", "-", "*", "/", "<", ">", "<=", ">=", "==", "!="])
    return "(%s %s %s)" % (a, op, b), ("bool" if op in ["<", ">", "<=", ">=", "==", "!="] else "f64")


def str_expr(rng, s1, s2):
    op = rng.choice(["==", "!="])
    return "(%s %s %s)" % (s1, op, s2), "bool"


def lit_f64(rng):
    return "%.1ff64" % rng.uniform(-10, 10)


def scalar_expr(rng, t):
    """型 t の値式（リテラル or 演算）。"""
    if t in UNSIGNED + SIGNED:
        a = lit(rng, t); b = lit(rng, t)
        r = rng.random()
        if r < 0.25:
            return "(%s %s %s)" % (a, rng.choice(["+", "-", "*", "/", "%"]), b), t
        if r < 0.5:
            return int_method(rng, a, t)
        if r < 0.65:
            return shift_expr(rng, a, t)
        if r < 0.75:
            op = rng.choice(["<", ">", "<=", ">=", "==", "!="])
            return "(%s %s %s)" % (a, op, b), "bool"
        return lit(rng, t), t
    if t == "bool":
        if rng.random() < 0.5:
            return lit(rng, "bool"), "bool"
        return "(%s == %s)" % (lit(rng, "u64"), lit(rng, "u64")), "bool"
    if t == "f64":
        if rng.random() < 0.5:
            return lit_f64(rng), "f64"
        return float_expr(rng, lit_f64(rng), lit_f64(rng))
    die("unknown type " + t)


def str_concat(rng, s):
    op = rng.choice(["push_str", "push_u64"])
    if op == "push_str":
        return "core.mem.drop(%s.push_str(\"x\"))" % s
    return "core.mem.drop(%s.push_u64(42u64))" % s


def gen_fn(rng, name, params, ret, fns, depth, structs):
    """1 関数の本体行リスト。structs はこのプログラムで宣言された構造体名。"""
    lines = []
    locals_ = []  # (name, type, is_mut) 関数レベル
    ctr = [0]
    n = rng.randint(2, 5)
    for _ in range(n):
        kind = rng.choice(["let", "if", "while", "match", "call", "field", "str", "float", "panic", "div0", "oob", "unwrap"])
        if kind == "let":
            scalar_types = INT_TYPES + BOOL + FLOAT
            struct_types = ["fuzz.%s" % s for s in structs]
            t = rng.choice(scalar_types + struct_types)
            mut = "mut " if rng.random() < 0.5 else ""
            vname = "v%d" % len(locals_)
            locals_.append((vname, t, mut == "mut "))
            if t == "fuzz.Pair":
                init = "fuzz.Pair { a: 1u64, b: 2i64 }"
            elif t == "fuzz.Triple":
                init = "fuzz.Triple { x: 1u32, y: 2i32, z: 3u64 }"
            else:
                if t == "alloc.vec.Vec<u64, std.alloc.System>":
                    t_ann = "alloc.vec.Vec<u64, std.alloc.System>"
                    init = "alloc.vec.Vec.new(std.alloc.SYSTEM)"
                elif t.startswith("alloc.string.String"):
                    t_ann = "alloc.string.String<std.alloc.System>"
                    init = "alloc.string.String.new(std.alloc.SYSTEM)"
                else:
                    t_ann = None
                    init = lit(rng, t)
                if t_ann:
                    lines.append("    let %s%s: %s = %s" % (mut, vname, t_ann, init))
                else:
                    lines.append("    let %s%s = %s" % (mut, vname, init))
        elif kind == "if":
            c, _ = scalar_expr(rng, "bool")
            then = gen_fn_block(rng, fns, 8, ctr)
            el_ = gen_fn_block(rng, fns, 8, ctr)
            lines.append("    if %s {" % c)
            lines.extend(then)
            lines.append("    } else {")
            lines.extend(el_)
            lines.append("    }")
        elif kind == "while" and depth < 2:
            c, _ = scalar_expr(rng, "bool")
            body = gen_fn_block(rng, fns, 8, ctr)
            lines.append("    while %s {" % c)
            lines.extend(body)
            lines.append("        break")
            lines.append("    }")
        elif kind == "match":
            cands = [(n, t) for n, t in params] + [(n, t) for n, t, _ in locals_]
            cands = [(n, t) for n, t in cands if t == "bool"]
            if not cands:
                continue
            var, _ = rng.choice(cands)
            lines.append("    core.mem.drop(match %s {" % var)
            lines.append("        true => ()")
            lines.append("        false => ()")
            lines.append("    })")
        elif kind == "call" and fns:
            fname, fparams, fret = rng.choice(fns)
            args = []
            ok = True
            for pn, pt in fparams:
                pool = [p for p, pt2 in params if pt2 == pt] + [n for n, t, _ in locals_ if t == pt]
                if pt in INT_TYPES + BOOL + FLOAT:
                    pool.append(lit(rng, pt))
                if not pool:
                    ok = False
                    break
                args.append(rng.choice(pool))
            if not ok:
                continue
            lines.append("    let r%d = fuzz.%s(%s)" % (len(locals_), fname, ", ".join(args)))
            locals_.append(("r%d" % len(locals_), fret, False))
        elif kind == "field" and locals_:
            struct_fields = {sname: dict(fields) for sname, fields in STRUCTS}
            cands = [(n, t, m) for n, t, m in locals_ if t in struct_fields]
            if not cands:
                continue
            vname, vt, mut = rng.choice(cands)
            fname, ft = rng.choice(list(struct_fields[vt].items()))
            if mut and rng.random() < 0.5:
                val = lit(rng, ft) if ft in INT_TYPES + BOOL else "0u64"
                lines.append("    %s.%s = %s" % (vname, fname, val))
            else:
                lines.append("    let a%d = %s.%s" % (len(locals_), vname, fname))
                locals_.append(("a%d" % len(locals_), ft, False))
        elif kind == "str":
            strs = [n for n, t, m in locals_ if t.startswith("alloc.string.String") and m]
            if not strs:
                continue
            sname = rng.choice(strs)
            lines.append("    %s" % str_concat(rng, sname))
        elif kind == "float":
            a = lit_f64(rng); b = lit_f64(rng)
            lines.append("    let f%d = (%s * %s).to_bits()" % (len(locals_), a, b))
            locals_.append(("f%d" % len(locals_), "u64", False))
        elif kind == "panic":
            if rng.random() < 0.5:
                lines.append("    core.panic.panic(\"boom\")")
        elif kind == "div0":
            idx = len(locals_)
            lines.append("    let d%d = 1u64 / 0u64" % idx)
            locals_.append(("d%d" % idx, "u64", False))
        elif kind == "oob":
            lines.append("    core.mem.drop(0u64)")
        elif kind == "unwrap":
            idx = len(locals_)
            lines.append("    let u%d: Option<u64> = None" % idx)
            locals_.append(("u%d" % idx, "Option<u64>", False))
            lines.append("    core.mem.drop(u%d.unwrap())" % idx)
    # 戻り値（tail）。ret と同じ型のパラメータを使う。tail は常に最後の式。
    tail_line = None
    if ret in INT_TYPES:
        same = [p for p, t in params if t == ret]
        if same:
            tail = same[0]
            for p in same[1:]:
                tail = "%s.wrapping_add(%s)" % (tail, p)
            tail_line = "    %s" % tail
            used = set(same)
        else:
            tail_line = "    %s" % lit(rng, ret)
            used = set()
    elif ret == "bool":
        same = [p for p, t in params if t == "bool"]
        if same:
            tail_line = "    %s" % same[0]
            used = set(same)
        else:
            tail_line = "    %s" % lit(rng, "bool")
            used = set()
    else:  # f64
        tail_line = "    %s" % lit_f64(rng)
        used = set()
    # 未使用パラメータと、本体で let 宣言されたコピー型ローカルを drop（tail の直前）。
    declared = set()
    for ln in lines:
        m = re.match(r"\s*let (?:mut\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*=", ln)
        if m:
            declared.add(m.group(1))
    local_types = {n: t for n, t, _ in locals_}
    copy_types = set(INT_TYPES + BOOL + FLOAT)
    for n in sorted(declared):
        if n in local_types and local_types[n] in copy_types:
            lines.append("    core.mem.drop(%s)" % n)
    for p, _ in params:
        if p not in used:
            lines.append("    core.mem.drop(%s)" % p)
    lines.append(tail_line)
    return lines


def gen_fn_block(rng, fns, indent, ctr):
    """if/while 本体。ブロックローカルは末尾で drop し、bare 式は使わない。"""
    sp = " " * indent
    lines = []
    locals_ = []
    n = rng.randint(1, 3)
    for _ in range(n):
        kind = rng.choice(["let", "expr", "match"])
        if kind == "let":
            t = rng.choice(INT_TYPES + BOOL)
            vname = "blk%d" % ctr[0]; ctr[0] += 1
            locals_.append(vname)
            lines.append("%slet %s = %s" % (sp, vname, lit(rng, t)))
        elif kind == "expr":
            lines.append("%score.mem.drop(0u64)" % sp)
        elif kind == "match":
            lines.append("%score.mem.drop(match true {" % sp)
            lines.append("%s    true => ()" % sp)
            lines.append("%s    false => ()" % sp)
            lines.append("%s})" % sp)
    for v in locals_:
        lines.append("%score.mem.drop(%s)" % (sp, v))
    if not lines:
        lines.append("%score.mem.drop(0u64)" % sp)
    return lines


def gen_program(rng, pid):
    decl_lines, declared_structs = decl_block(rng)
    # decl_block は空行で区切られた複数ブロックを返すので再構成
    decl_text = "\n".join(decl_lines).strip()
    decl_items = []
    if decl_text:
        for blk in decl_text.split("\n\n"):
            blk = blk.strip()
            if blk:
                decl_items.append(blk)
    # 項目名（FQN）でソート（check の定義順エラーを防ぐ）
    decl_items.sort(key=lambda b: b.splitlines()[0].split()[-1])

    nfn = rng.randint(2, 4)
    fns = []
    all_int = INT_TYPES + BOOL + FLOAT
    for fi in range(nfn):
        nparam = rng.randint(0, 3)
        if fi == 0:
            params = [("p%d" % i, rng.choice(INT_TYPES)) for i in range(nparam)]
        else:
            params = [("p%d" % i, rng.choice(INT_TYPES + BOOL + FLOAT)) for i in range(nparam)]
        ret = rng.choice(RET_TYPES) if fi == 0 else rng.choice(RET_TYPES + ["bool", "f64"])
        fname = "target" if fi == 0 else "fn%d" % fi
        fns.append((fname, params, ret))

    # 関数定義を構築（target=fns[0]）。順番は後でソート。
    fn_items = []
    target = fns[0]
    for fname, fparams, fret in fns:
        flines = ["fn fuzz.%s(%s) -> %s uses [alloc] panics may" % (fname, ", ".join("%s: %s" % p for p in fparams), fret)]
        if fname == target[0]:
            body = gen_fn(rng, "target", fparams, fret, [f for f in fns if f[0] != fname], 0, declared_structs)
        else:
            body = gen_fn(rng, fname, fparams, fret, fns, 0, declared_structs)
        flines.extend(body)
        flines.append("end fuzz.%s" % fname)
        fn_items.append("\n".join(flines))
    fn_items.sort(key=lambda b: b.split("\n")[0].split()[1])

    all_items = decl_items + fn_items  # 型/列挙体を先に、関数を後に
    for it in all_items:
        for ln in it.split("\n"):
            if len(ln) > MAX_LINE:
                return None  # 生成器の誤り：捨てる

    main_lits = [lit(rng, pt) for _, pt in target[1]]
    main_args = ", ".join(main_lits)
    ret = target[2]
    if ret in UNSIGNED:
        push = "core.mem.drop(s.push_u64(fuzz.target(%s)))" % main_args
    else:
        push = "core.mem.drop(s.push_i64(fuzz.target(%s)))" % main_args
    main_lines = [
        "fn fuzz.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may",
        "    let mut s = alloc.string.String.new(std.alloc.SYSTEM)",
        "    %s" % push,
    ]
    main_lines += [
        "    std.io.print_line(s.as_str())?",
        "    Ok(())",
        "end fuzz.main",
    ]
    main_item = "\n".join(main_lines)
    # main も定義順の対象（型/関数の後ろに固定）
    src = "\n\n".join(all_items + [main_item]) + "\n"
    return src, target[1], ret, main_lits


def make_pkg(tmpdir, seed, pid):
    rng = random.Random(seed * 100000 + pid)
    root = os.path.join(tmpdir, "pkg%d" % pid)
    os.makedirs(os.path.join(root, "src"), exist_ok=True)
    with open(os.path.join(root, "kasane.toml"), "w") as f:
        f.write('[package]\nname = "fuzz%d"\nversion = "0.1.0"\nedition = 1\nentry = "fuzz.main"\n' % pid)
    with open(os.path.join(root, "kasane.lock"), "w") as f:
        f.write("version = 1\n")
    res = gen_program(rng, pid)
    if res is None:
        return None
    src, params, ret, main_lits = res
    main_args_vals = [int(re.search(r"-?\d+", m).group(0)) for m in main_lits]
    with open(os.path.join(root, "src", "fuzz.ksnl"), "w") as f:
        f.write(src)
    return root, params, ret, src, main_args_vals


def check_pkg(ksn, root):
    rc, out, err = sh([ksn, "check"], cwd=root, timeout=120)
    return rc == 0, out.decode(errors="replace"), err.decode(errors="replace")


def run_tree(ksn, root):
    rc, out, err = sh([ksn, "run"], cwd=root, timeout=120)
    return rc, out.decode(errors="replace"), err.decode(errors="replace")


def kir_run(ksn, root, fqn, args):
    rc, out, err = sh([ksn, "kir", fqn, "--run"] + [str(a) for a in args], cwd=root, timeout=120)
    return rc, out.decode(errors="replace"), err.decode(errors="replace")


def kir_native(ksn, root, fqn, args, asan=False):
    env = dict(os.environ)
    if asan:
        env["KSN_CLANG_ARGS"] = "-fsanitize=address -O0 -g"
    rc, out, err = sh([ksn, "kir", fqn, "--native"] + [str(a) for a in args], cwd=root, env=env, timeout=180)
    return rc, out.decode(errors="replace"), err.decode(errors="replace")


def normalize(rc, out, err):
    # panic 文言と rc と stdout を比較対象にする
    return (rc, out.strip(), err.strip())


def compare(ksn, root, params, ret, args):
    fqn = "fuzz.target"
    results = {}
    rc, out, err = run_tree(ksn, root)
    results["tree"] = normalize(rc, out, err)
    rc, out, err = kir_run(ksn, root, fqn, args)
    if "未対応" in err or rc == 1 and "ユーザの関数ではない" in err:
        results["kir"] = ("unsupported", "", err.strip())
    else:
        results["kir"] = normalize(rc, out, err)
    rc, out, err = kir_native(ksn, root, fqn, args, asan=False)
    results["native"] = kir_status(rc, err, out, err)
    rc, out, err = kir_native(ksn, root, fqn, args, asan=True)
    results["asan"] = kir_status(rc, err, out, err)
    return results, args


def kir_status(rc, err, out, err2):
    if rc == 1 and "未対応" in err:
        return ("unsupported", "", err.strip())
    return normalize(rc, out, err2)


def process_one(ksn, seed, pid, found_dir):
    """1 プログラムを生成・比較。結果辞 dict を返す。"""
    res = make_pkg(tempfile.gettempdir(), seed, pid)
    if res is None:
        return {"status": "gen_fail"}
    root, params, ret, src, args = res
    ok, out, err = check_pkg(ksn, root)
    if not ok:
        return {"status": "check_fail", "src": src, "err": out + err}
    results, _ = compare(ksn, root, params, ret, args)
    # 4者のうち、未対応でない者どうしを比較
    valid = {k: v for k, v in results.items() if v[0] != "unsupported"}
    keys = list(valid.keys())
    first = valid[keys[0]] if keys else None
    mismatch = any(valid[k] != first for k in keys[1:])
    if not mismatch:
        return {"status": "ok"}
    # 食い違いあり：縮小して found/ に書き出し
    small = shrink(ksn, root, src, params, ret, args, results, found_dir)
    return {"status": "mismatch", "src": small, "results": results, "args": args}


def worker(item):
    ksn, seed, pid, found_dir = item
    return process_one(ksn, seed, pid, found_dir)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--ksn", required=True)
    ap.add_argument("--seed", type=int, required=True)
    ap.add_argument("--count", type=int, required=True)
    ap.add_argument("--jobs", type=int, default=1)
    args = ap.parse_args()

    args.ksn = os.path.abspath(args.ksn)
    if not os.path.isfile(args.ksn):
        die("--ksn が見つかりません: " + args.ksn)

    found_dir = os.path.join(os.path.dirname(os.path.abspath(__file__)), "found")
    os.makedirs(found_dir, exist_ok=True)

    counts = {"ok": 0, "gen_fail": 0, "check_fail": 0, "mismatch": 0, "unsupported": 0}
    items = [(args.ksn, args.seed, i, found_dir) for i in range(args.count)]

    shown = 0
    if args.jobs > 1:
        with ThreadPoolExecutor(max_workers=args.jobs) as ex:
            fut2it = {ex.submit(worker, it): it for it in items}
            for fut in as_completed(fut2it):
                it = fut2it[fut]
                r = fut.result()
                counts[r["status"]] = counts.get(r["status"], 0) + 1
                if r["status"] == "mismatch":
                    write_found(found_dir, r)
                elif r["status"] == "check_fail" and shown < 3:
                    shown += 1
                    print("CHECK_FAIL pid=%s:\n%s" % (it[2], r["err"]))
    else:
        for it in items:
            r = worker(it)
            counts[r["status"]] = counts.get(r["status"], 0) + 1
            if r["status"] == "mismatch":
                write_found(found_dir, r)
            elif r["status"] == "check_fail" and shown < 3:
                shown += 1
                print("CHECK_FAIL pid=%s:\n%s" % (it[2], r["err"]))

    print("結果: ok=%d gen_fail=%d check_fail=%d mismatch=%d unsupported=%d" % (
        counts["ok"], counts["gen_fail"], counts["check_fail"], counts["mismatch"], counts["unsupported"]))


def write_found(found_dir, r):
    idx = len(os.listdir(found_dir))
    d = os.path.join(found_dir, "case%04d" % idx)
    os.makedirs(d, exist_ok=True)
    with open(os.path.join(d, "program.ksnl"), "w") as f:
        f.write(r["src"])
    with open(os.path.join(d, "result.json"), "w") as f:
        json.dump({"args": r["args"], "results": {k: list(v) for k, v in r["results"].items()}},
                  f, ensure_ascii=False, indent=2)
    print("食い違い検出: %s" % d)


def split_top_level_units(lines):
    """body 行をトップレベルの文単位（if/while ブロックはまとまり）に分割して (start,end) のリストを返す。"""
    units = []
    i = 0
    n = len(lines)
    while i < n:
        ln = lines[i]
        if not ln.strip():
            i += 1
            continue
        start = i
        depth = ln.count("{") - ln.count("}")
        if depth > 0:
            j = i + 1
            while j < n and depth > 0:
                depth += lines[j].count("{") - lines[j].count("}")
                j += 1
            units.append((start, j))
            i = j
        else:
            units.append((start, start + 1))
            i += 1
    return units


def mismatch_persists(res2):
    valid = {k: v for k, v in res2.items() if v[0] != "unsupported"}
    keys = list(valid.keys())
    if not keys:
        return False
    first = valid[keys[0]]
    return any(valid[k] != first for k in keys[1:])


def find_fn_bounds(lines):
    """fn fuzz.X ... end fuzz.X の (start,end,name) をすべて返す。"""
    bounds = []
    i = 0
    n = len(lines)
    while i < n:
        ln = lines[i]
        if ln.startswith("fn fuzz."):
            name = ln.split("(")[0].split()[-1]  # fuzz.X
            j = i + 1
            while j < n and lines[j] != "end " + name:
                j += 1
            if j < n:
                j += 1  # end 行を含める
            bounds.append((i, j, name))
            i = j
        else:
            i += 1
    return bounds


def called_names(src):
    return set(re.findall(r"fuzz\.([A-Za-z0-9_]+)\s*\(", src))


def remove_unused_functions(ksn, root, src, params, ret, args):
    """target/main から到達不能な関数定義を 1 つずつ削り、食い違いが残るなら採用。"""
    while True:
        lines = src.split("\n")
        bounds = find_fn_bounds(lines)
        names = set(b[2] for b in bounds)
        if "fuzz.target" not in names or "fuzz.main" not in names:
            return src
        used = {"fuzz.target"}
        frontier = ["fuzz.target"]
        body_by_name = {b[2]: "\n".join(lines[b[0]:b[1]]) for b in bounds}
        while frontier:
            cur = frontier.pop()
            for c in called_names(body_by_name.get(cur, "")):
                full = "fuzz." + c
                if full in names and full not in used:
                    used.add(full)
                    frontier.append(full)
        used.add("fuzz.main")
        removable = [b for b in bounds if b[2] not in used]
        if not removable:
            return src
        # 1 つ削って食い違いが残れば採用してやり直し
        progressed = False
        for (s, e, name) in removable:
            trial = lines[:s] + lines[e:]
            trial_src = "\n".join(trial)
            root2 = write_tmp_pkg(root, trial_src)
            ok, _, _ = check_pkg(ksn, root2)
            if not ok:
                continue
            res2, _ = compare(ksn, root2, params, ret, args)
            if mismatch_persists(res2):
                src = trial_src
                progressed = True
                break
        if not progressed:
            return src


def shrink(ksn, root, src, params, ret, args, results, found_dir):
    """target 本体を文/ブロック単位で消し、未使用関数を削り、食い違いが残る形を採用。"""
    lines = src.split("\n")
    tstart = None
    tend = None
    for i, ln in enumerate(lines):
        if ln.startswith("fn fuzz.target"):
            tstart = i
        if tstart is not None and ln == "end fuzz.target":
            tend = i
            break
    if tstart is None or tend is None:
        return src
    body_lines = lines[tstart + 1:tend]
    cur = list(body_lines)
    changed = True
    while changed:
        changed = False
        units = split_top_level_units(cur)
        for (s, e) in units:
            trial = cur[:s] + cur[e:]
            new_src = build_src(lines, tstart, tend, trial)
            root2 = write_tmp_pkg(root, new_src)
            ok, _, _ = check_pkg(ksn, root2)
            if not ok:
                continue
            res2, _ = compare(ksn, root2, params, ret, args)
            if mismatch_persists(res2):
                cur = trial
                changed = True
                break
    new_src = build_src(lines, tstart, tend, cur)
    new_src = remove_unused_functions(ksn, root, new_src, params, ret, args)
    return new_src


def build_src(all_lines, tstart, tend, body):
    new = all_lines[:tstart + 1] + body + all_lines[tend:]
    return "\n".join(new)


def write_tmp_pkg(root, new_src):
    import shutil
    root2 = root + "_shrink"
    if os.path.exists(root2):
        shutil.rmtree(root2)
    shutil.copytree(root, root2)
    with open(os.path.join(root2, "src", "fuzz.ksnl"), "w") as f:
        f.write(new_src)
    return root2


if __name__ == "__main__":
    main()
