//! `match` の網羅性と到達しない腕の検査 (パターン行列。Rust の usefulness と同じ考え方)。
//! 入れ子のパターンも列ごとに分解する。整数・`char`・`&str` などのリテラルは無限の族で、`_` か束縛がなければ網羅しない。

use std::rc::Rc;

/// 構築子の族: `Some` は有限の族 (名前と引数の数の一覧)、`None` は無限の族 (リテラルなど)。
pub type Fam = Option<Rc<Vec<(String, usize)>>>;

#[derive(Clone, Debug)]
pub enum XP {
    Wild,
    Ctor { fam: Fam, key: String, args: Vec<XP> },
}

fn arity_of(fam: &Fam, key: &str, rows: &[Vec<XP>]) -> usize {
    if let Some(f) = fam {
        if let Some(x) = f.iter().find(|x| x.0 == key) {
            return x.1;
        }
    }
    for r in rows {
        if let XP::Ctor { key: k, args, .. } = &r[0] {
            if k == key {
                return args.len();
            }
        }
    }
    0
}

fn specialize(rows: &[Vec<XP>], key: &str, arity: usize) -> Vec<Vec<XP>> {
    let mut out = vec![];
    for r in rows {
        match &r[0] {
            XP::Ctor { key: k, args, .. } if k == key => {
                let mut n = args.clone();
                n.extend_from_slice(&r[1..]);
                out.push(n);
            }
            XP::Ctor { .. } => {}
            XP::Wild => {
                let mut n = vec![XP::Wild; arity];
                n.extend_from_slice(&r[1..]);
                out.push(n);
            }
        }
    }
    out
}

fn default_rows(rows: &[Vec<XP>]) -> Vec<Vec<XP>> {
    rows.iter().filter(|r| matches!(r[0], XP::Wild)).map(|r| r[1..].to_vec()).collect()
}

/// 先頭の列に現れる構築子 (キーの重複なし) と、その族。
fn heads(rows: &[Vec<XP>]) -> (Vec<String>, Fam) {
    let mut keys: Vec<String> = vec![];
    let mut fam: Fam = None;
    for r in rows {
        if let XP::Ctor { key, fam: f, .. } = &r[0] {
            if !keys.contains(key) {
                keys.push(key.clone());
            }
            fam = f.clone();
        }
    }
    (keys, fam)
}

fn complete(keys: &[String], fam: &Fam) -> bool {
    match fam {
        Some(f) => f.iter().all(|x| keys.contains(&x.0)),
        None => false,
    }
}

/// 行列 `rows` に覆われない値の組が `q` にあるか。
pub fn useful(rows: &[Vec<XP>], q: &[XP]) -> bool {
    if q.is_empty() {
        return rows.is_empty();
    }
    match &q[0] {
        XP::Ctor { key, args, .. } => {
            let mut nq = args.clone();
            nq.extend_from_slice(&q[1..]);
            useful(&specialize(rows, key, args.len()), &nq)
        }
        XP::Wild => {
            let (keys, fam) = heads(rows);
            if !keys.is_empty() && complete(&keys, &fam) {
                for (k, a) in fam.as_ref().unwrap().iter() {
                    let mut nq = vec![XP::Wild; *a];
                    nq.extend_from_slice(&q[1..]);
                    if useful(&specialize(rows, k, *a), &nq) {
                        return true;
                    }
                }
                false
            } else {
                useful(&default_rows(rows), &q[1..])
            }
        }
    }
}

fn show(key: &str, args: &[String]) -> String {
    if key == "tuple" {
        return format!("({})", args.join(", "));
    }
    if key == "()" {
        return "()".into();
    }
    if key.starts_with("lit:") {
        return key[4..].to_string();
    }
    let name = key.rsplit('.').next().unwrap_or(key);
    if args.is_empty() { name.to_string() } else { format!("{}({})", name, args.join(", ")) }
}

/// 覆われない値の組の例 (`n` 列)。網羅していれば `None`。
pub fn witness(rows: &[Vec<XP>], n: usize) -> Option<Vec<String>> {
    if n == 0 {
        return if rows.is_empty() { Some(vec![]) } else { None };
    }
    let (keys, fam) = heads(rows);
    if !keys.is_empty() && complete(&keys, &fam) {
        for (k, a) in fam.as_ref().unwrap().iter() {
            if let Some(mut w) = witness(&specialize(rows, k, *a), a + n - 1) {
                let rest = w.split_off(*a);
                let mut out = vec![show(k, &w)];
                out.extend(rest);
                return Some(out);
            }
        }
        return None;
    }
    let mut w = witness(&default_rows(rows), n - 1)?;
    let head = match &fam {
        Some(f) => match f.iter().find(|x| !keys.contains(&x.0)) {
            Some((k, a)) => show(k, &vec!["_".to_string(); *a]),
            None => "_".into(),
        },
        None => "_".into(),
    };
    w.insert(0, head);
    Some(w)
}

/// 腕のパターンの列 (1 列) から、覆われない例と、前の腕に覆われて到達しない腕の添字を返す。
pub fn analyze(pats: &[XP]) -> (Option<String>, Vec<usize>) {
    let rows: Vec<Vec<XP>> = pats.iter().map(|p| vec![p.clone()]).collect();
    let mut dead = vec![];
    for i in 1..rows.len() {
        if !useful(&rows[..i], &rows[i]) {
            dead.push(i);
        }
    }
    (witness(&rows, 1).map(|w| w[0].clone()), dead)
}
