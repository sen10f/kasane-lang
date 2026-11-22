//! 診断に付ける機械適用可能な修正案 (`fix`) の生成と適用 (仕様 7.1)。
//!
//! 修正は診断の位置とメッセージ、ソース行から作る。範囲は半開区間、行と列は 1 始まり、列は Unicode スカラ値単位。

use regex::Regex;

#[derive(Debug, Clone, PartialEq)]
pub struct Fix {
    pub file: String,
    pub line: usize,
    pub col: usize,
    pub end_line: usize,
    pub end_col: usize,
    pub replace: String,
}

/// 行 `text` の文字位置 (0 始まり) を、バイト位置から求める。
fn char_idx(text: &str, byte: usize) -> usize {
    text[..byte].chars().count()
}

/// 文字列として示された型の先頭の `&` の数 (参照の段数)。`&mut u8` は 1、&&`u8` は 2。
fn ref_depth(ty: &str) -> usize {
    ty.bytes().take_while(|&b| b == b'&').count()
}

/// 参照をちょうど1つ外した型 (`&mut u8` → `u8`)。一致の確認に使う。
fn strip_one_ref(ty: &str) -> &str {
    let t = ty.strip_prefix('&').unwrap_or(ty);
    t.strip_prefix("mut ").unwrap_or(t)
}

/// 診断のある行より上の、最も近い関数ヘッダ行 (`fn ... uses [...] ...`)。行番号は1始まり。
fn caller_header(lines: &[String], line: usize) -> Option<usize> {
    let mut n = line.saturating_sub(1);
    while n > 0 {
        let t = lines.get(n - 1)?;
        if t.contains("uses [") && t.split_whitespace().any(|w| w == "fn") {
            return Some(n);
        }
        n -= 1;
    }
    None
}

/// ヘッダが `pub` ならAPIを変えるので直さない (非pubの関数だけに限る)。
fn is_pub_header(text: &str) -> bool {
    text.split_whitespace().any(|w| w == "pub")
}

/// `line` より上の `let name` (または `let mut name`) を関数境界まで遡って探し、
/// `let name` なら `let mut name` にする (`let mut name` なら何もせず、`None` なら直せない)。
fn add_mut_to_let(file: &str, line: usize, name: &str, lines: &[String]) -> Vec<Fix> {
    let mut n = line.saturating_sub(1);
    while n > 0 {
        let Some(t) = lines.get(n.wrapping_sub(1)) else { break };
        let trimmed = t.trim_start();
        if trimmed.starts_with("fn ") || trimmed.starts_with("end ") {
            break;
        }
        let Some(rest) = trimmed.strip_prefix("let ") else {
            n -= 1;
            continue;
        };
        let (rest, already_mut) = match rest.strip_prefix("mut ") {
            Some(r) => (r, true),
            None => (rest, false),
        };
        if rest.starts_with(name)
            && rest[name.len()..].chars().next().map_or(true, |c| !(c.is_alphanumeric() || c == '_'))
        {
            if already_mut {
                return vec![];
            }
            let indent = t.len() - trimmed.len();
            let b = char_idx(t, indent + 4); // `let ` の後
            return vec![span(file, n, t, b, b, "mut ".to_string())];
        }
        n -= 1;
    }
    vec![]
}

/// `inferred so far: \`T\`` の `T` が、注釈として書いても意味を変えない確かな具体型か。
/// 推論の穴 (`_`、`{integer}` など) や未解決の型変数が含まれるときは直さない。
fn is_concrete_type(ty: &str) -> bool {
    !ty.is_empty()
        && ty != "_"
        && !ty.contains('_')
        && !ty.contains('{')
        && !ty.contains('}')
        && !ty.contains('?')
        && ty
            .chars()
            .all(|c| matches!(c, 'A'..='Z' | 'a'..='z' | '0'..='9' | '_' | '.' | ':' | '<' | '>' | '[' | ']' | ',' | ' ' | '&' | '\''))
}

fn span(file: &str, line: usize, text: &str, from: usize, to: usize, replace: String) -> Fix {
    Fix {
        file: file.to_string(),
        line,
        col: char_idx(text, from) + 1,
        end_line: line,
        end_col: char_idx(text, to) + 1,
        replace,
    }
}

/// `line` より上の `let <name>: <Type> =`（または `let mut <name>: <Type> =`）を関数境界まで遡って探し、
/// `<name>` の型注釈を返す。型注釈が無い（推論任せ）ときは None。
fn as_cast_src_type(lines: &[String], line: usize, name: &str) -> Option<String> {
    let mut n = line.saturating_sub(1);
    while n > 0 {
        let t = lines.get(n.wrapping_sub(1))?;
        let trimmed = t.trim_start();
        if trimmed.starts_with("fn ") || trimmed.starts_with("end ") {
            break;
        }
        let Some(rest) = trimmed.strip_prefix("let ") else {
            n -= 1;
            continue;
        };
        let Some((name_part, ty_part)) = rest.split_once(':') else {
            n -= 1;
            continue;
        };
        let var_name = name_part.trim().strip_prefix("mut ").unwrap_or(name_part.trim());
        if var_name == name {
            let ty_end = ty_part.find('=').unwrap_or(ty_part.len());
            let ty = ty_part[..ty_end].trim();
            if !ty.is_empty() {
                return Some(ty.to_string());
            }
        }
        n -= 1;
    }
    None
}

