//! KIR の決定的な最適化パス (設計_KIR.md 第 5 章 段階 3 の前半)。
//! 各パスは `kir::verify_module` を通す。意味を変えない変換だけを行う。
//!
//! 設計判断: すべてのパスを 1 つのモジュールにまとめ、ファイル分割を最小化する。
//! 各パスは独立した関数で、`Module` を受け取り `Module` を返す。

use std::collections::{HashMap, HashSet};

use crate::kir::{self, Aop, BitK, BoolOp, Cc, FBin, FUn, Func, Inst, Mode, Module, Op, Term, Ty, VId, Block, operands};

/// 最適化のエントリポイント。全パスを順に適用し、各パスの後に `verify_module` を通す。
/// 検証に失敗した場合は、その時点までの結果を返さずエラーにする（内部不整合のため）。
pub fn optimize(m: &Module) -> Result<Module, String> {
    let mut m = m.clone();
    m = const_fold(&m)?;
    m = dce(&m)?;
    m = copy_prop(&m)?;
    m = inline_small(&m)?;
    Ok(m)
}

/// 使われない命令の除去 (パス 2)。副作用の無い命令だけを除去する。
/// 呼び出し・メモリ・panic・write は副作用があるとみなして残す。
fn dce(m: &Module) -> Result<Module, String> {
    after(m, |m| {
        let mut m = m.clone();
        for f in &mut m.funcs {
            let mut changed = true;
            while changed {
                changed = false;
                // 現在の命令・終端命令が参照する値番号の集合。
                let mut referenced: HashSet<VId> = HashSet::new();
                for b in &f.blocks {
                    for i in &b.insts {
                        for a in operands(&i.op) {
                            referenced.insert(a);
                        }
                    }
                    match &b.term {
                        Term::Br(t) => referenced.extend(t.1.iter().copied()),
                        Term::BrIf(c, t1, t2) => {
                            referenced.insert(*c);
                            referenced.extend(t1.1.iter().copied());
                            referenced.extend(t2.1.iter().copied());
                        }
                        Term::Ret(v) => { referenced.insert(*v); }
                        Term::Unreachable => {}
                    }
                }
                let dead: Vec<(usize, usize)> = f
                    .blocks
                    .iter_mut()
                    .enumerate()
                    .flat_map(|(bi, b)| {
                        b.insts
                            .iter()
                            .enumerate()
                            .filter(|(_, i)| {
                                !side_effecting(&i.op) && !referenced.contains(&i.dst)
                            })
                            .map(move |(ii, _)| (bi, ii))
                            .collect::<Vec<_>>()
                    })
                    .collect();
                // 後ろから削除して添字をずらさない。
                for (bi, ii) in dead.into_iter().rev() {
                    f.blocks[bi].insts.remove(ii);
                    changed = true;
                }
            }
        }
        m
    }, "使われない命令の除去")
}

/// 命令が副作用を持つか。副作用を持つ命令は結果が未使用でも除去しない。
fn side_effecting(op: &Op) -> bool {
    match op {
        // 実時刻は呼ぶたびに値が変わる (結果が未使用でも読みは残す)。
        Op::TimeNowNs => true,
        // 乱数は呼ぶたびに値が変わり、バッファへ書く (結果が未使用でも除かない)。
        Op::RandFill { .. } => true,
        Op::Call { .. }
        | Op::Write { .. }
        | Op::Eprint { .. }
        | Op::FmtFloat { .. }
        | Op::NetListen { .. }
        | Op::NetConnect { .. }
        | Op::NetAccept { .. }
        | Op::NetRead { .. }
        | Op::NetWrite { .. }
        | Op::NetFlush { .. }
        | Op::NetClose { .. }
        | Op::NetLocalPort { .. }
        | Op::NetSys { .. }
        | Op::Proc { .. }
        | Op::FsCreate { .. }
        | Op::FsOpen { .. }
        | Op::FsExists { .. }
        | Op::FsRemove { .. }
        | Op::FsSys { .. }
        | Op::FsClose { .. }
        | Op::FsFlush { .. }
        | Op::FsRead { .. }
        | Op::FsSync { .. }
        | Op::FsWrite { .. }
        | Op::Abort
        | Op::Panic { .. }
        | Op::Store { .. }
        | Op::MemStore { .. }
        | Op::HeapFree(_)
        | Op::HeapAlloc(_)
        | Op::HeapRealloc { .. }
        | Op::IndexCheck { .. }
        | Op::Alloca
        | Op::Load(_)
        | Op::MemLoad { .. }
        | Op::Utf8Valid { .. }
        | Op::FrameBytes { .. } => true,
        // 純粋な演算・構築・変換は副作用を持たない。
        Op::Const(_)
        | Op::Arith { .. }
        | Op::Cmp { .. }
        | Op::Neg { .. }
        | Op::FArith { .. }
        | Op::FUn { .. }
        | Op::FBin { .. }
        | Op::Shift { .. }
        | Op::MinMax { .. }
        | Op::Conv { .. }
        | Op::Bitcast { .. }
        | Op::Not(_)
        | Op::Bool(..)
        | Op::BitCount { .. }
        | Op::Tag(_)
        | Op::Payload(_)
        | Op::Some(_)
        | Op::Ok(_)
        | Op::Err(_)
        | Op::Checked { .. }
        | Op::Make { .. }
        | Op::Field { .. }
        | Op::SetField { .. }
        | Op::PtrAdd { .. }
        | Op::StaticBytes(_)
        | Op::GlobalBytes { .. }
        | Op::AsShared(_)
        | Op::ParseInt { .. }
        | Op::FnPtr { .. }
        | Op::EnvArgCount
        | Op::EnvArg { .. }
        | Op::EnvVar { .. } => false,
        | Op::Reborrow(_)
        | Op::Borrow { .. }
        | Op::ScopeEnd(_)
        | Op::Own { .. }
        | Op::FieldAddr { .. } => false,
    }
}

