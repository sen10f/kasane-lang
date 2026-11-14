"""借用とライフタイムの形を作る生成器 (検査器の健全性を探る)。

各プログラムに `bad` (明らかに不健全 = `ksn check` が落とすべき) / `ok` (正しい = 通るべき) のラベルを付ける。
`bad` なのに通れば穴、`ok` なのに落ちれば誤検出。使い方は README.md。標準ライブラリのみ。

形 = 借用の元 (String.as_str / u32 / String の参照 / &mut u32) × 借用の包み方 (そのまま・恒等関数・再借用・
fst/snd の関数・let 経由) × 出口 (外の変数への代入・ブロックの値・構造体の欄・Option・タプル・Vec の push・
`&mut` 引数・メソッドのセッタ・クロージャの取り込み・ヘッダ付き関数の戻り値) × 元が内側/外側。
変更・move 系は別のテンプレート (借用中の push / move / 代入)。
"""

import random

# 借用の元。decl = 宣言 (名前 x)、b = 借用式、ty = 借用の型、outer_decl = 外側に置く既定値の宣言、use = 読み出し
KINDS = {
    "str": dict(
        decl='let mut x = alloc.string.String.new(std.alloc.SYSTEM)\ncore.mem.drop(x.push_str("hello"))',
        b="x.as_str()", ty="&{l}str", use=lambda e: f"{e}.len()", default='"d"', mut=False,
    ),
    "u32": dict(decl="let x = 7u32", b="&x", ty="&{l}u32", use=lambda e: f"*{e}", default="&o", mut=False),
    "string": dict(
        decl="let x = t.mk()", b="&x", ty="&{l}alloc.string.String", use=lambda e: f"t.peek({e})",
        default="&o", mut=False,
    ),
    "mutu32": dict(decl="let mut x = 7u32", b="&mut x", ty="&{l}mut u32", use=lambda e: f"*{e}", default="&mut o", mut=True),
}
OUTER_DECL = {
    "str": "",
    "u32": "let o = 1u32",
    "string": "let o = t.mk()",
    "mutu32": "let mut o = 1u32\nlet mut p = 2u32",
}


def ty(k, lt="'a"):
    return KINDS[k]["ty"].format(l=(lt + " ") if lt else "")


def helpers(k):
    """k 用の補助関数 (どれも本体は健全)。"""
    T = lambda lt: ty(k, lt)
    out = [
        f"fn t.id(x: {T('')}) -> {T('')} uses [] panics never\n    x\nend t.id",
        f"fn t.fst['a, 'b](x: {T(chr(39)+'a')}, _y: {T(chr(39)+'b')}) -> {T(chr(39)+'a')} uses [] panics never\n    x\nend t.fst",
        f"fn t.snd['a, 'b](_x: {T(chr(39)+'a')}, y: {T(chr(39)+'b')}) -> {T(chr(39)+'b')} uses [] panics never\n    y\nend t.snd",
        f"type t.H['a]\n    r: {T(chr(39)+'a')}\nend t.H",
        f"fn t.H.set['a](self: &mut t.H<'a>, r: {T(chr(39)+'a')}) -> () uses [] panics never\n    self.r = r\nend t.H.set",
        f"fn t.put['a](o: &mut {T(chr(39)+'a')}, r: {T(chr(39)+'a')}) -> () uses [] panics never\n    *o = r\nend t.put",
    ]
    return out


COMMON = """fn t.mk() -> alloc.string.String uses [alloc] panics never
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    core.mem.drop(s.push_str("x"))
    s
end t.mk

fn t.peek(s: &alloc.string.String) -> usize uses [] panics never
    s.len()
end t.peek"""

MAIN = """fn t.main() -> Result<(), std.io.Error> uses [alloc] panics may
    Ok(())
end t.main"""


def indent(s, n=1):
    return "\n".join(("    " * n + l) if l else l for l in s.split("\n"))


