//! Rust 風の書き方の検出 (診断の改善)。
//!
//! Kasane は Rust に似ているが別の言語で、`;` や波括弧の本体、`::`、`as` などを使わない。
//! 構文エラーのとき、これらをソース全体から拾い、すべてまとめて直し方つきで報告する
//! (構文エラーは最初の1件で止まるため、1回に1つずつしか直せなくなるのを避ける)。
//! 文字列・文字リテラルと `//` コメントの中は見ない。

use crate::grammar::RESERVED;

pub struct Hit {
    pub code: &'static str,
    pub line: usize,
    pub col: usize,
    pub msg: String,
}

/// 定義の名前がモジュールのパスを含まないときの文面 (`module` が無いときは `<module>` を出す)。
/// rustisms の E0062 と lint の E1001 の分流けの両方から使い、文面を 1 つにそろえる。
pub fn name_needs_module_msg(kw: &str, module: Option<&str>, name: &str) -> String {
    let m = module.unwrap_or("<module>");
    format!(
        "a `{}` is named by its full path, module first: write `{} {}.{}(..)` and close it with `end {}.{}` (e.g. `fn lib.util.apply(x: u32) -> u32` / `end lib.util.apply`). The same rule applies to `fn` / `type` / `enum` / `trait` / `test`",
        kw, kw, m, name, m, name
    )
}

/// E0055 (100 文字を超える行) の文面。構造体リテラルを 1 行に置いた行には、その直し方を 1 文足す
/// (構造体リテラルは折り返せないので、字段の値を `let` で先に束縛する)。文法の事前検査
/// (`grammar.rs` の `precheck`) とここで 1 つにそろえる。
pub fn long_line_msg(line: &str) -> String {
    let mut m = "this line is longer than 100 characters: shorten it (drop a redundant type annotation, or bind a sub-expression with `let`)".to_string();
    if let Some(h) = crate::fmt_hoist::suggest(line) {
        m.push_str(&format!("; e.g. {}", h));
    }
    m
}

/// match の腕の末尾の `,` (E0049) を行をまたぐ括弧の深さで追う (`scan` が 1 行ずつ渡す)。
/// リテラルとコメントは空白に置き換えたコード部分 (`code_part`) だけを見る。
#[derive(Default)]
struct Arms {
    /// 開いている波括弧の数。
    brace: usize,
    /// 開いている丸括弧・角括弧の数 (腕の末尾ではない `,` を除く)。
    bracket: usize,
    /// match の `{` を開いた時点の波括弧の数 (腕の深さ)。入れ子の match も持ちうる。
    arm: Vec<usize>,
    /// `match` を見かけて `{` を待っているか (式が次の行に続くこともある)。
    pending: bool,
}

impl Arms {
    /// 1 行を読み、腕の末尾の `,` の位置 (コード部分の 0 始まりの字) を返す。
    fn line(&mut self, cs: &[char]) -> Vec<usize> {
        let mut out = vec![];
        for (i, c) in cs.iter().enumerate() {
            match c {
                '{' => {
                    self.brace += 1;
                    if self.pending {
                        self.arm.push(self.brace);
                        self.pending = false;
                    }
                }
                '}' => {
                    if self.arm.last() == Some(&self.brace) {
                        self.arm.pop();
                    }
                    self.brace = self.brace.saturating_sub(1);
                }
                '(' | '[' => self.bracket += 1,
                ')' | ']' => self.bracket = self.bracket.saturating_sub(1),
                ',' if self.bracket == 0 && self.arm.last() == Some(&self.brace) => out.push(i),
                _ => {}
            }
            if has_match_word(cs, i) {
                self.pending = true;
            }
        }
        out
    }
}

/// `cs[i..]` が 1 語の `match` で始まるか (`.` や識別子の直後にあるものは除く)。
fn has_match_word(cs: &[char], i: usize) -> bool {
    let word: [char; 5] = ['m', 'a', 't', 'c', 'h'];
    if !cs[i..].starts_with(&word) {
        return false;
    }
    let prev = if i == 0 { ' ' } else { cs[i - 1] };
    if prev == '.' || prev.is_alphanumeric() || prev == '_' {
        return false;
    }
    cs.get(i + 5).map_or(true, |n| !(n.is_alphanumeric() || *n == '_'))
}

