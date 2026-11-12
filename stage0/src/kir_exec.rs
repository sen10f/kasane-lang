//! KIR の参照実行器 (設計_KIR.md 5 章 段階 1 の「素朴なインタプリタ」)。
//!
//! 目的は、木走査インタプリタ (`interp.rs`) と出力・終了コード・panic の文言が一致することを確かめる基準を作ること。
//! 速さは求めない。呼び出しは明示的なフレーム列で持ち、Rust の再帰を使わない。深さの上限は木走査と同じ 2500 (終了コード 102)。

use std::collections::{BTreeMap, HashMap};

use crate::kir::{Aop, BoolOp, Cc, Mode, Module, Op, Term, Ty, VId};

/// 木走査インタプリタの呼び出し深さの上限 (`interp.rs` の `depth > 2500`)。
const MAX_DEPTH: usize = 2500;

#[derive(Debug, PartialEq)]
pub enum Outcome {
    /// 正常終了。値は型に応じた整数 (bool は 0 / 1、unit は 0)。
    Value(i128),
    /// 実行時 panic (文言は木走査と同じ)。
    Panic(String),
    /// 呼び出し深さの超過 (木走査と同じく終了コード 102)。
    StackOverflow,
}

struct Frame {
    f: usize,
    blk: usize,
    /// 次に実行する命令の番号。
    ip: usize,
    vals: Vec<i128>,
    /// 呼び出し元で結果を受ける値番号。
    dst: Option<VId>,
    /// この関数の `frame.bytes` で確保した領域の先頭。関数が戻るときに解放する。
    allocs: Vec<i128>,
}

/// アドレスの基準値 (0 は null として空けておく)。
const BASE: i128 = 0x1000;

/// 仮想のヒープ。`clif` と同じく {ptr, len, cap} の実メモリを、バイト列の上で表す。
/// 解放済みの領域は再利用しない (誤用の検出を優先する)。生存する領域は `live` で、静的な領域は `pinned` で区別する。
struct Heap {
    mem: Vec<u8>,
    /// 先頭アドレス → 領域。
    blocks: BTreeMap<i128, Blk>,
}

struct Blk {
    off: usize,
    size: usize,
    live: bool,
    pinned: bool,
}

impl Heap {
    fn new() -> Heap {
        Heap { mem: vec![], blocks: BTreeMap::new() }
    }

    fn alloc(&mut self, size: usize, pinned: bool) -> i128 {
        let off = self.mem.len();
        self.mem.resize(off + size.max(1), 0);
        let addr = BASE + off as i128;
        self.blocks.insert(addr, Blk { off, size, live: true, pinned });
        addr
    }

    /// `addr` から `n` バイトが生存する領域の内側にあるとき、`mem` 上の位置を返す。
    fn locate(&self, addr: i128, n: usize) -> Result<usize, String> {
        let bad = || format!("不正なメモリアクセス {:#x} ({} バイト)", addr, n);
        let (&start, b) = self.blocks.range(..=addr).next_back().ok_or_else(bad)?;
        if !b.live || addr + n as i128 > start + b.size as i128 {
            return Err(bad());
        }
        Ok(b.off + (addr - start) as usize)
    }

    fn read(&self, addr: i128, ty: Ty) -> Result<i128, String> {
        let n = width(ty);
        let at = self.locate(addr, n)?;
        let mut buf = [0u8; 8];
        buf[..n].copy_from_slice(&self.mem[at..at + n]);
        let u = u64::from_le_bytes(buf) as i128;
        Ok(match ty {
            Ty::Bool => (u != 0) as i128,
            Ty::Ptr => u,
            _ => ty.wrap(u),
        })
    }

    fn write(&mut self, addr: i128, ty: Ty, v: i128) -> Result<(), String> {
        let n = width(ty);
        let at = self.locate(addr, n)?;
        let bytes = (v as u64).to_le_bytes();
        self.mem[at..at + n].copy_from_slice(&bytes[..n]);
        Ok(())
    }