/// コピーの伝播 (パス 3)。
/// `%dst = reborrow %src` (同じアドレスを指すコピー) なら、`%dst` の使用を `%src` に
/// 置き換えて reborrow 命令を除去する。意味は変わらない（同じアドレス）。
fn copy_prop(m: &Module) -> Result<Module, String> {
    after(m, |m| {
        let mut m = m.clone();
        for f in &mut m.funcs {
            let mut changed = true;
            while changed {
                changed = false;
                // dst → src のコピー関係 (reborrow は同じアドレスを指すコピー) を収集。
                let copies: Vec<(VId, VId)> = f
                    .blocks
                    .iter()
                    .flat_map(|b| {
                        b.insts.iter().filter_map(|i| {
                            if let Op::Reborrow(a) | Op::Borrow { a, .. } = i.op {
                                Some((i.dst, a))
                            } else {
                                None
                            }
                        })
                    })
                    .collect();
                if copies.is_empty() {
                    break;
                }
                // 連鎖 (%c = reborrow %b, %b = reborrow %a) を辿り、各 dst の最終的な送り先 (root) を求める。
                let copy_map: HashMap<VId, VId> = copies
                    .iter()
                    .filter_map(|(dst, mut src)| {
                        let mut visited = HashSet::new();
                        while let Some(&nxt) = copies.iter().find(|(d, _)| *d == src).map(|(_, s)| s) {
                            if !visited.insert(src) {
                                break; // 環
                            }
                            src = nxt;
                        }
                        if src == *dst {
                            None
                        } else {
                            Some((*dst, src))
                        }
                    })
                    .collect();
                if copy_map.is_empty() {
                    break;
                }
                let copy_dsts: HashSet<VId> = copy_map.keys().copied().collect();
                // 全命令の被演算子と終端命令の参照を root に置き換える。
                for b in &mut f.blocks {
                    for i in &mut b.insts {
                        for a in operands_mut(&mut i.op) {
                            if let Some(&s) = copy_map.get(a) {
                                *a = s;
                                changed = true;
                            }
                        }
                    }
                    match &mut b.term {
                        Term::Br(t) => {
                            for a in &mut t.1 {
                                if let Some(&s) = copy_map.get(a) {
                                    *a = s;
                                    changed = true;
                                }
                            }
                        }
                        Term::BrIf(c, t1, t2) => {
                            if let Some(&s) = copy_map.get(c) {
                                *c = s;
                                changed = true;
                            }
                            for a in &mut t1.1 {
                                if let Some(&s) = copy_map.get(a) {
                                    *a = s;
                                    changed = true;
                                }
                            }
                            for a in &mut t2.1 {
                                if let Some(&s) = copy_map.get(a) {
                                    *a = s;
                                    changed = true;
                                }
                            }
                        }
                        Term::Ret(v) => {
                            if let Some(&s) = copy_map.get(v) {
                                *v = s;
                                changed = true;
                            }
                        }
                        Term::Unreachable => {}
                    }
                }
                // reborrow 命令 (コピーの定義) を除去。
                for b in &mut f.blocks {
                    let pos: Vec<usize> = b
                        .insts
                        .iter()
                        .enumerate()
                        .filter(|(_, i)| copy_dsts.contains(&i.dst))
                        .map(|(pos, _)| pos)
                        .collect();
                    for pos in pos.into_iter().rev() {
                        b.insts.remove(pos);
                        changed = true;
                    }
                }
            }
        }
        m
    }, "コピーの伝播")
}

