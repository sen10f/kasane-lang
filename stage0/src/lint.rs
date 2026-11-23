//! lint (stage0)。Python 参照実装 spec/tools/ksnlint.py の移植。
//! FQN とパスの一致・命名・定義順・空行・属性の並び・インデント・未使用・pub の伝播を検査する。

use std::collections::HashMap;

use regex::Regex;

pub type LintErr = (String, usize, String);

/// 定義 1 つの診断 (4 段分)。行は定義の見出しの行からの相対。
pub type ItemErrs = [Vec<LintErr>; 4];

/// 定義ごとの結果の再利用 (`old` を読み、使ったものを `new` に入れる)。キーは `Lint::item_key`。
#[derive(Default)]
pub struct LintMemo {
    pub old: HashMap<String, ItemErrs>,
    pub new: HashMap<String, ItemErrs>,
}

/// 正規表現は 1 回だけコンパイルする (定義ごとの検査から繰り返し呼ばれる)。
macro_rules! re {
    ($p:expr) => {{
        static R: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
        R.get_or_init(|| Regex::new($p).unwrap())
    }};
}

const GROUP_IMPL_ONLY: usize = 4;
const DERIVE_ORDER: &[&str] = &["Debug", "Clone", "Copy", "TryClone", "Drop", "PartialEq", "Eq", "PartialOrd", "Ord", "Hash", "From", "ToJson", "FromJson"];
const PRIMS: &[&str] = &[
    "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize", "f32", "f64", "bool",
    "char", "str",
];

fn group(kw: &str) -> usize {
    match kw {
        "import" => 0,
        "const" | "static" => 1,
        "type" | "enum" | "trait" => 2,
        "extern" => 3,
        "impl_only" => 4,
        "fn" => 5,
        _ => 6,
    }
}

fn is_field_kind(k: &str) -> bool {
    matches!(k, "type" | "enum" | "trait")
}
fn is_body_kind(k: &str) -> bool {
    matches!(k, "fn" | "test")
}

#[derive(Clone, Default)]
struct Item {
    kw: String,
    line: usize,
    start: usize,
    text: String,
    name: String,
    end: usize,
    body: Vec<String>,
    body_start: usize,
    attached: bool,
}

fn word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// 直後の文字リテラルの中身の長さ (閉じの `'` の手前まで)。`X'` または `\X'` の形だけ数える。
/// ライフタイム (`'a` `'static`) や閉じのない `'` は None。`rest` は `'` の直後を指す。
fn char_lit_inner_len(rest: &std::iter::Peekable<std::str::Chars>) -> Option<usize> {
    let mut la = rest.clone();
    match la.next()? {
        '\\' => {
            la.next()?;
            (la.next()? == '\'').then_some(2)
        }
        '\'' | '\n' => None,
        _ => (la.next()? == '\'').then_some(1),
    }
}

/// 文字列リテラル (`"..."`、エスケープ `\"` を含む) と行コメント (`//` から行末まで) を空白に置き換える。
/// 置き換えなので、残りの文字・行構造は保たれ、後続の正規表現や `count_word` の
/// マッチ位置がずれない。
/// `"` はバイトリテラル (`b'"'` など) の中にも現れうるため、バイトリテラル (`b'...'`) は
/// 状態で追跡し、その中の `"` を文字列開始と誤らない。バイトリテラルと裸の `'`
/// (文字リテラル・ライフタイム)はそのまま残す (閉じを持たないライフタイムを壊さず、
/// `b','` のようなバイトリテラルをパターン束縛と誤読しないため)。
fn strip_literals_and_comments(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    let mut prev: Option<char> = None;
    enum State { Normal, String, Byte }
    let mut state = State::Normal;
    while let Some(ch) = chars.next() {
        match state {
            State::String => {
                if ch == '\\' {
                    out.push(' ');
                    let _ = chars.next();
                } else if ch == '"' {
                    out.push(' ');
                    state = State::Normal;
                } else {
                    out.push(' ');
                }
            }
            State::Byte => {
                out.push(ch);
                if ch == '\\' {
                    if let Some(nc) = chars.next() {
                        out.push(nc);
                    }
                } else if ch == '\'' {
                    state = State::Normal;
                }
            }
            State::Normal => match ch {
                '"' => {
                    out.push(' ');
                    state = State::String;
                }
                '\'' => {
                    if prev == Some('b') {
                        // バイトリテラル `b'X'` の開始: 中身をそのまま出力。
                        out.push('\'');
                        state = State::Byte;
                    } else if let Some(n) = char_lit_inner_len(&chars) {
                        // 文字リテラル `'"'` `'\n'`: 中身は残すが、`"` は文字列の開始と誤らないように空白にする。
                        out.push('\'');
                        for _ in 0..n {
                            let c = chars.next().unwrap_or(' ');
                            out.push(if c == '"' { ' ' } else { c });
                        }
                        let _ = chars.next();
                        out.push('\'');
                    } else {
                        out.push('\'');
                    }
                }
                '/' => {
                    if chars.peek() == Some(&'/') {
                        out.push(' ');
                        let _ = chars.next();
                        while let Some(c) = chars.next() {
                            if c == '\n' {
                                out.push('\n');
                                break;
                            }
                            out.push(' ');
                        }
                    } else {
                        out.push(ch);
                    }
                }
                '\n' => out.push('\n'),
                _ => out.push(ch),
            },
        }
        prev = Some(ch);
    }
    out
}

/// `(?<![A-Za-z0-9_.])nm(?![A-Za-z0-9_])` の出現回数。
fn count_word(text: &str, nm: &str) -> usize {
    if nm.is_empty() {
        return 0;
    }
    let mut n = 0;
    for (i, _) in text.match_indices(nm) {
        let before = text[..i].chars().next_back();
        let after = text[i + nm.len()..].chars().next();
        if before.map_or(true, |c| !(word_char(c) || c == '.')) && after.map_or(true, |c| !word_char(c)) {
            n += 1;
        }
    }
    n
}