/// 診断 (`code`, `msg`, 位置 `line`) に対する修正案。無ければ空。
pub fn fixes_for(code: &str, msg: &str, file: &str, line: usize, col: usize, lines: &[String]) -> Vec<Fix> {
    let get = |n: usize| lines.get(n.wrapping_sub(1)).map(|s| s.as_str());
    match code {
        "E1108" => {
            // 整数の `to_<整数型>` (types.rs の int_to_hint): `r.to_u32()` を `u32.from_u8(r)` にする。
            // 受け手は直前の単純な名前だけ、呼び出しはその行に 1 回だけのときに限る。
            let conv = Regex::new(r"unknown method `(to_[a-z0-9]+)` on `[a-z0-9]+`; use `([a-z0-9]+)\.(from_[a-z0-9]+)\(x\)` \(lossless\)").unwrap();
            if let Some(c) = conv.captures(msg) {
                let Some(text) = get(line) else { return vec![] };
                let call = format!(".{}()", &c[1]);
                if text.matches(&call).count() != 1 {
                    return vec![];
                }
                let at = text.find(&call).unwrap();
                let head = &text[..at];
                let ident_len: usize = head.chars().rev().take_while(|c| c.is_alphanumeric() || *c == '_').map(char::len_utf8).sum();
                if ident_len == 0 {
                    return vec![];
                }
                let start = at - ident_len;
                let recv = &head[start..];
                if recv.starts_with(|c: char| c.is_ascii_digit()) || head[..start].ends_with(|c: char| c == '.' || c.is_alphanumeric() || c == '_') {
                    return vec![];
                }
                return vec![span(file, line, text, start, at + call.len(), format!("{}.{}({})", &c[2], &c[3], recv))];
            }
            // unknown method `X`; did you mean `Y`?
            let re = Regex::new(r"unknown method `([^`]+)`; did you mean `([^`]+)`").unwrap();
            let Some(c) = re.captures(msg) else { return vec![] };
            let (bad, good) = (&c[1], &c[2]);
            let Some(text) = get(line) else { return vec![] };
            let needle = format!(".{}", bad);
            let mut from = 0;
            while let Some(p) = text[from..].find(&needle) {
                let start = from + p + 1;
                let end = start + bad.len();
                let after = text[end..].chars().next();
                if !matches!(after, Some(c) if c.is_alphanumeric() || c == '_') {
                    return vec![span(file, line, text, start, end, good.to_string())];
                }
                from = end;
            }
            vec![]
        }
        // 字句の規則 (lexrules.rs): エスケープを唯一の書き方に直す (E0005)、NFC に直す (E0006)。
        "E0005" => {
            let Some(text) = get(line) else { return vec![] };
            let Some(tag) = msg.strip_prefix('[').and_then(|m| m.split(']').next()) else { return vec![] };
            let Some(lit) = crate::lexrules::Lit::of_tag(tag) else { return vec![] };
            let Some((len, rep)) = crate::lexrules::escape_fix(text, col, lit) else { return vec![] };
            let start: usize = text.char_indices().nth(col - 1).map_or(text.len(), |(b, _)| b);
            let end: usize = text.char_indices().nth(col - 1 + len).map_or(text.len(), |(b, _)| b);
            vec![span(file, line, text, start, end, rep)]
        }
        "E0006" => {
            let Some(text) = get(line) else { return vec![] };
            let Some(rep) = crate::lexrules::nfc_fix(text, col) else { return vec![] };
            let start: usize = text.char_indices().nth(col - 1).map_or(text.len(), |(b, _)| b);
            vec![span(file, line, text, start, text.len(), rep)]
        }
        // Rust 風の書き方 (rustisms.rs): フィールドの省略形を `x: x` にする (E0061)、
        // `;` を消す (E0040)、`::` を `.` にする (E0041)。
        "E0061" => {
    let Some(text) = get(line) else { return vec![] };
    let bytes: Vec<(usize, char)> = text.char_indices().collect();
    let Some(&(b0, ch0)) = bytes.get(col.wrapping_sub(1)) else { return vec![] };
    if !(ch0.is_alphabetic() || ch0 == '_') {
        return vec![];
    }
    let mut e = col - 1;
    while bytes.get(e).map_or(false, |&(_, ch)| ch.is_alphanumeric() || ch == '_') {
        e += 1;
    }
    let b1 = bytes.get(e).map_or(text.len(), |&(b, _)| b);
    if b1 <= b0 {
        return vec![];
    }
    let name = &text[b0..b1];
    // 直後が `:` ならもう略形ではない (`x: x` は書けない)。
    if text[b1..].starts_with(':') || text[..b0].trim_end().ends_with('.') {
        return vec![];
    }
    vec![span(file, line, text, b0, b1, format!("{}: {}", name, name))]
}
"E0040" | "E0041" => {
            let Some(text) = get(line) else { return vec![] };
            let (len, rep) = if code == "E0040" { (1, "") } else { (2, ".") };
            let bytes: Vec<(usize, char)> = text.char_indices().collect();
            let Some(&(b0, _)) = bytes.get(col.wrapping_sub(1)) else { return vec![] };
            let b1 = bytes.get(col - 1 + len).map_or(text.len(), |&(b, _)| b);
            vec![span(file, line, text, b0, b1, rep.to_string())]
        }
        // `alloc.x.Y.<ctor>(args)`（アロケータだけが不足）→ `std.x.Y.<ctor>(args)` に短縮する修正。
        // アロケータだけを取るコンストラクタで、`given == want - 1`（他の引数は渡されて
        // いてアロケータだけが欠けている）に限る。
        "E2001" => {
            let re = Regex::new(r"^([^ ]+) takes (\d+) argument\(s\) but (\d+) given").unwrap();
            let Some(c) = re.captures(msg) else { return vec![] };
            let want: usize = c[2].parse().unwrap_or(0);
            let given: usize = c[3].parse().unwrap_or(0);
            if want < 1 || given != want.saturating_sub(1) {
                return vec![];
            }
            let callee = &c[1];
            let Some((alloc_type, ctor)) = callee.rsplit_once('.') else { return vec![] };
            let Some(alloc) = crate::types::ALLOC_ALIASES.iter().find_map(|(_, a, _)| {
                if *a == alloc_type { Some(*a) } else { None }
            }) else {
                return vec![];
            };
            let Some(std) = crate::types::alloc_std_alias(alloc) else { return vec![] };
            if !crate::types::alloc_ctor_takes_allocator_first(alloc, ctor) {
                return vec![];
            }
            let Some(text) = get(line) else { return vec![] };
            let start = col.wrapping_sub(1);
            let Some(start_byte) = text.char_indices().nth(start).map(|(b, _)| b) else { return vec![] };
            let rest = &text[start_byte..];
            let ctor_pat = format!(".{ctor}");
            let Some(dot) = rest.find(&ctor_pat) else { return vec![] };
            let head = &rest[..dot];
            // `alloc.x.Y` の後ろに `::<型引数>` がある場合はそれを残す。
            let (path, targs) = match head.rfind("::<") {
                Some(p) => (&head[..p], &head[p..]),
                None => (head, ""),
            };
            if path != alloc {
                return vec![];
            }
            let replaced_scalars = head.chars().count();
            vec![Fix {
                file: file.to_string(),
                line,
                col,
                end_line: line,
                end_col: col + replaced_scalars,
                replace: format!("{}{}", std, targs),
            }]
        }
        "E1101" => {
            let re = Regex::new(r"unresolved name (\S+); did you mean (\S+)\?").unwrap();
            let Some(c) = re.captures(msg) else { return vec![] };
            let (bad, good) = (&c[1], &c[2]);
            let Some(text) = get(line) else { return vec![] };
            let Some(p) = text.find(bad) else { return vec![] };
            vec![span(file, line, text, p, p + bad.len(), good.to_string())]
        }
        // 接尾辞のない整数リテラル (D12): 推論した型を接尾辞として足す。
        "E2028" => {
            let re = Regex::new(r"append `([a-z0-9]+)`").unwrap();
            let Some(c) = re.captures(msg) else { return vec![] };
            let Some(text) = get(line) else { return vec![] };
            let cs: Vec<(usize, char)> = text.char_indices().collect();
            let Some(&(b0, _)) = cs.get(col.wrapping_sub(1)) else { return vec![] };
            let mut b1 = b0;
            for &(b, ch) in &cs[col - 1..] {
                if ch.is_ascii_alphanumeric() || ch == '_' || (b == b0 && ch == '-') {
                    b1 = b + ch.len_utf8();
                } else {
                    break;
                }
            }
            if b1 == b0 {
                return vec![];
            }
            vec![span(file, line, text, b1, b1, c[1].to_string())]
        }
        // 構造体リテラルの欄を宣言順に並べ替える (D16)。初期化式に副作用・panic がありうる (呼び出し・添字・`?`) ときは直さない。
        "E2048" => struct_order_fix(file, msg, line, col, lines),
        // 受け手のあるメソッドの経路形の呼び出し (D8): `T.m(&x, a)` を `x.m(a)` にする。
        "E2029" => method_call_fix(file, line, col, lines),
        "E0049" => {
            let Some(text) = get(line) else { return vec![] };
            let bytes: Vec<(usize, char)> = text.char_indices().collect();
            let Some(&(b0, _)) = bytes.get(col.wrapping_sub(1)) else { return vec![] };
            vec![span(file, line, text, b0, b0 + 1, String::new())]
        }
        // match の腕: 次の行が `{` だけなら、`=>` の行末から `{` の行末までを ` {` にして改行を消す。
        "E0057" => {
            let (Some(text), Some(next)) = (get(line), get(line + 1)) else { return vec![] };
            if !text.trim_end().ends_with("=>") || next.trim() != "{" {
                return vec![];
            }
            vec![Fix {
                file: file.to_string(),
                line,
                col: text.trim_end().chars().count() + 1,
                end_line: line + 1,
                end_col: next.trim_end().chars().count() + 1,
                replace: " {".to_string(),
            }]
        }
        // match の腕の本体の代入 `Some(r) => result = r` を、腕の行に `{`、次の行に代入、閉じの `}` の 3 行にする。
        "E0058" => {
            let Some(text) = get(line) else { return vec![] };
            let re = Regex::new(r"^(\s*)(.*?=>)\s*([A-Za-z_][A-Za-z0-9_.]*\s*=[^=>].*)$").unwrap();
            let Some(c) = re.captures(text) else { return vec![] };
            let (indent, head, stmt) = (&c[1], &c[2], c[3].trim_end());
            vec![span(
                file,
                line,
                text,
                0,
                text.len(),
                format!("{}{} {{\n{}    {}\n{}}}", indent, head, indent, stmt, indent),
            )]
        }
        "E0045" => {
            let Some(text) = get(line) else { return vec![] };
            let re = Regex::new(r"^(\s*)(.+?)\s*([-+*/%&|^])=\s*(.+?)\s*$").unwrap();
            let Some(c) = re.captures(text) else { return vec![] };
            let new = format!("{}{} = {} {} {}", &c[1], &c[2], &c[2], &c[3], &c[4]);
            vec![span(file, line, text, 0, text.len(), new)]
        }
        // `as` キャスト (E0042): `x as T` を `T.from_src(x)` に書き換える（`x` が整数型で、幅が広がる変換に限る）。
        // `as` は構文エラーなので検査器に届かず、元の型は直上の `let x: Type =` からしか分からない。
        // 縮小・符号変更は `try_from` が `Result` を返すため機械適用不可。float も `from_*` が無いので直さない。
        "E0042" => {
            let Some(text) = get(line) else { return vec![] };
            let cs: Vec<char> = text.chars().collect();
            let as_pos = col.wrapping_sub(1);
            if as_pos + 2 > cs.len() || &cs[as_pos..as_pos + 2] != ['a', 's'] {
                return vec![];
            }
            // `as` の前の空白を飛ばし、さらに前の識別子（ソース変数名）を取り出す（`.` の直後はフィードアクセスなので除く）。
            let mut s = as_pos;
            while s > 0 && cs[s - 1] == ' ' {
                s -= 1;
            }
            let var_end = s;
            while s > 0 && (cs[s - 1].is_alphanumeric() || cs[s - 1] == '_') {
                s -= 1;
            }
            if s == var_end || s > 0 && cs[s - 1] == '.' {
                return vec![];
            }
            let src_var: String = cs[s..var_end].iter().collect();
            // `as` の後の空白を飛ばし、識別子（目標型）を取り出す。
            let mut e = as_pos + 2;
            while e < cs.len() && cs[e] == ' ' {
                e += 1;
            }
            let ty_start = e;
            while e < cs.len() && (cs[e].is_alphanumeric() || cs[e] == '_' || cs[e] == '.') {
                e += 1;
            }
            if e == ty_start {
                return vec![];
            }
            let target: String = cs[ty_start..e].iter().collect();
            // ソース変数の型を、直上の `let <var>: <Type> =` から探す（関数境界まで）。
            let Some(src_ty) = as_cast_src_type(lines, line, &src_var) else {
                return vec![];
            };
            let int = |t: &str| crate::types::INTS.contains(&t);
            if !int(&src_ty) || !int(&target) {
                return vec![];
            }
            let shape = |t: &str| -> (bool, u32) {
                let signed = t.starts_with('i');
                let bits = match &t[1..] {
                    "size" => 64,
                    n => n.parse().unwrap_or(64),
                };
                (signed, bits)
            };
            let ((ws, wb), (gs, gb)) = (shape(&target), shape(&src_ty));
            let widening = (ws == gs && wb >= gb) || (ws && !gs && wb > gb);
            if !widening {
                return vec![];
            }
            let replace = format!("{target}.from_{src_ty}({src_var})");
            vec![span(file, line, text, s, e, replace)]
        }
        // 演算子の片側だけ参照が多い (types.rs の operator_ref_operands): 単純な名前の被演算子に `*` を足す。
        "E2024" => {
            let re = Regex::new(r"^operands of `([^`]+)` are .*\(the (left|right) operand has more references\)").unwrap_or_else(|_| unreachable!());
            let Some(c) = re.captures(msg) else { return vec![] };
            let Some(text) = get(line) else { return vec![] };
            let bytes: Vec<(usize, char)> = text.char_indices().collect();
            let Some(&(b0, _)) = bytes.get(col.wrapping_sub(1)) else { return vec![] };
            let rest = &text[b0..];
            let id = r"[A-Za-z_][A-Za-z0-9_]*";
            let op = regex::escape(&c[1]);
            let at = if &c[2] == "left" {
                // 左の被演算子が単純な名前 (`c >= ..`)。
                let pat = format!(r"^{}\s*{}\s", id, op);
                if !Regex::new(&pat).unwrap().is_match(rest) {
                    return vec![];
                }
                b0
            } else {
                // 右の被演算子が単純な名前 (`.. >= n`)。左は空白を含まない一語に限る。
                let pat = format!(r"^(\S+\s*{}\s*){}\b([^.(\w\[']|$)", op, id);
                let Some(m) = Regex::new(&pat).unwrap().captures(rest) else { return vec![] };
                b0 + m[1].len()
            };
            vec![span(file, line, text, at, at, "*".to_string())]
        }
        "E2054" => {
            // `m.get(&s)` → `m.get_str(s.as_str())`。その行に `.NAME(` が 1 回だけで、引数が `&名前の鎖` か名前の鎖 (`&String` の値) のとき。
            let re = Regex::new(r"`([a-z_]+)` on a `Hash(?:Map|Set)`").unwrap();
            let Some(c) = re.captures(msg) else { return vec![] };
            let name = &c[1];
            let Some(text) = get(line) else { return vec![] };
            let call = format!(".{}(", name);
            if text.matches(&call).count() != 1 {
                return vec![];
            }
            let at = text.find(&call).unwrap();
            let arg_at = at + call.len();
            let arg = Regex::new(r"^(&(?:mut )?)?([A-Za-z_][A-Za-z0-9_]*(?:\.[A-Za-z_][A-Za-z0-9_]*)*)\)").unwrap();
            let Some(m) = arg.captures(&text[arg_at..]) else { return vec![] };
            let whole = m.get(0).unwrap();
            vec![span(file, line, text, at + 1, arg_at + whole.end(), format!("{}_str({}.as_str())", name, &m[2]))]
        }
        "E2053" => {
            // 冗長な型の注釈 (D17): `let x: T = e` の `: T ` を外す (`let x = e`)。1 行の `let` に限る。
            let Some(text) = get(line) else { return vec![] };
            let re = Regex::new(r"^\s*let\s+(?:mut\s+)?[^:=]+(:\s*[^=]+\s)=").unwrap();
            let Some(c) = re.captures(text) else { return vec![] };
            let g = c.get(1).unwrap();
            vec![span(file, line, text, g.start(), g.end(), " ".to_string())]
        }
        "E2041" => {
            let Some(text) = get(line) else { return vec![] };
            let indent = text.len() - text.trim_start().len();
            let Some(expr) = text.trim_start().strip_prefix("let _ = ") else { return vec![] };
            if expr.ends_with(['(', ',', '{']) {
                return vec![];
            }
            vec![span(file, line, text, indent, text.len(), format!("core.mem.drop({})", expr))]
        }
        "E2055" | "E2057" | "E2058" | "E2059" | "E2060" | "E2061" | "E2062" | "E2063" | "E2064" | "E2065" | "E2066" | "E2067" | "E2068" | "E2069" | "E2070" | "E2071" | "E2072" | "E2073" => oneway_fix(code, msg, file, line, col, lines),
        "E2042" => {
            let Some(text) = get(line) else { return vec![] };
            let indent = text.len() - text.trim_start().len();
            let stmt = text.trim_start();
            if stmt.is_empty() || stmt.ends_with(['(', ',', '{']) || stmt.starts_with("//") {
                return vec![];
            }
            vec![span(file, line, text, indent, text.len(), format!("core.mem.drop({})", stmt))]
        }
        "E1020" => {
            let re = Regex::new(r"^unused (parameter|variable) ([a-z][a-z0-9_]*)$").unwrap();
            let Some(c) = re.captures(msg) else { return vec![] };
            let (kind, name) = (&c[1], &c[2]);
            if kind == "parameter" {
                let Some(text) = get(line) else { return vec![] };
                let re = Regex::new(&format!(r"\b{}:", regex::escape(name))).unwrap();
                let Some(m) = re.find(text) else { return vec![] };
                return vec![span(file, line, text, m.start(), m.start() + name.len(), format!("_{}", name))];
            }
            let re = Regex::new(&format!(r"\b(?:let|with) (mut )?({})\b", regex::escape(name))).unwrap();
            let mut n = line;
            while let Some(text) = get(n) {
                if n > line && text.starts_with("end ") {
                    break;
                }
                if let Some(c) = re.captures(text) {
                    let m = c.get(2).unwrap();
                    return vec![span(file, n, text, m.start(), m.end(), format!("_{}", name))];
                }
                n += 1;
            }
            vec![]
        }
        // 効果の完全一致 (D5・D13、tast_rules/effects_exact.rs): 宣言を、メッセージの末尾の ``write `uses [..] panics ..` `` に書き直す。
        // `impls` メソッドのヘッダから `pub` を外す (`pub` はキーワード列の最後、可視性はトレイトと型に従う)。
        "E1123" => {
            let Some(text) = get(line) else { return vec![] };
            let t = text.trim_end();
            let Some(p) = t.rfind(" pub") else { return vec![] };
            let after = &t[p + 4..];
            if !after.chars().all(char::is_whitespace) {
                return vec![];
            }
            let b0 = char_idx(text, p);
            let b1 = text.chars().count();
            if b1 <= b0 {
                return vec![];
            }
            vec![span(file, line, text, b0, b1, String::new())]
        }
        // `mut` でない変数への代入を `let mut` に直す (代入行の名前から `let` 宣言を遡る)。
        // 共有参照越しの代入 (`write through &mut`) は `mut` では直らないので直さない。
        "E3001" => {
            if !msg.contains("is not declared `mut`") {
                return vec![];
            }
            let Some(text) = get(line) else { return vec![] };
            let bytes: Vec<(usize, char)> = text.char_indices().collect();
            let Some(&(b0, _)) = bytes.get(col.wrapping_sub(1)) else { return vec![] };
            let mut e = b0;
            while bytes.get(e).map_or(false, |&(_, ch)| ch.is_alphanumeric() || ch == '_') {
                e += 1;
            }
            if e == b0 {
                return vec![];
            }
            let name = &text[b0..e];
            // 直上から同じ名前の `let <name>` / `let mut <name>` を探す (関数境界で止める)。
            let mut found: Option<(usize, bool)> = None;
            let mut n = line.wrapping_sub(1);
            while n > 0 {
                let Some(t) = get(n) else { break };
                let trimmed = t.trim_start();
                if trimmed.starts_with("fn ") || trimmed.starts_with("end ") {
                    break;
                }
                if trimmed.starts_with("let mut ") || trimmed.starts_with("let ") {
                    let after_let = &trimmed[if trimmed.starts_with("let mut ") { 8 } else { 4 }..];
                    let after_let = after_let.trim_start();
                    if after_let.starts_with(name) {
                        let next = after_let[name.len()..].chars().next();
                        if next.map_or(true, |c| !(c.is_alphanumeric() || c == '_')) {
                            found = Some((n, trimmed.starts_with("let mut ")));
                            break;
                        }
                    }
                }
                n -= 1;
            }
            let Some((decl_line, already_mut)) = found else { return vec![] };
            if already_mut {
                return vec![];
            }
            let dt = get(decl_line).unwrap();
            let indent = dt.len() - dt.trim_start().len();
            let after_indent = &dt[indent..];
            let pref = if after_indent.starts_with("let mut ") { "let mut " } else { "let " };
            let p = indent + pref.len();
            let b = char_idx(dt, p);
            vec![span(file, decl_line, dt, b, b, "mut ".to_string())]
        }
        // ヘッダから ` stack N` を外す (再帰しない `panics never` 関数には不要)。
        "E4011" => {
            let Some(text) = get(line) else { return vec![] };
            let re = Regex::new(r" stack [0-9]+").unwrap();
            let Some(m) = re.find(text) else { return vec![] };
            let mut end_byte = m.end();
            if end_byte < text.len() && text.as_bytes()[end_byte] == b' ' {
                end_byte += 1;
            }
            let b0 = char_idx(text, m.start());
            let b1 = char_idx(text, end_byte);
            if b1 <= b0 {
                return vec![];
            }
            vec![span(file, line, text, b0, b1, String::new())]
        }
        // 構造体リテラルの丸括弧を波括弧に直す: `T(a: 1)` → `T { a: 1 }`。
        "E0048" => {
            let Some(text) = get(line) else { return vec![] };
            let bytes: Vec<(usize, char)> = text.char_indices().collect();
            let Some(&(b0, ch0)) = bytes.get(col.wrapping_sub(1)) else { return vec![] };
            if ch0 != '(' {
                return vec![];
            }
            let mut depth = 0i32;
            let mut close: Option<usize> = None;
            let mut j = b0;
            while j < text.len() {
                match text.as_bytes()[j] {
                    b'(' => depth += 1,
                    b')' => {
                        depth -= 1;
                        if depth == 0 {
                            close = Some(j);
                            break;
                        }
                    }
                    _ => {}
                }
                j += 1;
            }
            let Some(close) = close else { return vec![] };
            let inner = &text[b0 + 1..close];
            if inner.contains(['(', ')', '{', '}']) {
                return vec![];
            }
            let b0c = char_idx(text, b0);
            let b1c = char_idx(text, close + 1);
            vec![span(file, line, text, b0c, b1c, format!(" {{ {} }}", inner.trim()))]
        }
        "E4025" | "E4020" => {
            let Some(text) = get(line) else { return vec![] };
            let Some(want) = msg.rsplit_once("write `").and_then(|(_, r)| r.strip_suffix('`')) else { return vec![] };
            if code == "E4025" {
                let re = Regex::new(r"uses \[[^\]]*\] panics [A-Za-z]+").unwrap();
                let Some(m) = re.find(text) else { return vec![] };
                return vec![span(file, line, text, m.start(), m.end(), want.to_string())];
            }
            // 省略: `stack` / `impls` / `pub` の前 (無ければ行末) に ` uses [..] panics ..` を足す。
            let t = text.trim_end();
            let mut at = t.len();
            for pat in [" stack ", " impls "] {
                if let Some(p) = t.find(pat) {
                    at = at.min(p);
                }
            }
            if at == t.len() && t.ends_with(" pub") {
                at = t.len() - 4;
            }
            vec![span(file, line, text, at, at, format!(" {}", want))]
        }
        // 呼び出しの引数の型が期待型と参照の段数でちょうど1つだけずれる (E2023)。
        // 暗黙の参照の付与・外しは行わない (4.2)。段数の差が1で、それ以外の型が一致するときだけ機械的に直す:
        // 期待側が1段多い → `&x`、実引数側が1段多い → `*x`。行の中の呼び出しが1つだけで、
        // その引数が単純に特定できるときに限る。
        "E2023" => {
            let re = Regex::new(r"argument (\d+) of `([^`]+)`: expected `([^`]*)`, found `([^`]*)`").unwrap();
            let Some(c) = re.captures(msg) else { return vec![] };
            let k = match c[1].parse::<usize>() { Ok(k) if k > 0 => k - 1, _ => return vec![] };
            let want = &c[3];
            let got = &c[4];
            let (dw, dg) = (ref_depth(want), ref_depth(got));
            let kind = if dw == dg + 1 && strip_one_ref(want) == got {
                b'&'
            } else if dg == dw + 1 && strip_one_ref(got) == want {
                b'*'
            } else {
                return vec![]
            };
            let Some(text) = get(line) else { return vec![] };
            let seg = c[2].rsplit('.').next().unwrap_or(&c[2]);
            let needle = format!("{}(", seg);
            let mut occ = text.match_indices(&needle);
            let (Some(first), None) = (occ.next().map(|(p, _)| p), occ.next()) else { return vec![] };
            let open = first + seg.len(); // 行の中の `(` のバイト位置
            // `(` に対応する `)` と、深さ1のカンマを同じ行の中で探す (文字列・文字リテラルの中は飛ばす)。
            let mut depth = 1i32;
            let mut close: Option<usize> = None;
            let mut commas: Vec<usize> = vec![];
            let mut j = open + 1;
            let bs = text.as_bytes();
            while j < bs.len() {
                match bs[j] {
                    b'"' => {
                        j += 1;
                        while j < bs.len() && bs[j] != b'"' {
                            if bs[j] == b'\\' { j += 1; }
                            j += 1;
                        }
                    }
                    b'\'' => {
                        if j + 1 < bs.len() && bs[j + 1] == b'\\' { j += 2; }
                        while j < bs.len() && bs[j] != b'\'' { j += 1; }
                    }
                    b'(' | b'[' | b'{' => depth += 1,
                    b')' | b']' | b'}' => {
                        depth -= 1;
                        if depth == 0 { close = Some(j); break; }
                    }
                    b',' if depth == 1 => commas.push(j),
                    _ => {}
                }
                j += 1;
            }
            let Some(close) = close else { return vec![] };
            // カンマで区切った各引数のトリム済みのバイト位置。
            let mut segs: Vec<(usize, usize)> = vec![];
            let mut from = open + 1;
            for &comma in &commas {
                segs.push((from, comma));
                from = comma + 1;
            }
            segs.push((from, close));
            let mut trim = |(a, b): (usize, usize)| -> Option<(usize, usize)> {
                let mut a = a;
                while a < b && bs[a] == b' ' { a += 1; }
                let mut b = b;
                while b > a && bs[b - 1] == b' ' { b -= 1; }
                (a < b).then_some((a, b))
            };
            let segs: Vec<(usize, usize)> = segs.into_iter().filter_map(trim).collect();
            let Some(&(a0, a1)) = segs.get(k) else { return vec![] };
            let arg = &text[a0..a1];
            vec![span(file, line, text, a0, a1, format!("{}{}", kind as char, arg))]
        }
        // 呼び出し元の `uses` に足りない効果を追加する (E4001)。APIを変えるので `pub` の関数は直さない。
        "E4001" => {
            let re = Regex::new(r"uses effect `([^`]+)`").unwrap();
            let Some(c) = re.captures(msg) else { return vec![] };
            let eff = &c[1];
            let Some(h) = caller_header(lines, line) else { return vec![] };
            let Some(text) = get(h) else { return vec![] };
            if is_pub_header(text) {
                return vec![];
            }
            let re = Regex::new(r"uses \[([^\]]*)\]").unwrap();
            let Some(m) = re.find(text) else { return vec![] };
            let mut effs: Vec<&str> = m.as_str()[6..].trim_end_matches(']').split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
            if effs.iter().any(|&e| e == eff) {
                return vec![];
            }
            effs.push(eff);
            effs.sort();
            // 置き換えの範囲は `uses [...]` 全体なので、`uses ` のキーワードを残す。
            let new = if effs.is_empty() { "uses []".to_string() } else { format!("uses [{}]", effs.join(", ")) };
            let b0 = char_idx(text, m.start());
            let b1 = char_idx(text, m.end());
            vec![span(file, h, text, b0, b1, new)]
        }
        // 呼び出し元の `panics never` を `panics may` にする (E4002)。APIを変えるので `pub` の関数は直さない。
        "E4002" => {
            let Some(h) = caller_header(lines, line) else { return vec![] };
            let Some(text) = get(h) else { return vec![] };
            if is_pub_header(text) {
                return vec![];
            }
            let re = Regex::new(r"\bpanics never\b").unwrap();
            let Some(m) = re.find(text) else { return vec![] };
            let b0 = char_idx(text, m.start());
            let b1 = char_idx(text, m.end());
            vec![span(file, h, text, b0, b1, "panics may".to_string())]
        }
        // `&mut` で使う場所の束縛を `let mut` にする (E3031)。文面から変数名を得て、`let` 宣言を遡る。
        "E3031" => {
            let re = Regex::new(r"declare it with `let mut ([A-Za-z_][A-Za-z0-9_]*)`").unwrap();
            let Some(c) = re.captures(msg) else { return vec![] };
            let name = &c[1];
            add_mut_to_let(file, line, name, lines)
        }
        // 型が決まらない `let` 束縛に、推論された具体型の注釈を足す (E2044)。
        // 束縛が単純な名前で、推論型に穴がないときだけ機械的に直す。
        "E2044" => {
            let re = Regex::new(r"inferred so far: `([^`]+)`").unwrap();
            let Some(c) = re.captures(msg) else { return vec![] };
            let ty = &c[1];
            if !is_concrete_type(ty) {
                return vec![];
            }
            let Some(text) = get(line) else { return vec![] };
            let re = Regex::new(r"^\s*let (?:mut\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*=").unwrap();
            let Some(m) = re.captures(text) else { return vec![] };
            let name_end = m.get(1).unwrap().end();
            vec![span(file, line, text, name_end, name_end, format!(": {}", ty))]
        }
        _ => vec![],
    }
}