/// 命令の被演算子を可変に参照する `Vec` を返す。
fn operands_mut(op: &mut Op) -> Vec<&mut VId> {
    match op {
        Op::Const(_) | Op::Alloca | Op::FrameBytes { .. } | Op::StaticBytes(_) | Op::GlobalBytes { .. } | Op::FnPtr { .. } => vec![],
        Op::PtrAdd { p, off } => vec![p, off],
        Op::MemLoad { p, .. } | Op::HeapFree(p) => vec![p],
        Op::HeapAlloc(n) => vec![n],
        Op::HeapRealloc { p, size } => vec![p, size],
        Op::MemStore { p, v, .. } => vec![p, v],
        Op::IndexCheck { i, len } => vec![i, len],
        Op::Panic { arg, .. } => arg.iter_mut().collect(),
        Op::Write { fd, data } => vec![fd, data],
        Op::Eprint { data } => vec![data],
        Op::FmtFloat { a, .. } => vec![a],
        Op::Utf8Valid { p, len } => vec![p, len],
        Op::NetListen { addr } | Op::NetConnect { addr } => vec![addr],
        Op::NetAccept { sock } | Op::NetFlush { sock } | Op::NetClose { sock } | Op::NetLocalPort { sock } => vec![sock],
        Op::NetSys { args, .. } => args.iter_mut().collect(),
        Op::NetRead { sock, buf, len } | Op::NetWrite { sock, buf, len } => vec![sock, buf, len],
        Op::EnvArgCount => vec![],
        Op::EnvArg { index } | Op::EnvVar { name: index } => vec![index],
        Op::TimeNowNs => vec![],
        Op::RandFill { buf, len, .. } => vec![buf, len],
        Op::Proc { args, .. } => args.iter_mut().collect(),
        Op::FsCreate { path } | Op::FsOpen { path } | Op::FsExists { path } | Op::FsRemove { path } => vec![path],
        Op::FsSys { args, .. } => args.iter_mut().collect(),
        Op::FsClose { fd } | Op::FsFlush { fd } | Op::FsSync { fd } => vec![fd],
        Op::FsRead { fd, buf, len } | Op::FsWrite { fd, buf, len } => vec![fd, buf, len],
        Op::ParseInt { s, radix, .. } => vec![s, radix],
        Op::Abort => vec![],
        Op::Arith { a, b, .. }
        | Op::Cmp { a, b, .. }
        | Op::Bool(_, a, b)
        | Op::Checked { a, b, .. }
        | Op::FArith { a, b, .. }
        | Op::MinMax { a, b, .. }
        | Op::FBin { a, b, .. } => vec![a, b],
        Op::Shift { a, n, .. } => vec![a, n],
        Op::Store { ptr, val } => vec![ptr, val],
        Op::Neg { a, .. }
        | Op::Not(a)
        | Op::FUn { a, .. }
        | Op::Conv { a, .. }
        | Op::Bitcast { a, .. }
        | Op::BitCount { a, .. }
        | Op::Load(a)
        | Op::AsShared(a)
        | Op::Reborrow(a)
        | Op::Borrow { a, .. }
        | Op::ScopeEnd(a)
        | Op::Own { a, .. }
        | Op::Some(a)
        | Op::Ok(a)
        | Op::Err(a)
        | Op::Tag(a)
        | Op::Payload(a) => vec![a],
        Op::Call { args, .. } => args.iter_mut().collect(),
        Op::Make { fields, .. } => fields.iter_mut().collect(),
        Op::Field { a, .. } | Op::FieldAddr { a, .. } => vec![a],
        Op::SetField { a, v, .. } => vec![a, v],
    }
}

/// 小さな関数のインライン化 (パス 4)。
/// 命令数がしきい値以下で再帰しない関数を、呼び出し点に展開する。
/// 再帰関数は展開しない（深さの数え方 102 を変えないため）。
const INLINE_INST_LIMIT: usize = 12;

fn inline_small(m: &Module) -> Result<Module, String> {
    after(m, |m| {
        let mut m = m.clone();
        let recursive = recursive_keys(&m);
        let candidates: HashSet<String> = m
            .funcs
            .iter()
            // `extern` (link を持つ) は本体が仮の `unreachable` なので展開しない。
            .filter(|f| f.link.is_none() && !f.key.starts_with("ksn.thr.") && !recursive.contains(f.key.as_str()) && inst_count(f) <= INLINE_INST_LIMIT)
            .map(|f| f.key.clone())
            .collect();
        let mut changed = true;
        while changed {
            changed = false;
            'outer: for fi in 0..m.funcs.len() {
                for bi in 0..m.funcs[fi].blocks.len() {
                    let b = &m.funcs[fi].blocks[bi];
                    let Some(pos) = b.insts.iter().position(|i| {
                        matches!(i.op, Op::Call { ref callee, .. } if candidates.contains(callee.as_str()))
                    }) else {
                        continue;
                    };
                    let Op::Call { callee, .. } = &b.insts[pos].op else { continue };
                    let callee_key = callee.clone();
                    let callee = m
                        .funcs
                        .iter()
                        .find(|f| f.key == callee_key)
                        .unwrap()
                        .clone();
                    match inline_call_block(&mut m.funcs[fi], bi, pos, &callee) {
                        Ok(()) => {
                            changed = true;
                            break 'outer;
                        }
                        Err(_) => continue, // 展開できない呼び出しはスキップ
                    }
                }
            }
        }
        m
    }, "小さな関数のインライン化")
}

/// 関数の命令数（終端命令を除く）。
fn inst_count(f: &Func) -> usize {
    f.blocks.iter().map(|b| b.insts.len()).sum()
}

/// 再帰（直接・間接）を持つ関数の鍵の集合。
fn recursive_keys(m: &Module) -> HashSet<String> {
    let mut rec: HashSet<String> = HashSet::new();
    let adj: HashMap<&str, Vec<&str>> = m
        .funcs
        .iter()
        .map(|f| {
            let mut callees = vec![];
            for b in &f.blocks {
                for i in &b.insts {
                    if let Op::Call { callee, .. } = &i.op {
                        callees.push(callee.as_str());
                    }
                }
            }
            (f.key.as_str(), callees)
        })
        .collect();
    for k in adj.keys() {
        let mut seen: HashSet<&str> = HashSet::new();
        let mut todo = vec![*k];
        while let Some(x) = todo.pop() {
            if !seen.insert(x) {
                continue;
            }
            for &c in adj.get(x).unwrap_or(&vec![]) {
                if c == *k {
                    rec.insert(k.to_string());
                }
                todo.push(c);
            }
        }
    }
    rec
}