def wrap_borrow(rng, k, inner):
    """借用式を包む。戻り値: (前置の文, 式, 元が内側のとき不健全か) — inner=True なら元は内側のブロックにある。"""
    K = KINDS[k]
    b = K["b"]
    pick = rng.choice(["plain", "id", "reborrow", "let", "fst_b_o", "fst_o_b", "snd_o_b", "snd_b_o", "ifelse"])
    pre = ""
    # outer 側の借用式 (外の o。str は定数)
    o = {"str": '"d"', "u32": "&o", "string": "&o", "mutu32": "&mut p"}[k]
    if pick == "plain":
        return pre, b, True, pick
    if pick == "id":
        return pre, f"t.id({b})", True, pick
    if pick == "reborrow":
        if k == "str":
            return pre, f"&*{b}", True, pick
        return pre, f"&*{b}" if not K["mut"] else f"&mut *{b}", True, pick
    if pick == "let":
        return "let q = " + b, "q", True, pick
    if pick == "fst_b_o":
        return f"let q = t.fst({b}, {o})", "q", True, pick
    if pick == "fst_o_b":
        return f"let q = t.fst({o}, {b})", "q", False, pick
    if pick == "snd_o_b":
        return f"let q = t.snd({o}, {b})", "q", True, pick
    if pick == "snd_b_o":
        return f"let q = t.snd({b}, {o})", "q", False, pick
    if pick == "ifelse":
        return f"let q = if true {{\n    {b}\n}} else {{\n    {o}\n}}", "q", True, pick
    raise AssertionError


def escape(rng, k, inner):
    """1 本の `main`-でない関数 `t.bad` を作る。(src, label, tags)。"""
    K = KINDS[k]
    pre, e, tied, wname = wrap_borrow(rng, k, inner)
    sink = rng.choice([
        "assign", "blockval", "field", "ctor", "option", "tuple", "vecpush", "put", "setter", "closure",
        "retfn", "retstruct", "loopassign", "ifassign", "nested2",
    ])
    decl = K["decl"]
    odecl = OUTER_DECL[k]
    # 元が内側 (inner) のとき、借用は tied (=包んだ結果が元に結びつく) なら不健全
    # 元を外側に置くなら常に健全
    bad = inner and tied
    if "'" in e or True:
        pass
    d = decl if inner else ""
    # 外側に置く場合: 宣言をブロックの外 (関数の先頭) へ
    top = [] if inner else [decl]
    top = ([odecl] if odecl else []) + top
    # fst/snd が使う o と、既定値 `&o`
    needs_o = "&o" in e or "&mut o" in e or "&o" in K["default"]

    def blk(stmts, last=None):
        s = "\n".join(x for x in ([d] if d else []) + ([pre] if pre else []) + stmts if x)
        return s

    use = K["use"]
    dflt = K["default"]
    # outer 変数 (関数の頭に置く): o は常にある (str 以外)
    head = "\n".join(x for x in top if x)
    T = ty(k, "")
    tags = [k, wname, sink, "inner" if inner else "outer"]
    funcs = []
    body = None
    ret = "usize"
    if sink == "assign":
        body = f"let mut r = {dflt}\n{{\n{indent(blk([f'r = {e}']))}\n}}\ncore.mem.drop({use('r')})"
    elif sink == "loopassign":
        body = f"let mut r = {dflt}\nloop {{\n{indent(blk([f'r = {e}']))}\n    break\n}}\ncore.mem.drop({use('r')})"
    elif sink == "ifassign":
        body = f"let mut r = {dflt}\nif true {{\n{indent(blk([f'r = {e}']))}\n}}\ncore.mem.drop({use('r')})"
    elif sink == "nested2":
        body = (f"let mut r = {dflt}\n{{\n    {{\n{indent(blk([f'r = {e}']), 2)}\n    }}\n}}\n"
                f"core.mem.drop({use('r')})")
    elif sink == "blockval":
        body = f"let r = {{\n{indent(blk([e]))}\n}}\ncore.mem.drop({use('r')})"
    elif sink == "field":
        body = (f"let mut h = t.H {{ r: {dflt} }}\n{{\n{indent(blk([f'h.r = {e}']))}\n}}\n"
                f"core.mem.drop({use('h.r')})")
    elif sink == "ctor":
        body = f"let h = {{\n{indent(blk([f't.H {{ r: {e} }}']))}\n}}\ncore.mem.drop({use('h.r')})"
    elif sink == "option":
        body = (f"let o2 = {{\n{indent(blk([f'Some({e})']))}\n}}\n"
                f"match o2 {{\n    Some(r) => core.mem.drop({use('r')})\n    None => ()\n}}")
    elif sink == "tuple":
        body = (f"let pr = {{\n{indent(blk([f'({e}, 1u8)']))}\n}}\n"
                f"let (r, n) = pr\ncore.mem.drop({use('r')})\ncore.mem.drop(n)")
    elif sink == "vecpush":
        vt = f"alloc.vec.Vec<{T}, std.alloc.System>"
        body = (f"let mut v: {vt} = alloc.vec.Vec.new(std.alloc.SYSTEM)\n{{\n"
                f"{indent(blk([f'core.mem.drop(v.push({e}))']))}\n}}\ncore.mem.drop(v.len())\n"
                f"core.mem.drop(v)")
        tags.append("vec-last-use=drop")
    elif sink == "put":
        body = f"let mut r = {dflt}\n{{\n{indent(blk([f't.put(&mut r, {e})']))}\n}}\ncore.mem.drop({use('r')})"
    elif sink == "setter":
        body = (f"let mut h = t.H {{ r: {dflt} }}\n{{\n{indent(blk([f'h.set({e})']))}\n}}\n"
                f"core.mem.drop({use('h.r')})")
        tags.append("A2")
    elif sink == "closure":
        if k == "mutu32":
            return None
        cb = {"str": "x.len()", "u32": "*x", "string": "t.peek(x)"}[k]
        if not inner:
            return None
        body = (f"let f = {{\n{indent(blk([f'fn() -> usize capture [&x] {{ ' + cb.replace('*x', 'usize.from_u32(*x)') + ' }']))}\n}}\n"
                f"core.mem.drop(f())")
        if k == "u32":
            pass
        bad = True
        tags = [k, "closure", "inner"]
    elif sink in ("retfn", "retstruct"):
        # 関数の戻り値として外へ出す。元は引数 (外側) か、関数の局所 (内側)
        lt = "'a"
        if sink == "retfn":
            rty = ty(k, lt)
            ex = e
        else:
            rty = "t.H<'a>"
            ex = f"t.H {{ r: {e} }}"
        if inner:
            # 局所の借用を返す: ヘッダに借用の入力が無いので何らかの診断で落ちるべき
            hdr = "fn t.bad() -> " + (ty(k, "'static") if sink == "retfn" else "t.H<'static>")
            fb = blk([ex])
            return (hdr + f" uses [alloc] panics never\n{indent(fb)}\nend t.bad", "bad", tags + ["header"], k, True)
        # 外側: 引数 x のうち 'a の方を返す。fst/snd は同名を使うので引数名 x/y を使わず直接組み立てる
        return None
    if body is None:
        return None
    fn = f"fn t.bad() -> () uses [alloc] panics never\n{indent(head)}\n" if head else "fn t.bad() -> () uses [alloc] panics never\n"
    if not head:
        fn = "fn t.bad() -> () uses [alloc] panics never\n"
    src = fn + indent(body) + "\nend t.bad"
    import re
    if k == "mutu32" and "&mut p" not in src:
        src = src.replace("    let mut p = 2u32\n", "")
    if "&o" not in src and "&mut o" not in src:
        src = re.sub(r"\n    let (mut )?o = 1u32", "", src)
        src = re.sub(r"\n    let o = t.mk\(\)", "", src)
    # ライフタイム付き関数に載せるのでなく main から呼ぶ
    return src, ("bad" if bad else "ok"), tags, k, False