/// 同じファイルの修正を適用する。重なるものは後のものを捨てる。適用した件数を返す。
pub fn apply(text: &str, fixes: &[Fix]) -> (String, usize) {
    let lines: Vec<&str> = text.split('\n').collect();
    // 行頭からの文字位置を、テキスト全体のバイト位置へ変換する。
    let mut starts = vec![0usize];
    for l in &lines {
        starts.push(starts.last().unwrap() + l.len() + 1);
    }
    let to_byte = |line: usize, col: usize| -> Option<usize> {
        let l = lines.get(line.checked_sub(1)?)?;
        let mut idx = l.len();
        for (n, (b, _)) in l.char_indices().enumerate() {
            if n + 1 == col {
                idx = b;
                break;
            }
        }
        if col == l.chars().count() + 1 {
            idx = l.len();
        }
        Some(starts[line - 1] + idx)
    };
    let mut spans: Vec<(usize, usize, &str)> = fixes
        .iter()
        .filter_map(|f| Some((to_byte(f.line, f.col)?, to_byte(f.end_line, f.end_col)?, f.replace.as_str())))
        .collect();
    spans.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
    let mut out = String::new();
    let mut pos = 0;
    let mut n = 0;
    for (s, e, r) in spans {
        if s < pos || e < s || e > text.len() {
            continue;
        }
        out.push_str(&text[pos..s]);
        out.push_str(r);
        pos = e;
        n += 1;
    }
    out.push_str(&text[pos..]);
    (out, n)
}

