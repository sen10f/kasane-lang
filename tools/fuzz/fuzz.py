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


INT_TYPES = ["u8", "u16", "u32", "u64", "usize", "i8", "i16", "i32", "i64", "isize"]
UNSIGNED = ["u8", "u16", "u32", "u64", "usize"]
SIGNED = ["i8", "i16", "i32", "i64", "isize"]
BOOL = ["bool"]
FLOAT = ["f32", "f64"]
SCALAR = INT_TYPES + BOOL + FLOAT
RET_TYPES = ["u64", "i64"]
MAX_LINE = 100


def lit(rng, t):
    if t == "bool":
        return rng.choice(["true", "false"])
    if t in UNSIGNED:
        return "%du%s" % (rng.randint(0, 200), t[1:])
    if t in SIGNED:
        return "%d%s" % (rng.randint(-100, 100), t)
    if t in FLOAT:
        return "%.1f%s" % (rng.uniform(-10, 10), t)
    die("unknown type " + t)


def binop(rng, a, b, t):
    if t in UNSIGNED + SIGNED:
        op = rng.choice(["+", "-", "*", "/", "%", "<", ">", "<=", ">=", "==", "!="])
        if op in ["<", ">", "<=", ">=", "==", "!="]:
            return "(%s %s %s)" % (a, op, b), "bool"
        return "(%s.wrapping_%s(%s))" % (a, {"+": "add", "-": "sub", "*": "mul",
                                             "/": "div", "%": "rem"}[op], b), t
    if t == "bool":
        op = rng.choice(["&&", "||", "==", "!="])
        return "(%s %s %s)" % (a, op, b), "bool"
    if t in FLOAT:
        op = rng.choice(["+", "-", "*", "/", "<", ">", "<=", ">=", "==", "!="])
        return "(%s %s %s)" % (a, op, b), ("bool" if op in ["<", ">", "<=", ">=", "==", "!="] else t)
    die("unknown type " + t)


def unop(rng, a, t):
    if t in UNSIGNED + SIGNED:
        op = rng.choice(["wrapping_neg", "checked_neg"])
        if op == "checked_neg":
            return "(match %s.checked_neg() { Some(c) => c None => 0%s })" % (a, t), t
        return "(%s.wrapping_neg())" % a, t
    if t == "bool":
        return "(!%s)" % a, "bool"
    if t in FLOAT:
        return "(-%s)" % a, t
    die("unknown type " + t)


def shift(rng, a, t):
    if t in UNSIGNED:
        amt = lit(rng, "u32")
        op = rng.choice(["wrapping_shl", "wrapping_shr"])
        return "(%s.%s(%s))" % (a, op, amt), t
    die("no shift for " + t)


def checked_binop(rng, a, b, t):
    if t in UNSIGNED + SIGNED:
        op = rng.choice(["checked_add", "checked_sub", "checked_mul", "checked_div", "checked_rem"])
        return "(match %s.%s(%s) { Some(c) => c None => 0u64 })" % (a, op, b), t
    die("no checked for " + t)


def num_conv(rng, v, src, dst):
    if src == dst:
        return v
    if src in INT_TYPES and dst in INT_TYPES:
        return "%s.from_%s(%s)" % (dst, src, v)
    if src == "f64" and dst == "u64":
        return "%s.to_bits()" % v
    if src == "u64" and dst == "f64":
        return "%s.nearest_to_f64()" % v
    die("no conv %s->%s" % (src, dst))


def vec_stmt(rng, vname, etype):
    # 返す文リストと、最後に得られる値式 (Option<etype>)
    n = rng.randint(0, 3)
    stmts = []
    for _ in range(n):
        stmts.append("    core.mem.drop(match %s.push(%s) { Ok(_u) => () Err(_e) => core.panic.panic(\"oom\") })" % (vname, lit(rng, etype)))
    idx = lit(rng, "usize")
    stmts.append("    let _g = %s.get(%s)" % (vname, idx))
    return stmts, "_g"


