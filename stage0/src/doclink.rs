//! `///` ドキュメントの解析 (仕様 3.2)。本文の Markdown の部分集合・FQN のリンク・コード例の囲み・モジュールの説明。
//! 表示 (`doc.rs`)・検査 (`ksn check` の E1125、`ksn doc` のエラー)・コード例の実行 (`ksn test`) が同じ解析を使う。

use std::collections::{BTreeMap, BTreeSet};

use crate::cli::Loaded;
use crate::project::Source;

/// 本文の 1 行の中の要素。
#[derive(Debug, Clone, PartialEq)]
pub enum Inline {
    Text(String),
    Code(String),
    Strong(String),
    /// ``[`FQN`]`` (定義かモジュールを指す。解決は `Known` で行う)。
    Link(String),
}

/// 本文のブロック (段落・箇条書き・コードの囲み)。
#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Para(Vec<String>),
    List(Vec<String>),
    Fence { info: String, lines: Vec<String> },
}

/// `///` の本文の 1 行 (行番号は 1 始まり)。コードの囲みの中の行は `text` に入らない。
pub struct DocScan {
    pub text: Vec<(usize, String)>,
    /// コードの囲み。`line` は開きの ``` の行 (1 始まり)。
    pub fences: Vec<DocFence>,
}

pub struct DocFence {
    pub line: usize,
    pub info: String,
    pub body: Vec<String>,
}

pub fn trim_cr(s: &str) -> &str {
    s.trim_end_matches('\r')
}

/// `///` の 1 行から本文を取る (`///` と、その直後の空白 1 つを外す)。`///` の行でなければ `None`。
pub fn doc_text(line: &str) -> Option<&str> {
    let r = trim_cr(line).strip_prefix("///")?;
    Some(r.strip_prefix(' ').unwrap_or(r))
}

/// ファイル先頭の `///` の塊の直後に空行 1 行があるときの、塊の行数 (3.2 のモジュールの説明。0 なら無い)。
pub fn module_doc_len(lines: &[String]) -> usize {
    let k = lines.iter().take_while(|l| doc_text(l).is_some()).count();
    if k == 0 || lines.get(k).map(|l| trim_cr(l)) != Some("") {
        return 0;
    }
    k
}

/// ファイル先頭のモジュールの説明の本文 (3.2)。
pub fn module_doc(lines: &[String]) -> Option<Vec<String>> {
    let k = module_doc_len(lines);
    if k == 0 {
        return None;
    }
    Some(lines[..k].iter().filter_map(|l| doc_text(l).map(String::from)).collect())
}

fn fence_info(s: &str) -> Option<&str> {
    s.trim_start().strip_prefix("```")
}

/// 文書の全 `///` 行を走査する (ファイル全体。`///` でない行で本文は途切れる)。
pub fn scan(lines: &[String]) -> DocScan {
    let mut text = Vec::new();
    let mut fences = Vec::new();
    let mut open: Option<DocFence> = None;
    for (i, l) in lines.iter().enumerate() {
        let Some(body) = doc_text(l) else {
            // 囲みの途中で `///` でない行が来たら、囲みは閉じずに捨てる (ソースの書き方の誤り)。
            open = None;
            continue;
        };
        if let Some(f) = open.as_mut() {
            if fence_info(body).is_some() {
                fences.push(open.take().unwrap());
            } else {
                f.body.push(body.to_string());
            }
            continue;
        }
        if let Some(info) = fence_info(body) {
            open = Some(DocFence { line: i + 1, info: info.trim().to_string(), body: Vec::new() });
            continue;
        }
        text.push((i + 1, body.to_string()));
    }
    DocScan { text, fences }
}

/// 本文の行をブロックにする (段落は空行で区切る。箇条書きは `- ` の連続)。
pub fn blocks(lines: &[String]) -> Vec<Block> {
    fn flush_para(out: &mut Vec<Block>, para: &mut Vec<String>) {
        if !para.is_empty() {
            out.push(Block::Para(std::mem::take(para)));
        }
    }
    fn flush_list(out: &mut Vec<Block>, list: &mut Vec<String>) {
        if !list.is_empty() {
            out.push(Block::List(std::mem::take(list)));
        }
    }
    let mut out = Vec::new();
    let mut para: Vec<String> = Vec::new();
    let mut list: Vec<String> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let l = trim_cr(&lines[i]);
        if let Some(info) = fence_info(l) {
            flush_para(&mut out, &mut para);
            flush_list(&mut out, &mut list);
            let mut body = Vec::new();
            i += 1;
            while i < lines.len() && fence_info(trim_cr(&lines[i])).is_none() {
                body.push(trim_cr(&lines[i]).to_string());
                i += 1;
            }
            out.push(Block::Fence { info: info.trim().to_string(), lines: body });
        } else if let Some(r) = l.strip_prefix("- ") {
            flush_para(&mut out, &mut para);
            list.push(r.to_string());
        } else if l.trim().is_empty() {
            flush_para(&mut out, &mut para);
            flush_list(&mut out, &mut list);
        } else {
            flush_list(&mut out, &mut list);
            para.push(l.to_string());
        }
        i += 1;
    }
    flush_para(&mut out, &mut para);
    flush_list(&mut out, &mut list);
    out
}

