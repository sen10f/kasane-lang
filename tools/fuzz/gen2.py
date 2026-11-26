"""差分ファジングの生成器 (第 2 版)。

KIR に下ろせる範囲の機能 (整数の演算、構造体の欄の読み書き、`&mut` 渡し、ペイロード付き enum と match、
Option と `?`、Result、Vec の push / get / for、上限付きの while、関数の呼び出し) を、引数と前の値に
依存する計算として組み合わせる。結果は、すべての中間値を畳み込んだ u64 を返す (試されない文を減らす)。
panic はまれに、条件付きでだけ起きる (`+` の溢れ、範囲外の添字)。

`gen_program(rng, pid)` は fuzz.py と同じ形 `(src, params, ret, main_lits)` を返す (行が 100 字を超えたら None)。
"""

MAX_LINE = 100

HELPERS = """type fuzz.P
    a: u64
    b: i64
end fuzz.P

enum fuzz.E
    A(u64)
    B(i64)
    C
end fuzz.E

fn fuzz.bump(s: &mut fuzz.P, k: u64) -> () uses [] panics never
    s.a = s.a.wrapping_add(k)
    s.b = s.b.wrapping_sub(K1i64)
end fuzz.bump

fn fuzz.half(x: u64) -> Option<u64> uses [] panics may
    if x % 2u64 == 0u64 {
        return Some(x / 2u64)
    }
    None
end fuzz.half

fn fuzz.pick(c: u64, x: u64, y: i64) -> fuzz.E uses [] panics may
    if c % 3u64 == 0u64 {
        return fuzz.E.A(x)
    }
    if c % 3u64 == 1u64 {
        return fuzz.E.B(y)
    }
    fuzz.E.C
end fuzz.pick

fn fuzz.q(x: u64) -> Option<u64> uses [] panics may
    let h = fuzz.half(x)?
    Some(h.wrapping_add(K2u64))
end fuzz.q

fn fuzz.sum(s: &fuzz.P) -> u64 uses [] panics never
    s.a ^ fuzz.u(s.b)
end fuzz.sum

fn fuzz.u(x: i64) -> u64 uses [] panics never
    let c = u64.from_u32(x.count_ones()).wrapping_mul(131u64)
    if x < 0i64 {
        return c.wrapping_add(u64.from_u32(x.trailing_zeros())).wrapping_add(K3u64)
    }
    c.wrapping_add(u64.from_u32(x.leading_zeros()))
end fuzz.u

fn fuzz.val(e: fuzz.E) -> u64 uses [] panics never
    match e {
        fuzz.E.A(x) => x
        fuzz.E.B(y) => fuzz.u(y).wrapping_mul(3u64)
        fuzz.E.C => K4u64
    }
end fuzz.val
"""


