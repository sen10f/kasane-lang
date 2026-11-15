//! `std.rand.Rng` と `static` の `std.sync.Mutex` の KIR 下ろし (設計_KIR.md 22 章)。チャネルは `kir_lower_chan.rs`。
//!
//! - `std.rand.Rng` は構造体 `{ state: u64 }`。`from_seed(s)` は `{ s }`、`next_u64(&mut self)` は木走査 (`natives.rs` の `splitmix`) と同じ
//!   列 (`s += 0x9E3779B97F4A7C15`、`z = (s ^ s>>30) * 0xBF58476D1CE4E5B9`、`z = (z ^ z>>27) * 0x94D049BB133111EB`、`z ^ z>>31`)。
//!   `&mut` の受け手は局所の名前だけ (SSA の局所・アドレスを取られた局所・`&mut Rng` の引数)。
//! - `static N: std.sync.Mutex<T> = std.sync.Mutex.new(init)` (T は 8 バイト以下のスカラー): 大域領域 `global.bytes N 16`
//!   (`+0` が初期化の旗、`+8` が値)。`N.lock()` は旗が 0 なら `init` の式を評価して値に書き、旗を 1 にしてから、値の場所を指す
//!   `&mut T` を返す。この `&mut T` が `MutexGuard` の値 (`get()` は共有に、`get_mut()` はそのまま)。木走査の `lock` は無操作
//!   (各スレッドは最後まで走る) なのでロック・アンロックは出さない。

use crate::check::{items, nf, nlineno as nline, nname, strs};
use crate::kir::{Aop, BoolOp, Mode, Op, Pointee, Ty, VId};
use crate::parser::V;
use crate::program::Target as Tgt;
use crate::types::{from_v, Ty as Src};

use super::thread_lower::{rt_call, LOCK, MINIT, MLOCK, UNLOCK};
use super::{Lw, LR};

/// 大域領域の中の pthread mutex の位置 (`+0` 旗、`+8` 値、`+16` mutex)。
const MUTEX_OFF: u32 = 16;

const RNG: &str = "std.rand.Rng";

/// 型が `std.rand.Rng` (値・参照) か。
fn is_rng(lw: &Lw, t: Ty) -> bool {
    match t {
        Ty::Agg(ai) | Ty::Ref(_, Pointee::Agg(ai)) => lw.aggs.borrow().defs[ai as usize].name == RNG,
        _ => false,
    }
}

fn c64(lw: &mut Lw, v: u64) -> VId {
    lw.emit(Ty::U64, Op::Const(v as i128))
}

fn arith(lw: &mut Lw, op: Aop, a: VId, b: VId) -> VId {
    lw.emit(Ty::U64, Op::Arith { op, mode: Mode::Wrap, a, b })
}

fn xor(lw: &mut Lw, a: VId, b: VId) -> VId {
    lw.emit(Ty::U64, Op::Bool(BoolOp::Xor, a, b))
}

fn shr(lw: &mut Lw, a: VId, n: u32) -> VId {
    let n = lw.emit(Ty::U32, Op::Const(n as i128));
    lw.emit(Ty::U64, Op::Shift { left: false, a, n })
}

/// splitmix64 の 1 手: (次の状態, 出力)。
fn splitmix(lw: &mut Lw, state: VId) -> (VId, VId) {
    let k = c64(lw, 0x9E3779B97F4A7C15);
    let s = arith(lw, Aop::Add, state, k);
    let a = shr(lw, s, 30);
    let x = xor(lw, s, a);
    let m1 = c64(lw, 0xBF58476D1CE4E5B9);
    let z = arith(lw, Aop::Mul, x, m1);
    let b = shr(lw, z, 27);
    let x2 = xor(lw, z, b);
    let m2 = c64(lw, 0x94D049BB133111EB);
    let z2 = arith(lw, Aop::Mul, x2, m2);
    let c = shr(lw, z2, 31);
    let out = xor(lw, z2, c);
    (s, out)
}

/// `std.rand.Rng` の型 (型表)。
pub(super) fn rng_ty(lw: &Lw) -> Option<Ty> {
    lw.conv_t(&Src::Named(RNG.into(), vec![]))
}