def header_lifetime(rng, k):
    """ヘッダの 'a / 'b / 'static を通した戻り値 (本体が宣言に収まるか)。A1 の領域。"""
    T = lambda lt: ty(k, lt)
    if k == "string":
        k = "u32"
    ret = rng.choice(["plain", "struct", "option", "tuple", "static"])
    which = rng.choice(["x", "y"])
    ok_name = "x"
    if ret == "static":
        hdr = f"fn t.bad['a]({'mut ' if False else ''}x: {T(chr(39)+'a')}) -> {T('static')}"
        # `'a` を 'static として返す: 不健全
        src = f"{hdr} uses [] panics never\n    x\nend t.bad"
        return src, "bad", [k, "hdr-static"], k, True
    if ret == "plain":
        rt, ex = T("'a"), which
    elif ret == "struct":
        rt, ex = "t.H<'a>", f"t.H {{ r: {which} }}"
    elif ret == "option":
        rt, ex = f"Option<{T(chr(39)+'a')}>", f"Some({which})"
    else:
        rt, ex = f"({T(chr(39)+'a')}, u8)", f"({which}, 1u8)"
    un = "_y" if which == "x" else "_x"
    xn = "x" if which == "x" else "_x"
    yn = "_y" if which == "x" else "y"
    ex = ex.replace(which, which)
    src = (f"fn t.bad['a, 'b]({xn}: {T(chr(39)+'a')}, {yn}: {T(chr(39)+'b')}) -> {rt} uses [] panics never\n"
           f"    {ex}\nend t.bad")
    return src, ("bad" if which == "y" else "ok"), [k, "hdr-" + ret, "A1"], k, True