/// `T.m(&x, a, b)` / `m.f(x)` (位置 `line`:`col` から始まる呼び出し) を `x.m(a, b)` にする修正 (D8)。受け手は先頭の `&` `&mut ` を外す。
/// `*r` や演算子を含む受け手は括弧で包む (`(*r).m()` は `ksn fmt` が `r.m()` にする)。複数行の呼び出しも書き換える。
fn method_call_fix(file: &str, line: usize, col: usize, lines: &[String]) -> Vec<Fix> {
    let mut cs: Vec<(char, usize, usize)> = vec![];
    for (k, l) in lines.iter().enumerate().skip(line.saturating_sub(1)) {
        for (i, ch) in l.chars().enumerate() {
            if k + 1 == line && i + 1 < col {
                continue;
            }
            cs.push((ch, k + 1, i + 1));
        }
        cs.push(('\n', k + 1, l.chars().count() + 1));
        if cs.len() > 6000 {
            break;
        }
    }
    let mut i = 0;
    while i < cs.len() && (cs[i].0.is_ascii_alphanumeric() || cs[i].0 == '_' || cs[i].0 == '.') {
        i += 1;
    }
    if i == 0 || i >= cs.len() || cs[i].0 != '(' {
        return vec![];
    }
    let path: String = cs[..i].iter().map(|c| c.0).collect();
    let Some(m) = path.rsplit('.').next().filter(|m| !m.is_empty() && path.contains('.')) else { return vec![] };
    let open = i;
    // 引数の区切り (深さ 1 のカンマ) と閉じ括弧を探す。文字列・文字リテラルの中は飛ばす。
    let mut depth = 0i32;
    let mut commas: Vec<usize> = vec![];
    let mut close = None;
    let mut j = open;
    while j < cs.len() {
        let ch = cs[j].0;
        match ch {
            '"' => {
                j += 1;
                while j < cs.len() && cs[j].0 != '"' {
                    if cs[j].0 == '\\' {
                        j += 1;
                    }
                    j += 1;
                }
            }
            '\'' => {
                if cs.get(j + 1).map_or(false, |c| c.0 == '\\') {
                    j += 2;
                    while j < cs.len() && cs[j].0 != '\'' {
                        j += 1;
                    }
                } else if cs.get(j + 2).map_or(false, |c| c.0 == '\'') {
                    j += 2;
                }
            }
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(j);
                    break;
                }
            }
            ',' if depth == 1 => commas.push(j),
            _ => {}
        }
        j += 1;
    }
    let Some(close) = close else { return vec![] };
    let txt = |a: usize, b: usize| -> String { cs[a..b].iter().map(|c| c.0).collect() };
    let first_end = commas.first().copied().unwrap_or(close);
    let a0 = txt(open + 1, first_end);
    let a0 = a0.trim();
    if a0.is_empty() {
        return vec![];
    }
    let recv = a0.strip_prefix("&mut ").or_else(|| a0.strip_prefix('&')).unwrap_or(a0).trim_start().to_string();
    let needs = {
        let mut d = 0i32;
        let mut in_str = false;
        let mut flag = recv.starts_with(|c| matches!(c, '*' | '-' | '!' | '&'));
        let rc: Vec<char> = recv.chars().collect();
        let mut k = 0;
        while k < rc.len() {
            let c = rc[k];
            if in_str {
                if c == '\\' {
                    k += 1;
                } else if c == '"' {
                    in_str = false;
                }
            } else if c == '"' {
                in_str = true;
            } else if matches!(c, '(' | '[' | '{') {
                d += 1;
            } else if matches!(c, ')' | ']' | '}') {
                d -= 1;
            } else if d == 0 && (matches!(c, '+' | '*' | '/' | '%' | '|' | '^' | '<' | '>' | '=') || (c == ' ' && rc.get(k + 1) == Some(&'i') && rc.get(k + 2) == Some(&'s'))) {
                flag = true;
            } else if d == 0 && matches!(c, '-' | '&' | '!') && k > 0 {
                flag = true;
            }
            k += 1;
        }
        flag
    };
    let rest = if commas.is_empty() { String::new() } else { txt(commas[0] + 1, close).trim_start_matches(' ').to_string() };
    let rest = rest.trim_end_matches(|c: char| c == ',').to_string();
    let recv = if needs { format!("({})", recv) } else { recv };
    let call = format!("{}.{}({})", recv, m, rest.trim_end());
    let (el, ec) = (cs[close].1, cs[close].2 + 1);
    vec![Fix { file: file.to_string(), line, col, end_line: el, end_col: ec, replace: call }]
}