/// 呼び出し命令（ブロック `bidx` の `ii` 番目）を callee の本体で展開する。
/// `caller` は展開先の関数。`callee` は展開元（コピーされる）。
fn inline_call_block(
    caller: &mut Func,
    bidx: usize,
    ii: usize,
    callee: &Func,
) -> Result<(), String> {
    let inst = caller.blocks[bidx].insts[ii].clone();
    let Op::Call { callee: _key, args } = &inst.op else {
        return Err("call ではない".to_string());
    };
    let dst = inst.dst;
    let ret_ty = inst.ty;
    let args = args.clone();

    // 現 caller の最大値番号 + 1 を、callee の値番号の振り直しの基準とする。
    let max_val = caller
        .blocks
        .iter()
        .flat_map(|b| {
            b.insts
                .iter()
                .map(|i| i.dst)
                .chain(b.params.iter().map(|(v, _)| *v))
        })
        .chain(caller.blocks.iter().flat_map(|b| match &b.term {
            Term::Ret(v) => vec![*v],
            Term::BrIf(c, _, _) => vec![*c],
            Term::Br(_) => vec![],
            Term::Unreachable => vec![],
        }))
        .max()
        .unwrap_or(0);
    let offset = max_val + 1;

    // 引数の個数を確認。
    if callee.blocks[0].params.len() != args.len() {
        return Err(format!("{} の引数の個数が違う", callee.key));
    }
    // callee の全値番号（パラメタ・inst dst）を offset からの新番号に割り当てる。
    // ブロック引数はブランチの実引数 (caller 側の値番号) で束縛されるため、
    // callee 側のパラメタ番号は caller のものと衝突しないよう fresh に割り当てる。
    let mut val_map: HashMap<VId, VId> = HashMap::new();
    let mut next = offset;
    for cb in &callee.blocks {
        for (pv, _) in &cb.params {
            val_map.insert(*pv, next);
            next += 1;
        }
        for i in &cb.insts {
            val_map.insert(i.dst, next);
            next += 1;
        }
    }

    // callee のブロックを caller のブロック番号空間にコピー。
    let base = caller.blocks.len();
    let n = callee.blocks.len();
    let cont = base + n; // 継続ブロックの番号

    // 継続ブロック: %dst をパラメタにして、call 命令の後の命令と元の終端命令を実行。
    let b = &mut caller.blocks[bidx];
    let after: Vec<Inst> = b.insts.split_off(ii + 1);
    b.insts.truncate(ii); // call 自体は削除
    let term = std::mem::replace(&mut b.term, Term::Unreachable);
    let cont_block = Block {
        params: vec![(dst, ret_ty)],
        insts: after,
        term,
    };

    // callee ブロックのコピー。ret → br cont(%retval_mapped)。
    let mut callee_blocks: Vec<Block> = vec![];
    for cb in &callee.blocks {
        let mut nb = cb.clone();
        // 値番号の振り直し（パラメタ・被演算子・dst）。
        for (pv, _) in &mut nb.params {
            *pv = val_map[pv];
        }
        for i in &mut nb.insts {
            for a in operands_mut(&mut i.op) {
                *a = val_map[a];
            }
            i.dst = val_map[&i.dst];
        }
        match &mut nb.term {
            Term::Ret(v) => {
                *v = val_map[v];
                nb.term = Term::Br((cont, vec![*v]));
            }
            Term::Br(t) => {
                t.0 += base;
                for a in &mut t.1 {
                    *a = val_map[a];
                }
            }
            Term::BrIf(c, t1, t2) => {
                *c = val_map[c];
                t1.0 += base;
                t2.0 += base;
                for a in &mut t1.1 {
                    *a = val_map[a];
                }
                for a in &mut t2.1 {
                    *a = val_map[a];
                }
            }
            Term::Unreachable => {}
        }
        callee_blocks.push(nb);
    }

    // 呼び出し元ブロック b は callee エントリへジャンプ（引数をパラメタにバインド）。
    // callee ブロック群は caller のブロック配列の末尾 (base..base+n) に追加し、
    // 継続ブロックを base+n に置く。既存のブロック番号をずらさないので、
    // 呼び出し元の他のブロックが参照するブロック番号はそのまま有効。
    b.term = Term::Br((base, args));
    caller.blocks.extend(callee_blocks);
    caller.blocks.push(cont_block);

    Ok(())
}

/// 1 パスのラッパ: 変換を適用して `verify_module` を通す。違反があればエラー。
fn after<T>(m: &Module, f: impl FnOnce(&Module) -> T, name: &str) -> Result<Module, String>
where
    T: Into<Module>,
{
    let m = f(m).into();
    let errs = kir::verify_module(&m);
    if errs.is_empty() {
        Ok(m)
    } else {
        Err(format!("{}: 検証に失敗: {:?}", name, errs))
    }
}