/// 1 行の本文の要素。優先順は ``[`FQN`]``、`` `コード` ``、`**強調**`。閉じが無いものは文字として扱う。
pub fn inline(s: &str) -> Vec<Inline> {
    fn push_text(out: &mut Vec<Inline>, t: &mut String) {
        if !t.is_empty() {
            out.push(Inline::Text(std::mem::take(t)));
        }
    }
    let mut out = Vec::new();
    let mut text = String::new();
    let mut rest = s;
    while let Some(c) = rest.chars().next() {
        if let Some(r) = rest.strip_prefix("[`") {
            if let Some(end) = r.find("`]") {
                let inner = &r[..end];
                if !inner.is_empty() {
                    push_text(&mut out, &mut text);
                    out.push(Inline::Link(inner.to_string()));
                    rest = &r[end + 2..];
                    continue;
                }
            }
        }
        if let Some(r) = rest.strip_prefix('`') {
            if let Some(end) = r.find('`') {
                push_text(&mut out, &mut text);
                out.push(Inline::Code(r[..end].to_string()));
                rest = &r[end + 1..];
                continue;
            }
        }
        if let Some(r) = rest.strip_prefix("**") {
            if let Some(end) = r.find("**") {
                if end > 0 {
                    push_text(&mut out, &mut text);
                    out.push(Inline::Strong(r[..end].to_string()));
                    rest = &r[end + 2..];
                    continue;
                }
            }
        }
        text.push(c);
        rest = &rest[c.len_utf8()..];
    }
    push_text(&mut out, &mut text);
    out
}

/// 定義の名前空間 (リンクで指せる FQN) と、末尾の名前が一意な短い名前。
/// 読み込んだ全ソース (パッケージ・依存・標準ライブラリ) から作る。
#[derive(Default)]
pub struct Known {
    /// 定義の FQN (fn / type / enum / trait / const / static / extern) と、モジュール名。
    pub fqns: BTreeSet<String>,
    /// 末尾の名前 -> FQN。同じ末尾の名前が 2 つ以上ある名前は載せない。
    pub short: BTreeMap<String, String>,
}

impl Known {
    pub fn of(srcs: &[&Source]) -> Known {
        let mut k = Known::default();
        let mut cnt: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for s in srcs {
            k.fqns.insert(s.module.clone());
            for d in &s.defs {
                if matches!(d.kw.as_str(), "fn" | "type" | "enum" | "trait" | "const" | "static" | "extern") && k.fqns.insert(d.name.clone()) {
                    if let Some(last) = d.name.rsplit('.').next().filter(|_| d.name.contains('.')) {
                        cnt.entry(last.to_string()).or_default().push(d.name.clone());
                    }
                }
            }
        }
        for (last, v) in cnt {
            if v.len() == 1 {
                k.short.insert(last, v[0].clone());
            }
        }
        k
    }

    pub fn of_loaded(l: &Loaded) -> Known {
        let mut v: Vec<&Source> = l.pkg.sources.iter().collect();
        for d in &l.deps {
            v.extend(d.sources.iter());
        }
        v.extend(l.std.iter());
        Known::of(&v)
    }

    /// リンクの書き方 (FQN か短い名前) を FQN に解決する。
    pub fn resolve(&self, tok: &str) -> Option<String> {
        if self.fqns.contains(tok) {
            return Some(tok.to_string());
        }
        if !tok.contains('.') {
            return self.short.get(tok).cloned();
        }
        None
    }
}

/// 解決しないリンク (行番号 1 始まり, 書かれた名前)。
pub fn unresolved(lines: &[String], known: &Known) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for (ln, t) in scan(lines).text {
        for x in inline(&t) {
            if let Inline::Link(f) = x {
                if known.resolve(&f).is_none() {
                    out.push((ln, f));
                }
            }
        }
    }
    out
}

/// 例 (info 文字列が `ksn` の囲み)。
pub fn examples(lines: &[String]) -> Vec<DocFence> {
    scan(lines).fences.into_iter().filter(|f| f.info == "ksn").collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Vec<String> {
        s.split('\n').map(String::from).collect()
    }

    #[test]
    fn inline_parses_links_code_and_strong() {
        assert_eq!(
            inline("a [`demo.Counter.bump`] `x` **y** [b]"),
            vec![
                Inline::Text("a ".into()),
                Inline::Link("demo.Counter.bump".into()),
                Inline::Text(" ".into()),
                Inline::Code("x".into()),
                Inline::Text(" ".into()),
                Inline::Strong("y".into()),
                Inline::Text(" [b]".into()),
            ]
        );
        // 閉じの無いものは文字
        assert_eq!(inline("`a"), vec![Inline::Text("`a".into())]);
        assert_eq!(inline("[`a"), vec![Inline::Text("[`a".into())]);
    }

    #[test]
    fn module_doc_needs_a_blank_line_after_the_leading_block() {
        assert_eq!(module_doc(&v("/// 説明\n\nimport a.b")), Some(vec!["説明".to_string()]));
        // 空行なし: 定義の説明であって、モジュールの説明ではない
        assert_eq!(module_doc(&v("/// 説明\nimport a.b")), None);
        assert_eq!(module_doc(&v("import a.b")), None);
    }

    #[test]
    fn fences_are_examples_and_not_text() {
        let sc = scan(&v("/// 本文 `a`\n/// ```ksn\n/// x\n/// ```\n/// 後"));
        assert_eq!(sc.text.iter().map(|x| x.0).collect::<Vec<_>>(), vec![1, 5]);
        assert_eq!(sc.fences.len(), 1);
        assert_eq!(sc.fences[0].line, 2);
        assert_eq!(sc.fences[0].body, vec!["x".to_string()]);
    }
}
