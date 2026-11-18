//! `ksn sync`: LLM が変えた `.ksnl` の上に、人の `.ksnh` のコメントを付け直す (設計_ksnhとロック.md 2 章)。
//!
//! コメントは行の位置ではなく「次の構文要素」に結び付ける。定義は FQN (`end <FQN>` の行で決まる鍵) で対応づけ、
//! 定義の中は行の最長共通部分列 (LCS) で一致を取り、一致しない区間は同じ種別・同じ深さの複合文 (`... {`) どうしを
//! 対応づけて中へ再帰する。人と LLM が同じ定義を変えたときは衝突として止める (2.4)。

use crate::ksnh::{is_comment_line, keep_mask};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Clone)]
struct Cm {
    line: usize,
    raw: String,
}

struct Parsed {
    /// `strip` した `.ksnl` の行 (末尾の空要素を含む)。
    lines: Vec<String>,
    /// `cm[i]` = `lines[i]` の直前のコメント。
    cm: Vec<Vec<Cm>>,
    /// モジュールの説明 (ファイル先頭のコメントの塊 + 空行)。
    doc: Vec<Cm>,
    /// 最後の行より後ろのコメント (付ける先が無い)。
    trailing: Vec<Cm>,
}

fn parse(text: &str) -> Parsed {
    let src: Vec<&str> = text.split('\n').collect();
    let keep = keep_mask(&src);
    let mut p = Parsed { lines: vec![], cm: vec![], doc: vec![], trailing: vec![] };
    let mut pending: Vec<Cm> = vec![];
    for (i, l) in src.iter().enumerate() {
        if is_comment_line(l) {
            pending.push(Cm { line: i + 1, raw: l.to_string() });
        } else if keep[i] {
            p.lines.push(l.to_string());
            p.cm.push(std::mem::take(&mut pending));
        } else {
            p.doc = std::mem::take(&mut pending);
        }
    }
    p.trailing = pending;
    p
}

enum U {
    Def { key: String, s: usize, e: usize },
    Line(usize),
}

fn first_word(l: &str) -> &str {
    l.trim_start().split(|c| c == ' ' || c == '(' || c == '<').next().unwrap_or("")
}

fn indent(l: &str) -> usize {
    l.len() - l.trim_start_matches(' ').len()
}

/// 最上位の並び: 定義 (`end X` で閉じる) と、それ以外の 1 行。
fn units(lines: &[String]) -> Vec<U> {
    let mut out = vec![];
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut i = 0;
    while i < lines.len() {
        let l = &lines[i];
        let is_start = !l.is_empty() && !l.starts_with(' ') && !l.starts_with("end ") && !l.starts_with("import ") && !l.starts_with('}');
        if is_start {
            let mut j = i + 1;
            while j < lines.len() && (lines[j].is_empty() || lines[j].starts_with(' ')) {
                j += 1;
            }
            if j < lines.len() && lines[j].starts_with("end ") {
                let mut key = format!("{} {}", first_word(l), lines[j]);
                let n = seen.entry(key.clone()).or_insert(0);
                *n += 1;
                if *n > 1 {
                    key = format!("{}#{}", key, n);
                }
                out.push(U::Def { key, s: i, e: j });
                i = j + 1;
                continue;
            }
        }
        out.push(U::Line(i));
        i += 1;
    }
    out
}

struct View<'a> {
    lines: &'a [String],
    defs: HashMap<String, (usize, usize)>,
    order: Vec<String>,
    misc: Vec<usize>,
}

fn view<'a>(lines: &'a [String]) -> View<'a> {
    let mut v = View { lines, defs: HashMap::new(), order: vec![], misc: vec![] };
    for u in units(lines) {
        match u {
            U::Def { key, s, e } => {
                v.defs.insert(key.clone(), (s, e));
                v.order.push(key);
            }
            U::Line(i) => {
                if !lines[i].is_empty() {
                    v.misc.push(i);
                }
            }
        }
    }
    v
}

impl<'a> View<'a> {
    fn def(&self, k: &str) -> Option<&'a [String]> {
        self.defs.get(k).map(|&(s, e)| &self.lines[s..=e])
    }
    fn misc_texts(&self) -> Vec<&'a str> {
        self.misc.iter().map(|&i| self.lines[i].as_str()).collect()
    }
}

fn fqn_of(key: &str) -> String {
    let k = key.split('#').next().unwrap_or(key);
    match k.split_once(" end ") {
        Some((_, f)) => f.to_string(),
        None => k.to_string(),
    }
}

// ---- 一致 (LCS + 複合文の再帰) ----