class Gen:
    def __init__(self, rng):
        self.r = rng
        self.n = 0
        self.u = []  # u64 の名前
        self.i = []  # i64 の名前
        self.fns = []  # 呼べる利用者の関数 (名前)

    def fresh(self, p):
        self.n += 1
        return "%s%d" % (p, self.n)

    def ulit(self):
        r = self.r.random()
        if r < 0.3:
            return "%du64" % self.r.randint(2, 9)
        if r < 0.8:
            return "%du64" % self.r.randint(2, 100000)
        return "%du64" % self.r.choice([2**32 - 1, 2**63, 2**64 - 1, 2**64 - 2, 255, 65536])

    def ilit(self):
        v = self.r.choice([self.r.choice([-100, -7, -2, 2, 3, 100]), self.r.randint(2, 10**9), -2**63, 2**63 - 1])
        return "%di64" % v

    def uleaf(self):
        if self.u and self.r.random() < 0.75:
            return self.r.choice(self.u)
        return self.ulit()

    def ileaf(self):
        if self.i and self.r.random() < 0.7:
            return self.r.choice(self.i)
        return self.ilit()

    def uexpr(self, d=2):
        e = self._uexpr(d)
        return e if len(e) <= 48 else self.uleaf()

    def _uexpr(self, d=2):
        if d == 0 or self.r.random() < 0.3:
            return self.uleaf()
        a, b = self.uexpr(d - 1), self.uexpr(d - 1)
        while b == a:
            b = self.ulit()
        k = self.r.randint(0, 13)
        if k == 0:
            return "%s.wrapping_add(%s)" % (a, b)
        if k == 1:
            return "%s.wrapping_sub(%s)" % (a, b)
        if k == 2:
            return "%s.wrapping_mul(%s)" % (a, b)
        if k == 3:
            return "(%s ^ %s)" % (a, b)
        if k == 4:
            return "(%s & %s)" % (a, b)
        if k == 5:
            return "(%s | %s)" % (a, b)
        if k == 6:
            return "(%s / (%s | 1u64))" % (a, b)
        if k == 7:
            return "(%s %% (%s | 1u64))" % (a, b)
        if k == 8:
            return "%s.wrapping_shl(%s.count_ones())" % (a, b)
        if k == 9:
            return "%s.wrapping_shr(%s.count_ones())" % (a, b)
        if k == 10:
            return "%s.%s(%s)" % (a, self.r.choice(["min", "max", "saturating_sub", "saturating_add"]), b)
        if k == 11:
            return "fuzz.u(%s)" % self.iexpr(d - 1)
        if k == 12 and self.fns:
            return "%s(%s, %s)" % (self.r.choice(self.fns), a, self.iexpr(0))
        return "u64.from_u32(%s.leading_zeros())" % a

    def iexpr(self, d=2):
        e = self._iexpr(d)
        return e if len(e) <= 48 else self.ileaf()

    def _iexpr(self, d=1):
        if d == 0 or self.r.random() < 0.4:
            return self.ileaf()
        a, b = self.iexpr(d - 1), self.iexpr(d - 1)
        while b == a:
            b = self.ilit()
        k = self.r.randint(0, 5)
        if k == 0:
            return "%s.wrapping_add(%s)" % (a, b)
        if k == 1:
            return "%s.wrapping_sub(%s)" % (a, b)
        if k == 2:
            return "%s.wrapping_mul(%s)" % (a, b)
        if k == 3:
            return "%s.saturating_add(%s)" % (a, b)
        if k == 4:
            return "%s.wrapping_neg()" % a
        return "i64.from_u32(%s.count_ones())" % self.uleaf()

    def cond(self):
        a, b = self.uexpr(1), self.uexpr(1)
        while b == a:
            b = self.ulit()
        k = self.r.randint(0, 3)
        if k == 0:
            return "%s < %s" % (a, b)
        if k == 1:
            return "%s == %s" % (a, b)
        if k == 2:
            return "(%s & 1u64) == 0u64" % a
        return "%s > %s" % (self.iexpr(0), self.iexpr(0))

    def acc(self, e):
        return "acc = acc.wrapping_mul(31u64).wrapping_add(%s)" % e

    def stmts(self, ind, n, depth):
        out = []
        for _ in range(n):
            out += self.stmt(ind, depth)
        return out

    def stmt(self, ind, depth):
        for _ in range(8):
            saved = (list(self.u), list(self.i), self.n)
            out = self.stmt1(ind, depth)
            if all(len(l) <= MAX_LINE for l in out):
                return out
            self.u, self.i, self.n = saved
        return [" " * ind + self.acc(self.uleaf())]

    def stmt1(self, ind, depth):
        p = " " * ind
        k = self.r.randint(0, 11)
        if k <= 1:
            v = self.fresh("a")
            e = self.uexpr()
            self.u.append(v)
            return [p + "let %s = %s" % (v, e), p + self.acc(v)]
        if k == 2:
            v = self.fresh("b")
            e = self.iexpr()
            self.i.append(v)
            return [p + "let %s = %s" % (v, e), p + self.acc("fuzz.u(%s)" % v)]
        if k == 3 and depth < 2:
            saved = (list(self.u), list(self.i))
            out = [p + "if %s {" % self.cond()]
            out += self.stmts(ind + 4, self.r.randint(1, 2), depth + 1)
            self.u, self.i = list(saved[0]), list(saved[1])
            out += [p + "} else {"]
            out += self.stmts(ind + 4, 1, depth + 1)
            self.u, self.i = saved
            out += [p + "}"]
            return out
        if k == 4 and depth < 2:
            c = self.fresh("k")
            out = [p + "let mut %s = %s %% 6u64" % (c, self.uexpr(1)), p + "while %s > 0u64 {" % c]
            saved = (list(self.u), list(self.i))
            out += self.stmts(ind + 4, self.r.randint(1, 2), depth + 1)
            self.u, self.i = saved
            out += [p + "    %s = %s - 1u64" % (c, c), p + "}"]
            return out
        if k == 5:
            s = self.fresh("s")
            out = [p + "let mut %s = fuzz.P { a: %s, b: %s }" % (s, self.uexpr(1), self.iexpr(0))]
            out += [p + "fuzz.bump(&mut %s, %s)" % (s, self.uexpr(1))]
            if self.r.random() < 0.5:
                out += [p + "%s.a = %s.a.wrapping_add(%s)" % (s, s, self.uexpr(1))]
            out += [p + self.acc("fuzz.sum(&%s).wrapping_add(%s.a)" % (s, s))]
            return out
        if k == 6:
            e = self.fresh("e")
            out = [p + "let %s = fuzz.pick(%s, %s, %s)" % (e, self.uexpr(1), self.uexpr(0), self.iexpr(0))]
            if self.r.random() < 0.5:
                out += [p + self.acc("fuzz.val(%s)" % e)]
            else:
                xa, yb = self.fresh("m"), self.fresh("m")
                out += [p + "match %s {" % e,
                        p + "    fuzz.E.A(%s) => {" % xa,
                        p + "        " + self.acc(xa),
                        p + "    }",
                        p + "    fuzz.E.B(%s) => {" % yb,
                        p + "        " + self.acc("fuzz.u(%s)" % yb),
                        p + "    }",
                        p + "    fuzz.E.C => ()",
                        p + "}"]
            return out
        if k == 7:
            w = self.fresh("m")
            return [p + "match fuzz.q(%s) {" % self.uexpr(1),
                    p + "    Some(%s) => {" % w,
                    p + "        " + self.acc(w),
                    p + "    }",
                    p + "    None => {",
                    p + "        acc = acc ^ %s" % self.ulit(),
                    p + "    }",
                    p + "}"]
        if k == 8:
            v = self.fresh("v")
            t = "alloc.vec.Vec<u64, std.alloc.System>"
            out = [p + "let mut %s: %s = alloc.vec.Vec.new(std.alloc.SYSTEM)" % (v, t)]
            for _ in range(self.r.randint(0, 4)):
                out += [p + "core.mem.drop(%s.push(%s))" % (v, self.uexpr(1))]
            out += [p + self.acc("u64.from_usize(%s.len())" % v)]
            if self.r.random() < 0.08:
                out += [p + self.acc("%s[%s.len()]" % (v, v))]
            w1, w2 = self.fresh("m"), self.fresh("m")
            out += [p + "match %s.as_slice().get(%s.len() / 2usize) {" % (v, v),
                    p + "    Some(%s) => {" % w1,
                    p + "        " + self.acc("*%s" % w1),
                    p + "    }",
                    p + "    None => ()",
                    p + "}",
                    p + "for %s in %s {" % (w2, v),
                    p + "    " + self.acc(w2),
                    p + "}"]
            return out
        if k == 9 and self.r.random() < 0.15:
            # まれに溢れうる `+` (panic の文言を比べる)
            v = self.fresh("a")
            e = "%s + %s" % (self.uleaf(), self.uleaf())
            self.u.append(v)
            return [p + "let %s = %s" % (v, e), p + self.acc(v)]
        return [p + self.acc(self.uexpr())]

    def func(self, name, params, nstmt):
        self.u = [n for n, t in params if t == "u64"]
        self.i = [n for n, t in params if t == "i64"]
        sig = ", ".join("%s: %s" % x for x in params)
        out = ["fn %s(%s) -> u64 uses [alloc] panics may" % (name, sig), "    let mut acc = %s" % self.ulit()]
        out += self.stmts(4, nstmt, 0)
        for n, t in params:
            if t == "u32":
                out += ["    " + self.acc("u64.from_u32(%s)" % n)]
            elif t == "i64":
                out += ["    " + self.acc("fuzz.u(%s)" % n)]
            else:
                out += ["    " + self.acc(n)]
        out += ["    acc", "end %s" % name]
        return "\n".join(out)