/// `T { b: e2, a: e1 }` を宣言順 (メッセージの `: a, b` の並び) にする修正 (D16)。リテラルは 1 行 (折り返せない)。
fn struct_order_fix(file: &str, msg: &str, line: usize, col: usize, lines: &[String]) -> Vec<Fix> {
    let Some(order) = msg.rsplit_once(": ").map(|(_, o)| o.split(", ").map(String::from).collect::<Vec<String>>()) else { return vec![] };
    let Some(text) = lines.get(line.wrapping_sub(1)) else { return vec![] };
    let cs: Vec<char> = text.chars().collect();
    let Some(open) = (col.saturating_sub(1)..cs.len()).find(|&i| cs[i] == '{') else { return vec![] };
    // 欄 (深さ 1 のカンマ区切り) を集める。文字列・文字リテラルの中は飛ばす。
    let mut depth = 0i32;
    let mut items: Vec<(usize, usize)> = vec![];
    let mut from = open + 1;
    let mut close = None;
    let mut j = open;
    while j < cs.len() {
        match cs[j] {
            '"' => {
                j += 1;
                while j < cs.len() && cs[j] != '"' {
                    if cs[j] == '\\' {
                        j += 1;
                    }
                    j += 1;
                }
            }
            '\'' => {
                if cs.get(j + 1) == Some(&'\\') {
                    j += 2;
                    while j < cs.len() && cs[j] != '\'' {
                        j += 1;
                    }
                } else if cs.get(j + 2) == Some(&'\'') {
                    j += 2;
                }
            }
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
                depth -= 1;
                if depth == 0 {
                    items.push((from, j));
                    close = Some(j);
                    break;
                }
            }
            ',' if depth == 1 => {
                items.push((from, j));
                from = j + 1;
            }
            _ => {}
        }
        j += 1;
    }
    let Some(close) = close else { return vec![] };
    let mut by_name: Vec<(String, String)> = vec![];
    for (a, b) in items {
        let seg: String = cs[a..b].iter().collect();
        let seg = seg.trim().to_string();
        if seg.is_empty() {
            continue;
        }
        let Some((n, init)) = seg.split_once(':') else { return vec![] };
        // 副作用・panic がありうる式 (呼び出し・添字・`?`・ブロック・クロージャ) があれば直さない。
        let ic: Vec<char> = init.chars().collect();
        for (k, c) in ic.iter().enumerate() {
            let prev = if k > 0 { ic[k - 1] } else { ' ' };
            if matches!(c, '(' | '[') && (prev.is_alphanumeric() || matches!(prev, '_' | ')' | ']' | '>')) || matches!(c, '?' | '{') {
                return vec![];
            }
        }
        by_name.push((n.trim().to_string(), init.trim().to_string()));
    }
    let mut out: Vec<String> = vec![];
    for n in &order {
        let Some((_, init)) = by_name.iter().find(|(x, _)| x == n) else { return vec![] };
        out.push(format!("{}: {}", n, init));
    }
    if out.len() != by_name.len() {
        return vec![];
    }
    let new = format!("{{ {} }}", out.join(", "));
    vec![Fix { file: file.to_string(), line, col: open + 1, end_line: line, end_col: close + 2, replace: new }]
}