/// 行から、リテラルとコメントを空白に置き換えたコード部分 (文字数は変えない) を返す。
fn code_part(line: &str) -> Vec<char> {
    let chars: Vec<char> = line.chars().collect();
    let mut out = chars.clone();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '/' && chars.get(i + 1) == Some(&'/') {
            for o in out.iter_mut().skip(i) {
                *o = ' ';
            }
            break;
        }
        if c == '"' {
            let mut j = i + 1;
            while j < chars.len() && chars[j] != '"' {
                if chars[j] == '\\' {
                    j += 1;
                }
                j += 1;
            }
            for k in i..=j.min(chars.len() - 1) {
                out[k] = ' ';
            }
            i = j + 1;
            continue;
        }
        if c == '\'' {
            // 文字リテラル 'x' / '\n' (ライフタイム 'a と区別する)
            let close = if chars.get(i + 1) == Some(&'\\') { i + 3 } else { i + 2 };
            if chars.get(close) == Some(&'\'') {
                for k in i..=close {
                    out[k] = ' ';
                }
                i = close + 1;
                continue;
            }
        }
        i += 1;
    }
    out
}

pub fn scan(text: &str) -> Vec<Hit> {
    let mut hits = vec![];
    let lines: Vec<&str> = text.split('\n').collect();
    // match の腕の末尾の `,` (E0049) を行をまたぐ括弧の深さで追う。
    let mut arms = Arms::default();
    for (idx, line) in lines.iter().enumerate() {
        let ln = idx + 1;
        let code = code_part(line);
        let s: String = code.iter().collect();
        let t = s.trim_end();
        if t.trim().is_empty() {
            continue;
        }
        // match の腕の末尾の `,`: 腕の行 (`=>` がある行) のものと、腕の本体が複数行 (ブロック・
        // 呼び出しの引数) のときの末尾の `,`。`match` の波括弧の 1 つ内側で、括弧の外側にある
        // 行末の `,` だけを腕の末尾と見る (引数リスト・配列・構造体リテラル・タプルの中は除く)。
        for i in arms.line(&code) {
            hits.push(Hit {
                code: "E0049",
                line: ln,
                col: i + 1,
                msg: "Kasane match arms are separated by newlines: remove the trailing `,`".into(),
            });
        }
        let indent = s.len() - s.trim_start().len();
        // 行末の `;`
        if t.ends_with(';') {
            hits.push(Hit {
                code: "E0040",
                line: ln,
                col: t.chars().count(),
                msg: "Kasane statements end at the newline: remove the trailing `;`".into(),
            });
        }
        // `::` (式中のターボフィッシュ `::<` だけは Kasane にもある)
        let cs: Vec<char> = t.chars().collect();
        for i in 0..cs.len().saturating_sub(1) {
            if cs[i] == ':' && cs[i + 1] == ':' && cs.get(i + 2) != Some(&'<') && (i == 0 || cs[i - 1] != ':') {
                hits.push(Hit {
                    code: "E0041",
                    line: ln,
                    col: i + 1,
                    msg: "Kasane paths use `.`, not `::` (e.g. `bench.Shape.Circle(1i32)`)".into(),
                });
            }
        }
        // `as` キャスト
        for (pos, w) in word_positions(&cs) {
            if w == "as" {
                hits.push(Hit {
                    code: "E0042",
                    line: ln,
                    col: pos + 1,
                    msg: "Kasane has no `as` casts: use conversion functions such as `u64.from_u8(x)` (lossless), `u8.try_from_u32(x)` (returns Result), `c.to_u32()` (char)"
                        .into(),
                });
            }
        }
        // シフト演算子 (ジェネリクスの閉じ `>>` は空白なしなので、空白に挟まれたものだけを見る)
        for op in [" >> ", " << "] {
            if let Some(b) = t.find(op) {
                hits.push(Hit {
                    code: "E0044",
                    line: ln,
                    col: s[..b].chars().count() + 2,
                    msg: "Kasane has no `<<` / `>>` operators: use `x.wrapping_shl(n)` / `x.wrapping_shr(n)` (or `checked_shl` / `checked_shr`)"
                        .into(),
                });
                break;
            }
        }
        // 複合代入 (`+=` など)
        for op in ["+=", "-=", "*=", "/=", "%=", "&=", "|=", "^="] {
            if let Some(b) = t.find(op) {
                if t[b + op.len()..].chars().next() != Some('=') {
                    hits.push(Hit {
                        code: "E0045",
                        line: ln,
                        col: s[..b].chars().count() + 1,
                        msg: format!("Kasane has no compound assignment `{}`: write `x = x {} y`", op, &op[..1]),
                    });
                    break;
                }
            }
        }
        // 波括弧の本体: `fn ... {` / `enum X {` / `type X (...)`  と、列 0 の `}`
        let first = t.trim_start().split_whitespace().next().unwrap_or("");
        if indent == 0 && matches!(first, "fn" | "enum" | "type" | "trait" | "impl") && t.ends_with('{') {
            // `impl` は Kasane にもある (1 行の宣言だけで、本体はメソッドの `fn` 側) ので、波括弧の直し方を別に案内する。
            let impl_msg = |tr: &str, ty: &str| {
                format!(
                    "Kasane has no `impl` block: `impl {} for {}` is a ONE-LINE declaration with no body (no `{{`). Write each method as `fn {}.<method>(self: ...) -> .. impls {}` and close it with `end {}.<method>`",
                    tr, ty, ty, tr, ty
                )
            };
            let msg = if first == "impl" {
                let ws: Vec<&str> = t.trim_start().split_whitespace().collect();
                let tr = ws.get(1).unwrap_or(&"Trait").split(['(', '[', '<']).next().unwrap_or("Trait");
                let ty = ws.iter().position(|w| *w == "for").and_then(|i| ws.get(i + 1)).unwrap_or(&"Type").split(['(', '[', '<']).next().unwrap_or("Type");
                impl_msg(tr, ty)
            } else {
                format!(
                    "Kasane definitions have no braces: write the header on one line, indent the body by 4 spaces, and close with `end <FQN>` (e.g. `end {}`). Blocks INSIDE the body (`if` / `else` / `while` / `for` / `loop` / `match`) still use `{{ ... }}`",
                    t.trim_start()
                        .split_whitespace()
                        .nth(1)
                        .unwrap_or("app.name")
                        .split(['(', '[', '<'])
                        .next()
                        .unwrap_or("app.name")
                )
            };
            hits.push(Hit {
                code: "E0043",
                line: ln,
                col: t.chars().count(),
                msg,
            });
        }
        // `fn` の名前がモジュールのパスを含まない (Rust のフリーメソッド)。FQN の検査 (E1001) は通るので
        // 別のコードで、書き方を 1 つに直す指示を出す (`fn` は行頭の語のときだけ見る)。
        {
            let head = t.trim_start();
            if head.starts_with("fn") && head[2..].starts_with(char::is_whitespace) {
                let rest = head[2..].trim_start();
                // 名前は `[A-Za-z0-9_.]+` (パスの区切りも読む)。`.` が無ければモジュールを含まない。
                let n = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.').collect::<String>();
                if !n.is_empty() && !n.contains('.') {
                    let spaces = rest.chars().count() - rest.trim_start().chars().count();
                    let col = head.chars().take_while(|c| c.is_whitespace()).count() + 2 + spaces + 1;
                    hits.push(Hit {
                        code: "E0062",
                        line: ln,
                        col,
                        msg: name_needs_module_msg("fn", None, &n),
                    });
                }
            }
        }
        // 構造体リテラル・構造体パターンのフィールドの省略形 (`bench.Point { x, y }`)。省略形はない。
        // Rust と違って型情報が無いので、`{` の直前の語が大文字始まり (型名) の枠だけを見る
        // (rustisms は構文エラーのファイルでだけ動くので、正しいコードを誤って止めることはない)。
        for at in shorthand_fields(&cs) {
            hits.push(Hit {
                code: "E0061",
                line: ln,
                col: at + 1,
                msg: "Kasane has no field shorthand: every field of a struct literal or a struct pattern is `name: value` (e.g. `bench.Point { x: x, y: y }`)".into(),
            });
        }
        // Rust のクロージャ `|x| ...`: Kasane は `fn(x: T) -> U capture [] { ... }`。
        if let Some(b) = find_pipe_closure(&cs) {
            hits.push(Hit {
                code: "E0050",
                line: ln,
                col: b + 1,
                msg: "Kasane closures are written `fn(x: u32) -> u32 capture [] {` ... `}` (parameter types, return type and `capture [..]` are required), not `|x| ...`"
                    .into(),
            });
        }
        // `Vec<T>::new()` / `String<A>.new()`: 型引数つきのコンストラクタ呼び出しはない。
        if let Some(b) = find_generic_constructor(&cs) {
            hits.push(Hit {
                code: "E0051",
                line: ln,
                col: b + 1,
                msg: "Kasane constructors take no type arguments at the call: write `alloc.vec.Vec.new(std.alloc.SYSTEM)` / `alloc.string.String.new(std.alloc.SYSTEM)` / `alloc.collections.HashMap.new(std.alloc.SYSTEM)` (the allocator is an argument; the element types are inferred)"
                    .into(),
            });
        }
        // 1行ブロック: `{` の直後は必ず改行 (`=> {}` / `fn(..) -> T capture [] { expr }`)。構造体リテラルは除く。
        if let Some(b) = find_inline_block(&cs) {
            hits.push(Hit {
                code: "E0056",
                line: ln,
                col: b + 1,
                msg: "in Kasane a `{` must be followed by a newline (no one-line blocks): put the block body on its own indented lines and close with `}` on its own line; an empty block still needs the two lines"
                    .into(),
            });
        }
        // ファイル先頭の宣言行 (`package x` / `use x` / `uses x` / `panics x`): Kasane のファイルは定義だけを書く。
        if indent == 0 {
            let w = t.trim_start();
            let is_junk = w.starts_with("package ")
                || w.starts_with("use ")
                || w.starts_with("mod ")
                || w.starts_with("import_")
                || (w.starts_with("uses ") && !w.contains('['))
                || w.starts_with("panics ");
            if is_junk {
                hits.push(Hit {
                    code: "E0054",
                    line: ln,
                    col: 1,
                    msg: "Kasane files contain only definitions (no `package` / `use` / `mod` lines); effects are part of each function header, after the return type: `fn bench.f(x: u32) -> u32 uses [] panics never` (required on every function, and it must match the body exactly)"
                        .into(),
                });
            }
        }
        // 行が長すぎる (100 文字超。定義のヘッダ行と文字列を含む行を除く)
        if t.chars().count() > 100 && !t.contains('"') && !matches!(first, "fn" | "type" | "enum" | "trait" | "impl" | "const" | "static" | "extern" | "test" | "import" | "end") {
            hits.push(Hit {
                code: "E0055",
                line: ln,
                col: 101,
                msg: long_line_msg(t)
                .into(),
            });
        }
        // 標準ライブラリの型名を裸で書いている (`Vec` `HashMap` `String` `BTreeMap` `VecDeque` `Box`)。
        for (pos, w) in word_positions(&cs) {
            let bare = pos == 0 || cs[pos - 1] != '.';
            if bare && matches!(w.as_str(), "Vec" | "HashMap" | "String" | "BTreeMap" | "BtreeMap" | "VecDeque" | "Box") {
                hits.push(Hit {
                    code: "E0052",
                    line: ln,
                    col: pos + 1,
                    msg: format!(
                        "`{}` must be written with its full path in Kasane: `alloc.vec.Vec<T, std.alloc.System>`, `alloc.string.String<std.alloc.System>`, `alloc.collections.HashMap<K, V, std.alloc.System>`, `alloc.boxed.Box<T, std.alloc.System>`; constructors take the allocator: `alloc.vec.Vec.new(std.alloc.SYSTEM)`",
                        w
                    ),
                });
                break;
            }
        }
        // `type X (a: T)` / `enum X (A, B)`: 本体は 1 行ずつインデントして書く。
        if indent == 0 && matches!(first, "type" | "enum") {
            let name_end = t.trim_start().find(|c: char| c == '(' || c == '[').unwrap_or(usize::MAX);
            if name_end != usize::MAX && t.trim_start().as_bytes()[name_end] == b'(' {
                let name = t.trim_start().split_whitespace().nth(1).unwrap_or("app.Name");
                let name = name.split(['(', '[']).next().unwrap_or("app.Name");
                hits.push(Hit {
                    code: "E0047",
                    line: ln,
                    col: 1,
                    msg: format!(
                        "a {} has no parenthesized body: write the header alone, then one {} per line indented by 4 spaces, then `end {}` (e.g. `{} {}` / `    {}` / `end {}`)",
                        first,
                        if first == "type" { "field (`name: Type`)" } else { "variant (`Name` or `Name(Type)`)" },
                        name,
                        first,
                        name,
                        if first == "type" { "count: u32" } else { "Circle(i32)" },
                        name
                    ),
                });
            }
        }
        // `Name(field: value)` の形の構造体リテラル
        if let Some(b) = find_paren_struct_literal(&cs) {
            hits.push(Hit {
                code: "E0048",
                line: ln,
                col: b + 1,
                msg: "Kasane struct literals use braces: `bench.Counter { count: 0u32 }` (fields as `name: value`), not parentheses"
                    .into(),
            });
        }
        // match の腕: `=>` の後で改行しない。本体は `=>` と同じ行から始める (ブロックなら `=> {` と書く)。
        if t.ends_with("=>") {
            let next_is_brace = lines.get(idx + 1).map_or(false, |n| n.trim() == "{");
            hits.push(Hit {
                code: "E0057",
                line: ln,
                col: t.chars().count(),
                msg: if next_is_brace {
                    "a match arm's `=>` must be followed by its body on the same line: move the `{` from the next line up to this line (`Some(v) => {`)".into()
                } else {
                    "a match arm's `=>` must be followed by its body on the same line: write `Some(v) => value` or `Some(v) => {` with the `{` on this line".into()
                },
            });
        }
        // match の腕の本体に代入 (`Some(r) => result = r`): 本体は式で、代入はブロックの中に書く。
        if let Some(st) = find_arm_assign(&cs) {
            hits.push(Hit {
                code: "E0058",
                line: ln,
                col: st + 1,
                msg: "a match arm body is an expression, so an assignment cannot follow `=>`: write the arm as a block: `Some(r) => {` on this line, `result = r` indented on the next line, and `}` on its own line".into(),
            });
        }
        // 本体の中の制御構文は波括弧を使う: `if x {` ... `}`。行末の `{` が無い行を指摘する。
        if indent > 0 {
            let head = t.trim_start();
            let kw = head.split(|c: char| !c.is_alphanumeric() && c != '_').next().unwrap_or("");
            if matches!(kw, "if" | "while" | "for" | "loop" | "match" | "else") && !head.ends_with('{') && !head.ends_with(',') && !head.ends_with('(') {
                hits.push(Hit {
                    code: "E0046",
                    line: ln,
                    col: t.chars().count() + 1,
                    msg: format!(
                        "`{}` blocks use braces in Kasane: end this line with `{{` and close the block with an indented `}}` (only definitions are closed by `end <FQN>`)",
                        kw
                    ),
                });
            }
        }
        if indent == 0 && t == "}" {
            hits.push(Hit {
                code: "E0043",
                line: ln,
                col: 1,
                msg: "a lone `}` closes nothing in Kasane: definitions are closed with `end <FQN>`".into(),
            });
        }
        // `type X = T`: Kasane に型の別名はない（`= T` の形は無効。`type` は構造体/列挙子を宣言する）。
        if indent == 0 {
            let w = t.trim_start();
            if w.starts_with("type") && w[4..].chars().next().map_or(false, |c| c.is_whitespace()) {
                let after_kw = &w[4..];
                let lead = after_kw.chars().take_while(|c| c.is_whitespace()).count();
                let name: String = after_kw[lead..]
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.')
                    .collect();
                let after_name = &after_kw[lead + name.len()..];
                if !name.is_empty() && after_name.trim_start().starts_with('=') {
                    let eq_lead = after_name.chars().take_while(|c| c.is_whitespace()).count();
                    let col = 4 + lead + name.len() + eq_lead + 1;
                    hits.push(Hit {
                        code: "E0063",
                        line: ln,
                        col,
                        msg: "Kasane has no type aliases: `type X = T` is not a definition. A `type` declares a struct or enum — write the header alone, then its fields/variants indented by 4 spaces, and close with `end X`.".into(),
                    });
                }
            }
        }
        // `end test X` (`end <予約語>`): `end` の後にはキーワードではなく FQN を書く。
        if indent == 0 {
            let w = t.trim_start();
            if w.starts_with("end") && w[3..].chars().next().map_or(false, |c| c.is_whitespace()) {
                let rest = w[3..].trim_start();
                let word: String = rest
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                if RESERVED.contains(&word.as_str()) {
                    hits.push(Hit {
                        code: "E0064",
                        line: ln,
                        col: 1,
                        msg: format!(
                            "Kasane has no `end {}`: close a definition with `end <FQN>` (the full path from the header, e.g. `end t.foo.test_bar`), not `end {}`",
                            word, word
                        ),
                    });
                }
            }
        }
        // `p.0`: タプルに番号付きの欄はない。分解して使う。
        for i in 0..cs.len().saturating_sub(1) {
            if cs[i] == '.' && cs[i + 1].is_ascii_digit() && (i == 0 || !cs[i - 1].is_ascii_digit()) {
                hits.push(Hit {
                    code: "E0065",
                    line: ln,
                    col: i + 1,
                    msg: "Kasane has no `p.0` tuple field access: tuples have no numbered fields; destructure with `let (a, b) = p` or `match p { (a, b) => .. }`."
                        .into(),
                });
                break;
            }
        }
        // 定義の名前の一部に予約語 (`fn t.end` など)。予約語は名前に使えない。
        if indent == 0 {
            let w = t.trim_start();
            let kw = w
                .split(|c: char| !c.is_alphanumeric() && c != '_')
                .next()
                .unwrap_or("");
            if matches!(
                kw,
                "fn" | "type" | "enum" | "trait" | "test" | "import" | "const" | "static" | "extern"
            ) {
                let after_kw = &w[kw.len()..];
                let lead = after_kw.chars().take_while(|c| c.is_whitespace()).count();
                let name: String = after_kw[lead..]
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.')
                    .collect();
                if !name.is_empty() {
                    let mut seg_off = 0usize;
                    for seg in name.split('.') {
                        if !seg.is_empty() && RESERVED.contains(&seg) {
                            let col = kw.len() + lead + seg_off + 1;
                            hits.push(Hit {
                                code: "E0066",
                                line: ln,
                                col,
                                msg: format!(
                                    "`{}` is a reserved word and cannot be used in a name; use a different name (e.g. `t.finish` instead of `t.end`)",
                                    seg
                                ),
                            });
                            break;
                        }
                        seg_off += seg.len() + 1;
                    }
                }
            }
        }
    }
    hits
}