/// パスの呼び出し (`std.rand.Rng.from_seed(..)`、`rng.next_u64()`、`NAME.lock()`)。対象外なら `None` (親へ戻す)。
pub(super) fn sync_call(lw: &mut Lw, e: &V) -> Option<LR<Option<VId>>> {
    let f = nf(e);
    if nname(&f[0]) != "Path" {
        return None;
    }
    let segs = strs(&nf(&f[0])[0]);
    let args = items(&f[1]).to_vec();
    let line = nline(e);
    if segs.join(".") == "std.rand.Rng.from_seed" {
        return Some(from_seed(lw, &args, line));
    }
    if segs.join(".") == "std.rand.Rng.from_entropy" {
        return Some(from_entropy(lw, &args, line));
    }
    if segs.len() >= 2 {
        let meth = segs[segs.len() - 1].as_str();
        if let (Some(s), 2) = (lw.lookup(&segs[0]), segs.len()) {
            let t = lw.slots[s].1;
            // `guard.get()` / `guard.get_mut()`: ガードは値の場所への `&mut T`。
            if matches!(meth, "get" | "get_mut") && args.is_empty() {
                if let Ty::Ref(true, p) = t {
                    let g = lw.read_slot(s);
                    let r = if meth == "get" { lw.ref_value(g, p, true) } else { lw.ref_value(g, p, false) };
                    return Some(Ok(Some(r)));
                }
            }
            if meth == "next_u64" && is_rng(lw, t) {
                return Some(next_u64(lw, s, &args, line));
            }
        }
        if meth == "lock" && args.is_empty() {
            let owner = &segs[..segs.len() - 1];
            if let Some(Tgt::Const(di)) = lw.p.resolve(owner, &lw.def.file).map(|r| r.target) {
                if lw.p.defs[di].kw == "static" {
                    return Some(static_lock(lw, di, line));
                }
            }
        }
    }
    None
}

fn from_seed(lw: &mut Lw, args: &[V], line: usize) -> LR<Option<VId>> {
    if args.len() != 1 {
        return lw.err(line, "Rng.from_seed の引数は 1 個のはず");
    }
    let Some(seed) = lw.expr(&args[0])? else { return Ok(None) };
    if lw.vty(seed) != Ty::U64 {
        return lw.err(line, "Rng.from_seed の引数が u64 でない");
    }
    let Some(ty) = rng_ty(lw) else { return lw.err(line, "std.rand.Rng が型表に入らない") };
    Ok(Some(lw.emit(ty, Op::Make { variant: None, fields: vec![seed] })))
}

/// `Rng.from_entropy()`: OS の乱数で 8 バイトの種を作り、`{ state: 種 }` にする (生成器は splitmix64。暗号用途ではない。仕様 8.3)。
/// 乱数源が読めなければ `ENTROPY_FAILED` を出して終了する (`Abort`。`panics never` のまま。木走査も同じ)。
fn from_entropy(lw: &mut Lw, args: &[V], line: usize) -> LR<Option<VId>> {
    if !args.is_empty() {
        return lw.err(line, "Rng.from_entropy の引数は 0 個のはず");
    }
    let Some(ty) = rng_ty(lw) else { return lw.err(line, "std.rand.Rng が型表に入らない") };
    let buf = lw.emit(Ty::Ptr, Op::FrameBytes { size: 8 });
    let len = lw.emit(Ty::Usize, Op::Const(8));
    // 失敗は文言を出して終了する (`fatal`。ランタイムが `ENTROPY_FAILED` を出す)。
    lw.emit(Ty::Unit, Op::RandFill { buf, len, fatal: true });
    let seed = lw.emit(Ty::U64, Op::MemLoad { p: buf, off: 0 });
    Ok(Some(lw.emit(ty, Op::Make { variant: None, fields: vec![seed] })))
}

fn next_u64(lw: &mut Lw, s: usize, args: &[V], line: usize) -> LR<Option<VId>> {
    if !args.is_empty() {
        return lw.err(line, "Rng.next_u64 の引数は 0 個のはず");
    }
    let Some(ty) = rng_ty(lw) else { return lw.err(line, "std.rand.Rng が型表に入らない") };
    let (sv, _) = lw.slots[s];
    // `&mut Rng` の引数: 指す先の state を読み書きする。
    if let Ty::Ref(m, Pointee::Agg(_)) = lw.vty(sv) {
        if !m {
            return lw.err(line, "共有参照を通した Rng.next_u64 は呼べない");
        }
        let addr = lw.field_addr(sv, "state", line)?;
        let st = lw.emit(Ty::U64, Op::Load(addr));
        let (ns, out) = splitmix(lw, st);
        lw.emit(Ty::Unit, Op::Store { ptr: addr, val: ns });
        return Ok(Some(out));
    }
    let cur = lw.read_slot(s);
    let st = lw.emit(Ty::U64, Op::Field { a: cur, off: 0 });
    let (ns, out) = splitmix(lw, st);
    let nv = lw.emit(ty, Op::Make { variant: None, fields: vec![ns] });
    lw.assign_slot(s, nv);
    Ok(Some(out))
}