/// 定数の畳み込み (パス 1)。整数・bool の定数演算を畳む。
/// 溢れうる演算 (`trap` モード、`div`/`rem`、`neg` trap)は畳まない（panic の意味を保つ）。
/// `wrap`/`sat` モードと論理・比較・変換・ビット演算は畳む。
fn const_fold(m: &Module) -> Result<Module, String> {
    after(m, |m| {
        let mut m = m.clone();
        for f in &mut m.funcs {
            // 不変のスナップショットで畳める場所を求め、適用する。新しい定数ができると
            // さらに畳める命令が現れるので、変化が無くなるまで繰り返す。
            let mut changed = true;
            while changed {
                changed = false;
                let mut folds: Vec<(usize, usize, Inst)> = vec![];
                for (bi, b) in f.blocks.iter().enumerate() {
                    for (ii, inst) in b.insts.iter().enumerate() {
                        if let Some(op) = fold_inst(f, inst) {
                            folds.push((bi, ii, op));
                        }
                    }
                }
                for (bi, ii, inst) in folds {
                    f.blocks[bi].insts[ii] = inst;
                    changed = true;
                }
            }
        }
        m
    }, "定数の畳み込み")
}

/// 命令が定数に畳めるなら、新しい `Inst` を返す。そうでなければ `None`。
/// `dst` と `ty` は元のまま保つ（定数は同じ型）。
/// `Option` / `Result` / 集成体の型の定数は `Op::Const` として表せない（`Ok`/`Err`/`Some`
/// で作るか、verify が拒む）ため、畳まない。
fn fold_inst(f: &crate::kir::Func, inst: &Inst) -> Option<Inst> {
    if matches!(inst.op, Op::Const(_)) {
        return None;
    }
    if matches!(inst.ty, Ty::Opt(_) | Ty::Res(..) | Ty::Agg(_)) {
        return None;
    }
    let c = match &inst.op {
        Op::Arith { op, mode, a, b } => {
            let (x, y) = const_args(f, inst, *a, *b)?;
            match mode {
                // 畳み込みは参照実行器と同じ算術を使う (別に書くと食い違う。Fable の最終レビュー M1 の saturating)。
                // 参照実行器が Err (0 での除算など) を返すときは畳まない。
                Mode::Wrap | Mode::Sat => crate::kir_exec::arith(*op, *mode, inst.ty, x, y).ok().map(Op::Const),
                Mode::Trap => None, // 溢れ・0除算で panic するので畳まない
            }
        }
        Op::Neg { a, mode } => {
            let x = const_arg(f, inst, *a)?;
            match mode {
                Mode::Wrap => Some(Op::Const(wrap_neg(inst.ty, x))),
                Mode::Trap => None,
                Mode::Sat => None,
            }
        }
        Op::Cmp { cc, ty, a, b } => {
            let (x, y) = const_args(f, inst, *a, *b)?;
            Some(Op::Const(cmp_val(*cc, *ty, x, y) as i128))
        }
        Op::FArith { op, a, b } => {
            let (x, y) = const_args(f, inst, *a, *b)?;
            Some(Op::Const(farith(*op, inst.ty, x, y)))
        }
        Op::FUn { f: fun, src, a } => {
            let x = const_arg(f, inst, *a)?;
            Some(Op::Const(funary(*fun, *src, inst.ty, x)))
        }
        Op::FBin { f: fb, a, b } => {
            let (x, y) = const_args(f, inst, *a, *b)?;
            Some(Op::Const(fbin(*fb, inst.ty, x, y)))
        }
        Op::Shift { left, a, n } => {
            let x = const_arg(f, inst, *a)?;
            let n = const_arg(f, inst, *n)?;
            Some(Op::Const(shift(*left, inst.ty, x, n)))
        }
        Op::MinMax { max, a, b } => {
            let (x, y) = const_args(f, inst, *a, *b)?;
            Some(Op::Const(minmax(*max, inst.ty, x, y)))
        }
        Op::Conv { src, a } => {
            let x = const_arg(f, inst, *a)?;
            Some(Op::Const(conv(*src, inst.ty, x)))
        }
        Op::Bitcast { src, a } => {
            let x = const_arg(f, inst, *a)?;
            Some(Op::Const(bitcast(*src, x)))
        }
        Op::Not(a) => {
            let x = const_arg(f, inst, *a)?;
            Some(Op::Const(not_val(inst.ty, x)))
        }
        Op::Bool(op, a, b) => {
            let (x, y) = const_args(f, inst, *a, *b)?;
            Some(Op::Const(bool_val(*op, inst.ty, x, y)))
        }
        Op::BitCount { k, src, a } => {
            let x = const_arg(f, inst, *a)?;
            Some(Op::Const(bitcount(*k, *src, x)))
        }
        Op::Tag(a) => {
            let x = const_arg(f, inst, *a)?;
            Some(Op::Const(tag_val(x)))
        }
        Op::Payload(a) => {
            let x = const_arg(f, inst, *a)?;
            Some(Op::Const(payload_val(x)))
        }
        Op::Some(a) => {
            let x = const_arg(f, inst, *a)?;
            Some(Op::Const(tag_or_none(x)))
        }
        Op::Ok(a) => {
            let x = const_arg(f, inst, *a)?;
            Some(Op::Const(ok_val(x)))
        }
        Op::Err(a) => {
            let x = const_arg(f, inst, *a)?;
            Some(Op::Const(err_val(x)))
        }
        Op::Checked { op, a, b } => {
            let (x, y) = const_args(f, inst, *a, *b)?;
            Some(Op::Const(checked_val(*op, inst.ty, x, y)))
        }
        _ => None,
    };
    c.map(|op| Inst { dst: inst.dst, ty: inst.ty, op })
}