fn word_positions(cs: &[char]) -> Vec<(usize, String)> {
    let mut out = vec![];
    let mut i = 0;
    while i < cs.len() {
        if cs[i].is_alphanumeric() || cs[i] == '_' {
            let st = i;
            while i < cs.len() && (cs[i].is_alphanumeric() || cs[i] == '_') {
                i += 1;
            }
            out.push((st, cs[st..i].iter().collect()));
        } else {
            i += 1;
        }
    }
    out
}

/// 構造体リテラル・構造体パターンのフィールドの省略形 (`bench.Point { x, y }`) の名前の位置 (0 始まり) を返す。
/// `{` の直前の語が大文字始まり (型名) のときだけ、その中を `,` で区切り、
/// 識別子だけの要素 (`x` ・ `x, y` の `y`) を省略形と見る (`x: x` と書くのが Kasane の唯一の形)。
fn shorthand_fields(cs: &[char]) -> Vec<usize> {
    let mut out = vec![];
    for i in 0..cs.len() {
        if cs[i] != '{' {
            continue;
        }
        let mut k = i;
        while k > 0 && cs[k - 1] == ' ' {
            k -= 1;
        }
        let mut w = k;
        while w > 0 && (cs[w - 1].is_alphanumeric() || cs[w - 1] == '_') {
            w -= 1;
        }
        if w == k || !cs[w].is_uppercase() {
            continue;
        }
        // 対応する `}` までを、括弧の深さを見ながら `,` で区切る (入れ子の `{ a, b }` は外側では 1 要素)。
        let mut depth = 0usize;
        let mut start = i + 1;
        let mut j = i + 1;
        while j < cs.len() {
            match cs[j] {
                '{' | '[' | '(' => depth += 1,
                ']' | ')' => depth = depth.saturating_sub(1),
                '}' if depth == 0 => {
                    push_shorthand(&cs[start..j], start, &mut out);
                    break;
                }
                '}' => depth -= 1,
                ',' if depth == 0 => {
                    push_shorthand(&cs[start..j], start, &mut out);
                    start = j + 1;
                }
                _ => {}
            }
            j += 1;
        }
    }
    out
}