def enum_match(rng, var, etype, branches):
    # bool / Option 等の match 本体。branches: [(pattern, expr, type)]
    lines = ["    match %s {" % var]
    for pat, expr, bt in branches:
        lines.append("        %s => %s" % (pat, expr))
    lines.append("    }")
    return lines


def gen_body(rng, depth, ret, seq, tail, vecs):
    lines = []
    n = rng.randint(1, 4)
    def ebool():
        if rng.random() < 0.5:
            return lit(rng, "bool")
        return "(%s == %s)" % (lit(rng, "u64"), lit(rng, "u64"))

    for _ in range(n):
        kind = rng.choice(["if", "while", "match", "vec", "panic"])
        if kind == "if":
            c = ebool()
            then = ["    " + ln for ln in gen_body(rng, depth + 1, ret, seq, False, vecs)]
            el_ = ["    " + ln for ln in gen_body(rng, depth + 1, ret, seq, False, vecs)]
            lines.append("    if %s {" % c)
            lines.extend(then)
            lines.append("    } else {")
            lines.extend(el_)
            lines.append("    }")
        elif kind == "while" and depth < 2:
            c = ebool()
            body = ["    " + ln for ln in gen_body(rng, depth + 1, ret, seq, False, vecs)]
            lines.append("    while %s {" % c)
            lines.extend(body)
            lines.append("        break")
            lines.append("    }")
        elif kind == "match" and depth < 2:
            c = lit(rng, "bool")
            lines.append("    core.mem.drop(match %s {" % c)
            lines.append("        true => ()")
            lines.append("        false => ()")
            lines.append("    })")
        elif kind == "vec" and depth == 0:
            etype = rng.choice(["u64", "i64", "u32", "i32"])
            seq[0] += 1
            vname = "vec%d" % seq[0]
            vecs.append((vname, etype))
            lines.append("    let mut %s = alloc.vec.Vec.new(std.alloc.SYSTEM)" % vname)
            for _ in range(rng.randint(1, 3)):
                lines.append("    core.mem.drop(match %s.push(%s) {" % (vname, lit(rng, etype)))
                lines.append("        Ok(_u) => ()")
                lines.append("        Err(_e) => ()")
                lines.append("    })")
            idx = lit(rng, "usize")
            lines.append("    core.mem.drop(match %s.get(%s) {" % (vname, idx))
            lines.append("        Some(_c) => ()")
            lines.append("        None => ()")
            lines.append("    })")
        elif kind == "panic":
            lines.append("    core.panic.panic(\"boom\")")
    if tail:
        lines.append("    %s" % ret_expr(rng, ret, vecs))
    return lines


def mform(scrut, arms):
    lines = ["(match %s {" % scrut]
    for pat, ex in arms:
        lines.append("        %s => %s" % (pat, ex))
    lines.append("    })")
    return "\n".join(lines)


