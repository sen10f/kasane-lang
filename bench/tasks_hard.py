"""ベンチの難しい課題(第2弾)。HashMap・クロージャ・エラー型と ?・再帰・型+メソッド・String の返却など。"""
import re


def _target(name, cases):
    """テストの対象FQN (3.2)。本体が最初に参照する `bench.<名前>` を使い、
    無ければ課題名から作る。`test` の対象FQNは存在する定義でなければならない。"""
    body = "\n".join(l for _, lines in cases for l in lines)
    m = re.search(r"\bbench\.[A-Za-z_][A-Za-z0-9_]*(?:\.[A-Za-z_][A-Za-z0-9_]*)*", body)
    return ".".join(m.group(0).split(".")[:2]) if m else f"bench.{name}"


def t(name, cases):
    tg = _target(name, cases)
    out = []
    for n, lines in cases:
        out.append(f"test {tg}.test_{n}")
        out.extend(f"    {l}" for l in lines)
        out.append(f"end {tg}.test_{n}")
        out.append("")
    return "\n".join(out)


HARD = [
    {
        "id": "word_count",
        "title": "単語数",
        "spec": "文字列を ASCII の空白(スペース・改行・タブ)で区切った単語の個数を返す。連続する空白や前後の空白は無視する。",
        "decl": "fn bench.word_count(text: &str) -> u32",
        "tests": t("word_count", [
            ("basic", ['core.check.eq(bench.word_count("hello world"), 2u32)',
                       'core.check.eq(bench.word_count("  a  b   c "), 3u32)',
                       'core.check.eq(bench.word_count(""), 0u32)',
                       'core.check.eq(bench.word_count("one\\ntwo\\tthree"), 3u32)']),
        ]),
    },
    {
        "id": "palindrome",
        "title": "回文判定",
        "spec": "バイト列として前後から比べ、回文なら true。空文字列と1文字は回文。",
        "decl": "fn bench.is_palindrome(text: &str) -> bool",
        "tests": t("palindrome", [
            ("basic", ['core.check.eq(bench.is_palindrome("racecar"), true)',
                       'core.check.eq(bench.is_palindrome("abca"), false)',
                       'core.check.eq(bench.is_palindrome(""), true)',
                       'core.check.eq(bench.is_palindrome("a"), true)',
                       'core.check.eq(bench.is_palindrome("abba"), true)']),
        ]),
    },
    {
        "id": "distinct",
        "title": "異なる値の個数(HashMap)",
        "spec": "u32 のスライスに含まれる異なる値の個数を、HashMap(キー u32、値 bool)に挿入して数える。"
                "挿入は失敗しうる(Result)ので、失敗したら core.panic.panic(\"oom\") でよい。",
        "decl": "fn bench.distinct_count(xs: &[u32]) -> usize",
        "tests": t("distinct", [
            ("basic", ["core.check.eq(bench.distinct_count(&[1u32, 2u32, 2u32, 3u32, 1u32]), 3usize)",
                       "core.check.eq(bench.distinct_count(&[7u32]), 1usize)",
                       "core.check.eq(bench.distinct_count(&[5u32, 5u32, 5u32]), 1usize)"]),
        ]),
    },
    {
        "id": "fold_squares",
        "title": "平方和(fold とクロージャ)",
        "spec": "0 以上 n 未満の整数の平方の和を、core.iter.fold と core.iter.range とクロージャで求める(ループは使わない)。",
        "decl": "fn bench.sum_of_squares(n: u32) -> u32",
        "tests": t("fold_squares", [
            ("basic", ["core.check.eq(bench.sum_of_squares(0u32), 0u32)", "core.check.eq(bench.sum_of_squares(4u32), 14u32)",
                       "core.check.eq(bench.sum_of_squares(10u32), 285u32)"]),
        ]),
    },
    {
        "id": "digit_sum",
        "title": "エラー型と ? 演算子",
        "spec": "enum bench.ParseError を定義する(バリアント Empty と BadChar(u8)、derive [Debug, PartialEq])。"
                "digit_sum は、数字だけの文字列の各桁の和を Ok で返す。空文字列は Err(Empty)、"
                "数字以外が含まれていたら最初のそのバイトで Err(BadChar(バイト))。"
                "double_digit_sum は digit_sum の結果を ? で受けて 2 倍にして返す(エラーはそのまま返す)。",
        "decl": "enum bench.ParseError: variants Empty, BadChar(u8)\n"
                "fn bench.digit_sum(text: &str) -> Result<u32, bench.ParseError>\n"
                "fn bench.double_digit_sum(text: &str) -> Result<u32, bench.ParseError>",
        "tests": t("digit_sum", [
            ("ok", ['core.check.eq(bench.digit_sum("123"), Ok(6u32))',
                    'core.check.eq(bench.double_digit_sum("123"), Ok(12u32))']),
            ("err", ['core.check.eq(bench.digit_sum(""), Err(bench.ParseError.Empty))',
                     'core.check.eq(bench.digit_sum("1x3"), Err(bench.ParseError.BadChar(120u8)))',
                     'core.check.eq(bench.double_digit_sum(""), Err(bench.ParseError.Empty))']),
        ]),
    },
    {
        "id": "fact",
        "title": "階乗(再帰とオーバーフロー検査)",
        "spec": "n の階乗を再帰で計算し、u64 に収まれば Some、オーバーフローするなら None を返す(0 の階乗は 1)。"
                "オーバーフローは checked_mul で検査する。panic しない関数として宣言すること。",
        "decl": "fn bench.fact_checked(n: u64) -> Option<u64>",
        "tests": t("fact", [
            ("basic", ["core.check.eq(bench.fact_checked(0u64), Some(1u64))",
                       "core.check.eq(bench.fact_checked(5u64), Some(120u64))",
                       "core.check.eq(bench.fact_checked(20u64), Some(2432902008176640000u64))",
                       "core.check.eq(bench.fact_checked(21u64), None)"]),
        ]),
    },
    {
        "id": "stack",
        "title": "スタック(Vec を持つ型)",
        "spec": "type bench.Stack を定義する(フィールド items: alloc.vec.Vec<u32, std.alloc.System>)。"
                "new は空のスタック、push は末尾に追加して成功なら true(確保に失敗したら false)、"
                "pop は末尾を取り出して Some(値)(空なら None)、len は要素数を返す。",
        "decl": "type bench.Stack: one field items of type alloc.vec.Vec<u32, std.alloc.System>\n"
                "fn bench.Stack.new() -> bench.Stack\n"
                "fn bench.Stack.push(self: &mut bench.Stack, v: u32) -> bool\n"
                "fn bench.Stack.pop(self: &mut bench.Stack) -> Option<u32>\n"
                "fn bench.Stack.len(self: &bench.Stack) -> usize",
        "tests": t("stack", [
            ("basic", ["let mut s = bench.Stack.new()", "core.check.eq(s.push(1u32), true)",
                       "core.check.eq(s.push(2u32), true)", "core.check.eq(s.push(3u32), true)",
                       "core.check.eq(s.len(), 3usize)", "core.check.eq(s.pop(), Some(3u32))",
                       "core.check.eq(s.pop(), Some(2u32))", "core.check.eq(s.len(), 1usize)"]),
            ("empty", ["let mut s = bench.Stack.new()", "core.check.eq(s.pop(), None)",
                       "core.check.eq(s.len(), 0usize)"]),
        ]),
    },
    {
        "id": "bsearch",
        "title": "二分探索",
        "spec": "昇順に並んだ i32 のスライスから target を二分探索し、見つかった添字を Some で返す。無ければ None。",
        "decl": "fn bench.bsearch(xs: &[i32], target: i32) -> Option<usize>",
        "tests": t("bsearch", [
            ("found", ["core.check.eq(bench.bsearch(&[1i32, 3i32, 5i32, 7i32, 9i32], 7i32), Some(3usize))",
                       "core.check.eq(bench.bsearch(&[1i32, 3i32, 5i32, 7i32, 9i32], 1i32), Some(0usize))",
                       "core.check.eq(bench.bsearch(&[1i32, 3i32, 5i32, 7i32, 9i32], 9i32), Some(4usize))"]),
            ("missing", ["core.check.eq(bench.bsearch(&[1i32, 3i32, 5i32, 7i32, 9i32], 4i32), None)",
                         "core.check.eq(bench.bsearch(&[2i32], 2i32), Some(0usize))",
                         "core.check.eq(bench.bsearch(&[2i32], 3i32), None)"]),
        ]),
    },
    {
        "id": "repeat_str",
        "title": "文字列の繰り返し(String を返す)",
        "spec": "文字列 s を n 回つなげた String を返す。確保に失敗したら core.panic.panic(\"oom\") でよい。n=0 なら空の String。",
        "decl": "fn bench.repeat_str(s: &str, n: u32) -> alloc.string.String<std.alloc.System>",
        "tests": t("repeat_str", [
            ("basic", ['core.check.eq(bench.repeat_str("ab", 3u32).as_str(), "ababab")',
                       'core.check.eq(bench.repeat_str("x", 0u32).as_str(), "")',
                       'core.check.eq(bench.repeat_str("é", 2u32).as_str(), "éé")']),
        ]),
    },
]