fn const_args(f: &crate::kir::Func, inst: &Inst, a: VId, b: VId) -> Option<(i128, i128)> {
    Some((const_arg(f, inst, a)?, const_arg(f, inst, b)?))
}

fn const_arg(_f: &crate::kir::Func, _inst: &Inst, v: VId) -> Option<i128> {
    const_of(_f, v)
}

/// 値番号 `v` が定数ならその値を返す。ブロック引数や未定義の値は `None`。
fn const_of(f: &crate::kir::Func, v: VId) -> Option<i128> {
    for b in &f.blocks {
        for i in &b.insts {
            if i.dst == v {
                return match &i.op {
                    Op::Const(c) => Some(*c),
                    _ => None,
                };
            }
        }
    }
    None
}

// ---- 整数演算の畳み込み (wrap / sat) ----

fn wrap_neg(ty: Ty, x: i128) -> i128 {
    ty.wrap(x.wrapping_neg())
}

// ---- 比較 ----

fn cmp_val(cc: Cc, ty: Ty, x: i128, y: i128) -> bool {
    crate::kir_scalar::cmp(cc, ty, x, y)
}

// ---- 浮動小数 ----

fn farith(op: Aop, ty: Ty, x: i128, y: i128) -> i128 {
    crate::kir_scalar::farith(op, ty, x, y)
}

fn funary(op: FUn, src: Ty, res: Ty, a: i128) -> i128 {
    crate::kir_scalar::funary(op, src, res, a)
}

fn fbin(op: FBin, ty: Ty, x: i128, y: i128) -> i128 {
    crate::kir_scalar::fbin(op, ty, x, y)
}

fn shift(left: bool, ty: Ty, x: i128, n: i128) -> i128 {
    crate::kir_scalar::shift(left, ty, x, n)
}

fn minmax(max: bool, ty: Ty, x: i128, y: i128) -> i128 {
    crate::kir_scalar::minmax(max, ty, x, y)
}

fn conv(src: Ty, dst: Ty, a: i128) -> i128 {
    crate::kir_scalar::conv(src, dst, a)
}

fn bitcast(_src: Ty, a: i128) -> i128 {
    // ビット列をそのまま使う。型は同じ幅なので値は不変。
    a
}

fn not_val(ty: Ty, x: i128) -> i128 {
    if ty == Ty::Bool {
        (x == 0) as i128
    } else {
        crate::kir_scalar::not_int(ty, x)
    }
}

fn bool_val(op: BoolOp, ty: Ty, x: i128, y: i128) -> i128 {
    let r = match op {
        BoolOp::And => x & y,
        BoolOp::Or => x | y,
        BoolOp::Xor => x ^ y,
    };
    if ty == Ty::Bool {
        (r != 0) as i128
    } else {
        ty.wrap(r)
    }
}

fn bitcount(k: BitK, src: Ty, x: i128) -> i128 {
    crate::kir_scalar::bitcount(k, src, x)
}

fn tag_val(x: i128) -> i128 {
    x & 1
}

fn payload_val(x: i128) -> i128 {
    x & crate::kir::PAY_MASK
}

fn tag_or_none(x: i128) -> i128 {
    crate::kir::TAG_BIT | x
}

fn ok_val(x: i128) -> i128 {
    x
}

fn err_val(x: i128) -> i128 {
    crate::kir::TAG_BIT | x
}

