//! NLL 借用検査の近似 (誤検出の取り消しフィルタ)。
//!
//! 既存の字句的な借用検査 (check.rs の `borrows` / tc_block) は、「借用変数 `r`
//! が最後に言及される文まで」を借用の生存期間とみなす。多くの場合それは NLL
//! (Rust 2021 相当) と一致するが、1 点だけ字句的に広すぎる: **借用変数への再代入**。
//! `r = &mut y` は `r` への言及だが、古い借用の *利用* ではない。代入は古い値を捨てる
//! だけなので、使われることのない借用はそこで (あるいは作られた直後から) 死んでいる
//! と NLL はみなす。例:
//!
//! ```text
//! let mut r = &mut x
//! *r = 9u8        // 借用の最後の利用 → ここで死ぬ
//! let z = x       // NLL では OK。字句検査は E3010 (r への言及がまだ先にある)
//! r = &mut y      // この再代入が字句検査の誤検出を引き起こす
//! ```
//!
//! このモジュールは、fn 本体 (とクロージャの本体) の文の列を解析し、既存の字句検査が
//! 報告した E3010 のうち「実際には借用が死んでいて衝突が成立しない」と確信できるものを
//! 取り消す。確信できない (ループやクロージャが絡む・ネストした再代入など) 場合は、
//! 既存の字句判定をそのまま通す。新しい診断コードは作らず、新規の報告もしない
//! (誤検出より取りこぼしの統一姿勢)。

use std::collections::BTreeMap;

use crate::check::{children, items, nf, nline, nname, st, walk};
use crate::parser::V;

/// (行, メッセージ) ごとに、字句検査が出したであろう E3010 候補の総数と、
/// そのうち「確信して取り消せる」数を覚える表。
pub type Cancels = BTreeMap<(usize, String), (usize, usize)>;

/// fn 本体の解析。本体の文の列と、その中の全クロージャの本体 (字句検査が
/// 同じく走る場所) の文の列を調べ、確信できる取り消しの集合を返す。
pub fn cancellable(body: &V) -> Cancels {
    let mut out = Cancels::new();
    if nname(body) == "Block" {
        let sts: Vec<&V> = items(&nf(body)[0]).iter().collect();
        analyse_list(&sts, &mut out);
    }
    let mut lists: Vec<Vec<&V>> = vec![];
    closure_lists(body, &mut lists);
    for ls in &lists {
        analyse_list(ls.as_slice(), &mut out);
    }
    out
}

/// 字句検査が走るのは fn 本体とクロージャの本体だけ (tc_block)。クロージャの
/// 本体の文の列を集める (ネストしたクロージャの分も含む)。
fn closure_lists<'a>(v: &'a V, out: &mut Vec<Vec<&'a V>>) {
    for c in children(v) {
        if nname(c) == "Closure" {
            let f = nf(c);
            if nname(&f[3]) == "Block" {
                out.push(items(&nf(&f[3])[0]).iter().collect());
            }
        }
        closure_lists(c, out);
    }
}

// ---------------------------------------------------------------- 文の列の解析

#[derive(Clone, Copy)]
struct Flags {
    flat: bool,
    in_loop: bool,
    in_closure: bool,
}

#[derive(PartialEq)]
enum Kind {
    /// 借用変数の利用 (言及)。
    Use,
    /// トップレベルの文そのものの再代入 (`r = &mut y`)。古い借用はここで死ぬとみなせる。
    Kill,
    /// ネストした場所・ループ・クロージャの中での再代入。経路依存で扱いきれない。
    KillMaybe,
}

struct Ev {
    pos: usize,
    kind: Kind,
}

fn first_seg(v: &V) -> String {
    let segs = items(&nf(v)[0]);
    if !segs.is_empty() {
        st(&segs[0])
    } else {
        String::new()
    }
}

/// check.rs `mentions` と同じ述語: 部分木に「先頭セグメントが name の Path」があるか。
fn mentions(v: &V, name: &str) -> bool {
    let mut all = vec![];
    walk(v, &mut all);
    all.iter().any(|n| nname(n) == "Path" && first_seg(n) == name)
}

