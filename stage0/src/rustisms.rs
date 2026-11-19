//! Rust 風の書き方の検出 (診断の改善)。
//!
//! Kasane は Rust に似ているが別の言語で、`;` や波括弧の本体、`::`、`as` などを使わない。
//! 構文エラーのとき、これらをソース全体から拾い、すべてまとめて直し方つきで報告する
//! (構文エラーは最初の1件で止まるため、1回に1つずつしか直せなくなるのを避ける)。
//! 文字列・文字リテラルと `//` コメントの中は見ない。

pub struct Hit {
    pub code: &'static str,
    pub line: usize,
    pub col: usize,
    pub msg: String,
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
    for (idx, line) in lines.iter().enumerate() {
        let ln = idx + 1;
        let code = code_part(line);
        let s: String = code.iter().collect();
        let t = s.trim_end();
        if t.trim().is_empty() {
            continue;
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
            hits.push(Hit {
                code: "E0043",
                line: ln,
                col: t.chars().count(),
                msg: format!(
                    "Kasane definitions have no braces: write the header on one line, indent the body by 4 spaces, and close with `end <FQN>` (e.g. `end {}`). Blocks INSIDE the body (`if` / `else` / `while` / `for` / `loop` / `match`) still use `{{ ... }}`",
                    t.trim_start()
                        .split_whitespace()
                        .nth(1)
                        .unwrap_or("app.name")
                        .split(['(', '[', '<'])
                        .next()
                        .unwrap_or("app.name")
                ),
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
                msg: "this line is longer than 100 characters: shorten it (drop a redundant type annotation, or bind a sub-expression with `let`)"
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
        // 構造体リテラルを複数行に折り返している: `bench.Stack {` で行が終わる。
        if t.ends_with('{') && !matches!(first, "if" | "while" | "for" | "loop" | "match" | "else" | "fn" | "impl" | "trait" | "enum" | "type" | "unsafe" | "with") {
            let before = t[..t.len() - 1].trim_end();
            let last = before.rsplit(|c: char| !(c.is_alphanumeric() || c == '_')).next().unwrap_or("");
            if last.chars().next().map_or(false, |c| c.is_uppercase()) {
                hits.push(Hit {
                    code: "E0053",
                    line: ln,
                    col: t.chars().count(),
                    msg: "Kasane struct literals must be on ONE line (they cannot wrap): bind each field to a variable first, then write `bench.Stack { items: items }`"
                        .into(),
                });
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
        // match のアームは行で区切る: 末尾の `,` は不要
        if t.contains("=>") && t.ends_with(',') {
            hits.push(Hit {
                code: "E0049",
                line: ln,
                col: t.chars().count(),
                msg: "Kasane match arms are separated by newlines: remove the trailing `,`".into(),
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