/// `static` の `Mutex<T>` の `lock()`。値の場所を指す `&mut T` を返す。
fn static_lock(lw: &mut Lw, di: usize, line: usize) -> LR<Option<VId>> {
    let d = &lw.p.defs[di];
    let name = d.name.clone();
    let Src::Named(tn, targs) = from_v(&d.ret, &std::collections::HashSet::new()) else {
        return lw.err(line, format!("static {} の型が Mutex<T> でない", name));
    };
    if tn != "std.sync.Mutex" || targs.len() != 1 {
        return lw.err(line, format!("static {} の型が Mutex<T> でない", name));
    }
    let Some(elem) = lw.conv_t(&targs[0]) else {
        return lw.err(line, format!("static {} の Mutex の値の型 {} は段階 1 の対象外", name, super::show(&targs[0])));
    };
    if !elem.is_scalar() || elem == Ty::Unit || matches!(elem, Ty::I128 | Ty::U128) {
        return lw.err(line, format!("static {} の Mutex の値の型 {} は 64 ビット以下のスカラーだけ対象", name, super::show(&targs[0])));
    }
    let Some(p) = Pointee::of(elem) else { return lw.err(line, "Mutex の値の型が参照の指す先になれない") };
    // 初期化の式 `std.sync.Mutex.new(init)` の `init`。
    let init = match &d.init {
        Some(v) if nname(v) == "Call" && strs(&nf(&nf(v)[0])[0]).join(".") == "std.sync.Mutex.new" && items(&nf(v)[1]).len() == 1 => items(&nf(v)[1])[0].clone(),
        _ => return lw.err(line, format!("static {} の初期化が std.sync.Mutex.new(値) の形でない", name)),
    };
    let g = lw.emit(Ty::Ptr, Op::GlobalBytes { name, size: 80 });
    // 初期化の判定は大域のロックの下 (実スレッドで 2 重に初期化しない)。
    rt_call(lw, LOCK, vec![], Ty::Unit);
    let flag = lw.emit(Ty::U8, Op::MemLoad { p: g, off: 0 });
    let zero = lw.emit(Ty::U8, Op::Const(0));
    let fresh = lw.emit(Ty::Bool, Op::Cmp { cc: crate::kir::Cc::Eq, ty: Ty::U8, a: flag, b: zero });
    let (run, _) = lw.new_block(&[]);
    let (cont, _) = lw.new_block(&[]);
    lw.branch(fresh, (run, vec![]), (cont, vec![]));
    lw.cur = Some(run);
    // static の初期化の式は型表に載らないので、整数・bool のリテラルだけを直接読む。
    let iv = match (nname(&init), nf(&init).first().map(|x| crate::check::st(x))) {
        ("Lit", Some(k)) if k == "int" && elem.is_int() => {
            let Some(n) = super::parse_int(&nf(&init)[1]).filter(|n| elem.in_range(*n)) else {
                return lw.err(line, "static の Mutex の初期値の整数が読めない・範囲外");
            };
            lw.emit(elem, Op::Const(n))
        }
        ("Lit", Some(k)) if k == "bool" && elem == Ty::Bool => lw.emit(Ty::Bool, Op::Const(matches!(nf(&init)[1], V::B(true)) as i128)),
        _ => return lw.err(line, "static の Mutex の初期値は整数か bool のリテラルだけ対象"),
    };
    lw.emit(Ty::Unit, Op::MemStore { ty: elem, p: g, off: 8, v: iv });
    let one = lw.emit(Ty::U8, Op::Const(1));
    lw.emit(Ty::Unit, Op::MemStore { ty: Ty::U8, p: g, off: 0, v: one });
    let mo = lw.emit(Ty::Usize, Op::Const(MUTEX_OFF as i128));
    let mp = lw.emit(Ty::Ptr, Op::PtrAdd { p: g, off: mo });
    rt_call(lw, MINIT, vec![mp], Ty::Unit);
    lw.goto(cont, vec![]);
    lw.cur = Some(cont);
    rt_call(lw, UNLOCK, vec![], Ty::Unit);
    // 実スレッドの相互排他: 値ごとの再帰 mutex。ガードの寿命 (外す時点) は `coll::register_unlock` が決める (4.4):
    // 束縛された変数のスコープの終わり・`core.mem.drop(g)`、束縛されない一時なら文の終わり。協調実行では何もしない。
    let mo = lw.emit(Ty::Usize, Op::Const(MUTEX_OFF as i128));
    let mp = lw.emit(Ty::Ptr, Op::PtrAdd { p: g, off: mo });
    rt_call(lw, MLOCK, vec![mp], Ty::Unit);
    let off = lw.emit(Ty::Usize, Op::Const(8));
    let vp = lw.emit(Ty::Ptr, Op::PtrAdd { p: g, off });
    let r = lw.emit(Ty::Ref(true, p), Op::Bitcast { src: Ty::Ptr, a: vp });
    super::coll::register_unlock(lw, mp, r);
    Ok(Some(r))
}