/// check.rs `mutates` と同じ述語: `name = v`・`name.f = v`・`&mut name` があるか。
fn mutates(v: &V, name: &str) -> bool {
    let mut all = vec![];
    walk(v, &mut all);
    all.iter().any(|n| {
        let f = nf(n);
        match nname(n) {
            "Assign" => nname(&f[0]) == "Path" && first_seg(&f[0]) == name,
            "Unary" => st(&f[0]) == "&mut" && nname(&f[1]) == "Path" && first_seg(&f[1]) == name,
            _ => false,
        }
    })
}

/// 1 つの文について、`target` (借用変数の名前) への言及・再代入を集める。
/// check.rs `mentions` の walk と同じ範囲をたどるので、言及の対象は一致する。
fn collect(v: &V, fl: Flags, target: &str, pos: usize, evs: &mut Vec<Ev>) {
    match nname(v) {
        "Path" => {
            if first_seg(v) == target {
                evs.push(Ev { pos, kind: Kind::Use });
            }
        }
        "Assign" => {
            let t = &nf(v)[0];
            let rhs = &nf(v)[1];
            if nname(t) == "Path" && first_seg(t) == target {
                let kind = if nname(rhs) == "Unary" && matches!(st(&nf(rhs)[0]).as_str(), "&" | "&mut") {
                    if fl.flat && !fl.in_loop && !fl.in_closure {
                        Kind::Kill
                    } else {
                        Kind::KillMaybe
                    }
                } else {
                    // 借用値以外の再代入は型検査の担当。黙って利用として扱う (誤検出禁止)。
                    Kind::Use
                };
                evs.push(Ev { pos, kind });
            }
            let fl2 = Flags { flat: false, ..fl };
            for c in children(v) {
                collect(c, fl2, target, pos, evs);
            }
        }
        // ループの中での再利用は「ループ出口まで生存」とみなす (扱いきれないため
        // 字句判定に戻す)。条件部・列挙子も含めて、部分木全体を同じ扱いにする。
        _ if matches!(nname(v), "Loop" | "While" | "For") => {
            let fl2 = Flags { flat: false, in_loop: true, ..fl };
            for c in children(v) {
                collect(c, fl2, target, pos, evs);
            }
        }
        // クロージャは借用を capture する。capture 節の寿命は追跡できないので、
        // この借用が絡む衝突は字句判定のままにする。
        _ if nname(v) == "Closure" => {
            let fl2 = Flags { flat: false, in_closure: true, ..fl };
            for c in children(v) {
                collect(c, fl2, target, pos, evs);
            }
        }
        _ => {
            let fl2 = Flags { flat: false, ..fl };
            for c in children(v) {
                collect(c, fl2, target, pos, evs);
            }
        }
    }
}

/// 1 つの文の列 (fn 本体のトップレベル、またはクロージャの本体) の解析。
fn analyse_list(sts: &[&V], out: &mut Cancels) {
    for (i, s0) in sts.iter().enumerate() {
        // check.rs `borrows` と同じ条件で借用変数を拾う。
        if nname(s0) != "Let" {
            continue;
        }
        let f = nf(s0);
        if nname(&f[0]) != "PBind" {
            continue;
        }
        let e = &f[3];
        if nname(e) != "Unary" || !matches!(st(&nf(e)[0]).as_str(), "&" | "&mut") {
            continue;
        }
        let inner = &nf(e)[1];
        if nname(inner) != "Path" || items(&nf(inner)[0]).len() != 1 {
            continue;
        }
        let r = st(&nf(&f[0])[0]);
        let x = st(&items(&nf(inner)[0])[0]);
        let m = st(&nf(e)[0]) == "&mut";
        analyse_pair(sts, i, &r, &x, m, out);
    }
}

