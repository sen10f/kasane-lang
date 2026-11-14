//! panic の下ろし (設計_KIR.md 11 章の続き)。`kir_lower` の子モジュールとして `Lw` の非公開の項目を使う。
//!
//! - `core.panic.panic("文言")` (引数は文字列リテラルだけ): `Op::Panic` で文言を出す。制御はそこで終わる。
//! - `x.unwrap()` (`Option<P>` / `Result<T, E>`): `None` / `Err` のとき panic。文言は木走査 (`ksn run`) と同じ:
//!   - `None`: "called `Option::unwrap()` on a `None` value"
//!   - `Err(e)`: "called `Result::unwrap()` on an `Err` value: " に `e` の Debug 表示を続ける。
//!     Debug を KIR で同じに作れるのは整数 (10 進) と `()` だけ。他の型 (文字列・構造体など) は理由付きで止める。
//!
//! `expect` は Kasane の Option / Result のメソッドではない (E1108) ので扱わない。

use crate::check::{items, nf, nlineno as nline, nname, st, strs};
use crate::kir::{Op, Pay, Term, Ty, VId};
use crate::parser::V;
use crate::types::Ty as Src;

use super::{coll::unhex, Lw, LR};

impl<'a> Lw<'a> {
    /// `core.panic.panic(<文字列リテラル>)`。対象外の形なら `None` (親の処理に任せる)。
    pub(super) fn panic_call(&mut self, e: &V) -> Option<LR<Option<VId>>> {
        let f = nf(e);
        if nname(&f[0]) != "Path" || strs(&nf(&f[0])[0]) != ["core", "panic", "panic"] {
            return None;
        }
        Some(self.panic_lit(e))
    }

    fn panic_lit(&mut self, e: &V) -> LR<Option<VId>> {
        let line = nline(e);
        let args = items(&nf(e)[1]);
        if args.len() != 1 || nname(&args[0]) != "Lit" || st(&nf(&args[0])[0]) != "str" {
            return self.err(line, "core.panic.panic の引数は文字列リテラルだけ段階 1 の対象");
        }
        let bytes = unhex(&st(&nf(&args[0])[1]));
        let Ok(msg) = String::from_utf8(bytes) else {
            return self.err(line, "core.panic.panic の文言が UTF-8 でない");
        };
        self.emit(Ty::Unit, Op::Panic { msg, arg: None });
        self.terminate(Term::Unreachable);
        Ok(None)
    }

    /// `e.unwrap()` (受け手が式の形)。受け手が `Option` / `Result` でなければ `None` (親の処理に任せる)。
    pub(super) fn unwrap_mcall(&mut self, e: &V) -> Option<LR<Option<VId>>> {
        if st(&nf(e)[1]) != "unwrap" {
            return None;
        }
        // `Result` の E の型 (Err の表示に使う)。`Option` なら None。
        let err_src = match self.src_ty(&nf(e)[0]) {
            Ok(Src::Named(n, _)) if n == "Option" => None,
            Ok(Src::Named(n, args)) if n == "Result" => args.get(1).cloned(),
            _ => return None,
        };
        let f = nf(e);
        let line = nline(e);
        if !items(&f[2]).is_empty() {
            return Some(self.err(line, "unwrap の引数は 0 個のはず"));
        }
        Some(self.expr(&f[0]).and_then(|r| match r {
            Some(recv) => self.unwrap_on(recv, line, err_src),
            None => Ok(None),
        }))
    }

    /// `x.unwrap()` の呼び出しの形 (`Call` で経路 `x.unwrap`、`x` は局所の Option / Result)。対象外なら `None`。
    pub(super) fn unwrap_path(&mut self, e: &V) -> Option<LR<Option<VId>>> {
        let f = nf(e);
        if nname(&f[0]) != "Path" || !items(&f[1]).is_empty() {
            return None;
        }
        let segs = strs(&nf(&f[0])[0]);
        if segs.len() != 2 || segs[1] != "unwrap" {
            return None;
        }
        let s = self.lookup(&segs[0])?;
        if !matches!(self.slots[s].1, Ty::Opt(_) | Ty::Res(..)) {
            return None;
        }
        // 所有物を持つ Option / Result の局所は、unwrap がペイロードを取り出すので移す (印を下ろす)。
        // そうしないと、取り出した値とスコープの終わりの解放が重なって二重解放になる (Fable レビュー n7)。
        let recv = if super::coll::owns(self, self.slots[s].1) {
            let v = super::coll::path_read(self, s);
            self.own_emit(&segs[0], s, crate::kir::OwnKind::Consume);
            v
        } else {
            self.read_slot(s)
        };
        Some(self.unwrap_on(recv, nline(e), None))
    }