def gen_program(rng, pid):
    g = Gen(rng)
    helpers = HELPERS
    for k in ("K1", "K2", "K3", "K4"):
        helpers = helpers.replace(k, str(rng.randint(0, 50)))
    items = helpers.strip().split("\n\n")
    nf = rng.randint(0, 2)
    for j in range(nf):
        name = "fuzz.f%d" % j
        items.append(g.func(name, [("x", "u64"), ("y", "i64")], rng.randint(2, 5)))
        g.fns.append(name)
    params = [("p0", "u64"), ("p1", "i64"), ("p2", "u32")]
    items.append(g.func("fuzz.target", params, rng.randint(4, 10)))
    lits = [rng.choice([rng.randint(0, 1000), rng.randint(0, 2**64 - 1)]), rng.randint(-1000, 1000), rng.randint(0, 50)]
    main_lits = ["%du64" % lits[0], "%di64" % lits[1], "%du32" % lits[2]]
    items.append("\n".join([
        "fn fuzz.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may",
        "    let mut s = alloc.string.String.new(std.alloc.SYSTEM)",
        "    core.mem.drop(s.push_u64(fuzz.target(%s)))" % ", ".join(main_lits),
        "    std.io.print_line(s.as_str())?",
        "    Ok(())",
        "end fuzz.main",
    ]))

    def key(b):
        first = b.split("\n")[0].split()
        return first[1].split("(")[0].split("[")[0]

    types = [b for b in items if b.startswith(("type ", "enum "))]
    fns = sorted([b for b in items if b.startswith("fn ")], key=lambda b: key(b).encode())
    types.sort(key=lambda b: key(b).encode())
    src = "\n\n".join(types + fns) + "\n"
    if any(len(l) > MAX_LINE for l in src.split("\n")):
        return None
    return src, params, "u64", main_lits