/// 1 つの借用 (借用変数 `r` が `&x` / `&mut x` を持つ) の解析。
/// 既存の字句検査が報告する衝突候補をその位置・文面で再構成し、NLL で
/// 「その位置で借用がすでに死んでいる」と確信できるものだけ取り消しに数える。
fn analyse_pair(sts: &[&V], i: usize, r: &str, x: &str, m: bool, out: &mut Cancels) {
    // 借用変数の言及・再代入イベントを集める。
    let mut evs: Vec<Ev> = vec![];
    for (j, s) in sts.iter().enumerate() {
        if j > i {
            collect(s, Flags { flat: true, in_loop: false, in_closure: false }, r, j, &mut evs);
        }
    }
    // 字句検査と同じ「最後の言及」までが候補の範囲。
    let last = evs.iter().map(|e| e.pos).max().unwrap_or(i);
    if last <= i {
        return;
    }
    let mut cand: Vec<(usize, String)> = vec![]; // (文の位置, 報告文面)
    for k in i + 1..last {
        if mentions(sts[k], r) {
            continue;
        }
        if m && mentions(sts[k], x) {
            cand.push((k, format!("cannot use `{}` while it is mutably borrowed by `{}`", x, r)));
        } else if !m && mutates(sts[k], x) {
            cand.push((k, format!("cannot mutate `{}` while it is borrowed by `{}`", x, r)));
        }
    }
    if cand.is_empty() {
        return;
    }
    // 扱いきれない要素 (ループ・クロージャ・経路依存の再代入) がある借用は、
    // 字句判定のままにする (誤検出より取りこぼし)。
    let uncertain = evs.iter().any(|e| match e.kind {
        Kind::Use => false,
        Kind::Kill => false,
        Kind::KillMaybe => true,
    });
    if uncertain {
        note(sts, &cand, None, out);
        return;
    }
    // 再代入で区切られた「借用の実体」ごとに生存期間を計算する。
    // 実体の生存期間は「作られた位置から、その実体の最後の利用位置まで」。
    let mut bounds: Vec<usize> = evs.iter().filter(|e| e.kind == Kind::Kill).map(|e| e.pos).collect();
    bounds.sort();
    bounds.dedup();
    let mut spans: Vec<(usize, usize)> = vec![]; // (開始位置, 排他終端)
    let mut start = i + 1;
    for &b in &bounds {
        spans.push((start, b));
        start = b; // 再代入の文は新しい実体の作られた位置
    }
    spans.push((start, sts.len()));
    let mut keep = vec![false; cand.len()];
    let mut regions: Vec<Option<usize>> = vec![];
    for (a, b) in &spans {
        let end = evs
            .iter()
            .filter(|e| e.kind == Kind::Use && e.pos >= *a && e.pos < *b)
            .map(|e| e.pos)
            .max();
        regions.push(end);
    }
    for (ci, (k, _)) in cand.iter().enumerate() {
        let active = spans.iter().zip(&regions).any(|((a, b), re)| {
            *a <= *k && *k <= *b && re.map_or(false, |re| *k <= re)
        });
        keep[ci] = active;
    }
    note(sts, &cand, Some(&keep), out);
}

/// 候補を (行, 文面) の表に書き込む。`decide` が None のときは取り消しを数えず、
/// 候補の総数だけを覚える (一切取り消さない)。同じ (行, 文面) の集合として
/// 全てが同じ判定になるときにのみ有効にする (部分的な除去は安定性のため避ける)。
fn note(sts: &[&V], cand: &[(usize, String)], decide: Option<&[bool]>, out: &mut Cancels) {
    // 同じ (行, 文面) に複数の候補があるときは、その組が同じ判定のときにのみ数える。
    let mut by_key: BTreeMap<(usize, String), (usize, usize)> = BTreeMap::new();
    for (ci, (k, msg)) in cand.iter().enumerate() {
        let line = nline(sts[*k]);
        let e = by_key.entry((line, msg.clone())).or_insert((0, 0));
        e.1 += 1;
        if decide.map_or(true, |keep| !keep[ci]) {
            e.0 += 1;
        }
    }
    for (key, (c, t)) in by_key {
        let e = out.entry(key).or_insert((0, 0));
        e.0 += c;
        e.1 += t;
    }
}