    fn write_bytes(&mut self, addr: i128, bytes: &[u8]) -> Result<(), String> {
        let at = self.locate(addr, bytes.len())?;
        self.mem[at..at + bytes.len()].copy_from_slice(bytes);
        Ok(())
    }

    /// 解放 (`null` は何もしない)。二重解放や静的領域の解放は誤用として `Err` にする。
    fn free(&mut self, addr: i128) -> Result<(), String> {
        if addr == 0 {
            return Ok(());
        }
        match self.blocks.get_mut(&addr) {
            Some(b) if b.live && !b.pinned => {
                b.live = false;
                Ok(())
            }
            _ => Err(format!("不正な解放 {:#x}", addr)),
        }
    }

    /// `old` の内容を先頭から複写した、新しい大きさ `size` の領域を作って旧い領域を解放する (`old` が 0 なら確保だけ)。
    fn realloc(&mut self, old: i128, size: usize) -> Result<i128, String> {
        let new = self.alloc(size, false);
        if old == 0 {
            return Ok(new);
        }
        let (old_size, live) = match self.blocks.get(&old) {
            Some(b) if b.live && !b.pinned => (b.size, true),
            _ => (0, false),
        };
        if !live {
            return Err(format!("不正な再確保 {:#x}", old));
        }
        let n = old_size.min(size);
        let src = self.locate(old, n)?;
        let bytes: Vec<u8> = self.mem[src..src + n].to_vec();
        self.write_bytes(new, &bytes)?;
        self.free(old)?;
        Ok(new)
    }

    /// フレームの終わりの解放 (誤用の検査は行わない: 戻り値に含まれない領域だけが対象)。
    fn release(&mut self, addr: i128) {
        if let Some(b) = self.blocks.get_mut(&addr) {
            b.live = false;
        }
    }

    /// 生存する (解放されていない、静的でない) 領域の数。
    fn leaks(&self) -> usize {
        self.blocks.values().filter(|b| b.live && !b.pinned).count()
    }
}

/// 型の幅 (バイト)。`unit` は持たない。
fn width(ty: Ty) -> usize {
    match ty {
        Ty::Bool => 1,
        Ty::Ptr => 8,
        t => (t.bits() / 8) as usize,
    }
}

/// 整数の算術。結果は型の範囲内に収まる値か、panic の文言。
fn arith(op: Aop, mode: Mode, ty: crate::kir::Ty, x: i128, y: i128) -> Result<i128, String> {
    if mode == Mode::Wrap {
        let r = match op {
            Aop::Add => x.wrapping_add(y),
            Aop::Sub => x.wrapping_sub(y),
            _ => x.wrapping_mul(y),
        };
        return Ok(ty.wrap(r));
    }
    if matches!(op, Aop::Div | Aop::Rem) && y == 0 {
        // 文言は木走査 (interp.rs の `int_arith`) と同じにする。剰余の文言は木走査の誤植 (`of by zero`) を写している。
        return Err(if op == Aop::Div {
            "attempt to divide by zero".to_string()
        } else {
            "attempt to calculate the remainder with a divisor of by zero".to_string()
        });
    }
    let r = match op {
        Aop::Add => x.checked_add(y),
        Aop::Sub => x.checked_sub(y),
        Aop::Mul => x.checked_mul(y),
        Aop::Div => x.checked_div(y),
        Aop::Rem => x.checked_rem(y),
    };
    match r {
        Some(v) if v >= ty.min() && v <= ty.max() => Ok(v),
        _ => Err(match op {
            Aop::Add => "attempt to add with overflow",
            Aop::Sub => "attempt to subtract with overflow",
            Aop::Mul => "attempt to multiply with overflow",
            Aop::Div => "attempt to divide with overflow",
            Aop::Rem => "attempt to calculate the remainder with overflow",
        }
        .to_string()),
    }
}

fn compare(cc: Cc, x: i128, y: i128) -> bool {
    match cc {
        Cc::Eq => x == y,
        Cc::Ne => x != y,
        Cc::Lt => x < y,
        Cc::Le => x <= y,
        Cc::Gt => x > y,
        Cc::Ge => x >= y,
    }
}

