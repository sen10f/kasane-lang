"""ベンチの課題。各課題は、説明・必須の宣言(名前と型)・テスト(Kasane の test 定義)からなる。

テストは評価側が追記する。モデルが書くのは実装だけ。パッケージ名は bench、ファイルは src/bench.ksnl。
"""


def _target(name, cases):
    """テストの対象FQN (3.2)。本体が最初に参照する `bench.<名前>` を使い、
    無ければ課題名から作る。`test` の対象FQNは存在する定義でなければならない。"""
    import re

    body = "\n".join(l for _, lines in cases for l in lines)
    m = re.search(r"\bbench\.[A-Za-z_][A-Za-z0-9_]*(?:\.[A-Za-z_][A-Za-z0-9_]*)*", body)
    return ".".join(m.group(0).split(".")[:2]) if m else f"bench.{name}"


def t(name, cases):
    """test 定義を作る。cases は (テスト名, [本体の行]) の配列。"""
    tg = _target(name, cases)
    out = []
    for n, lines in cases:
        out.append(f"test {tg}.test_{n}")
        out.extend(f"    {l}" for l in lines)
        out.append(f"end {tg}.test_{n}")
        out.append("")
    return "\n".join(out)


BASIC = [
    {
        "id": "gcd",
        "title": "最大公約数",
        "spec": "2つの u32 の最大公約数を返す。gcd(0, n) は n。",
        "decl": "fn bench.gcd(a: u32, b: u32) -> u32",
        "tests": t("gcd", [
            ("basic", ["core.check.eq(bench.gcd(12u32, 18u32), 6u32)", "core.check.eq(bench.gcd(7u32, 13u32), 1u32)"]),
            ("zero", ["core.check.eq(bench.gcd(0u32, 5u32), 5u32)", "core.check.eq(bench.gcd(9u32, 0u32), 9u32)"]),
        ]),
    },
    {
        "id": "fib",
        "title": "フィボナッチ",
        "spec": "n 番目のフィボナッチ数を返す(fib(0)=0, fib(1)=1)。ループで計算する。fib(50) までオーバーフローしない。",
        "decl": "fn bench.fib(n: u32) -> u64",
        "tests": t("fib", [
            ("small", ["core.check.eq(bench.fib(0u32), 0u64)", "core.check.eq(bench.fib(1u32), 1u64)",
                       "core.check.eq(bench.fib(10u32), 55u64)"]),
            ("big", ["core.check.eq(bench.fib(50u32), 12586269025u64)"]),
        ]),
    },
    {
        "id": "is_prime",
        "title": "素数判定",
        "spec": "n が素数なら true。0 と 1 は false。",
        "decl": "fn bench.is_prime(n: u32) -> bool",
        "tests": t("is_prime", [
            ("small", ["core.check.eq(bench.is_prime(0u32), false)", "core.check.eq(bench.is_prime(1u32), false)",
                       "core.check.eq(bench.is_prime(2u32), true)", "core.check.eq(bench.is_prime(9u32), false)"]),
            ("big", ["core.check.eq(bench.is_prime(97u32), true)", "core.check.eq(bench.is_prime(7919u32), true)",
                     "core.check.eq(bench.is_prime(7921u32), false)"]),
        ]),
    },
    {
        "id": "collatz",
        "title": "コラッツ予想の停止ステップ数",
        "spec": "n が 1 になるまでに必要なステップ数を返す(偶数なら n/2、奇数なら 3n+1 を 1 ステップとする)。n=1 は 0。",
        "decl": "fn bench.collatz_steps(n: u64) -> u32",
        "tests": t("collatz", [
            ("small", ["core.check.eq(bench.collatz_steps(1u64), 0u32)", "core.check.eq(bench.collatz_steps(6u64), 8u32)"]),
            ("big", ["core.check.eq(bench.collatz_steps(27u64), 111u32)"]),
        ]),
    },
    {
        "id": "count_digits",
        "title": "文字列中の数字の個数",
        "spec": "文字列に含まれる ASCII の数字('0'〜'9')の個数を返す。",
        "decl": "fn bench.count_digits(text: &str) -> u32",
        "tests": t("count_digits", [
            ("basic", ["core.check.eq(bench.count_digits(\"a1b22c\"), 3u32)", "core.check.eq(bench.count_digits(\"\"), 0u32)",
                       "core.check.eq(bench.count_digits(\"abc\"), 0u32)", "core.check.eq(bench.count_digits(\"2024-11-09\"), 8u32)"]),
        ]),
    },
    {
        "id": "max_of",
        "title": "スライスの最大値",
        "spec": "i32 のスライスの最大値を Some で返す。空なら None。",
        "decl": "fn bench.max_of(xs: &[i32]) -> Option<i32>",
        "tests": t("max_of", [
            ("basic", ["core.check.eq(bench.max_of(&[3i32, 9i32, 2i32]), Some(9i32))",
                       "core.check.eq(bench.max_of(&[5i32]), Some(5i32))",
                       "core.check.eq(bench.max_of(&[-4i32, -1i32, -7i32]), Some(-1i32))"]),
            ("empty", ["let xs = [1i32, 2i32]", "core.check.eq(bench.max_of(xs.sub(1usize, 1usize).unwrap()), None)"]),
        ]),
    },
    {
        "id": "shape",
        "title": "図形の面積(enum と match)",
        "spec": "enum bench.Shape を定義する: Circle(i32)(半径)、Rect(i32, i32)(幅と高さ)、Square(i32)(辺)。"
                "area は円を 3*r*r、長方形を w*h、正方形を s*s で返す。enum のバリアントは宣言順に Circle, Rect, Square。",
        "decl": "enum bench.Shape: variants Circle(i32), Rect(i32, i32), Square(i32)\nfn bench.area(s: bench.Shape) -> i32",
        "tests": t("area", [
            ("basic", ["core.check.eq(bench.area(bench.Shape.Rect(3i32, 4i32)), 12i32)",
                       "core.check.eq(bench.area(bench.Shape.Circle(2i32)), 12i32)",
                       "core.check.eq(bench.area(bench.Shape.Square(5i32)), 25i32)"]),
        ]),
    },
    {
        "id": "counter",
        "title": "カウンタ(構造体とメソッド)",
        "spec": "type bench.Counter を定義する(フィールド count: u32)。new は 0 から始め、incr で 1 増やし、get で値を返す。",
        "decl": "type bench.Counter: one field count: u32\n"
                "fn bench.Counter.new() -> bench.Counter\n"
                "fn bench.Counter.incr(self: &mut bench.Counter) -> ()\n"
                "fn bench.Counter.get(self: &bench.Counter) -> u32",
        "tests": t("counter", [
            ("basic", ["let mut c = bench.Counter.new()", "c.incr()", "c.incr()", "c.incr()",
                       "core.check.eq(c.get(), 3u32)"]),
            ("fresh", ["let c = bench.Counter.new()", "core.check.eq(c.get(), 0u32)"]),
        ]),
    },
    {
        "id": "sum_squares",
        "title": "平方和(Vec を使う)",
        "spec": "1 から n までの平方を Vec(要素 u64)に push し、その合計を返す。n=0 なら 0。"
                "push は失敗しうる(Result)ので、失敗したら core.panic.panic(\"oom\") でよい。",
        "decl": "fn bench.sum_squares(n: u32) -> u64",
        "tests": t("sum_squares", [
            ("basic", ["core.check.eq(bench.sum_squares(0u32), 0u64)", "core.check.eq(bench.sum_squares(3u32), 14u64)",
                       "core.check.eq(bench.sum_squares(10u32), 385u64)"]),
        ]),
    },
    {
        "id": "parse_u32",
        "title": "10進数の解析",
        "spec": "文字列を 10 進数の u32 として解析する。数字以外を含む場合と空文字列は None。u32 に収まらない場合も None。",
        "decl": "fn bench.parse_u32(text: &str) -> Option<u32>",
        "tests": t("parse_u32", [
            ("ok", ["core.check.eq(bench.parse_u32(\"123\"), Some(123u32))", "core.check.eq(bench.parse_u32(\"0\"), Some(0u32))",
                    "core.check.eq(bench.parse_u32(\"4294967295\"), Some(4294967295u32))"]),
            ("bad", ["core.check.eq(bench.parse_u32(\"\"), None)", "core.check.eq(bench.parse_u32(\"12a\"), None)",
                     "core.check.eq(bench.parse_u32(\"4294967296\"), None)"]),
        ]),
    },
    {
        "id": "popcount",
        "title": "立っているビットの数",
        "spec": "u32 の値で 1 になっているビットの個数を返す。ビット演算とシフトを使って数える(count_ones は使わない)。",
        "decl": "fn bench.popcount(n: u32) -> u32",
        "tests": t("popcount", [
            ("basic", ["core.check.eq(bench.popcount(0u32), 0u32)", "core.check.eq(bench.popcount(255u32), 8u32)",
                       "core.check.eq(bench.popcount(1023u32), 10u32)", "core.check.eq(bench.popcount(4294967295u32), 32u32)"]),
        ]),
    },
]

from tasks_hard import HARD  # noqa: E402

TASKS = BASIC + HARD