def mutation(rng, k_):
    """借用中の変更・move・代入。"""
    shape = rng.choice([
        "push_str_after_as_str", "move_after_borrow", "consume_after_borrow", "push_vec_after_slice",
        "assign_after_borrow", "closure_move", "struct_move", "vec_move", "loop_mutate", "option_move",
        "field_move", "mutborrow_twice", "ok_dead_borrow", "ok_reborrow_then_use", "ok_loop_inner",
        "ok_move_after_last_use", "write_while_shared", "ok_disjoint_fields", "bad_disjoint_same",
        "ret_borrow_then_mut", "mut_via_method", "tuple_move", "match_borrow_assign",
    ])
    S = "alloc.string.String"
    new = "alloc.string.String.new(std.alloc.SYSTEM)"
    tg = [shape]
    consume = "fn t.eat(s: alloc.string.String) -> () uses [] panics never\n    core.mem.drop(s)\nend t.eat"
    extra = [consume]
    lab = "bad"
    body = None
    if shape == "push_str_after_as_str":
        body = f"let mut s = {new}\nlet r = s.as_str()\ncore.mem.drop(s.push_str(\"y\"))\ncore.mem.drop(r.len())"
    elif shape == "move_after_borrow":
        body = "let s = t.mk()\nlet r = &s\nlet u = s\ncore.mem.drop(t.peek(r))\ncore.mem.drop(u)"
    elif shape == "consume_after_borrow":
        body = "let s = t.mk()\nlet r = s.as_str()\nt.eat(s)\ncore.mem.drop(r.len())"
    elif shape == "push_vec_after_slice":
        body = ("let mut v: alloc.vec.Vec<u32, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)\n"
                "core.mem.drop(v.push(1u32))\nlet r = v.as_slice()\ncore.mem.drop(v.push(2u32))\n"
                "core.mem.drop(r.len())")
    elif shape == "assign_after_borrow":
        body = "let mut s = t.mk()\nlet r = &s\ns = t.mk()\ncore.mem.drop(t.peek(r))"
    elif shape == "closure_move":
        body = ("let s = t.mk()\nlet r = &s\nlet f = fn() -> () capture [move s] {\n    t.eat(s)\n}\n"
                "f()\ncore.mem.drop(t.peek(r))")
    elif shape == "struct_move":
        extra.append(f"type t.W\n    s: {S}\nend t.W")
        body = "let s = t.mk()\nlet r = &s\nlet w = t.W { s: s }\ncore.mem.drop(t.peek(r))\ncore.mem.drop(w)"
    elif shape == "vec_move":
        body = (f"let s = t.mk()\nlet r = &s\nlet mut v: alloc.vec.Vec<{S}, std.alloc.System> = "
                "alloc.vec.Vec.new(std.alloc.SYSTEM)\ncore.mem.drop(v.push(s))\ncore.mem.drop(t.peek(r))\ncore.mem.drop(v)")
    elif shape == "loop_mutate":
        body = ("let mut s = t.mk()\nlet r = s.as_str()\nloop {\n    core.mem.drop(s.push_str(\"y\"))\n    break\n}\n"
                "core.mem.drop(r.len())")
    elif shape == "option_move":
        body = "let s = t.mk()\nlet r = &s\nlet o = Some(s)\ncore.mem.drop(t.peek(r))\ncore.mem.drop(o)"
    elif shape == "field_move":
        extra.append(f"type t.W\n    s: {S}\nend t.W")
        body = "let w = t.W { s: t.mk() }\nlet r = &w.s\nt.eat(w.s)\ncore.mem.drop(t.peek(r))"
    elif shape == "mutborrow_twice":
        body = "let mut x = 1u32\nlet a = &mut x\nlet b = &mut x\n*a = 2u32\n*b = 3u32"
    elif shape == "write_while_shared":
        body = "let mut x = 1u32\nlet a = &x\nx = 5u32\ncore.mem.drop(*a)"
    elif shape == "tuple_move":
        body = "let s = t.mk()\nlet r = &s\nlet p = (s, 1u8)\ncore.mem.drop(t.peek(r))\ncore.mem.drop(p)"
    elif shape == "bad_disjoint_same":
        extra.append(f"type t.W\n    a: {S}\n    b: {S}\nend t.W")
        body = "let mut w = t.W { a: t.mk(), b: t.mk() }\nlet r = &w.a\nw.a = t.mk()\ncore.mem.drop(t.peek(r))\ncore.mem.drop(w)"
    elif shape == "match_borrow_assign":
        body = ("let mut o = Some(1u32)\nmatch &o {\n    Some(p) => {\n        o = None\n        core.mem.drop(*p)\n    }\n"
                "    None => ()\n}")
    elif shape == "mut_via_method":
        extra.append("type t.C\n    n: u32\nend t.C\n\nfn t.C.bump(self: &mut t.C) -> &u32 uses [] panics never\n    self.n = self.n.wrapping_add(1u32)\n    &self.n\nend t.C.bump")
        body = "let mut c = t.C { n: 0u32 }\nlet r = c.bump()\nlet q = c.bump()\ncore.mem.drop(*r)\ncore.mem.drop(*q)"
    elif shape == "ret_borrow_then_mut":
        extra.append("fn t.first(s: &alloc.string.String) -> &str uses [] panics never\n    s.as_str()\nend t.first")
        body = "let mut s = t.mk()\nlet r = t.first(&s)\ncore.mem.drop(s.push_str(\"y\"))\ncore.mem.drop(r.len())"
    # ok 側
    elif shape == "ok_dead_borrow":
        lab = "ok"
        body = "let mut s = t.mk()\nlet r = s.as_str()\ncore.mem.drop(r.len())\ncore.mem.drop(s.push_str(\"y\"))"
    elif shape == "ok_reborrow_then_use":
        lab = "ok"
        body = "let mut x = 1u32\n{\n    let a = &mut x\n    *a = 2u32\n}\nlet b = &mut x\n*b = 3u32\ncore.mem.drop(x)"
    elif shape == "ok_loop_inner":
        lab = "ok"
        body = ("let mut s = t.mk()\nloop {\n    let r = s.as_str()\n    core.mem.drop(r.len())\n"
                "    core.mem.drop(s.push_str(\"y\"))\n    break\n}\ncore.mem.drop(s)")
    elif shape == "ok_move_after_last_use":
        lab = "ok"
        body = "let s = t.mk()\nlet r = &s\ncore.mem.drop(t.peek(r))\nt.eat(s)"
    elif shape == "ok_disjoint_fields":
        lab = "ok"
        extra.append(f"type t.W\n    a: {S}\n    b: {S}\nend t.W")
        body = "let w = t.W { a: t.mk(), b: t.mk() }\nlet r = &w.a\nt.eat(w.b)\ncore.mem.drop(t.peek(r))\ncore.mem.drop(w.a)"
    if body is None:
        return None
    src = "fn t.bad() -> () uses [alloc] panics never\n" + indent(body) + "\nend t.bad"
    return src, lab, tg + ["mut"], "mut", False, extra