/// 鍵 `key` の関数を引数 `args` で実行する。鍵が無い・引数の個数が違う場合は `Err`。
pub fn run(m: &Module, key: &str, args: &[i128]) -> Result<Outcome, String> {
    let index: HashMap<&str, usize> = m.funcs.iter().enumerate().map(|(i, f)| (f.key.as_str(), i)).collect();
    let Some(&entry) = index.get(key) else {
        return Err(format!("関数 {} がない", key));
    };
    if args.len() != m.funcs[entry].params().len() {
        return Err(format!("関数 {} の引数の個数が違う", key));
    }
    let counts: Vec<usize> = m.funcs.iter().map(|f| f.value_count()).collect();
    let enter = |f: usize, args: &[i128], dst: Option<VId>| -> Frame {
        let mut vals = vec![0i128; counts[f]];
        for ((v, _), a) in m.funcs[f].params().iter().zip(args.iter()) {
            vals[*v as usize] = *a;
        }
        Frame { f, blk: 0, ip: 0, vals, dst, allocs: vec![] }
    };
    let mut stack = vec![enter(entry, args, None)];
    let mut heap = Heap::new();
    // 静的な領域 (`data`) は、命令の位置ごとに 1 回だけ確保する。
    let mut statics: HashMap<(usize, usize, usize), i128> = HashMap::new();
    loop {
        let top = stack.len() - 1;
        let (fi, bi, ip) = (stack[top].f, stack[top].blk, stack[top].ip);
        let func = &m.funcs[fi];
        let blk = &func.blocks[bi];
        if ip < blk.insts.len() {
            stack[top].ip += 1;
            let inst = &blk.insts[ip];
            let get = |fr: &Frame, v: &VId| fr.vals[*v as usize];
            let result: i128 = match &inst.op {
                Op::Const(v) => *v,
                Op::FrameBytes { size } => {
                    let a = heap.alloc(*size as usize, false);
                    stack[top].allocs.push(a);
                    a
                }
                Op::StaticBytes(bytes) => match statics.get(&(fi, bi, ip)) {
                    Some(a) => *a,
                    None => {
                        let a = heap.alloc(bytes.len(), true);
                        heap.write_bytes(a, bytes)?;
                        statics.insert((fi, bi, ip), a);
                        a
                    }
                },
                Op::PtrAdd { p, off } => get(&stack[top], p) + get(&stack[top], off),
                Op::MemLoad { p, off } => heap.read(get(&stack[top], p) + *off as i128, inst.ty)?,
                Op::MemStore { ty, p, off, v } => {
                    let (a, x) = (get(&stack[top], p) + *off as i128, get(&stack[top], v));
                    heap.write(a, *ty, x)?;
                    0
                }
                Op::HeapAlloc(n) => heap.alloc(get(&stack[top], n) as usize, false),
                Op::HeapRealloc { p, size } => heap.realloc(get(&stack[top], p), get(&stack[top], size) as usize)?,
                Op::HeapFree(p) => {
                    heap.free(get(&stack[top], p))?;
                    0
                }
                Op::IndexCheck { i, len } => {
                    let (ix, n) = (get(&stack[top], i), get(&stack[top], len));
                    if ix >= n {
                        return Ok(Outcome::Panic(format!("index out of bounds: the len is {} but the index is {}", n, ix)));
                    }
                    0
                }
                Op::Arith { op, mode, a, b } => {
                    let (x, y) = (get(&stack[top], a), get(&stack[top], b));
                    match arith(*op, *mode, inst.ty, x, y) {
                        Ok(v) => v,
                        Err(msg) => return Ok(Outcome::Panic(msg)),
                    }
                }
                Op::Cmp { cc, a, b, .. } => compare(*cc, get(&stack[top], a), get(&stack[top], b)) as i128,
                Op::Neg { a } => {
                    let x = get(&stack[top], a);
                    if x == inst.ty.min() {
                        return Ok(Outcome::Panic("attempt to negate with overflow".to_string()));
                    }
                    -x
                }
                Op::Not(a) => (get(&stack[top], a) == 0) as i128,
                Op::Bool(bop, a, b) => {
                    let (x, y) = (get(&stack[top], a) != 0, get(&stack[top], b) != 0);
                    let r = match bop {
                        BoolOp::And => x & y,
                        BoolOp::Or => x | y,
                        BoolOp::Xor => x ^ y,
                    };
                    r as i128
                }
                Op::Call { callee, args } => {
                    if stack.len() >= MAX_DEPTH {
                        return Ok(Outcome::StackOverflow);
                    }
                    let Some(&ci) = index.get(callee.as_str()) else {
                        return Err(format!("呼び先 {} がない", callee));
                    };
                    let argv: Vec<i128> = args.iter().map(|a| get(&stack[top], a)).collect();
                    stack.push(enter(ci, &argv, Some(inst.dst)));
                    continue;
                }
            };
            stack[top].vals[inst.dst as usize] = result;
            continue;
        }
        // 終端命令
        match &blk.term {
            Term::Br((t, targs)) => {
                jump(&mut stack[top], m, fi, *t, targs);
            }
            Term::BrIf(c, (t1, a1), (t2, a2)) => {
                if stack[top].vals[*c as usize] != 0 {
                    jump(&mut stack[top], m, fi, *t1, a1);
                } else {
                    jump(&mut stack[top], m, fi, *t2, a2);
                }
            }
            Term::Ret(v) => {
                let val = stack[top].vals[*v as usize];
                let done = stack.pop().expect("フレーム");
                for a in &done.allocs {
                    heap.release(*a);
                }
                match stack.last_mut() {
                    None => {
                        // 正常終了では、解放されていない領域があってはならない (所有の下ろしの誤り)。
                        let n = heap.leaks();
                        if n > 0 {
                            return Err(format!("解放されていない領域が {} 個ある", n));
                        }
                        return Ok(Outcome::Value(val));
                    }
                    Some(caller) => {
                        if let Some(d) = done.dst {
                            caller.vals[d as usize] = val;
                        }
                    }
                }
            }
            Term::Unreachable => return Err("到達しないブロックに入った".to_string()),
        }
    }
}