fn checked_val(op: Aop, t: Ty, x: i128, y: i128) -> i128 {
    let crate::kir::Ty::Opt(p) = t else { return 0 };
    let pt = p.ty();
    if matches!(op, Aop::Div | Aop::Rem) && y == 0 {
        return 0;
    }
    let r = match op {
        Aop::Add => x.checked_add(y),
        Aop::Sub => x.checked_sub(y),
        Aop::Mul => x.checked_mul(y),
        Aop::Div => x.checked_div(y),
        Aop::Rem => {
            if pt.signed() && y == -1 && x == pt.min() {
                None
            } else {
                x.checked_rem(y)
            }
        }
    };
    match r {
        Some(v) if v >= pt.min() && v <= pt.max() => crate::kir::TAG_BIT | crate::kir::payload_bits(v),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kir::{Block, Func, Inst, Pointee, Term, verify_module};

    fn sample() -> Module {
        let f = Func {
            fqn: "demo.fib".into(),
            key: "demo.fib".into(),
            uses: vec![],
            may_panic: false,
            stack: Some(4096),
            link: None,
            ret: Ty::U64,
            blocks: vec![
                Block {
                    params: vec![(0, Ty::U64)],
                    insts: vec![
                        Inst { dst: 1, ty: Ty::U64, op: Op::Const(2) },
                        Inst { dst: 2, ty: Ty::Bool, op: Op::Cmp { cc: Cc::Lt, ty: Ty::U64, a: 0, b: 1 } },
                    ],
                    term: Term::BrIf(2, (1, vec![]), (2, vec![0])),
                },
                Block { params: vec![], insts: vec![], term: Term::Ret(0) },
                Block {
                    params: vec![(3, Ty::U64)],
                    insts: vec![
                        Inst { dst: 4, ty: Ty::U64, op: Op::Const(1) },
                        Inst { dst: 5, ty: Ty::U64, op: Op::Arith { op: Aop::Sub, mode: Mode::Wrap, a: 3, b: 4 } },
                        Inst { dst: 6, ty: Ty::U64, op: Op::Call { callee: "demo.fib".into(), args: vec![5] } },
                    ],
                    term: Term::Ret(6),
                },
            ],
        };
        Module { funcs: vec![f], ..Default::default() }
    }

    #[test]
    fn const_fold_wrap_add_and_cmp() {
        let m = sample();
        let out = const_fold(&m).unwrap();
        let b = &out.funcs[0].blocks[0];
        // %1: u64 = const 2 はそのまま、%2: bool = icmp.lt.u64 %0, %1 は %0 が変数なので畳めない
        assert_eq!(b.insts[0].op, Op::Const(2));
        // %5: sub.u64.wrap %3, %4(%4=const 1) は畳めない（%3 はブロック引数）
    }

    #[test]
    fn const_fold_rejects_trap_div_by_zero() {
        // trap モードの div は畳まない（0 除算で panic の意味を保つ）
        let f = Func {
            fqn: "t.div".into(),
            key: "t.div".into(),
            uses: vec![],
            may_panic: true,
            stack: None,
            link: None,
            ret: Ty::I32,
            blocks: vec![Block {
                params: vec![],
                insts: vec![
                    Inst { dst: 0, ty: Ty::I32, op: Op::Const(7) },
                    Inst { dst: 1, ty: Ty::I32, op: Op::Const(0) },
                    Inst { dst: 2, ty: Ty::I32, op: Op::Arith { op: Aop::Div, mode: Mode::Trap, a: 0, b: 1 } },
                ],
                term: Term::Ret(2),
            }],
        };
        let m = Module { funcs: vec![f], ..Default::default() };
        let out = const_fold(&m).unwrap();
        assert!(matches!(out.funcs[0].blocks[0].insts[2].op, Op::Arith { .. }));
    }

    #[test]
    fn const_fold_wrap_div_by_zero_ok() {
        // wrap モードの div は 0 除算でも panic しないので畳む
        let f = Func {
            fqn: "t.div".into(),
            key: "t.div".into(),
            uses: vec![],
            may_panic: false,
            stack: None,
            link: None,
            ret: Ty::U32,
            blocks: vec![Block {
                params: vec![],
                insts: vec![
                    Inst { dst: 0, ty: Ty::U32, op: Op::Const(7) },
                    Inst { dst: 1, ty: Ty::U32, op: Op::Const(0) },
                    Inst { dst: 2, ty: Ty::U32, op: Op::Arith { op: Aop::Div, mode: Mode::Wrap, a: 0, b: 1 } },
                ],
                term: Term::Ret(2),
            }],
        };
        let m = Module { funcs: vec![f], ..Default::default() };
        let out = const_fold(&m).unwrap();
        assert!(matches!(out.funcs[0].blocks[0].insts[2].op, Op::Const(_)));
    }

    #[test]
    fn const_fold_optimize_pipeline_runs() {
        let m = sample();
        let _ = optimize(&m).unwrap();
    }

    /// 使われていない純粋な算術命令は除去され、副作用のある命令は残る。
    #[test]
    fn dce_removes_dead_pure_inst_keeps_side_effects() {
        let f = Func {
            fqn: "t.dce".into(),
            key: "t.dce".into(),
            uses: vec![],
            may_panic: false,
            stack: None,
            link: None,
            ret: Ty::U32,
            blocks: vec![Block {
                params: vec![],
                insts: vec![
                    Inst { dst: 0, ty: Ty::U32, op: Op::Const(1) },   // 未使用 → 削除
                    Inst { dst: 1, ty: Ty::U32, op: Op::Const(2) },   // %2 で使われる → 残る
                    Inst { dst: 2, ty: Ty::U32, op: Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: 1, b: 1 } }, // 自己参照は不正だがテスト用
                    Inst { dst: 3, ty: Ty::U32, op: Op::Call { callee: "t.dce".into(), args: vec![] } }, // 副作用 → 残る（未使用でも）
                ],
                term: Term::Ret(2),
            }],
        };
        let m = Module { funcs: vec![f], ..Default::default() };
        let out = dce(&m).unwrap();
        let insts = &out.funcs[0].blocks[0].insts;
        // %0 (const 1) は除去される
        assert!(!insts.iter().any(|i| matches!(i.op, Op::Const(1))));
        // %1 (const 2), %2 (add), call は残る
        assert!(insts.iter().any(|i| matches!(i.op, Op::Const(2))));
        assert!(insts.iter().any(|i| matches!(i.op, Op::Call { .. })));
        // ret %2 は %2 を参照するので %2 生成元は残る
        assert_eq!(out.funcs[0].blocks[0].term, Term::Ret(2));
    }

    #[test]
    fn copy_prop_reborrow_replaces_and_removes() {
        let f = Func {
            fqn: "t.cp".into(),
            key: "t.cp".into(),
            uses: vec![],
            may_panic: false,
            stack: None,
            link: None,
            ret: Ty::I32,
            blocks: vec![Block {
                params: vec![(0, Ty::Ref(true, Pointee::I32))],
                insts: vec![
                    Inst { dst: 1, ty: Ty::Ref(true, Pointee::I32), op: Op::Reborrow(0) },
                    Inst { dst: 2, ty: Ty::I32, op: Op::Load(1) },
                ],
                term: Term::Ret(2),
            }],
        };
        let m = Module { funcs: vec![f], ..Default::default() };
        let out = copy_prop(&m).unwrap();
        let insts = &out.funcs[0].blocks[0].insts;
        // reborrow は除去され、load の被演算子は %0 に置き換わる
        assert_eq!(insts.len(), 1);
        assert!(matches!(insts[0].op, Op::Load(0)));
        assert_eq!(out.funcs[0].blocks[0].term, Term::Ret(2));
        assert!(verify_module(&out).is_empty(), "{:?}", verify_module(&out));
    }

    #[test]
    fn copy_prop_chain_is_resolved() {
        // %1 = reborrow %0, %2 = reborrow %1 → 両方 %0 に置き換えて両方の reborrow を除去
        let f = Func {
            fqn: "t.cp2".into(),
            key: "t.cp2".into(),
            uses: vec![],
            may_panic: false,
            stack: None,
            link: None,
            ret: Ty::I32,
            blocks: vec![Block {
                params: vec![(0, Ty::Ref(true, Pointee::I32))],
                insts: vec![
                    Inst { dst: 1, ty: Ty::Ref(true, Pointee::I32), op: Op::Reborrow(0) },
                    Inst { dst: 2, ty: Ty::Ref(true, Pointee::I32), op: Op::Reborrow(1) },
                    Inst { dst: 3, ty: Ty::I32, op: Op::Load(2) },
                ],
                term: Term::Ret(3),
            }],
        };
        let m = Module { funcs: vec![f], ..Default::default() };
        let out = copy_prop(&m).unwrap();
        let insts = &out.funcs[0].blocks[0].insts;
        assert_eq!(insts.len(), 1);
        assert!(matches!(insts[0].op, Op::Load(0)));
        assert!(verify_module(&out).is_empty(), "{:?}", verify_module(&out));
    }

    #[test]
    fn inline_small_removes_call() {
        // t.double(x) = x + x (wrap) を t.twice(y) = t.double(y) にインライン展開。
        let double = Func {
            fqn: "t.double".into(),
            key: "t.double".into(),
            uses: vec![],
            may_panic: false,
            stack: None,
            link: None,
            ret: Ty::I64,
            blocks: vec![Block {
                params: vec![(0, Ty::I64)],
                insts: vec![Inst {
                    dst: 1,
                    ty: Ty::I64,
                    op: Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: 0, b: 0 },
                }],
                term: Term::Ret(1),
            }],
        };
        let twice = Func {
            fqn: "t.twice".into(),
            key: "t.twice".into(),
            uses: vec![],
            may_panic: false,
            stack: None,
            link: None,
            ret: Ty::I64,
            blocks: vec![Block {
                params: vec![(0, Ty::I64)],
                insts: vec![Inst {
                    dst: 1,
                    ty: Ty::I64,
                    op: Op::Call { callee: "t.double".into(), args: vec![0] },
                }],
                term: Term::Ret(1),
            }],
        };
        let m = Module {
            funcs: vec![double, twice],
            ..Default::default()
        };
        let out = inline_small(&m).unwrap();
        let f = out.funcs.iter().find(|f| f.key == "t.twice").unwrap();
        // 呼び出しは除去される
        assert!(!f.blocks.iter().any(|b| b.insts.iter().any(|i| matches!(i.op, Op::Call { .. }))));
        assert!(verify_module(&out).is_empty(), "{:?}", verify_module(&out));
    }

    #[test]
    fn inline_skips_recursive_function() {
        // t.fact は自分を再帰呼び出しするので展開されない。
        let fact = Func {
            fqn: "t.fact".into(),
            key: "t.fact".into(),
            uses: vec![],
            may_panic: false,
            stack: Some(4096),
            link: None,
            ret: Ty::U64,
            blocks: vec![Block {
                params: vec![(0, Ty::U64)],
                insts: vec![Inst {
                    dst: 1,
                    ty: Ty::U64,
                    op: Op::Call { callee: "t.fact".into(), args: vec![0] },
                }],
                term: Term::Ret(1),
            }],
        };
        // 呼び出し元も含める（再帰判定のため）
        let use_fact = Func {
            fqn: "t.use".into(),
            key: "t.use".into(),
            uses: vec![],
            may_panic: false,
            stack: None,
            link: None,
            ret: Ty::U64,
            blocks: vec![Block {
                params: vec![(0, Ty::U64)],
                insts: vec![Inst {
                    dst: 1,
                    ty: Ty::U64,
                    op: Op::Call { callee: "t.fact".into(), args: vec![0] },
                }],
                term: Term::Ret(1),
            }],
        };
        let m = Module {
            funcs: vec![fact, use_fact],
            ..Default::default()
        };
        let out = inline_small(&m).unwrap();
        let f = out.funcs.iter().find(|f| f.key == "t.use").unwrap();
        assert!(f.blocks.iter().any(|b| b.insts.iter().any(|i| matches!(i.op, Op::Call { .. }))));
        assert!(verify_module(&out).is_empty(), "{:?}", verify_module(&out));
    }

    #[test]
    fn optimize_pipeline_runs_on_sample() {
        let m = sample();
        let _ = optimize(&m).unwrap();
    }
}