/// 1 要素 (`cs[start..end]`、空白は除く) が識別子だけなら、その位置を拾う。
fn push_shorthand(part: &[char], start: usize, out: &mut Vec<usize>) {
    let lead = part.iter().take_while(|c| **c == ' ').count();
    let tail = part.iter().rev().take_while(|c| **c == ' ').count();
    let mid = part.len().saturating_sub(tail);
    let el = if mid < lead { &part[0..0] } else { &part[lead..mid] };
    // 識別子だけの要素 (`x`・`x, y` の `y`) を省略形と見る (`x: x` と書くのが Kasane の唯一の形)。
    if !el.is_empty() && (el[0].is_alphabetic() || el[0] == '_') && el.iter().all(|c| c.is_alphanumeric() || *c == '_') {
        out.push(start + lead);
    }
}

/// `Path(ident: ...` を探し、`(` の位置を返す (関数呼び出しやヘッダの引数 `f(a: u8)` は除く)。
fn find_paren_struct_literal(cs: &[char]) -> Option<usize> {
    let line: String = cs.iter().collect();
    let t = line.trim_start();
    // 定義のヘッダ (fn / type / enum など) の引数リストは対象外。
    let first = t.split_whitespace().next().unwrap_or("");
    if matches!(first, "fn" | "type" | "enum" | "trait" | "impl" | "const" | "static" | "extern" | "test" | "end" | "import") {
        return None;
    }
    let mut i = 0;
    while i < cs.len() {
        if cs[i] == '(' && i > 0 && (cs[i - 1].is_alphanumeric() || cs[i - 1] == '_') {
            // `(` の後に `ident:` (`::` ではない) が続くか。
            let mut j = i + 1;
            while j < cs.len() && cs[j] == ' ' {
                j += 1;
            }
            let st = j;
            while j < cs.len() && (cs[j].is_alphanumeric() || cs[j] == '_') {
                j += 1;
            }
            if j > st && cs.get(j) == Some(&':') && cs.get(j + 1) != Some(&':') {
                // 直前の語が大文字始まりの型名 (パスの最後の成分) のときだけ。
                let mut k = i;
                while k > 0 && (cs[k - 1].is_alphanumeric() || cs[k - 1] == '_') {
                    k -= 1;
                }
                if cs[k].is_uppercase() {
                    return Some(i);
                }
            }
        }
        i += 1;
    }
    None
}