/// 移動先のブロックへ入る。引数は移動の前の値で受け取る (並列代入)。
fn jump(fr: &mut Frame, m: &Module, fi: usize, t: usize, args: &[VId]) {
    let vals: Vec<i128> = args.iter().map(|a| fr.vals[*a as usize]).collect();
    let params = &m.funcs[fi].blocks[t].params;
    for ((p, _), v) in params.iter().zip(vals) {
        fr.vals[*p as usize] = v;
    }
    fr.blk = t;
    fr.ip = 0;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kir::{Block, Func, Inst, Ty};

    #[test]
    fn arith_matches_interpreter_messages() {
        assert_eq!(arith(Aop::Add, Mode::Trap, Ty::U8, 200, 100), Err("attempt to add with overflow".into()));
        assert_eq!(arith(Aop::Sub, Mode::Trap, Ty::U32, 0, 1), Err("attempt to subtract with overflow".into()));
        assert_eq!(arith(Aop::Div, Mode::Trap, Ty::I8, 1, 0), Err("attempt to divide by zero".into()));
        assert_eq!(arith(Aop::Div, Mode::Trap, Ty::I8, -128, -1), Err("attempt to divide with overflow".into()));
        assert_eq!(arith(Aop::Rem, Mode::Trap, Ty::I8, 1, 0), Err("attempt to calculate the remainder with a divisor of by zero".into()));
        assert_eq!(arith(Aop::Mul, Mode::Wrap, Ty::U8, 16, 17), Ok(16));
    }

    /// `fn f(n: u64) -> u64`: 引数を 1 つ減らして呼ぶ再帰 (深さの検査用)。
    fn deep() -> Module {
        let f = Func {
            fqn: "t.deep".into(),
            key: "t.deep".into(),
            uses: vec![],
            may_panic: false,
            stack: None,
            ret: Ty::U64,
            blocks: vec![Block {
                params: vec![(0, Ty::U64)],
                insts: vec![
                    Inst { dst: 1, ty: Ty::U64, op: Op::Const(1) },
                    Inst { dst: 2, ty: Ty::U64, op: Op::Call { callee: "t.deep".into(), args: vec![0] } },
                ],
                term: Term::Ret(2),
            }],
        };
        Module { funcs: vec![f] }
    }

    /// テキストの 1 関数のモジュールを実行する (鍵は関数名)。
    fn exec(text: &str, key: &str) -> Result<Outcome, String> {
        let m = crate::kir::parse_module(text).expect("parse");
        run(&m, key, &[])
    }

    #[test]
    fn heap_round_trip_and_realloc_keep_contents() {
        let t = crate::kir::MEM_TEXT;
        assert_eq!(exec(t, "t.mem"), Ok(Outcome::Value(77)));
    }

    #[test]
    fn leaks_and_bad_frees_are_reported() {
        let leak = "kir 1

fn t.leak key=t.leak uses=[alloc] panics=never stack=- ret=unit
  b0():
    %0: usize = const.usize 8
    %1: ptr = heap.alloc %0
    %2: unit = const.unit ()
    ret %2
end t.leak
";
        assert_eq!(exec(leak, "t.leak"), Err("解放されていない領域が 1 個ある".to_string()));
        let twice = leak.replace("    %2: unit = const.unit ()\n    ret %2", "    %2: unit = heap.free %1\n    %3: unit = heap.free %1\n    ret %2")
            .replace("t.leak", "t.twice");
        assert!(exec(&twice, "t.twice").unwrap_err().contains("不正な解放"));
    }

    #[test]
    fn out_of_bounds_access_is_an_internal_error() {
        let src = "kir 1

fn t.oob key=t.oob uses=[alloc] panics=never stack=- ret=u8
  b0():
    %0: usize = const.usize 2
    %1: ptr = heap.alloc %0
    %2: usize = const.usize 2
    %3: ptr = ptr.add %1, %2
    %4: u8 = mem.load.u8 %3 +0
    %5: unit = heap.free %1
    ret %4
end t.oob
";
        assert!(exec(src, "t.oob").unwrap_err().contains("不正なメモリアクセス"));
    }

    #[test]
    fn frame_allocas_are_released_on_return() {
        let src = "kir 1

fn t.inner key=t.inner uses=[] panics=never stack=- ret=u8
  b0():
    %0: ptr = frame.bytes 4
    %1: u8 = const.u8 9
    %2: unit = mem.store.u8 %0 +3, %1
    %3: u8 = mem.load.u8 %0 +3
    ret %3
end t.inner

fn t.outer key=t.outer uses=[] panics=never stack=- ret=u8
  b0():
    %0: u8 = call t.inner()
    ret %0
end t.outer
";
        assert_eq!(exec(src, "t.outer"), Ok(Outcome::Value(9)));
    }

    #[test]
    fn bounds_check_reports_the_interpreter_message() {
        let src = "kir 1

fn t.bc key=t.bc uses=[] panics=may stack=- ret=unit
  b0():
    %0: usize = const.usize 2
    %1: usize = const.usize 5
    %2: unit = index.check %1, %0
    ret %2
end t.bc
";
        assert_eq!(
            exec(src, "t.bc"),
            Ok(Outcome::Panic("index out of bounds: the len is 2 but the index is 5".to_string()))
        );
    }

    #[test]
    fn data_is_static_and_not_a_leak() {
        let src = "kir 1

fn t.d key=t.d uses=[] panics=never stack=- ret=u8
  b0():
    %0: ptr = static.bytes 0102
    %1: usize = const.usize 1
    %2: ptr = ptr.add %0, %1
    %3: u8 = mem.load.u8 %2 +0
    ret %3
end t.d
";
        assert_eq!(exec(src, "t.d"), Ok(Outcome::Value(2)));
    }

    #[test]
    fn deep_recursion_reports_stack_overflow() {
        assert_eq!(run(&deep(), "t.deep", &[1]), Ok(Outcome::StackOverflow));
    }
}