fn is_snake(s: &str) -> bool {
    // _?[a-z][a-z0-9]*(?:_[a-z][a-z0-9]*)*
    let s = s.strip_prefix('_').unwrap_or(s);
    if s.is_empty() {
        return false;
    }
    s.split('_').all(|w| {
        let mut cs = w.chars();
        matches!(cs.next(), Some(c) if c.is_ascii_lowercase()) && cs.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
    })
}
fn is_upper_camel(s: &str) -> bool {
    // [A-Z][a-z0-9]*(?:[A-Z][a-z0-9]*)*
    let mut it = s.chars().peekable();
    if !matches!(it.peek(), Some(c) if c.is_ascii_uppercase()) {
        return false;
    }
    while let Some(c) = it.next() {
        if !(c.is_ascii_uppercase() || c.is_ascii_lowercase() || c.is_ascii_digit()) {
            return false;
        }
        let _ = c;
    }
    // 先頭以外の語は大文字で始まり小文字/数字が続く。大文字連続は別途検査される。
    s.chars().all(|c| c.is_ascii_alphanumeric())
}
fn is_screaming(s: &str) -> bool {
    // [A-Z][A-Z0-9]*(?:_[A-Z][A-Z0-9]*)*
    !s.is_empty()
        && s.split('_').all(|w| {
            let mut cs = w.chars();
            matches!(cs.next(), Some(c) if c.is_ascii_uppercase()) && cs.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        })
}
fn two_upper(s: &str) -> bool {
    let cs: Vec<char> = s.chars().collect();
    cs.windows(2).any(|w| w[0].is_ascii_uppercase() && w[1].is_ascii_uppercase())
}