/// `|x|` / `|acc, x|` 形のクロージャ。`(` `,` `=` の直後の `|` から、次の `|` までが識別子・`,`・`:`・`&` だけのもの。
fn find_pipe_closure(cs: &[char]) -> Option<usize> {
    let mut i = 0;
    while i < cs.len() {
        if cs[i] == '|' && cs.get(i + 1) != Some(&'|') && (i == 0 || cs[i - 1] != '|') {
            let mut k = i;
            while k > 0 && cs[k - 1] == ' ' {
                k -= 1;
            }
            let prev = if k == 0 { None } else { Some(cs[k - 1]) };
            if matches!(prev, Some('(') | Some(',') | Some('=')) {
                let mut j = i + 1;
                while j < cs.len() && (cs[j].is_alphanumeric() || matches!(cs[j], '_' | ',' | ' ' | ':' | '&')) {
                    j += 1;
                }
                if j > i + 1 && cs.get(j) == Some(&'|') {
                    return Some(i);
                }
            }
        }
        i += 1;
    }
    None
}

/// `Name<...>::f(` または `Name<...>.f(` (Name は大文字始まり)。
fn find_generic_constructor(cs: &[char]) -> Option<usize> {
    let mut i = 0;
    while i < cs.len() {
        if cs[i] == '<' && i > 0 && (cs[i - 1].is_alphanumeric() || cs[i - 1] == '_') {
            let mut k = i;
            while k > 0 && (cs[k - 1].is_alphanumeric() || cs[k - 1] == '_') {
                k -= 1;
            }
            if cs[k].is_uppercase() {
                let mut depth = 0;
                let mut j = i;
                while j < cs.len() {
                    match cs[j] {
                        '<' => depth += 1,
                        '>' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    j += 1;
                }
                if j < cs.len() && cs.get(j + 1) == Some(&':') && cs.get(j + 2) == Some(&':') {
                    return Some(k);
                }
                if j < cs.len() && cs.get(j + 1) == Some(&'.') && cs.get(j + 2).map_or(false, |c| c.is_alphabetic()) {
                    // `Name<..>.method(` — 型に対するメソッド呼び出し (式の中)
                    let mut e = j + 2;
                    while e < cs.len() && (cs[e].is_alphanumeric() || cs[e] == '_') {
                        e += 1;
                    }
                    if cs.get(e) == Some(&'(') {
                        return Some(k);
                    }
                }
            }
        }
        i += 1;
    }
    None
}

/// `=>` の直後の `name = e` (代入)。`name` は `a.b` の形も含む。`==` `=>` などの比較・矢印は除く。
/// 代入先の名前の位置 (0 始まり) を返す。
fn find_arm_assign(cs: &[char]) -> Option<usize> {
    let mut i = 0;
    while i + 1 < cs.len() {
        if cs[i] == '=' && cs[i + 1] == '>' {
            let mut j = i + 2;
            while j < cs.len() && cs[j] == ' ' {
                j += 1;
            }
            let st = j;
            if st < cs.len() && (cs[st].is_alphabetic() || cs[st] == '_') {
                while j < cs.len() && (cs[j].is_alphanumeric() || matches!(cs[j], '_' | '.')) {
                    j += 1;
                }
                let mut k = j;
                while k < cs.len() && cs[k] == ' ' {
                    k += 1;
                }
                if cs.get(k) == Some(&'=') && !matches!(cs.get(k + 1), Some('=') | Some('>')) {
                    return Some(st);
                }
            }
            i += 2;
            continue;
        }
        i += 1;
    }
    None
}

/// 同じ行で閉じる `{ ... }` / `{}` (直前の語が大文字始まり = 構造体リテラルは除く)。
fn find_inline_block(cs: &[char]) -> Option<usize> {
    for i in 0..cs.len() {
        if cs[i] != '{' {
            continue;
        }
        let rest: String = cs[i + 1..].iter().collect();
        if rest.trim().is_empty() {
            continue; // 行末の `{` は正しい形
        }
        let mut k = i;
        while k > 0 && cs[k - 1] == ' ' {
            k -= 1;
        }
        let mut w = k;
        while w > 0 && (cs[w - 1].is_alphanumeric() || cs[w - 1] == '_') {
            w -= 1;
        }
        if w < k && cs[w].is_uppercase() {
            continue; // 構造体リテラル
        }
        return Some(i);
    }
    None
}