fn lcs(a: &[&str], b: &[&str]) -> Vec<(usize, usize)> {
    let mut pre = 0;
    while pre < a.len() && pre < b.len() && a[pre] == b[pre] {
        pre += 1;
    }
    let mut suf = 0;
    while suf < a.len() - pre && suf < b.len() - pre && a[a.len() - 1 - suf] == b[b.len() - 1 - suf] {
        suf += 1;
    }
    let mut out: Vec<(usize, usize)> = (0..pre).map(|i| (i, i)).collect();
    let am = &a[pre..a.len() - suf];
    let bm = &b[pre..b.len() - suf];
    let (n, m) = (am.len(), bm.len());
    if n > 0 && m > 0 && n * m <= 25_000_000 {
        let w = m + 1;
        let mut dp = vec![0u32; (n + 1) * w];
        for i in (0..n).rev() {
            for j in (0..m).rev() {
                dp[i * w + j] = if am[i] == bm[j] { dp[(i + 1) * w + j + 1] + 1 } else { dp[(i + 1) * w + j].max(dp[i * w + j + 1]) };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < n && j < m {
            if am[i] == bm[j] {
                out.push((pre + i, pre + j));
                i += 1;
                j += 1;
            } else if dp[(i + 1) * w + j] >= dp[i * w + j + 1] {
                i += 1;
            } else {
                j += 1;
            }
        }
    }
    for k in 0..suf {
        out.push((a.len() - suf + k, b.len() - suf + k));
    }
    out
}

/// `o[or]` の各行が `n[nr]` のどの行に当たるかを `res` (旧の添字 → 新の添字) に入れる。
fn align(o: &[String], n: &[String], or: (usize, usize), nr: (usize, usize), res: &mut [Option<usize>]) {
    if or.0 >= or.1 || nr.0 >= nr.1 {
        return;
    }
    let a: Vec<&str> = o[or.0..or.1].iter().map(|s| s.as_str()).collect();
    let b: Vec<&str> = n[nr.0..nr.1].iter().map(|s| s.as_str()).collect();
    let (mut po, mut pn) = (or.0, nr.0);
    for (i, j) in lcs(&a, &b) {
        res[or.0 + i] = Some(nr.0 + j);
        gap(o, n, (po, or.0 + i), (pn, nr.0 + j), res);
        po = or.0 + i + 1;
        pn = nr.0 + j + 1;
    }
    gap(o, n, (po, or.1), (pn, nr.1), res);
}

/// 一致しない区間: 同じ深さ・同じ先頭語の複合文 (`... {`) の見出しどうしを順に対応づけ、間を再帰で比べる。
fn gap(o: &[String], n: &[String], or: (usize, usize), nr: (usize, usize), res: &mut [Option<usize>]) {
    if or.0 >= or.1 || nr.0 >= nr.1 {
        return;
    }
    let opener = |l: &String| l.trim_end().ends_with('{');
    let new_open: Vec<usize> = (nr.0..nr.1).filter(|&j| opener(&n[j])).collect();
    let mut pairs: Vec<(usize, usize)> = vec![];
    let mut ptr = 0;
    for i in or.0..or.1 {
        if !opener(&o[i]) {
            continue;
        }
        if let Some(k) = (ptr..new_open.len()).find(|&k| {
            let j = new_open[k];
            indent(&n[j]) == indent(&o[i]) && first_word(&n[j]) == first_word(&o[i])
        }) {
            pairs.push((i, new_open[k]));
            ptr = k + 1;
        }
    }
    if pairs.is_empty() {
        return;
    }
    let (mut po, mut pn) = (or.0, nr.0);
    for (i, j) in pairs {
        res[i] = Some(j);
        align(o, n, (po, i), (pn, j), res);
        po = i + 1;
        pn = j + 1;
    }
    align(o, n, (po, or.1), (pn, nr.1), res);
}

// ---- 付け直し ----

#[derive(Clone)]
pub struct Lost {
    pub fqn: String,
    pub line: usize,
    pub text: String,
    pub reason: &'static str,
}

pub struct Merged {
    pub ksnh: String,
    pub ksnl: String,
    pub lost: Vec<Lost>,
    pub notes: Vec<String>,
}

pub enum MergeErr {
    Conflict(Vec<String>),
    Internal(String),
}

fn comment_text(raw: &str) -> String {
    raw.trim().trim_start_matches('/').trim().to_string()
}

fn reindent(c: &Cm, old_line: &str, new_line: &str) -> String {
    let ci = indent(&c.raw) as i64;
    let oi = if old_line.is_empty() { 0 } else { indent(old_line) as i64 };
    let ni = if new_line.is_empty() { 0 } else { indent(new_line) as i64 };
    let n = (ci - oi + ni).max(0) as usize;
    format!("{}{}", " ".repeat(n), c.raw.trim_start())
}

struct Out {
    lines: Vec<String>,
    cm: Vec<Vec<String>>,
    lost: Vec<Lost>,
    hdr: HashMap<String, usize>,
}

impl Out {
    fn push(&mut self, l: &str) -> usize {
        self.lines.push(l.to_string());
        self.cm.push(vec![]);
        self.lines.len() - 1
    }
    fn lose(&mut self, fqn: &str, c: &Cm, reason: &'static str) {
        self.lost.push(Lost { fqn: fqn.to_string(), line: c.line, text: comment_text(&c.raw), reason });
    }
}

#[derive(PartialEq, Clone, Copy)]
enum Dec {
    H,
    L,
    Del,
}

enum Item {
    Blank,
    Def(String),
    Misc(usize),
    HMisc(usize),
}

pub fn merge(cur_h: &str, cur_l: &str, base: Option<&str>, take: Option<&str>, keep_lost: bool) -> Result<Option<Merged>, MergeErr> {
    let ph = parse(cur_h);
    let ll: Vec<String> = cur_l.split('\n').map(String::from).collect();
    if ph.lines == ll {
        return Ok(None);
    }
    if take == Some("ksnh") {
        return Ok(Some(Merged { ksnh: cur_h.to_string(), ksnl: ph.lines.join("\n"), lost: vec![], notes: vec![] }));
    }
    let base_parsed = base.map(parse);
    if take.is_none() && base_parsed.is_none() {
        return Err(MergeErr::Conflict(vec![
            "conflict: <whole file>: .ksnh と .ksnl が食い違い、前の版 (target/sync か git の HEAD の .ksnh) が取れない。どちらかを選ぶ: --take ksnh / --take ksnl".to_string(),
        ]));
    }
    let vh = view(&ph.lines);
    let vl = view(&ll);
    let vb = base_parsed.as_ref().map(|b| view(&b.lines));

    // 名前の変更: 人の側にだけある定義と LLM の側にだけある定義で、本体 (見出しと end を除く) が同じもの。
    let mut renamed: HashMap<String, String> = HashMap::new(); // 新 → 旧
    let mut notes = vec![];
    let inner = |d: &[String]| d[1..d.len() - 1].to_vec();
    let mut used_old: HashSet<String> = HashSet::new();
    for lk in &vl.order {
        if vh.defs.contains_key(lk) {
            continue;
        }
        let ld = vl.def(lk).unwrap();
        if ld.len() <= 2 {
            continue;
        }
        if let Some(hk) = vh.order.iter().find(|hk| !vl.defs.contains_key(*hk) && !used_old.contains(*hk) && inner(vh.def(hk).unwrap()) == inner(ld)) {
            used_old.insert(hk.clone());
            notes.push(format!("renamed {} -> {} (本体が同じなのでコメントを移した)", fqn_of(hk), fqn_of(lk)));
            renamed.insert(lk.clone(), hk.clone());
        }
    }

    // 定義ごとの決定。
    let mut keys: Vec<String> = vl.order.clone();
    for k in &vh.order {
        if !vl.defs.contains_key(k) {
            keys.push(k.clone());
        }
    }
    let mut dec: HashMap<String, Dec> = HashMap::new();
    let mut conflicts: Vec<String> = vec![];
    let conflict_msg = |what: &str, h: Option<&[String]>, l: Option<&[String]>, b: Option<&[String]>| {
        let mut s = format!("conflict: {} (人 (.ksnh) と LLM (.ksnl) が同じ定義を変えた。--take ksnh / --take ksnl で選ぶ)\n", what);
        let b_txt = b.map(|x| x.join("\n")).unwrap_or_default();
        for (name, side) in [("ksnh (人)", h), ("ksnl (LLM)", l)] {
            let t = side.map(|x| x.join("\n")).unwrap_or_default();
            let d = similar::TextDiff::from_lines(b_txt.as_str(), t.as_str());
            s.push_str(&format!("{}\n", d.unified_diff().header("base", name)));
        }
        s
    };
    for k in &keys {
        if renamed.values().any(|o| o == k) {
            dec.insert(k.clone(), Dec::Del);
            continue;
        }
        let h = vh.def(k);
        let l = vl.def(k);
        let b = vb.as_ref().and_then(|v| v.def(k));
        let d = if take == Some("ksnl") {
            if l.is_some() { Dec::L } else { Dec::Del }
        } else if h == l {
            Dec::L
        } else if vb.is_none() {
            Dec::L
        } else if b.is_none() {
            match (h.is_some(), l.is_some()) {
                (true, false) => Dec::H,
                (false, true) => Dec::L,
                _ => {
                    conflicts.push(conflict_msg(&fqn_of(k), h, l, b));
                    Dec::L
                }
            }
        } else {
            let (hc, lc) = (h != b, l != b);
            match (hc, lc) {
                (false, _) => if l.is_some() { Dec::L } else { Dec::Del },
                (true, false) => if h.is_some() { Dec::H } else { Dec::Del },
                (true, true) => {
                    conflicts.push(conflict_msg(&fqn_of(k), h, l, b));
                    Dec::L
                }
            }
        };
        dec.insert(k.clone(), d);
    }
    // 最上位の行 (import など)。
    let (hm, lm) = (vh.misc_texts(), vl.misc_texts());
    let misc_dec = if take == Some("ksnl") || hm == lm {
        Dec::L
    } else {
        let bm = vb.as_ref().map(|v| v.misc_texts()).unwrap_or_default();
        match (hm != bm, lm != bm) {
            (true, true) => {
                conflicts.push(conflict_msg("<最上位の行 (import など)>", None, None, None).replacen("\n", "\n", 1));
                Dec::L
            }
            (true, false) => Dec::H,
            _ => Dec::L,
        }
    };
    if !conflicts.is_empty() {
        return Err(MergeErr::Conflict(conflicts));
    }

    // 並び: LLM の並びを土台に、人が足した定義を人の並びの直前の定義の後ろに入れる。
    let mut items: Vec<Item> = vec![];
    for u in units(&ll) {
        match u {
            U::Def { key, .. } => items.push(Item::Def(key)),
            U::Line(i) => items.push(if ll[i].is_empty() { Item::Blank } else { Item::Misc(i) }),
        }
    }
    if misc_dec == Dec::H {
        let first = items.iter().position(|it| matches!(it, Item::Misc(_)));
        items.retain(|it| !matches!(it, Item::Misc(_)));
        let at = first.unwrap_or(0).min(items.len());
        for (n, &i) in vh.misc.iter().enumerate() {
            items.insert(at + n, Item::HMisc(i));
        }
    }
    for hk in &vh.order {
        if dec.get(hk) == Some(&Dec::H) && !vl.defs.contains_key(hk) {
            let hpos = vh.order.iter().position(|x| x == hk).unwrap();
            let pred = vh.order[..hpos].iter().rev().find_map(|p| items.iter().position(|it| matches!(it, Item::Def(k) if k == p)));
            let at = pred.map_or(0, |p| p + 1);
            items.insert(at, Item::Def(hk.clone()));
            items.insert(at, Item::Blank);
        }
    }

    let mut out = Out { lines: vec![], cm: vec![], lost: vec![], hdr: HashMap::new() };
    let mut misc_new: Vec<usize> = vec![];
    // 消える定義 (LLM が消した / 選ばれなかった)。
    for k in &vh.order {
        if dec.get(k) == Some(&Dec::Del) && !renamed.values().any(|o| o == k) {
            let (s, e) = vh.defs[k];
            for i in s..=e {
                for c in &ph.cm[i] {
                    out.lose(&fqn_of(k), c, "定義が消えた");
                }
            }
        }
    }
    for it in &items {
        match it {
            Item::Blank => {
                if !out.lines.is_empty() && out.lines.last().map(|l| !l.is_empty()).unwrap_or(false) {
                    out.push("");
                }
            }
            Item::HMisc(i) => {
                let n = out.push(&ph.lines[*i]);
                out.cm[n] = ph.cm[*i].iter().map(|c| c.raw.clone()).collect();
            }
            Item::Misc(i) => {
                misc_new.push(out.push(&ll[*i]));
            }
            Item::Def(k) => {
                let d = dec[k];
                if d == Dec::Del {
                    continue;
                }
                let new_def: Vec<String> = if d == Dec::H { vh.def(k).unwrap().to_vec() } else { vl.def(k).unwrap().to_vec() };
                let base_idx = out.lines.len();
                for l in &new_def {
                    out.push(l);
                }
                out.hdr.insert(fqn_of(k), base_idx);
                let old_key = if d == Dec::H { Some(k.clone()) } else if vh.defs.contains_key(k) { Some(k.clone()) } else { renamed.get(k).cloned() };
                let Some(ok) = old_key else { continue };
                let (hs, he) = vh.defs[&ok];
                let old = &ph.lines[hs..=he];
                let mut res: Vec<Option<usize>> = vec![None; old.len()];
                res[0] = Some(0);
                res[old.len() - 1] = Some(new_def.len() - 1);
                if d == Dec::H {
                    for (i, r) in res.iter_mut().enumerate() {
                        *r = Some(i);
                    }
                } else if old.len() > 2 && new_def.len() > 2 {
                    align(old, &new_def, (1, old.len() - 1), (1, new_def.len() - 1), &mut res);
                }
                for i in 0..old.len() {
                    if ph.cm[hs + i].is_empty() {
                        continue;
                    }
                    match res[i] {
                        Some(j) => {
                            for c in &ph.cm[hs + i] {
                                let r = reindent(c, &old[i], &new_def[j]);
                                out.cm[base_idx + j].push(r);
                            }
                        }
                        None => {
                            for c in ph.cm[hs + i].clone() {
                                out.lose(&fqn_of(k), &c, "文が消えた");
                            }
                        }
                    }
                }
            }
        }
    }
    // 最上位の行のコメント (LLM の側を採ったとき)。
    if misc_dec == Dec::L {
        let old: Vec<String> = vh.misc.iter().map(|&i| ph.lines[i].clone()).collect();
        let new: Vec<String> = misc_new.iter().map(|&i| out.lines[i].clone()).collect();
        let mut res: Vec<Option<usize>> = vec![None; old.len()];
        align(&old, &new, (0, old.len()), (0, new.len()), &mut res);
        for (n, &hi) in vh.misc.iter().enumerate() {
            for c in &ph.cm[hi] {
                match res[n] {
                    Some(j) => {
                        let r = reindent(c, &old[n], &new[j]);
                        out.cm[misc_new[j]].push(r);
                    }
                    None => out.lose("<top>", c, "文が消えた"),
                }
            }
        }
    }
    // 最上位の行や定義の間にあったコメントで、LLM の並びのどの行にも付かなかったもの (空行の直前など)。
    for u in units(&ph.lines) {
        if let U::Line(i) = u {
            if ph.lines[i].is_empty() {
                for c in &ph.cm[i] {
                    out.lose("<top>", c, "経路が一致しない");
                }
            }
        }
    }
    for c in &ph.trailing {
        out.lose("<top>", c, "経路が一致しない");
    }
    if out.lines.last().map(|l| !l.is_empty()).unwrap_or(true) {
        out.push("");
    }
    if keep_lost {
        for l in out.lost.clone() {
            let idx = out.hdr.get(&l.fqn).copied().unwrap_or(0);
            out.cm[idx].push(format!("// LOST (消えた {}): {}", l.fqn, l.text));
        }
    }
    // 組み立て。
    let mut text: Vec<String> = vec![];
    if !ph.doc.is_empty() {
        text.extend(ph.doc.iter().map(|c| c.raw.clone()));
        text.push(String::new());
    }
    for (i, l) in out.lines.iter().enumerate() {
        text.extend(out.cm[i].iter().cloned());
        text.push(l.clone());
    }
    let ksnh = text.join("\n");
    let ksnl = out.lines.join("\n");
    if crate::ksnh::strip(&ksnh) != ksnl {
        return Err(MergeErr::Internal("付け直した .ksnh を strip しても新しい .ksnl に一致しない (内部エラー。何も書いていない)".to_string()));
    }
    Ok(Some(Merged { ksnh, ksnl, lost: out.lost, notes }))
}

// ---- 基準 (target/sync) ----

pub fn base_path(root: &Path, module: &str) -> PathBuf {
    root.join("target").join("sync").join(format!("{}.ksnh", module))
}

pub fn lost_path(root: &Path, module: &str) -> PathBuf {
    root.join("target").join("sync").join("lost").join(format!("{}.json", module))
}

/// `.ksnl` と整合している `.ksnh` を、次の `ksn sync` の基準として保存する。
pub fn save_base(root: &Path, module: &str, text: &str) {
    let p = base_path(root, module);
    if std::fs::read_to_string(&p).ok().as_deref() == Some(text) {
        return;
    }
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = std::fs::write(&p, text);
}

/// 基準: `target/sync/<モジュール>.ksnh`、無ければ git の HEAD の `.ksnh`。
pub fn load_base(root: &Path, module: &str, ksnh_path: &Path) -> Option<String> {
    if let Ok(t) = std::fs::read_to_string(base_path(root, module)) {
        return Some(t);
    }
    let rel = ksnh_path.strip_prefix(root).ok()?.to_string_lossy().replace('\\', "/");
    let o = std::process::Command::new("git").current_dir(root).args(["show", &format!("HEAD:./{}", rel)]).output().ok()?;
    if o.status.success() {
        Some(String::from_utf8_lossy(&o.stdout).into_owned())
    } else {
        None
    }
}