    /// `recv` (`Option` / `Result`) の unwrap。失敗の枝は panic、値の枝はペイロードを返す。
    /// `err_src` は `Result` の E の型 (木走査と同じ Debug 表示を、E の `$debug` で書くために使う)。
    fn unwrap_on(&mut self, recv: VId, line: usize, err_src: Option<Src>) -> LR<Option<VId>> {
        let rt = self.vty(recv);
        // (値を持つ枝の型, 失敗の枝の判定 (真なら値の枝), 文言, Err の値の型 (整数・() は文言に値を続ける))
        let mut dbg: Option<(Src, Ty)> = None;
        let (pay, good, msg, arg_ty) = match rt {
            Ty::Opt(Pay::Agg(bi)) if self.agg_def(bi).name == "bref" => {
                return self.err(line, "Option<&T> (T は所有物を持つ型) の unwrap は段階 1 の対象外 (借用として取り出す形は `match` の `Some(x)` で書く)");
            }
            Ty::Opt(p) => {
                let good = self.emit(Ty::Bool, Op::Tag(recv));
                (p.ty(), good, "called `Option::unwrap()` on a `None` value".to_string(), None)
            }
            Ty::Res(t0, e0) => {
                let tag = self.emit(Ty::Bool, Op::Tag(recv));
                let good = self.emit(Ty::Bool, Op::Not(tag));
                let et = e0.ty();
                let head = "called `Result::unwrap()` on an `Err` value";
                if et.is_int() && et.bits() <= 64 {
                    (t0.ty(), good, head.to_string(), Some(et))
                } else if et == Ty::Unit {
                    (t0.ty(), good, format!("{}: ()", head), None)
                } else if let (Some(es), Ty::Agg(_)) = (err_src, et) {
                    // 集成体の E (派生の Debug を持つ型): 失敗の枝で `<E>.$debug` を呼んで表示する
                    dbg = Some((es, et));
                    (t0.ty(), good, head.to_string(), None)
                } else {
                    return self.err(line, "unwrap の Err の値の表示は段階 1 の対象外 (表示を下ろせるのは整数・() と、派生の Debug を持つ型だけ)");
                }
            }
            other => return self.err(line, format!("メソッド unwrap は型 {} に対して段階 1 の対象外", other.name())),
        };
        let (yes, _) = self.new_block(&[]);
        let (no, _) = self.new_block(&[]);
        self.branch(good, (yes, vec![]), (no, vec![]));
        // 失敗の枝: 文言 (と Err の値) を出して終わる
        self.cur = Some(no);
        if let Some((es, et)) = dbg {
            // 文言の接頭辞と Err の表示を部分ごとに書き、終了コード 101 で止まる (`core.check.eq` の失敗と同じ形)。
            let file = self.def.file.clone();
            let mut g = super::debug::Gen::new(self.p, self.aggs);
            let dbg_fn = match g.need(&es, et, &file) {
                Ok(k) => k,
                Err(w) => return self.err(line, format!("unwrap の Err の表示を下ろせない: {}", w)),
            };
            for (f, c) in std::mem::take(&mut g.out) {
                self.coll.extra.push((f, c));
            }
            self.callees.insert(dbg_fn.clone());
            let fat = self.aggs.borrow_mut().stderr_fat(self.p)?;
            let ev = self.emit(et, Op::Payload(recv));
            self.put_static(fat, &format!("panic: {}: ", msg));
            self.call_debug(&dbg_fn, ev);
            self.put_static(fat, "\n");
            self.emit(Ty::Unit, Op::Abort);
            self.terminate(Term::Unreachable);
            self.cur = Some(yes);
            return Ok(Some(self.emit(pay, Op::Payload(recv))));
        }
        let arg = match arg_ty {
            Some(et) => Some(self.emit(et, Op::Payload(recv))),
            None => None,
        };
        self.emit(Ty::Unit, Op::Panic { msg, arg });
        self.terminate(Term::Unreachable);
        // 値の枝
        self.cur = Some(yes);
        Ok(Some(self.emit(pay, Op::Payload(recv))))
    }
}