pub(crate) fn split_top(s: &str, sep: char) -> Vec<String> {
    let mut out = vec![];
    let mut d = 0i32;
    let mut cur = String::new();
    for ch in s.chars() {
        if "[(<{".contains(ch) {
            d += 1;
        } else if "])>}".contains(ch) {
            d -= 1;
        }
        if ch == sep && d == 0 {
            out.push(cur.trim().to_string());
            cur.clear();
        } else {
            cur.push(ch);
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

/// 対応する閉じカッコの位置 (`open` は開きカッコの位置)。
fn matching_close(s: &str, open: usize) -> Option<usize> {
    let mut depth = 0i32;
    for (k, ch) in s[open..].char_indices() {
        match ch {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(open + k);
                }
            }
            _ => {}
        }
    }
    None
}

/// パターンの文字列から束縛名を集める (束縛を集める唯一の所。`let` / `if let` / `while let` / `for` /
/// クロージャの引数 / `match` の腕で共通)。タプル・`path(args)`・`path { f: p, .. }`・入れ子・`mut` / `ref` /
/// `&`・`x @ p`・`A | B` (最初の選択肢) を辿る。小文字または `_` で始まる単純な識別子が束縛で、
/// `_` 単体・リテラル・`true` / `false` と FQN の成分は束縛ではない。大文字始まり (`None` など) は、
/// 反駁できない `let` / `for` の中 (`allow_upper`。命名 E0010 のため) だけ束縛とする。
fn pattern_binds(pat: &str, allow_upper: bool, out: &mut Vec<String>) {
    let pat = pat.trim();
    if let Some(alts) = Some(split_top(pat, '|')).filter(|a| a.len() > 1) {
        pattern_binds(&alts[0], allow_upper, out);
        return;
    }
    let mut p = pat;
    loop {
        let q = p.trim_start();
        let q = q.strip_prefix("mut ").or_else(|| q.strip_prefix("ref ")).or_else(|| q.strip_prefix('&')).unwrap_or(q);
        if q.len() == p.len() {
            break;
        }
        p = q;
    }
    let p = p.trim();
    if p.is_empty() || p == "_" || p == ".." {
        return;
    }
    if let Some((name, rest)) = p.split_once('@') {
        let name = name.trim();
        if !rest.starts_with('=') && is_plain_bind(name) {
            pattern_binds(name, allow_upper, out);
            pattern_binds(rest, allow_upper, out);
            return;
        }
    }
    if p.starts_with('(') || p.starts_with('[') {
        if let Some(end) = matching_close(p, 0) {
            for part in split_top(&p[1..end], ',') {
                pattern_binds(&part, allow_upper, out);
            }
        }
        return;
    }
    let path_end = p.find(|c: char| !(word_char(c) || c == '.')).unwrap_or(p.len());
    let (path, rest) = p.split_at(path_end);
    let rest = rest.trim_start();
    if rest.starts_with('(') || rest.starts_with('{') {
        // `t.V(a, b)` / `t.S { f: p, g }`
        if let Some(end) = matching_close(rest, 0) {
            let inner = &rest[1..end];
            if rest.starts_with('(') {
                for part in split_top(inner, ',') {
                    pattern_binds(&part, allow_upper, out);
                }
            } else {
                for part in split_top(inner, ',') {
                    match part.split_once(':') {
                        Some((_, v)) => pattern_binds(v, allow_upper, out),
                        None => pattern_binds(&part, allow_upper, out),
                    }
                }
            }
        }
        return;
    }
    if rest.is_empty() && is_plain_bind(path) && path.starts_with(|c: char| c.is_ascii_lowercase() || c == '_' || (allow_upper && c.is_ascii_uppercase())) && path != "true" && path != "false" {
        out.push(path.to_string());
    }
}

/// 本体のテキストから、束縛のパターンの文字列を集める: `(種類, パターン)`。種類は未使用の判定に使う
/// (`let` = 変数、`cparam` = クロージャの引数)。`match` の腕は腕ごとの範囲で数えるので含めない。
fn binding_sites(body: &str) -> Vec<(&'static str, String, bool)> {
    let mut out: Vec<(&'static str, String, bool)> = vec![];
    let bytes = body.as_bytes();
    let kw_at = |i: usize, kw: &str| -> bool {
        body[i..].starts_with(kw) && body[..i].chars().next_back().map_or(true, |c| !(word_char(c) || c == '.'))
    };
    for i in 0..body.len() {
        if !body.is_char_boundary(i) {
            continue;
        }
        if kw_at(i, "let ") || kw_at(i, "for ") || kw_at(i, "with ") {
            let kw_len = if body[i..].starts_with("let ") || body[i..].starts_with("for ") { 4 } else { 5 };
            let start = i + kw_len;
            let is_for = body[i..].starts_with("for ");
            let mut depth = 0i32;
            let mut end = body.len();
            for (k, ch) in body[start..].char_indices() {
                let at = start + k;
                match ch {
                    '(' | '[' | '{' => depth += 1,
                    ')' | ']' | '}' => depth -= 1,
                    _ => {}
                }
                if depth < 0 || (depth == 0 && (ch == ';' || ch == '\n')) {
                    end = at;
                    break;
                }
                if depth == 0 && is_for && body[at..].starts_with(" in ") {
                    end = at;
                    break;
                }
                if depth == 0 && !is_for && ch == '=' && bytes.get(at + 1) != Some(&b'=') && bytes.get(at + 1) != Some(&b'>') {
                    end = at;
                    break;
                }
                if depth == 0 && !is_for && ch == ':' {
                    end = at;
                    break;
                }
            }
            let refutable = body[..i].trim_end().ends_with("if") || body[..i].trim_end().ends_with("while");
            out.push(("let", body[start..end].to_string(), !refutable));
        } else if kw_at(i, "fn(") {
            let open = i + 2;
            if let Some(close) = matching_close(body, open) {
                for part in split_top(&body[open + 1..close], ',') {
                    // 関数型 `fn(u32)` は名前を持たない。クロージャの引数は `名前: 型`。
                    if let Some((name, _)) = part.split_once(':') {
                        out.push(("cparam", name.to_string(), false));
                    }
                }
            }
        }
    }
    out
}

/// 束縛として数える単純な識別子: 英字または `_` で始まり、英数字と `_` のみ (`_` 単体と数値リテラルは除く)。
fn is_plain_bind(s: &str) -> bool {
    !s.is_empty()
        && s != "_"
        && s.chars().all(word_char)
        && matches!(s.chars().next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
}

/// `capture [.., &mut nm, ..]` に現れる `nm` の数。
fn count_capture_mut(text: &str, nm: &str) -> usize {
    let re_cap = re!(r"\bcapture \[([^\]]*)\]");
    let mut n = 0;
    for m in re_cap.captures_iter(text) {
        n += m[1].split(',').filter(|c| c.trim().strip_prefix("&mut ").map_or(false, |r| r.trim() == nm)).count();
    }
    n
}

/// 代入の左辺として現れる `nm` の数 (`nm = ..` が文の頭にある。`==` と `=>` は除く)。
fn count_assign(text: &str, nm: &str) -> usize {
    let mut n = 0;
    for (i, _) in text.match_indices(nm) {
        let before = text[..i].chars().next_back();
        let after = text[i + nm.len()..].chars().next();
        if !(before.map_or(true, |c| !(word_char(c) || c == '.')) && after.map_or(true, |c| !word_char(c))) {
            continue;
        }
        let rest = text[i + nm.len()..].trim_start_matches([' ', '\t']);
        if !rest.starts_with('=') || rest.starts_with("==") || rest.starts_with("=>") {
            continue;
        }
        let pre = text[..i].trim_end_matches([' ', '\t']);
        if pre.is_empty() || pre.ends_with(['\n', '{', ';']) || pre.ends_with("=>") {
            n += 1;
        }
    }
    n
}

/// `match` の腕: `(パターン, ガードと本体)`。腕は行頭の `パターン =>` で、本体は同じ行の残り (空なら次の行)。
/// `{` で終わるなら対応する `}` までのブロック全体。
fn arms(body: &str) -> Vec<(String, String)> {
    let re_arm = re!(r"(?m)^[ \t]*([^\n]*?)[ \t]*=>(.*)$");
    let mut out = vec![];
    for m in re_arm.captures_iter(body) {
        let mut pat = m[1].to_string();
        // 腕でない行 (行頭が文の続き) を避けるため、パターンは `let` / `=` を含まない。
        if pat.is_empty() || pat.contains(" = ") || pat.starts_with("let ") || pat.starts_with("if let ") {
            continue;
        }
        let mut guard = String::new();
        if let Some(idx) = pat.find(" if ") {
            guard = pat[idx..].to_string();
            pat.truncate(idx);
        }
        let mut arm_body = format!("{} {}", guard, &m[2]);
        let mut rest_start = m.get(0).unwrap().end();
        if m[2].trim().is_empty() {
            let rest = &body[rest_start..];
            let rest = rest.strip_prefix('\n').unwrap_or(rest);
            let line = rest.lines().next().unwrap_or("");
            rest_start += (body[rest_start..].len() - rest.len()) + line.len();
            arm_body.push_str(line);
        }
        if arm_body.trim_end().ends_with('{') {
            let rest = &body[rest_start..];
            let mut depth = 1i32;
            let mut cut = rest.len();
            for (i, ch) in rest.char_indices() {
                match ch {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            cut = i;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            arm_body.push(chr_nl());
            arm_body.push_str(&rest[..cut]);
        }
        out.push((pat, arm_body));
    }
    out
}

pub(crate) fn bracket(s: &str, start: usize) -> String {
    let mut d = 0;
    for (i, ch) in s[start..].char_indices() {
        if ch == '[' {
            d += 1;
        } else if ch == ']' {
            d -= 1;
            if d == 0 {
                return s[start + 1..start + i].to_string();
            }
        }
    }
    String::new()
}

pub(crate) fn strip_args(s: &str) -> String {
    let mut out = String::new();
    let mut d = 0i32;
    for ch in s.chars() {
        if "<([".contains(ch) {
            d += 1;
        } else if ">)]".contains(ch) {
            d -= 1;
        } else if d == 0 {
            out.push(ch);
        }
    }
    out
}

fn first_word_lower(e: &str) -> bool {
    e.chars().next().map_or(false, |c| c.is_lowercase())
}
fn first_word_upper(e: &str) -> bool {
    e.chars().next().map_or(false, |c| c.is_uppercase())
}

pub struct Lint {
    module: String,
    sig: bool,
    lines: Vec<String>,
    errs: Vec<LintErr>,
}

impl Lint {
    /// `rel` は <root> からの相対パス ("app/net.ksnh")。
    pub fn new(rel: &str, text: &str) -> Lint {
        let sig = rel.ends_with(".sig");
        let module = {
            let m = rel.trim_end_matches(".ksnh").trim_end_matches(".ksnl").trim_end_matches(".sig");
            m.replace('/', ".")
        };
        let mut lines: Vec<String> = text.split('\n').map(String::from).collect();
        if lines.last().map_or(false, |l| l.is_empty()) {
            lines.pop();
        }
        Lint { module, sig, lines, errs: vec![] }
    }

    fn err(&mut self, code: &str, n: usize, msg: String) {
        self.errs.push((code.to_string(), n, msg));
    }

    fn items(&mut self) -> Vec<Item> {
        let re_kw = re!(r"^(import|const|static|extern|type|enum|trait|impl|fn|test|end)\b");
        let re_name = re!(r"^\w+\s+([A-Za-z0-9_.]+)");
        let l = self.lines.clone();
        let mut items = vec![];
        let mut i = 0;
        let mut lead_start: Option<usize> = None;
        while i < l.len() {
            let line = &l[i];
            if line.is_empty() {
                lead_start = None;
                i += 1;
                continue;
            }
            if line.starts_with("//") {
                if lead_start.is_none() {
                    lead_start = Some(i);
                }
                i += 1;
                continue;
            }
            let m = re_kw.captures(line);
            if m.is_none() || line.starts_with(' ') {
                let head: String = line.chars().take(30).collect();
                self.err("indent", i + 1, format!("top-level line not at column 0 or unknown: {}", crate::grammar::py_repr(&head)));
                i += 1;
                lead_start = None;
                continue;
            }
            let kw = m.unwrap().get(1).unwrap().as_str().to_string();
            let start = lead_start.unwrap_or(i);
            let mut it = Item { kw: kw.clone(), line: i, start, text: line.clone(), ..Default::default() };
            it.name = match kw.as_str() {
                "import" => line.split_whitespace().nth(1).unwrap_or("").to_string(),
                "impl" => line.clone(),
                _ => re_name.captures(line).map_or(String::new(), |c| c[1].to_string()),
            };
            if is_field_kind(&kw) || (is_body_kind(&kw) && !(self.sig && kw == "fn")) {
                let mut j = i + 1;
                while j < l.len() && !l[j].starts_with("end ") {
                    j += 1;
                }
                if j >= l.len() {
                    self.err("fqn", i + 1, "missing end".into());
                    j = l.len() - 1;
                } else {
                    let endname = l[j].split_whitespace().nth(1).unwrap_or("").to_string();
                    if endname != it.name {
                        self.err("fqn", j + 1, format!("end {} != header {}", endname, it.name));
                    }
                }
                it.end = j;
                it.body = l[i + 1..j.max(i + 1)].to_vec();
                it.body_start = i + 1;
                i = j + 1;
            } else {
                it.end = i;
                i += 1;
            }
            items.push(it);
            lead_start = None;
        }
        items
    }

    #[cfg(test)]
    fn check_fqn_and_names(&mut self, items: &[Item]) {
        for it in items {
            self.fqn_one(it);
        }
    }

    fn fqn_one(&mut self, it: &Item) {
        let pref = format!("{}.", self.module);
        {
            let (kw, name, n) = (it.kw.as_str(), &it.name, it.line + 1);
            if kw == "import" || kw == "impl" {
                return;
            }
            if !name.starts_with(&pref) {
                // モジュールのパスを含まない名前 (Rust のフリーメソッド) は、書き方を案内する E0062 で受ける。
                if !name.contains('.') {
                    self.err("nfqn", n, crate::rustisms::name_needs_module_msg(kw, Some(&self.module), name));
                    return;
                }
                self.err("fqn", n, format!("{} does not start with module {}.", name, self.module));
                return;
            }
            let rest: Vec<&str> = name[pref.len()..].split('.').collect();
            match kw {
                "const" | "static" => {
                    if !is_screaming(rest[rest.len() - 1]) {
                        self.err("name", n, format!("{} not SCREAMING_SNAKE_CASE", rest[rest.len() - 1]));
                    }
                }
                "type" | "enum" | "trait" => {
                    if rest.len() != 1 || !is_upper_camel(rest[0]) || two_upper(rest[0]) {
                        self.err("name", n, format!("{} not UpperCamelCase (no consecutive capitals)", py_list(&rest)));
                    }
                }
                "test" => {
                    if !rest[rest.len() - 1].starts_with("test_") {
                        self.err("name", n, "test name must start with test_".into());
                    }
                }
                _ => {
                    for c in rest {
                        if PRIMS.contains(&c) || c == "blanket" {
                            continue;
                        }
                        if c.chars().next().map_or(false, |x| x.is_uppercase()) {
                            if !is_upper_camel(c) || two_upper(c) {
                                self.err("name", n, format!("{} not UpperCamelCase", c));
                            }
                        } else if !is_snake(c) {
                            self.err("name", n, format!("{} not snake_case", c));
                        }
                    }
                }
            }
        }
    }

    fn check_order(&mut self, items: &mut Vec<Item>) {
        let re_impls = re!(r" impls (.+?)(?: pub)?$");
        let re_gen = re!(r"^impl(\[(?:[^\[\]]|\[[^\]]*\])*\])?\s+");
        let mut prev_g: i64 = -1;
        let mut prev_key: Option<String> = None;
        let mut entries: Vec<(usize, String, usize)> = vec![];
        for idx in 0..items.len() {
            let kw = items[idx].kw.clone();
            if kw == "end" {
                continue;
            }
            let (g, key);
            if kw == "impl" {
                let attached = items.get(idx + 1).map_or(false, |n| n.kw == "fn" && n.text.contains(" impls "));
                items[idx].attached = attached;
                if attached {
                    continue;
                }
                g = GROUP_IMPL_ONLY;
                key = items[idx].text.clone();
            } else if kw == "fn" && idx > 0 && items[idx - 1].kw == "impl" && items[idx - 1].attached {
                g = group("fn");
                key = items[idx].name.clone();
            } else {
                g = group(&kw);
                key = items[idx].name.clone();
            }
            entries.push((g, key, items[idx].line + 1));
        }
        let mut first_seen: Vec<(String, String)> = vec![];
        for idx in 0..items.len() {
            if items[idx].kw != "fn" {
                continue;
            }
            let Some(mi) = re_impls.captures(&items[idx].text) else { continue };
            let tr = mi[1].to_string();
            let owner = items[idx].name.rsplit_once('.').map_or(items[idx].name.clone(), |x| x.0.to_string());
            let key = (tr.clone(), owner.clone());
            if first_seen.contains(&key) {
                continue;
            }
            first_seen.push(key);
            let mut ok = false;
            if idx > 0 && items[idx - 1].kw == "impl" {
                let prev = &items[idx - 1].text;
                let body = re_gen.replace(prev, "").to_string();
                if let Some((trait_, target)) = body.split_once(" for ") {
                    let target = target.trim();
                    let tg = target.split(|c| c == '<' || c == '[' || c == ' ').next().unwrap_or("");
                    let last = tg.rsplit('.').next().unwrap_or("");
                    ok = trait_.trim() == tr && (owner == tg || owner.ends_with(&format!(".{}", last)));
                }
            }
            if !ok {
                let n = items[idx].line + 1;
                self.err("order", n, format!("first method of impl {} for {} must be directly preceded by its impl line", tr, owner));
            }
        }
        for (g, key, n) in entries {
            if (g as i64) < prev_g {
                self.err("order", n, format!("group order violated at {}", key));
            } else if g as i64 == prev_g && prev_key.as_ref().map_or(false, |p| key < *p) {
                let pk = prev_key.clone().unwrap();
                self.err("order", n, format!("{} should come before {} (UTF-8 byte order)", key, pk));
            }
            prev_g = g as i64;
            prev_key = Some(key);
        }
    }

    fn check_blank(&mut self, items: &[Item]) {
        if self.sig {
            return;
        }
        for k in 1..items.len() {
            let (a, b) = (&items[k - 1], &items[k]);
            let gap = b.start as i64 - a.end as i64 - 1;
            let both_imports = a.kw == "import" && b.kw == "import";
            let impl_attach = a.kw == "impl" && b.kw == "fn" && b.text.contains(" impls ");
            if both_imports || impl_attach {
                if gap != 0 {
                    self.err("blank", b.line + 1, "no blank line allowed here".into());
                }
            } else if gap != 1 {
                self.err("blank", b.line + 1, format!("exactly one blank line required (found {})", gap));
            }
        }
        // ファイル先頭の `///` の塊の直後の空行は、モジュールの説明の区切り (3.2)
        let head_doc = self.lines.iter().take_while(|l| l.starts_with("///")).count();
        for n in 0..self.lines.len().saturating_sub(1) {
            if self.lines[n].starts_with("//") && self.lines[n + 1].is_empty() {
                if head_doc > 0 && n + 1 == head_doc {
                    continue;
                }
                self.err("blank", n + 2, "blank line after comment".into());
            }
        }
    }

    #[cfg(test)]
    fn check_attrs(&mut self, items: &[Item]) {
        for it in items {
            self.attrs_one(it);
        }
        self.check_capture();
    }

    fn attrs_one(&mut self, it: &Item) {
        let re_uses = re!(r"\buses \[");
        let re_derive = re!(r"\bderive \[([^\]]*)\]");
        let re_req = re!(r"\brequires (.*?)(?: pub)?$");
        let re_gen = re!(r"^(?:fn|type|enum|trait)\s+[A-Za-z0-9_.]+\[");
        {
            let (text, n) = (&it.text, it.line + 1);
            for m in re_uses.find_iter(text) {
                let inner = bracket(text, m.end() - 1);
                let els = split_top(&inner, ',');
                let mut conc: Vec<String> = els.iter().filter(|e| first_word_lower(e)).cloned().collect();
                let var: Vec<String> = els.iter().filter(|e| first_word_upper(e)).cloned().collect();
                let mut expect = conc.clone();
                expect.sort();
                expect.extend(var);
                if els != expect {
                    self.err("order", n, format!("uses list order: {}", py_str_list(&els)));
                }
                conc.sort();
                for a in &conc {
                    for b in &conc {
                        if a != b && b.starts_with(&format!("{}.", a)) {
                            self.err("attr", n, format!("redundant uses: {} includes {}", a, b));
                        }
                    }
                }
                let mut seen = els.clone();
                seen.sort();
                seen.dedup();
                if seen.len() != els.len() {
                    self.err("attr", n, "duplicate uses element".into());
                }
            }
            if let Some(m) = re_derive.captures(text) {
                let ds: Vec<String> = m[1].split(',').map(|d| d.trim().to_string()).collect();
                let idx: Vec<usize> = ds.iter().filter_map(|d| DERIVE_ORDER.iter().position(|x| x == d)).collect();
                let mut sorted = idx.clone();
                sorted.sort();
                if idx != sorted || idx.len() != ds.len() {
                    self.err("order", n, format!("derive order: {}", py_str_list(&ds)));
                }
            }
            if it.kw == "trait" {
                if let Some(m) = re_req.captures(text) {
                    let rs = split_top(&m[1], ',');
                    let mut s = rs.clone();
                    s.sort();
                    if rs != s {
                        self.err("order", n, format!("requires order: {}", py_str_list(&rs)));
                    }
                }
            }
            let gen: Option<String> = if text.starts_with("impl[") {
                Some(bracket(text, 4))
            } else {
                re_gen.find(text).map(|m| bracket(text, m.end() - 1))
            };
            if let Some(gen) = gen {
                for ent in split_top(&gen, ',') {
                    let Some((_, b)) = ent.split_once(':') else { continue };
                    let bound = b.trim();
                    if bound == "uses" || bound == "panics" || !strip_args(bound).contains('+') {
                        continue;
                    }
                    let parts: Vec<String> = split_top(bound, '+').iter().map(|p| p.trim().to_string()).collect();
                    let rank = |p: &String| -> (u8, String) {
                        if p == "linear" {
                            (0, String::new())
                        } else if p.starts_with('\'') {
                            (2, p.clone())
                        } else {
                            (1, p.clone())
                        }
                    };
                    let mut sorted = parts.clone();
                    sorted.sort_by_key(|p| rank(p));
                    if parts != sorted {
                        self.err("order", n, format!("bound order: {}", py_str_list(&parts)));
                    }
                }
            }
        }
    }

    /// `capture [..]` の並び順。行ごとに決まる (定義の中にも外にもありうるのでファイル全体を毎回走る。安い)。
    fn check_capture(&mut self) {
        let re_cap = re!(r"\bcapture \[([^\]]*)\]");
        for k in 0..self.lines.len() {
            let l = self.lines[k].clone();
            if let Some(m) = re_cap.captures(&l) {
                if m[1].trim().is_empty() {
                    continue;
                }
                let names: Vec<String> = m[1]
                    .split(',')
                    .map(|c| {
                        let c = c.trim();
                        for p in ["&mut ", "&", "move "] {
                            if let Some(r) = c.strip_prefix(p) {
                                return r.to_string();
                            }
                        }
                        c.to_string()
                    })
                    .collect();
                let mut s = names.clone();
                s.sort();
                if names != s {
                    self.err("order", k + 1, format!("capture order: {}", py_str_list(&names)));
                }
            }
        }
    }

    #[cfg(test)]
    fn check_indent(&mut self, items: &[Item]) {
        for it in items {
            self.indent_one(it);
        }
    }

    fn indent_one(&mut self, it: &Item) {
        {
            if is_field_kind(&it.kw) {
                for (k, l) in it.body.iter().enumerate() {
                    if !l.is_empty() && !(l.starts_with("    ") && !l.starts_with("     ")) {
                        self.err("indent", it.body_start + k + 1, "declaration line must be indented by exactly 4".into());
                    }
                }
                return;
            }
            if !is_body_kind(&it.kw) || (self.sig && it.kw == "fn") {
                return;
            }
            let mut stack: Vec<(&str, usize)> = vec![];
            let mut stmt_base = 0usize;
            for (k, l) in it.body.iter().enumerate() {
                // 空白だけの行は字下げの対象外。trim().is_empty() で飛ばさないと、
                // l.trim() が空になった行で s.chars().next().unwrap() がパニックする (src/lint.rs:924)。
                if l.trim().is_empty() {
                    continue;
                }
                let n = it.body_start + k + 1;
                let ind = l.len() - l.trim_start_matches(' ').len();
                let s = l.trim();
                let mut errs: Vec<String> = vec![];
                let expect = |e: usize, errs: &mut Vec<String>| {
                    if ind != e {
                        errs.push(format!("indent {}, expected {}", ind, e));
                    }
                };
                if ind % 4 != 0 {
                    self.err("indent", n, format!("indent {} not multiple of 4", ind));
                }
                let first = s.chars().next().unwrap();
                if s.starts_with("//") {
                    expect(stack.last().map_or(4, |t| t.1 + 4), &mut errs);
                } else if first == '}' {
                    if stack.last().map_or(true, |t| t.0 != "brace") {
                        errs.push("unbalanced }".into());
                    } else {
                        expect(stack.last().unwrap().1, &mut errs);
                        stack.pop();
                    }
                    if s.ends_with('{') {
                        stack.push(("brace", ind));
                    }
                } else if first == ')' || first == ']' {
                    if stack.last().map_or(true, |t| t.0 != "wrap") {
                        errs.push("unbalanced closing bracket".into());
                    } else {
                        expect(stack.last().unwrap().1, &mut errs);
                        stack.pop();
                    }
                    // `) {` (`if f(` / `with .. close f(` の折り返しの閉じ) と `)? close g(` は、同じ行で次の波括弧・括弧を開く。
                    if s.ends_with('{') {
                        stack.push(("brace", ind));
                    } else if s.ends_with('(') || s.ends_with('[') {
                        stack.push(("wrap", ind));
                    }
                } else if s.starts_with('.') {
                    expect(stmt_base + 4, &mut errs);
                    if s.ends_with('{') {
                        // `with .. = a` の次の `.b()? close c() {` のように、チェーンの最後の行が波括弧を開く。
                        // 閉じは文頭の行と同じ深さ。
                        stack.push(("brace", stmt_base));
                    } else if s.ends_with('(') || s.ends_with('[') {
                        stack.push(("wrap", ind));
                    }
                } else {
                    let in_wrap = stack.last().map_or(false, |t| t.0 == "wrap");
                    expect(stack.last().map_or(4, |t| t.1 + 4), &mut errs);
                    if !in_wrap {
                        stmt_base = ind;
                    }
                    if s.ends_with('{') {
                        stack.push(("brace", ind));
                    } else if s.ends_with('(') || s.ends_with('[') {
                        stack.push(("wrap", ind));
                    }
                }
                for e in errs {
                    self.err("indent", n, e);
                }
            }
            if !stack.is_empty() {
                self.err("indent", it.end + 1, "unclosed block/bracket in body".into());
            }
        }
    }

    fn idents_declared(&self, it: &Item) -> (Vec<(&'static str, String)>, String) {
        let mut names: Vec<(&'static str, String)> = vec![];
        let head = &it.text;
        if is_body_kind(&it.kw) {
            let re_h = re!(r"^(?:fn|test)\s+[A-Za-z0-9_.]+");
            let mut pos = re_h.find(head).map_or(0, |m| m.end());
            if head[pos..].starts_with('[') {
                let mut d = 0;
                for (i, ch) in head[pos..].char_indices() {
                    if ch == '[' {
                        d += 1;
                    } else if ch == ']' {
                        d -= 1;
                        if d == 0 {
                            pos += i + 1;
                            break;
                        }
                    }
                }
            }
            if let Some(off) = head[pos..].find('(') {
                let i = pos + off;
                let mut depth = 0;
                let mut j = head.len();
                for (k, ch) in head[i..].char_indices() {
                    if ch == '(' {
                        depth += 1;
                    } else if ch == ')' {
                        depth -= 1;
                        if depth == 0 {
                            j = i + k;
                            break;
                        }
                    }
                }
                for p in split_top(&head[i + 1..j], ',') {
                    let nm = p.split(':').next().unwrap().trim().to_string();
                    if !nm.is_empty() && nm != "self" {
                        names.push(("param", nm));
                    }
                }
            }
        }
        // 文字列リテラルとコメントの中の `let` などを宣言と誤らないため、
        // それらは空白に置き換えてから走査・カウントする。
        // (`'` は Kasane でバイトリテラル `b'X'` やライフタイム `'static` にも使われるため、
        //  strip 対象から外している。)
        let body = strip_literals_and_comments(&it.body.join("\n"));
        // 束縛の場所 (`let` / `if let` / `while let` / `for` / `with` / クロージャの引数) は、パターンの
        // 文字列を取り出して `pattern_binds` へ渡す一か所で集める。`match` の腕は、腕ごとの範囲で数えるので
        // `names_one` が同じ `pattern_binds` で集める。
        let re_st = re!(r"\blet [A-Za-z0-9_.]+ \{([^}]*)\}");
        for (kind, pat, upper) in binding_sites(&body) {
            let mut bs = vec![];
            pattern_binds(&pat, upper, &mut bs);
            for b in bs {
                names.push((kind, b));
            }
        }
        // 構造体パターンのフィールドラベル (`b` in `let t.L { b: b } = l`) は変数の使用ではない。
        // 使用回数を数える前に空白へ置き換え、束縛名と同じラベルを使用として数えないようにする。
        // (置換は ASCII の `{` `,` `:` の位置だけで行うので UTF-8 の境界は壊れない。)
        let mut bytes = body.clone().into_bytes();
        for m in re_st.captures_iter(&body) {
            let g = m.get(1).unwrap();
            let mut off = g.start();
            for f in g.as_str().split(',') {
                if let Some(c) = f.find(':') {
                    for b in &mut bytes[off..off + c] {
                        *b = b' ';
                    }
                }
                off += f.len() + 1;
            }
        }
        let count_body = String::from_utf8(bytes).unwrap_or_else(|_| body.clone());
        (names, count_body)
    }

    #[cfg(test)]
    fn check_names_and_unused(&mut self, items: &[Item]) {
        for it in items {
            self.names_one(it);
        }
    }

    fn names_one(&mut self, it: &Item) {
        let re_arm = re!(r"(?m)^\s*[A-Za-z0-9_.]+\(([^)]*)\)\s*=>(.*)$");
        {
            let n = it.line + 1;
            if it.kw == "type" {
                for (k, l) in it.body.iter().enumerate() {
                    let nm = l.trim().split(':').next().unwrap();
                    if !is_snake(nm) {
                        self.err("name", it.body_start + k + 1, format!("field {} not snake_case", nm));
                    }
                }
            }
            if !is_body_kind(&it.kw) || (self.sig && it.kw == "fn") {
                return;
            }
            let (names, body) = self.idents_declared(it);
            // 同じ名前の宣言は 1 つの判定にまとめる (宣言の数 `decls` を、出現の数から引く)。
            let arm_list = arms(&body);
            let mut arm_names: Vec<String> = vec![];
            for (pat, _) in &arm_list {
                pattern_binds(pat, false, &mut arm_names);
            }
            let mut seen: Vec<(&'static str, String, usize)> = vec![];
            for (kind, nm) in names {
                match seen.iter_mut().find(|(k, m, _)| *k == kind && *m == nm) {
                    Some(e) => e.2 += 1,
                    None => seen.push((kind, nm, 1)),
                }
            }
            for (kind, nm, decls) in seen {
                // `if let true = b` / `(n, false)` の `true` / `false` は予約語のリテラルで束縛ではない。
                if nm == "_" || nm == "true" || nm == "false" {
                    continue;
                }
                if !is_snake(&nm) {
                    self.err("name", n, format!("{} {} not snake_case", kind, nm));
                    continue;
                }
                let cnt = count_word(&body, &nm);
                if kind == "param" {
                    if nm.starts_with('_') {
                        if cnt > 0 {
                            self.err("unused", n, format!("{} is marked unused but used", nm));
                        }
                    } else if cnt == 0 {
                        self.err("unused", n, format!("unused parameter {}", nm));
                    }
                } else if nm.starts_with('_') {
                    // `match` の腕の同名の束縛 (`Some(_x) =>`) は別の宣言なので、その分は使用に数えない。
                    let arm_decls = arm_names.iter().filter(|a| **a == nm).count();
                    if cnt > decls + arm_decls {
                        self.err("unused", n, format!("{} is marked unused but used", nm));
                    }
                } else {
                    // 読む出現 = 全体 − 宣言 − 代入の左辺 (`x = ..` の文)。書くだけの変数は未使用。
                    // 書くだけの変数を `capture [&mut x]` で取り込む並びも、読む使用ではない。
                    let assigns = count_assign(&body, &nm);
                    let cap_mut = if assigns > 0 { count_capture_mut(&body, &nm) } else { 0 };
                    let reads = cnt.saturating_sub(decls + assigns + cap_mut);
                    if reads == 0 {
                        let what = if kind == "cparam" { "parameter" } else { "variable" };
                        self.err("unused", n, format!("unused {} {}", what, nm));
                    }
                }
            }
            for (pat, guard_and_body) in arm_list {
                let mut bs = vec![];
                pattern_binds(&pat, false, &mut bs);
                bs.dedup();
                for nm in bs {
                    if is_snake(&nm) && !nm.starts_with('_') && count_word(&guard_and_body, &nm) == 0 {
                        self.err("unused", n, format!("unused pattern binding {}", nm));
                    }
                }
            }
        }
    }

    fn check_imports(&mut self, items: &[Item]) {
        let text = self.lines.iter().filter(|l| !l.starts_with("import ")).cloned().collect::<Vec<_>>().join("\n");
        // 3.3: 末尾名が衝突する2つのモジュールは同一ファイルで import できない。
        let mut tails: HashMap<&str, ()> = HashMap::new();
        for it in items {
            if it.kw != "import" {
                continue;
            }
            let tail = it.name.rsplit('.').next().unwrap();
            if tails.insert(tail, ()).is_some() {
                self.err(
                    "impcollide",
                    it.line + 1,
                    format!("import tail `{}` collides with another import; use the FQN instead", tail),
                );
            }
            // 3.3: 短縮形 (`net.f`) でも FQN (`app.net.f`) でも、ヘッダでも本体でも、参照があれば使用済み。
            let used = [format!("{}.", tail), format!("{}.", it.name)].iter().any(|needle| {
                text.match_indices(needle.as_str()).any(|(i, _)| {
                    text[..i].chars().next_back().map_or(true, |c| !(word_char(c) || c == '.'))
                })
            });
            if !used {
                self.err("unused", it.line + 1, format!("unused import {}", it.name));
            }
        }
    }

    fn check_pub(&mut self, items: &[Item]) {
        let private: Vec<String> = items
            .iter()
            .filter(|it| is_field_kind(&it.kw) && !it.text.trim_end().ends_with(" pub"))
            .map(|it| it.name.clone())
            .collect();
        for it in items {
            if matches!(it.kw.as_str(), "fn" | "type" | "enum" | "trait") && it.text.trim_end().ends_with(" pub") {
                for p in &private {
                    if count_word(&it.text, p) > 0 {
                        self.err("pub", it.line + 1, format!("pub definition mentions private {}", p));
                    }
                }
            }
        }
    }

    pub fn run(self) -> Vec<LintErr> {
        self.run_memo(&mut LintMemo::default())
    }

    /// 定義 1 つの中だけで決まる 4 つの検査 (FQN と命名・属性・インデント・名前と未使用) の結果。
    /// 行は定義の見出しの行からの相対 (見出しの行 = 1)。
    fn item_errs(&mut self, it: &Item) -> ItemErrs {
        let saved = std::mem::take(&mut self.errs);
        let mut out: ItemErrs = Default::default();
        let phases: [fn(&mut Lint, &Item); 4] = [Lint::fqn_one, Lint::attrs_one, Lint::indent_one, Lint::names_one];
        for (k, f) in phases.iter().enumerate() {
            f(self, it);
            out[k] = std::mem::take(&mut self.errs)
                .into_iter()
                .map(|(c, n, m)| {
                    debug_assert!(n > it.line);
                    (c, n - it.line, m)
                })
                .collect();
        }
        self.errs = saved;
        out
    }

    /// 定義 1 つの結果のキー。定義の文面 (見出し・本体・`end` までの行数) とモジュール名・`.sig` かどうかだけで決まる
    /// (`item_errs` が読むのはこれだけ)。位置は含めない。
    fn item_key(&self, it: &Item) -> String {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        let sig = [self.sig as u8];
        let span = ((it.end - it.line) as u64).to_le_bytes();
        let body = it.body.join("\n");
        for part in [self.module.as_bytes(), &sig[..], it.kw.as_bytes(), it.text.as_bytes(), &span[..], body.as_bytes()] {
            h.update((part.len() as u64).to_le_bytes());
            h.update(part);
        }
        h.finalize().iter().take(16).map(|b| format!("{:02x}", b)).collect()
    }

    /// 定義ごとの結果を `memo` から再利用して lint する。`memo.new` に今回使った分が入る。
    /// 結果 (診断の列と順序) は `run` と完全に同じ。
    pub fn run_memo(mut self, memo: &mut LintMemo) -> Vec<LintErr> {
        let mut items = self.items();
        let mut per: Vec<ItemErrs> = vec![];
        for it in &items {
            let k = self.item_key(it);
            let r = match memo.old.get(&k) {
                Some(r) => r.clone(),
                None => self.item_errs(it),
            };
            memo.new.insert(k, r.clone());
            per.push(r);
        }
        // 段 (fqn → order → blank → attrs → capture → indent → names → imports → pub) の順に積む。
        // 同じ (行, コード) の診断の並びが `run` と変わらないように、段ごとに全定義を回す。
        let push = |s: &mut Lint, items: &[Item], ph: usize| {
            for (it, r) in items.iter().zip(&per) {
                for (c, n, m) in &r[ph] {
                    s.errs.push((c.clone(), n + it.line, m.clone()));
                }
            }
        };
        push(&mut self, &items, 0);
        self.check_order(&mut items);
        self.check_blank(&items);
        push(&mut self, &items, 1);
        self.check_capture();
        push(&mut self, &items, 2);
        push(&mut self, &items, 3);
        self.check_imports(&items);
        self.check_pub(&items);
        let mut e = self.errs;
        e.sort_by(|a, b| (a.1, &a.0).cmp(&(b.1, &b.0)));
        e
    }

    /// 定義ごとの再利用をしない版 (`run_memo` と同じ結果になることをテストで確かめる)。
    #[cfg(test)]
    fn run_whole(mut self) -> Vec<LintErr> {
        let mut items = self.items();
        self.check_fqn_and_names(&items);
        self.check_order(&mut items);
        self.check_blank(&items);
        self.check_attrs(&items);
        self.check_indent(&items);
        self.check_names_and_unused(&items);
        self.check_imports(&items);
        self.check_pub(&items);
        let mut e = self.errs;
        e.sort_by(|a, b| (a.1, &a.0).cmp(&(b.1, &b.0)));
        e
    }
}

fn py_list(xs: &[&str]) -> String {
    format!("[{}]", xs.iter().map(|x| crate::grammar::py_repr(x)).collect::<Vec<_>>().join(", "))
}
fn py_str_list(xs: &[String]) -> String {
    format!("[{}]", xs.iter().map(|x| crate::grammar::py_repr(x)).collect::<Vec<_>>().join(", "))
}

fn chr_nl() -> char {
    char::from(10u8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    pub(crate) fn corpus() -> Vec<(String, String)> {
        fn walk(d: &Path, out: &mut Vec<PathBuf>) {
            let Ok(rd) = std::fs::read_dir(d) else { return };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else if p.extension().map_or(false, |x| x == "ksnl") {
                    out.push(p);
                }
            }
        }
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let mut files = vec![];
        for d in ["examples", "stage0/tests/data", "bench/tasks"] {
            walk(&root.join(d), &mut files);
        }
        files.sort();
        files
            .into_iter()
            .filter_map(|p| {
                let text = std::fs::read_to_string(&p).ok()?;
                let rel = p.file_stem()?.to_string_lossy().to_string();
                Some((rel, text))
            })
            .collect()
    }

    /// 定義ごとの再利用 (`run_memo`) は、全体を一度に検査する版と診断の列 (順序を含む) が同じ。
    /// 2 回目 (全部命中) も同じ。ファイルにはモジュール名と合わないものも混じるので、診断のあるものも通る。
    #[test]
    fn memoized_lint_equals_whole_file_lint() {
        let corpus = corpus();
        assert!(corpus.len() > 50, "corpus too small: {}", corpus.len());
        let mut with_diags = 0;
        for (rel, text) in corpus {
            let want = Lint::new(&rel, &text).run_whole();
            if !want.is_empty() {
                with_diags += 1;
            }
            let mut memo = LintMemo::default();
            assert_eq!(Lint::new(&rel, &text).run_memo(&mut memo), want, "cold {}", rel);
            let mut again = LintMemo { old: std::mem::take(&mut memo.new), new: Default::default() };
            assert_eq!(Lint::new(&rel, &text).run_memo(&mut again), want, "warm {}", rel);
            assert_eq!(again.old.len(), again.new.len(), "{}", rel);
            // 別のモジュール名で (キーにモジュール名が入っている)。
            assert_eq!(Lint::new("other.mod", &text).run_memo(&mut again), Lint::new("other.mod", &text).run_whole(), "module {}", rel);
        }
        assert!(with_diags > 5, "the corpus should include files with lint diagnostics");
    }
}

#[cfg(test)]
pub(crate) fn tests_corpus() -> Vec<(String, String)> {
    tests::corpus()
}