def build(rng, pid):
    """(src, label, tags) を返す。None なら作れなかった。"""
    r = rng.random()
    if r < 0.18:
        res = mutation(rng, None)
        if res is None:
            return None
        src, lab, tags, k, hdr, extra = res
        parts = [src] + extra + [COMMON, MAIN]
        return sorted_defs(parts), lab, tags
    k = rng.choice(list(KINDS))
    if r < 0.30:
        res = header_lifetime(rng, k)
        k_ = k
    else:
        inner = rng.random() < 0.8
        res = escape(rng, k, inner)
    if res is None:
        return None
    src, lab, tags, k, hdr = res[:5]
    parts = [src] + helpers(k) + [COMMON, MAIN]
    d = sorted_defs(parts)
    if any(len(l) > 100 for l in d.split("\n")):
        return None
    return d, lab, tags


def sorted_defs(parts):
    """`ksn fmt` の正規形ではないので、診断のためだけに型を先頭へ。fmt は別に走らせる。"""
    import re

    def name(p):
        return re.match(r"(?:fn|type|enum) ([^\[( \n]+)", p).group(1)

    return "\n\n".join(sorted(parts, key=name)) + "\n"


if __name__ == "__main__":
    import sys
    rng = random.Random(int(sys.argv[1]) if len(sys.argv) > 1 else 1)
    for i in range(int(sys.argv[2]) if len(sys.argv) > 2 else 3):
        b = build(rng, i)
        if b:
            print("//", b[1], b[2])
            print(b[0])