/// 書き方は 1 通りの規則 (規則 O、`tast_rules/oneway.rs`) の直し。行の中で一意に決まる形だけ直す (それ以外は直さず診断だけ残す)。
fn oneway_fix(code: &str, msg: &str, file: &str, line: usize, col: usize, lines: &[String]) -> Vec<Fix> {
    let Some(text) = lines.get(line.wrapping_sub(1)).map(|s| s.as_str()) else { return vec![] };
    let ident_before = |at: usize| -> usize {
        let head = &text[..at];
        head.chars().rev().take_while(|c| c.is_alphanumeric() || *c == '_').map(char::len_utf8).sum()
    };
    match code {
        "E2057" => {
            let call = "core.panic.todo()";
            if text.matches(call).count() != 1 {
                return vec![];
            }
            let at = text.find(call).unwrap();
            vec![span(file, line, text, at, at + call.len(), "core.panic.panic(\"todo\")".into())]
        }
        "E2058" => {
            let Some(old) = ["sort_unstable_by", "sort_unstable"].into_iter().find(|n| msg.contains(&format!("`{n}`"))) else { return vec![] };
            let pat = format!("{old}(");
            if text.matches(&pat).count() != 1 {
                return vec![];
            }
            let at = text.find(&pat).unwrap();
            if text[..at].ends_with(|c: char| c.is_alphanumeric() || c == '_') {
                return vec![];
            }
            let new = old.replacen("sort_unstable", "sort", 1);
            vec![span(file, line, text, at, at + old.len(), new)]
        }
        "E2059" => {
            let call = "core.check.is_true(";
            if text.matches(call).count() != 1 {
                return vec![];
            }
            let start = text.find(call).unwrap();
            let open = start + call.len();
            let mut depth = 1usize;
            let mut close = None;
            let mut eqs = vec![];
            for (i, c) in text[open..].char_indices() {
                match c {
                    '(' | '[' | '{' => depth += 1,
                    ')' | ']' | '}' => {
                        depth -= 1;
                        if depth == 0 {
                            close = Some(open + i);
                            break;
                        }
                    }
                    '"' => return vec![],
                    _ => {}
                }
            }
            let Some(close) = close else { return vec![] };
            // 括弧の中の `==` は深さ 1 の 1 か所だけ。
            let inner = &text[open..close];
            let mut d = 0i32;
            let b = inner.as_bytes();
            let mut i = 0;
            while i < b.len() {
                match b[i] {
                    b'(' | b'[' | b'{' => d += 1,
                    b')' | b']' | b'}' => d -= 1,
                    b'=' if d == 0 && i + 1 < b.len() && b[i + 1] == b'=' => {
                        eqs.push(i);
                        i += 1;
                    }
                    _ => {}
                }
                i += 1;
            }
            if eqs.len() != 1 {
                return vec![];
            }
            let (a, c) = (inner[..eqs[0]].trim(), inner[eqs[0] + 2..].trim());
            if a.is_empty() || c.is_empty() {
                return vec![];
            }
            vec![span(file, line, text, start, close + 1, format!("core.check.eq({a}, {c})"))]
        }
        "E2060" => {
            for pat in [".trim_start().trim_end()", ".trim_end().trim_start()"] {
                if text.matches(pat).count() == 1 {
                    let at = text.find(pat).unwrap();
                    return vec![span(file, line, text, at, at + pat.len(), ".trim()".into())];
                }
            }
            vec![]
        }
        "E2061" if msg.contains("alloc.fmt.write") => {
            let head = "alloc.fmt.write(&mut ";
            if text.matches(head).count() != 1 {
                return vec![];
            }
            let at = text.find(head).unwrap();
            let rest = &text[at + head.len()..];
            let Some(c) = rest.find(", &\"") else { return vec![] };
            let name = &rest[..c];
            let lit_from = c + 4;
            let Some(rel) = rest[lit_from..].find('"') else { return vec![] };
            let lit = &rest[lit_from..lit_from + rel];
            if name.is_empty() || name.contains(['(', ')', ',', ' ']) || lit.contains('\\') || !rest[lit_from + rel + 1..].starts_with(')') {
                return vec![];
            }
            vec![span(file, line, text, at, at + head.len() + lit_from + rel + 2, format!("{name}.push_str(\"{lit}\")"))]
        }
        "E2061" => {
            let head = ".push_display(&\"";
            if text.matches(head).count() != 1 {
                return vec![];
            }
            let at = text.find(head).unwrap();
            let lit_from = at + head.len();
            let Some(rel) = text[lit_from..].find('"') else { return vec![] };
            let lit_end = lit_from + rel; // 閉じの `"` の位置
            if !text[lit_end + 1..].starts_with(')') || text[lit_from..lit_end].contains('\\') {
                return vec![];
            }
            let body = &text[lit_from..lit_end];
            vec![span(file, line, text, at, lit_end + 2, format!(".push_str(\"{body}\")"))]
        }
        "E2065" => {
            if text.matches(".clone()").count() != 1 {
                return vec![];
            }
            let at = text.find(".clone()").unwrap();
            // 受け手は名前と `.` だけ。
            let n = text[..at].chars().rev().take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.').map(char::len_utf8).sum::<usize>();
            if n == 0 || text[..at - n].ends_with(|c: char| c.is_alphanumeric() || c == '_' || c == ')' || c == ']') {
                return vec![];
            }
            vec![span(file, line, text, at, at + 8, String::new())]
        }
        "E2066" => {
            let Some(c) = ["<=", ">=", "<", ">"].into_iter().find(|c| msg.contains(&format!("`!(a {c} b)`"))) else { return vec![] };
            let new = match c { "<" => ">=", "<=" => ">", ">" => "<=", _ => "<" };
            if text.matches("!(").count() != 1 {
                return vec![];
            }
            let start = text.find("!(").unwrap();
            let open = start + 2;
            let mut depth = 1i32;
            let mut close = None;
            let mut ops = vec![];
            let b = text.as_bytes();
            for i in open..b.len() {
                match b[i] {
                    b'(' | b'[' | b'{' => depth += 1,
                    b')' | b']' | b'}' => {
                        depth -= 1;
                        if depth == 0 {
                            close = Some(i);
                            break;
                        }
                    }
                    b'"' | b'\'' | b'&' | b'|' => return vec![],
                    b'<' | b'>' if depth == 1 => ops.push(i),
                    _ => {}
                }
            }
            let Some(close) = close else { return vec![] };
            let inner = &text[open..close];
            let Some(rel) = inner.find(&format!(" {c} ")) else { return vec![] };
            if inner.matches(&format!(" {c} ")).count() != 1 || ops.len() != 1 || inner.contains("=>") || inner.contains("->") || inner.contains("::<") {
                return vec![];
            }
            let (a, r) = (&inner[..rel], &inner[rel + c.len() + 2..]);
            // 周りは式の頭と終わり (`if` `&&` `||` `(` `=` `,` の後、行の終わり `{` `&&` `||` `)` `,` の前)。
            let before = text[..start].trim_end();
            let after = text[close + 1..].trim_start();
            let ok_before = before.is_empty() || ["if", "while", "(", "=", "&&", "||", ",", "return"].iter().any(|w| before.ends_with(w));
            let ok_after = after.is_empty() || after.starts_with(['{', ')', ',']) || after.starts_with("&&") || after.starts_with("||");
            if !ok_before || !ok_after || a.is_empty() || r.is_empty() {
                return vec![];
            }
            vec![span(file, line, text, start, close + 1, format!("{a} {new} {r}"))]
        }
        "E2067" => {
            // `col` は oneway 側で特定した `::<` の列 (同じ行に複数の `::<` があるときの修正対象)。
            // 列不明 (0) のときは、`::<` が 1 つだけの行だけ直す (従来どおり)。
            let at: Option<usize> = if col > 0 {
                text.char_indices()
                    .nth(col.wrapping_sub(1))
                    .and_then(|(b, _)| text[b..].starts_with("::<").then_some(b))
            } else {
                None
            };
            let at = match at {
                Some(a) => a,
                None => {
                    if text.matches("::<").count() != 1 {
                        return vec![];
                    }
                    text.find("::<").unwrap()
                }
            };
            let mut depth = 0i32;
            for (i, c) in text[at + 2..].char_indices() {
                match c {
                    '<' => depth += 1,
                    '>' => {
                        depth -= 1;
                        if depth == 0 {
                            let end = at + 2 + i + 1;
                            if !text[end..].starts_with('(') {
                                return vec![];
                            }
                            return vec![span(file, line, text, at, end, String::new())];
                        }
                    }
                    _ => {}
                }
            }
            vec![]
        }
        "E2062" if msg.starts_with('[') => identity_fix(file, line, msg, text),
        "E2072" => {
            let Some(name) = msg.strip_prefix("`let mut ").and_then(|r| r.split('`').next()) else { return vec![] };
            let pat = format!("let mut {name}");
            let Some(at) = text.find(&pat) else { return vec![] };
            if text.matches(&pat).count() != 1 || text[at + pat.len()..].starts_with(|c: char| c.is_alphanumeric() || c == '_') || !text[..at].trim().is_empty() {
                return vec![];
            }
            vec![span(file, line, text, at + 4, at + 8, String::new())]
        }
        "E2073" => closure_fix(file, line, lines),
        "E2068" | "E2069" => match_fix(code, file, line, lines),
        "E2070" => while_fix(file, line, msg, lines),
        "E2071" => some_lit_fix(file, line, msg, text),
        "E2062" => {
            let Some(name) = ["wrapping_add", "wrapping_sub", "wrapping_mul"].into_iter().find(|n| msg.contains(&format!("`.{n}(..)`"))) else { return vec![] };
            let lit = if name == "wrapping_mul" { "1" } else { "0" };
            let head = format!(".{name}({lit}");
            if text.matches(&head).count() != 1 {
                return vec![];
            }
            let at = text.find(&head).unwrap();
            let rest = &text[at + head.len()..];
            let suffix_len = rest.chars().take_while(|c| c.is_alphanumeric()).map(char::len_utf8).sum::<usize>();
            let after = &rest[suffix_len..];
            if !after.starts_with(')') || rest[..suffix_len].starts_with(|c: char| c.is_ascii_digit()) {
                return vec![];
            }
            vec![span(file, line, text, at, at + head.len() + suffix_len + 1, String::new())]
        }
        "E2064" => {
            let head = ".from_str_radix(";
            if text.matches(head).count() != 1 {
                return vec![];
            }
            let at = text.find(head).unwrap();
            // `T.from_str_radix(arg, 10)` → `T.parse(arg)`。引数は 1 つめに `,` を含まない形だけ。
            let rest = &text[at + head.len()..];
            let Some(close) = rest.find(')') else { return vec![] };
            let inner = &rest[..close];
            let Some((a, r)) = inner.split_once(", ") else { return vec![] };
            let rn = r.trim_end_matches(|c: char| c.is_alphabetic() || c.is_ascii_digit() && false);
            if a.contains(['(', ')', ',']) || !(rn == "10" || r.starts_with("10") && r[2..].chars().all(|c| c.is_alphanumeric())) {
                return vec![];
            }
            vec![span(file, line, text, at, at + head.len() + close + 1, format!(".parse({a})"))]
        }
        "E2063" => {
            let head = ".then_some(";
            if text.matches(head).count() != 1 {
                return vec![];
            }
            let at = text.find(head).unwrap();
            let n = ident_before(at);
            if n == 0 || text[..at - n].ends_with(|c: char| c == '.' || c == '_' || c.is_alphanumeric()) {
                return vec![];
            }
            let start = at - n;
            let rest = &text[at + head.len()..];
            let mut depth = 1usize;
            let mut close = None;
            for (i, c) in rest.char_indices() {
                match c {
                    '"' | '\'' => return vec![],
                    '(' | '[' | '{' => depth += 1,
                    ')' | ']' | '}' => {
                        depth -= 1;
                        if depth == 0 {
                            close = Some(i);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let Some(close) = close else { return vec![] };
            let (recv, v) = (&text[start..at], &rest[..close]);
            // 1 行ブロックは書けないので、複数行の `if` 式にする (ブロックの中身は字下げ + 4)。式の前後が `if` の式に置き換えてよい位置
            // (`(` `,` `[` `=` `return` の後、`)` `,` `]` `}` か行末の前) のときだけ。メソッド連鎖や演算子の途中は直さない。
            let ind: String = text.chars().take_while(|c| *c == ' ').collect();
            let before = text[..start].trim_end();
            let tail = &text[at + head.len() + close + 1..];
            let after = tail.trim_start();
            let ok_before = before.ends_with(['(', ',', '[', '=']) && !before.ends_with("==") || before.ends_with("return") || before.is_empty();
            let ok_after = after.is_empty() || after.starts_with([')', ',', ']', '}']);
            if !ok_before || !ok_after || text[..start].ends_with("= =") {
                return vec![];
            }
            vec![span(file, line, text, start, at + head.len() + close + 1, format!("if {recv} {{\n{ind}    Some({v})\n{ind}}} else {{\n{ind}    None\n{ind}}}"))]
        }
        "E2055" => {
            // `recv.is_some()` 等の形 (受け手は単純な名前だけ)。
            let method = [("is_some", "Some(_)"), ("is_none", "None"), ("is_ok", "Ok(_)"), ("is_err", "Err(_)")]
                .into_iter()
                .find(|(m, _)| msg.contains(&format!("`.{m}()`")));
            if let Some((m, pat)) = method {
                let call = format!(".{m}()");
                if text.matches(&call).count() != 1 {
                    return vec![];
                }
                let at = text.find(&call).unwrap();
                let n = ident_before(at);
                if n == 0 || text[..at - n].ends_with(|c: char| c == '.' || c == '_' || c.is_alphanumeric()) {
                    return vec![];
                }
                let start = at - n;
                let after = text[at + call.len()..].trim_start();
                if !(after.is_empty() || after.starts_with([')', '{', ',', ';', '&', '|'])) {
                    return vec![];
                }
                let recv = &text[start..at];
                let neg = text[..start].ends_with('!');
                let rep = if neg { format!("({recv} is {pat})") } else { format!("{recv} is {pat}") };
                return vec![span(file, line, text, start, at + call.len(), rep)];
            }
            // `x == None` / `x != None` (左の受け手は単純な名前)。
            for (op, neg) in [(" == None", false), (" != None", true)] {
                if text.matches(op).count() != 1 {
                    continue;
                }
                let at = text.find(op).unwrap();
                let n = ident_before(at);
                if n == 0 || text[..at - n].ends_with(|c: char| c == '.' || c == '_' || c.is_alphanumeric()) || text[..at - n].ends_with('!') {
                    return vec![];
                }
                let start = at - n;
                let recv = &text[start..at];
                let rep = if neg { format!("{recv} is Some(_)") } else { format!("{recv} is None") };
                return vec![span(file, line, text, start, at + op.len(), rep)];
            }
            vec![]
        }
        _ => vec![],
    }
}


/// 位置 `end` (バイト) の直前にある、名前と `.` だけの式の長さ。
fn simple_back(text: &str, end: usize) -> usize {
    text[..end].chars().rev().take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.').map(char::len_utf8).sum()
}

/// E2062 の `[タグ]` ごとの書き換え。行の中で形が 1 か所に決まるときだけ直す。
fn identity_fix(file: &str, line: usize, msg: &str, text: &str) -> Vec<Fix> {
    let tag = msg[1..].split(']').next().unwrap_or("");
    let lit_ok = |s: &str| !s.is_empty() && s.chars().next().unwrap().is_ascii_digit() && s.chars().all(|c| c.is_alphanumeric() || c == '_');
    let once = |pat: &str| (text.matches(pat).count() == 1).then(|| text.find(pat).unwrap());
    match tag {
        "neg2" => {
            let pat = ".wrapping_neg().wrapping_neg()";
            once(pat).map_or(vec![], |at| vec![span(file, line, text, at, at + pat.len(), String::new())])
        }
        "addsub" => {
            for (a, b) in [(".wrapping_add(", ".wrapping_sub("), (".wrapping_sub(", ".wrapping_add(")] {
                let Some(at) = once(a) else { continue };
                let rest = &text[at + a.len()..];
                let Some(c) = rest.find(')') else { continue };
                let lit = &rest[..c];
                let tail = format!("){b}{lit})");
                if lit_ok(lit) && rest[c..].starts_with(&tail) {
                    return vec![span(file, line, text, at, at + a.len() + c + tail.len(), String::new())];
                }
            }
            vec![]
        }
        "call0" => {
            for m in [".wrapping_shl(", ".wrapping_shr(", ".shl(", ".shr("] {
                if text.matches(m).count() == 1 {
                    let at = text.find(m).unwrap();
                    let rest = &text[at + m.len()..];
                    if let Some(c) = rest.find(')') {
                        let lit = &rest[..c];
                        if lit.starts_with('0') && lit_ok(lit) && lit.trim_start_matches('0').chars().next().map_or(true, |ch| ch.is_alphabetic()) {
                            return vec![span(file, line, text, at, at + m.len() + c + 1, String::new())];
                        }
                    }
                }
            }
            vec![]
        }
        "maxmin" => {
            for m in [".max(", ".min("] {
                let Some(at) = once(m) else { continue };
                let n = simple_back(text, at);
                let rest = &text[at + m.len()..];
                let Some(c) = rest.find(')') else { continue };
                if n > 0 && rest[..c] == text[at - n..at] {
                    return vec![span(file, line, text, at, at + m.len() + c + 1, String::new())];
                }
            }
            vec![]
        }
        "bit0" | "ones" => {
            let ops: &[&str] = if tag == "ones" { &["&"] } else { &["|", "^"] };
            for op in ops {
                let pat = format!(" {op} ");
                if text.matches(&pat).count() != 1 {
                    continue;
                }
                let at = text.find(&pat).unwrap();
                let (n, rest) = (simple_back(text, at), &text[at + pat.len()..]);
                let rl = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.').map(char::len_utf8).sum::<usize>();
                let lit = &rest[..rl];
                let ok_lit = |l: &str| (lit_ok(l) || (tag == "ones" && l.starts_with("core.num.") && l.ends_with(".MAX"))) && (tag == "ones" || l.starts_with('0') && l.trim_start_matches('0').chars().next().map_or(true, |ch| ch.is_alphabetic()));
                // 右がリテラル: `x | 0u32` の ` | 0u32` を消す。左がリテラル: `0u32 | x` の `0u32 | ` を消す。
                if n > 0 && ok_lit(lit) && !text[at - n..at].chars().next().unwrap().is_ascii_digit() {
                    return vec![span(file, line, text, at, at + pat.len() + rl, String::new())];
                }
                let ll = &text[at - n..at];
                if n > 0 && ok_lit(ll) && rl > 0 && !rest.starts_with(|c: char| c.is_ascii_digit()) {
                    return vec![span(file, line, text, at - n, at + pat.len(), String::new())];
                }
            }
            vec![]
        }
        "same==" | "same!=" => {
            let op = if tag == "same==" { " == " } else { " != " };
            let Some(at) = once(op) else { return vec![] };
            let n = simple_back(text, at);
            let rest = &text[at + op.len()..];
            let rl = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.').map(char::len_utf8).sum::<usize>();
            if n > 0 && rl == n && text[at - n..at] == rest[..rl] {
                return vec![span(file, line, text, at - n, at + op.len() + rl, if tag == "same==" { "true" } else { "false" }.into())];
            }
            vec![]
        }
        t if t.starts_with("subself:") => {
            let ty = &t["subself:".len()..];
            let pat = ".wrapping_sub(";
            let Some(at) = once(pat) else { return vec![] };
            let n = simple_back(text, at);
            let rest = &text[at + pat.len()..];
            let Some(c) = rest.find(')') else { return vec![] };
            if n > 0 && rest[..c] == text[at - n..at] && matches!(ty, "u8" | "u16" | "u32" | "u64" | "u128" | "usize" | "i8" | "i16" | "i32" | "i64" | "i128" | "isize") {
                return vec![span(file, line, text, at - n, at + pat.len() + c + 1, format!("0{ty}"))];
            }
            vec![]
        }
        _ => vec![],
    }
}

/// `x == Some(リテラル)` → `x is Some(リテラル)`、`!=` → `!(x is Some(リテラル))`。
fn some_lit_fix(file: &str, line: usize, msg: &str, text: &str) -> Vec<Fix> {
    let (op, neg) = if msg.contains("`x == Some") { (" == Some(", false) } else { (" != Some(", true) };
    if text.matches(op).count() != 1 {
        return vec![];
    }
    let at = text.find(op).unwrap();
    let n = simple_back(text, at);
    let rest = &text[at + op.len()..];
    let Some(c) = rest.find(')') else { return vec![] };
    let lit = &rest[..c];
    let end = at + op.len() + c + 1;
    let after = text[end..].trim_start();
    if n == 0 || !lit.chars().all(|ch| ch.is_alphanumeric() || ch == '_') || !(after.is_empty() || after.starts_with([')', '{', ',', '&', '|'])) {
        return vec![];
    }
    let before = text[..at - n].trim_end();
    if neg && !(before.is_empty() || before.ends_with(['(', ',', '=', '&', '|', '!']) || before.ends_with("if") || before.ends_with("return")) {
        return vec![];
    }
    let recv = &text[at - n..at];
    let rep = if neg { format!("!({recv} is Some({lit}))") } else { format!("{recv} is Some({lit})") };
    vec![span(file, line, text, at - n, end, rep)]
}

struct MatchText {
    indent: String,
    prefix: String,
    scrut: String,
    arms: Vec<(String, Vec<String>)>,
    end_line: usize,
}

/// `match X {` から閉じの `}` までの腕を行で読む (腕の本体は字下げを 4 減らした行にする)。
fn parse_match(line: usize, lines: &[String]) -> Option<MatchText> {
    let first = lines.get(line.checked_sub(1)?)?;
    let indent: String = first.chars().take_while(|c| *c == ' ').collect();
    let body = &first[indent.len()..];
    let at = body.find("match ")?;
    let prefix = &body[..at];
    if !(prefix.is_empty() || prefix.starts_with("let ") && prefix.ends_with(" = ") || prefix.ends_with(" => ") && !prefix.contains("match") || prefix == "return ") {
        return None;
    }
    let scrut = body[at + 6..].strip_suffix(" {")?.to_string();
    let arm_ind = format!("{indent}    ");
    let mut arms: Vec<(String, Vec<String>, bool)> = vec![];
    let mut i = line; // `lines[i]` は次の行
    loop {
        let l = lines.get(i)?;
        i += 1;
        if *l == format!("{indent}}}") {
            break;
        }
        let is_arm_start = l.starts_with(&arm_ind) && !l[arm_ind.len()..].starts_with(' ') && !l[arm_ind.len()..].starts_with('}') && l.contains(" => ");
        if is_arm_start {
            let t = &l[arm_ind.len()..];
            let (head, rest) = t.split_once(" => ")?;
            if rest == "{" {
                arms.push((head.to_string(), vec![], true));
            } else {
                arms.push((head.to_string(), vec![format!("{arm_ind}{rest}")], false));
            }
        } else {
            let a = arms.last_mut()?;
            // 1 行の腕の続きの行は、書き出し (字下げ + 4) と同じ基準なので動かさない。ブロックの腕の中身は 4 減らす。
            if a.2 {
                a.1.push(l.strip_prefix("    ")?.to_string());
            } else {
                a.1.push(l.clone());
            }
        }
    }
    let arms: Vec<(String, Vec<String>)> = arms
        .into_iter()
        .map(|(h, mut b, block)| {
            if block {
                // 最後の行は腕を閉じる `}` (字下げは腕と同じ → 4 減らして `indent}`)。
                if b.last().map(|x| x.as_str()) == Some(&format!("{indent}}}")) {
                    b.pop();
                    Some((h, b))
                } else {
                    None
                }
            } else {
                Some((h, b))
            }
        })
        .collect::<Option<Vec<_>>>()?;
    Some(MatchText { indent, prefix: prefix.to_string(), scrut, arms, end_line: i })
}

fn match_fix(code: &str, file: &str, line: usize, lines: &[String]) -> Vec<Fix> {
    let Some(m) = parse_match(line, lines) else { return vec![] };
    if m.arms.len() != 2 {
        return vec![];
    }
    let ind = &m.indent;
    let join = |b: &Vec<String>| b.iter().map(|x| format!("{x}\n")).collect::<String>();
    let rep = if code == "E2068" {
        let (t, f) = match (m.arms[0].0.as_str(), m.arms[1].0.as_str()) {
            ("true", "false") => (&m.arms[0].1, &m.arms[1].1),
            ("false", "true") => (&m.arms[1].1, &m.arms[0].1),
            _ => return vec![],
        };
        match (t.is_empty(), f.is_empty()) {
            (true, true) => return vec![],
            // 空の腕は `else` を書かない (空の `else` は書き方が 1 通りでない)。値を返す `match` (前置きがある) では空の腕はない。
            (false, true) if m.prefix.is_empty() => format!("{ind}if {} {{\n{}{ind}}}", m.scrut, join(t)),
            (true, false) if m.prefix.is_empty() => {
                let c = if m.scrut.contains([' ', '&', '|', '<', '>', '=']) { format!("!({})", m.scrut) } else { format!("!{}", m.scrut) };
                format!("{ind}if {c} {{\n{}{ind}}}", join(f))
            }
            (false, false) => format!("{ind}{}if {} {{\n{}{ind}}} else {{\n{}{ind}}}", m.prefix, m.scrut, join(t), join(f)),
            _ => return vec![],
        }
    } else {
        if !(m.prefix.is_empty() || m.prefix.ends_with(" => ")) {
            return vec![];
        }
        let (some, none) = match (&m.arms[0], &m.arms[1]) {
            (a, b) if a.0.starts_with("Some(") && b.0 == "None" => (a, b),
            (a, b) if b.0.starts_with("Some(") && a.0 == "None" => (b, a),
            _ => return vec![],
        };
        let unit = none.1.is_empty() || none.1.iter().all(|x| x.trim() == "()");
        if !unit || some.1.is_empty() {
            return vec![];
        }
        format!("{ind}{}if let {} = {} {{\n{}{ind}}}", m.prefix, some.0, m.scrut, join(&some.1))
    };
    let last = &lines[m.end_line - 1];
    vec![Fix { file: file.to_string(), line, col: 1, end_line: m.end_line, end_col: last.chars().count() + 1, replace: rep }]
}

/// 数え上げの `while` → `for`。`let mut i = A` の行は、直前ならまとめて、離れていれば (初期値がリテラル) 消す。
fn while_fix(file: &str, line: usize, msg: &str, lines: &[String]) -> Vec<Fix> {
    let Some(let_line) = msg.rsplit("at line ").next().and_then(|r| r.trim_end_matches(')').parse::<usize>().ok()) else { return vec![] };
    let (Some(wl), Some(ll)) = (lines.get(line.wrapping_sub(1)), lines.get(let_line.wrapping_sub(1))) else { return vec![] };
    let indent: String = wl.chars().take_while(|c| *c == ' ').collect();
    let Some(wt) = wl.strip_prefix(&format!("{indent}while ")).and_then(|r| r.strip_suffix(" {")) else { return vec![] };
    let Some((i, b)) = wt.split_once(" < ") else { return vec![] };
    let Some(a) = ll.strip_prefix(&format!("{indent}let mut {i} = ")) else { return vec![] };
    // 本体: 次の `indent}` まで。最後の文は増分。
    let mut end = line;
    let mut body = vec![];
    loop {
        let Some(l) = lines.get(end) else { return vec![] };
        end += 1;
        if *l == format!("{indent}}}") {
            break;
        }
        body.push(l.clone());
    }
    let step_pre = format!("{indent}    {i} = {i}.wrapping_add(1");
    let Some(last) = body.pop() else { return vec![] };
    if !last.starts_with(&step_pre) || !last.ends_with(')') || last[step_pre.len()..last.len() - 1].chars().any(|c| !c.is_alphanumeric()) || body.is_empty() {
        return vec![];
    }
    // 本体が `i` を使わないなら `_` (未使用の変数は E1020)。
    let uses_i = body.iter().any(|l| l.split(|c: char| !(c.is_alphanumeric() || c == '_')).any(|w| w == i));
    let var = if uses_i { i } else { "_" };
    let rep = format!("{indent}for {var} in core.iter.range({a}, {b}) {{\n{}{indent}}}", body.iter().map(|x| format!("{x}\n")).collect::<String>());
    let endl = &lines[end - 1];
    if let_line + 1 == line {
        return vec![Fix { file: file.to_string(), line: let_line, col: 1, end_line: end, end_col: endl.chars().count() + 1, replace: rep }];
    }
    vec![
        Fix { file: file.to_string(), line: let_line, col: 1, end_line: let_line + 1, end_col: 1, replace: String::new() },
        Fix { file: file.to_string(), line, col: 1, end_line: end, end_col: endl.chars().count() + 1, replace: rep },
    ]
}


/// 引数を同じ順で関数に渡すだけの閉包 (`fn(a: T) -> R capture [] {` + 本体 1 行 + `}`) を、その関数の名前にする。
fn closure_fix(file: &str, line: usize, lines: &[String]) -> Vec<Fix> {
    let Some(first) = lines.get(line.wrapping_sub(1)) else { return vec![] };
    if first.matches("fn(").count() != 1 || !first.ends_with(" {") || !first.contains(" capture [] {") {
        return vec![];
    }
    let indent: String = first.chars().take_while(|c| *c == ' ').collect();
    let (Some(body), Some(close)) = (lines.get(line), lines.get(line + 1)) else { return vec![] };
    let b = body.trim();
    if *body != format!("{indent}    {b}") || !close.starts_with(&format!("{indent}}}")) {
        return vec![];
    }
    let Some(open) = b.find('(') else { return vec![] };
    let Some(args) = b[open + 1..].strip_suffix(')') else { return vec![] };
    let callee = &b[..open];
    // 引数は名前だけ (入れ子の呼びを含まない)。閉包の引数の名前と同じ順であることは型の側で確かめてある。
    if callee.is_empty() || args.contains(['(', ')']) || callee.contains(' ') {
        return vec![];
    }
    let at = first.find("fn(").unwrap();
    vec![Fix { file: file.to_string(), line, col: char_idx(first, at) + 1, end_line: line + 2, end_col: indent.chars().count() + 2, replace: callee.to_string() }]
}
