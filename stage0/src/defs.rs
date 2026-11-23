//! トップレベル定義の走査 (stage0)。Python 参照実装 ksn/syntax.py の parse_source と同じ範囲を返す。

#[derive(Debug, Clone, PartialEq)]
pub struct DefSpan {
    pub kw: String,
    pub name: String,
    pub line: usize,
    pub end_line: usize,
}

const KWS: &[&str] = &[
    "import", "const", "static", "extern", "type", "enum", "trait", "impl", "fn", "test", "end",
];

fn leading_kw(line: &str) -> Option<&'static str> {
    if line.starts_with(' ') || line.is_empty() {
        return None;
    }
    for k in KWS {
        if let Some(rest) = line.strip_prefix(k) {
            let ok = match rest.chars().next() {
                None => true,
                Some(c) => !(c.is_alphanumeric() || c == '_'),
            };
            if ok {
                return Some(k);
            }
        }
    }
    None
}

fn first_name(s: &str) -> String {
    let s = s.trim_start();
    s.chars().take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.').collect()
}

/// FQN の直後(属性・ジェネリクス等の前)までを読む。`fn`/`type` 等のヘッダ用。
fn name_of(kw: &str, line: &str) -> String {
    let rest = &line[kw.len()..];
    if kw == "impl" {
        return line.to_string();
    }
    first_name(rest)
}

pub fn scan(src: &str) -> Result<Vec<DefSpan>, String> {
    scan_with(src, false)
}

pub fn scan_with(src: &str, sig: bool) -> Result<Vec<DefSpan>, String> {
    let lines: Vec<&str> = src.split('\n').collect();
    let n = lines.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        let l = lines[i];
        let kw = match leading_kw(l) {
            Some(k) => k,
            None => {
                i += 1;
                continue;
            }
        };
        let ln = i + 1;
        if kw == "end" {
            return Err(format!("E0003|{}|1|unexpected end", ln));
        }
        let name = name_of(kw, l);
        let mut d = DefSpan { kw: kw.to_string(), name: name.clone(), line: ln, end_line: 0 };
        i += 1;
        let has_body = matches!(kw, "fn" | "type" | "enum" | "trait" | "test") && !(sig && kw == "fn");
        if has_body {
            let mut j = i;
            while j < n && !(lines[j].starts_with("end ") && lines[j][4..].find(' ').is_none()) {
                j += 1;
            }
            if j >= n {
                return Err(format!("E0003|{}|1|missing end for {}", ln, name));
            }
            if lines[j][4..] != name {
                return Err(format!("E0003|{}|1|end name mismatch: {}", j + 1, lines[j]));
            }
            d.end_line = j + 1;
            i = j + 1;
        }
        // `const` / `static` の初期化式が構造体リテラルなら、行頭の `}` で閉じるまでが 1 つの定義 (いつも 1 行 1 欄)。
        if matches!(kw, "const" | "static") && l.trim_end().ends_with('{') {
            let mut j = i;
            while j < n && !lines[j].starts_with('}') {
                j += 1;
            }
            if j >= n {
                return Err(format!("E0003|{}|1|missing `}}` for {}", ln, name));
            }
            d.end_line = j + 1;
            i = j + 1;
        }
        out.push(d);
    }
    Ok(out)
}