def ret_expr(rng, ret, vecs):
    r = rng.random()
    if ret in INT_TYPES and vecs and r < 0.25:
        cand = [(v, t) for (v, t) in vecs if t == ret]
        if cand:
            v, _ = rng.choice(cand); idx = lit(rng, "usize")
            zero = "0u64" if ret in UNSIGNED else "0i64"
            return mform("%s.get(%s)" % (v, idx), [("Some(c)", "*c"), ("None", zero)])
    if ret in INT_TYPES:
        if r < 0.18:
            return lit(rng, ret)
        if r < 0.34:
            a = lit(rng, ret); b = lit(rng, ret); op = rng.choice(["+", "-", "*", "/", "%"])
            return "(%s %s %s)" % (a, op, b)
        if r < 0.50:
            a = lit(rng, ret); b = lit(rng, ret); op = rng.choice(["wrapping_add", "wrapping_sub", "wrapping_mul"])
            return "(%s.%s(%s))" % (a, op, b)
        if r < 0.70:
            a = lit(rng, ret); amt = lit(rng, "u32"); op = rng.choice(["wrapping_shl", "wrapping_shr"])
            return "(%s.%s(%s))" % (a, op, amt)
        if r < 0.82:
            return "(%s.wrapping_neg())" % lit(rng, ret)
        if r < 0.94:
            return mform(lit(rng, "bool"), [("true", lit(rng, ret)), ("false", lit(rng, ret))])
        return lit(rng, ret)
    # ret == "bool"
    if r < 0.3:
        return lit(rng, "bool")
    if r < 0.6:
        return "(%s == %s)" % (lit(rng, "u64"), lit(rng, "u64"))
    if r < 0.85:
        return "(%s %s %s)" % (lit(rng, "bool"), rng.choice(["&&", "||"]), lit(rng, "bool"))
    return mform(lit(rng, "bool"), [("true", lit(rng, "bool")), ("false", lit(rng, "bool"))])


def eliteral(rng, t):
    if t == "bool":
        return lit(rng, "bool"), "bool"
    if t in FLOAT:
        return lit(rng, t), t
    if t in INT_TYPES:
        if rng.random() < 0.3:
            op = rng.choice(["+", "-", "*", "/", "%"])
            return "(%s %s %s)" % (lit(rng, t), op, lit(rng, t)), t
        return lit(rng, t), t
    return lit(rng, t), t


def ebinop(rng, t):
    return eliteral(rng, t)


def gen_program(rng, pid):
    nparam = rng.randint(1, 3)
    params = [("_xp%d" % i, rng.choice(INT_TYPES)) for i in range(nparam)]
    ret = rng.choice(RET_TYPES)
    uses = "uses [alloc]"
    body = gen_body(rng, 0, ret, [0], True, [])
    target_lines = ["fn fuzz.target(%s) -> %s %s panics may" % (
        ", ".join("%s: %s" % p for p in params), ret, uses)]
    target_lines.extend(body)
    target_lines.append("end fuzz.target")
    for ln in target_lines:
        if len(ln) > MAX_LINE:
            return None  # 生成器の誤り：捨てる
    main_lits = [lit(rng, pt) for _, pt in params]
    main_args_vals = [int(re.search(r"-?\d+", m).group(0)) for m in main_lits]
    main_args = ", ".join(main_lits)
    push = "push_i64" if ret in SIGNED else "push_u64"
    main_lines = [
        "fn fuzz.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may",
        "    let mut s = alloc.string.String.new(std.alloc.SYSTEM)",
        "    core.mem.drop(s.%s(fuzz.target(%s)))" % (push, main_args),
        "    std.io.print_line(s.as_str())?",
        "    Ok(())",
        "end fuzz.main",
    ]
    src = "\n".join(main_lines) + "\n\n" + "\n".join(target_lines) + "\n"
    return src, params, ret, main_args_vals


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
    src, params, ret, main_args_vals = res
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


def shrink(ksn, root, src, params, ret, args, results, found_dir):
    """target の文を1つずつ消し、食い違いが残るならその形を採用。"""
    lines = src.split("\n")
    # target の本体行を特定（fn fuzz.target から end fuzz.target の間）
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
        for i in range(len(cur)):
            trial = cur[:i] + cur[i + 1:]
            new_src = build_src(lines, tstart, tend, trial)
            root2 = write_tmp_pkg(root, new_src)
            ok, _, _ = check_pkg(ksn, root2)
            if not ok:
                continue
            res2, _ = compare(ksn, root2, params, ret, args)
            valid = {k: v for k, v in res2.items() if v[0] != "unsupported"}
            keys = list(valid.keys())
            if not keys:
                continue
            first = valid[keys[0]]
            if any(valid[k] != first for k in keys[1:]):
                cur = trial
                changed = True
                break
    return build_src(lines, tstart, tend, cur)


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
