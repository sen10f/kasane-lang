//! ツリーウォーキング・インタプリタ (stage0)。Python 参照実装 ksn/interp.py に対応する。

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Write;
use std::rc::Rc;

use crate::check::{children as _children, items, nf, nlineno, nname, st, strs};
use crate::parser::V;
use crate::program::{Def, Program, Resolved, Target};
use crate::value::*;

#[path = "ffi_interp.rs"]
mod ffi_interp;

const PRIMS: &[&str] = &[
    "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize", "f32", "f64",
    "bool", "char", "str",
];

/// 定数の評価 (`const` / `static` の初期化式) が超えた場合の panic の文面 (cli.rs の E4024 が見分ける)。
pub const STEP_LIMIT_MSG: &str = "the evaluation of the constant exceeded the step limit";
/// 定数の評価が、評価中の自分自身に戻った (関数越しの循環。cli.rs の E4024 が見分ける)。
pub const CONST_CYCLE_MSG: &str = "the evaluation of the constant depends on itself";
/// 定数の評価が入れ子で深すぎる (Rust のスタックを守る上限。cli.rs の E4024 が見分ける)。
pub const CONST_DEPTH_MSG: &str = "the evaluation of the constants is nested too deeply";
/// `const_value` の入れ子の上限 (関数呼び出しの深さ 2500 と同じ程度。Rust のスタックは実行の子スレッドで 1 GiB)。
const CONST_DEPTH_LIMIT: usize = 2000;

thread_local! {
    /// このスレッドの Rust のスタックの下限 (これより低い番地まで使うと溢れる)。0 は未設定 (検査しない)。
    static STACK_FLOOR: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// 実行用のスレッドの先頭で、スタックの大きさ `size` (スレッドを作ったときの値) を登録する。
/// 木走査の 1 回の呼び出しが使う Rust のスタックは本体の入れ子しだいなので、深さの上限 (`kir_exec::MAX_DEPTH`) より先に
/// スタックが尽きそうなときは、Rust の `SIGSEGV` / abort にせず `stack overflow` と終了コード 102 で止める (余白は 16 MiB)。
pub fn set_stack_floor(size: usize) {
    let here = 0u8;
    let top = &here as *const u8 as usize;
    STACK_FLOOR.with(|f| f.set(top.saturating_sub(size).saturating_add(16 << 20)));
}

fn stack_exhausted() -> bool {
    let here = 0u8;
    let sp = &here as *const u8 as usize;
    STACK_FLOOR.with(|f| sp < f.get())
}

pub struct Interp {
    pub p: Rc<Program>,
    pub depth: usize,
    /// 呼び出しの深さの上限 (今走っているスレッド / タスクの。主スレッドは入口の `depth M`)。
    pub max_depth: usize,
    /// 評価した式の数 (定数の評価の上限のため。`step_limit` が `Some` のときだけ数える)
    pub steps: u64,
    /// 評価ステップの上限 (仕様 4.1)。`None` は上限なし (実行時)。
    pub step_limit: Option<u64>,
    pub const_cache: HashMap<String, Val>,
    /// 評価中の定数の名前 (外側から順)。自分自身に戻る循環と、入れ子の深さを見る。
    const_busy: Vec<String>,
    pub args: Vec<String>,
    pub out: std::io::BufWriter<Box<dyn Write>>,
    /// `ksn debug` / `ksn dap` のデバッガ (`None` なら何も起きない。debug.rs)
    pub dbg: Option<Box<crate::debug::Debugger>>,
    /// デバッガが式を評価している間 true: 副作用・panic のありうる呼び出しを拒否する (debug.rs)
    pub dbg_guard: bool,
    pub reload: Option<std::sync::mpsc::Receiver<Program>>,
    keep: Vec<Rc<Program>>,
    /// std.fs で開いたファイル (記述子 -> ファイル)
    pub files: HashMap<i128, std::fs::File>,
    pub next_fd: i128,
    /// 生ポインタのアドレス → その指す型の名前。`extern` の `*mut T` の戻り値や `core.ptr.*` が
    /// 作ったアドレスに付ける (`core.ptr.read` / `add` が要素の幅を知るため)。ネイティブ生成は
    /// ポインタの静的型から知るが、インタプリタの値は型を持たないので、作った側で覚えておく。
    ptr_elem: RefCell<HashMap<i128, String>>,
    /// いま評価している `core.ptr.*` の呼び出しの明示型引数 (`core.ptr.cast::<T, U>`)。ポインタの値の要素型を決める。
    ptr_targs: Vec<V>,
    /// C に見せている Kasane の配列・構造体・変数の写し (`core.ptr.from_mut`)
    pins: Vec<ffi_interp::Pin>,
    /// C へ渡した関数 (コールバック) の入口の割り当て
    cbs: Vec<ffi_interp::CbSlot>,
    /// いま実行中の `extern` の呼び出しの深さ (0 のときだけ、ピンを読み書きのたびに同期する)
    ffi_depth: usize,
    /// std.net のソケット表
    pub net: crate::net::NetTable,
    pub procs: crate::process::ProcTable,
    /// `std.thread.spawn` したスレッド (spawn の順)。
    ///
    /// インタプリタは単一スレッド (値は `Rc`)。スレッドは spawn の時点では走らせず、本体を保留する。
    /// 保留したスレッドは、(1) `join` が完了を待つとき、(2) `recv` が空のキューで待つとき、に spawn の順で 1 つずつ最後まで走らせる
    /// (途中で切り替えない。走っている最中に別のスレッドへ制御を渡すのは、上の 2 つで待つときだけ)。
    /// `Mutex.lock` は何もしない (各スレッドは最後まで走るので、同時に 2 つがロックを持つことはない)。
    /// チャネルの容量は無視する (`send` は待たない)。`join` / `Mutex` / チャネルだけで通信する正しいプログラムは、
    /// ネイティブ生成と同じ結果になる。どの順で走らせても進めないプログラム (走っているスレッドが、自分を待っている側からの
    /// 後の `send` を `recv` で待つ往復の通信、`join` の循環、送信側が生きたままの空チャネルの `recv` など) は、
    /// 止まらずに「deadlock」の panic で失敗する。
    pub threads: Vec<Rc<ThreadV>>,
    /// `std.task` の協調実行器の状態 (task.rs。`block_on` の間だけ動く)。
    pub tasks: crate::task::Tasks,
}

pub const DEADLOCK_JOIN: &str = "deadlock: a thread waits for itself, or two threads wait for each other (the interpreter runs each thread to completion and cannot suspend one)";
pub const DEADLOCK_RECV: &str = "deadlock: recv() on an empty channel whose Sender is still alive, and no other thread can run \
(native threads would block here forever; or the program needs a back-and-forth with a thread that is waiting on this one, which the \
single-threaded interpreter cannot schedule. Drop the Sender when done sending)";

fn unhex(s: &str) -> String {
    let bytes: Vec<u8> = (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap_or(0)).collect();
    String::from_utf8_lossy(&bytes).to_string()
}

fn parse_int(s: &str) -> i128 {
    s.parse::<i128>().unwrap_or_else(|_| s.parse::<u128>().map(|u| u as i128).unwrap_or(0))
}

// ------------------------------------------------------------ 生ポインタの読み書き
/// 要素の型が分からない生ポインタの読み書きの実行時エラー。
fn unknown_elem(name: &str) -> String {
    format!(
        "{}: the interpreter does not know the element type of the pointer (raw pointer memory); use a native build (`ksn build`)",
        name
    )
}

/// スカラーでない (構造体など) 要素の読み書きの実行時エラー。
fn unsupported_elem(name: &str, elem: &str) -> String {
    format!("{}: the element type `{}` is not supported by the interpreter (raw pointer memory); use a native build (`ksn build`)", name, elem)
}

/// 生ポインタの値の型名 (`Val::Int` の型名) と要素型の対応。扱うのはスカラーだけ。
const PTR_TAGS: &[(&str, &str)] = &[
    ("i8", "*i8"),
    ("i16", "*i16"),
    ("i32", "*i32"),
    ("i64", "*i64"),
    ("isize", "*isize"),
    ("u8", "*u8"),
    ("u16", "*u16"),
    ("u32", "*u32"),
    ("u64", "*u64"),
    ("usize", "*usize"),
    ("f32", "*f32"),
    ("f64", "*f64"),
    ("bool", "*bool"),
];

fn ptr_tag(elem: &str) -> Option<&'static str> {
    PTR_TAGS.iter().find(|(e, _)| *e == elem).map(|(_, t)| *t)
}

fn ptr_tag_elem(tag: &str) -> Option<&'static str> {
    PTR_TAGS.iter().find(|(_, t)| *t == tag).map(|(e, _)| *e)
}

/// 型の構文 (`*mut T` の `T` など) から名前を取り出す (スカラーと名前付きの型だけ)。
fn elem_name(t: &V) -> Option<String> {
    match crate::check::tag(t).as_str() {
        "prim" | "named" => Some(crate::check::st(crate::check::tel(t, 1))),
        _ => None,
    }
}

/// 値そのものが持っている型のタグ (`write` がポインタの要素型を知らないときの代わり)。
fn scalar_ty(v: &Val) -> Option<&'static str> {
    match v {
        Val::Bool(_) => Some("bool"),
        Val::Char(_) => Some("char"),
        Val::F32(_) => Some("f32"),
        Val::Float(_) => Some("f64"),
        Val::Int(_, Some(t)) => Some(t),
        _ => None,
    }
}

/// アドレスから要素型 1 つぶんを読む。
unsafe fn read_scalar(name: &str, p: i128, elem: &str) -> Res {
    let a = p as usize as *const u8;
    Ok(match elem {
        "bool" => Val::Bool(std::ptr::read_unaligned(a) != 0),
        "i8" => int(std::ptr::read_unaligned(a as *const i8) as i128, "i8"),
        "u8" => int(std::ptr::read_unaligned(a) as i128, "u8"),
        "i16" => int(std::ptr::read_unaligned(a as *const i16) as i128, "i16"),
        "u16" => int(std::ptr::read_unaligned(a as *const u16) as i128, "u16"),
        "i32" => int(std::ptr::read_unaligned(a as *const i32) as i128, "i32"),
        "u32" => int(std::ptr::read_unaligned(a as *const u32) as i128, "u32"),
        "i64" => int(std::ptr::read_unaligned(a as *const i64) as i128, "i64"),
        "isize" => int(std::ptr::read_unaligned(a as *const isize) as i128, "isize"),
        "u64" => int(std::ptr::read_unaligned(a as *const u64) as i128, "u64"),
        "usize" => int(std::ptr::read_unaligned(a as *const usize) as i128, "usize"),
        "i128" => int(std::ptr::read_unaligned(a as *const i128), "i128"),
        "u128" => int(std::ptr::read_unaligned(a as *const u128) as i128, "u128"),
        "f32" => Val::F32(std::ptr::read_unaligned(a as *const f32)),
        "f64" => Val::Float(std::ptr::read_unaligned(a as *const f64)),
        "char" => Val::Char(char::from_u32(std::ptr::read_unaligned(a as *const u32)).unwrap_or('\0')),
        _ => return panic(unsupported_elem(name, elem)),
    })
}

/// アドレスへ要素型 1 つぶんを書く (値は宣言の型に合わせて読む)。
unsafe fn write_scalar(name: &str, p: i128, elem: &str, v: &Val) -> Result<(), Ctl> {
    let a = p as usize as *mut u8;
    let i = match v {
        Val::Int(x, _) => *x,
        Val::Bool(b) => *b as i128,
        Val::Char(c) => *c as i128,
        _ => 0,
    };
    let f = match v {
        Val::F32(x) => *x as f64,
        Val::Float(x) => *x,
        Val::Int(x, _) => *x as f64,
        _ => 0.0,
    };
    match elem {
        "bool" => std::ptr::write_unaligned(a, (i != 0) as u8),
        "i8" | "u8" => std::ptr::write_unaligned(a, i as u8),
        "i16" | "u16" => std::ptr::write_unaligned(a as *mut u16, i as u16),
        "i32" | "u32" => std::ptr::write_unaligned(a as *mut u32, i as u32),
        "char" => std::ptr::write_unaligned(a as *mut u32, i as u32),
        "i64" | "u64" | "isize" | "usize" => std::ptr::write_unaligned(a as *mut u64, i as u64),
        "i128" | "u128" => std::ptr::write_unaligned(a as *mut u128, i as u128),
        "f32" => std::ptr::write_unaligned(a as *mut f32, f as f32),
        "f64" => std::ptr::write_unaligned(a as *mut f64, f),
        _ => return panic(unsupported_elem(name, elem)),
    }
    Ok(())
}

impl Interp {
    pub fn new(p: Rc<Program>) -> Interp {
        Interp {
            p,
            depth: 0,
            max_depth: crate::kir_exec::MAX_DEPTH,
            steps: 0,
            step_limit: None,
            const_cache: HashMap::new(),
            const_busy: vec![],
            args: vec![],
            out: std::io::BufWriter::new(Box::new(std::io::stdout())),
            dbg: None,
            dbg_guard: false,
            reload: None,
            keep: vec![],
            files: HashMap::new(),
            next_fd: 3,
            ptr_elem: RefCell::new(HashMap::new()),
            ptr_targs: vec![],
            pins: vec![],
            cbs: vec![],
            ffi_depth: 0,
            net: Default::default(),
            procs: Default::default(),
            threads: vec![],
            tasks: Default::default(),
        }
    }

    /// 式 `e` が局所変数そのもの (`x`) で、その値 `v` がチャネルの端を持つなら、変数を空にする (所有権の移動の近似)。
    /// インタプリタは値を共有して持つので、元の変数を残すと、送信側が「まだ生きている」ままになってしまう。
    pub fn move_out(&self, e: &V, sc: &Scope, v: &Val) {
        if nname(e) != "Path" || !has_chan(v, 0) {
            return;
        }
        let segs = strs(&nf(e)[0]);
        if segs.len() == 1 {
            if let Some(owner) = sc_owner(sc, &segs[0]) {
                owner.borrow_mut().vars.insert(segs[0].clone(), Val::Unit);
            }
        }
    }

    /// 保留中のスレッドを 1 つ (spawn の順で先頭のもの) 最後まで走らせる。走らせるものがなければ false。
    pub fn run_one_pending(&mut self) -> Result<bool, Ctl> {
        let Some(t) = self.threads.iter().find(|t| t.state.get() == 0).cloned() else { return Ok(false) };
        self.run_thread(&t)?;
        Ok(true)
    }

    pub fn run_thread(&mut self, t: &Rc<ThreadV>) -> Result<(), Ctl> {
        match t.state.get() {
            2 => return Ok(()),
            1 => return panic(DEADLOCK_JOIN),
            _ => {}
        }
        t.state.set(1);
        let body = t.body.borrow_mut().take();
        let tracing = self.dbg.is_some();
        if tracing {
            self.dbg_thread_enter(self.threads.iter().position(|x| Rc::ptr_eq(x, t)).unwrap_or(0));
        }
        // スレッドの深さは、そのスレッドの根から数える (上限はスレッドごと。呼んだ側の深さとは別)。
        let (saved_depth, saved_max) = (self.depth, self.max_depth);
        self.depth = 0;
        self.max_depth = t.depth;
        let r = match body {
            Some(b) => self.call_value(&b, vec![]),
            None => Ok(Val::Unit),
        };
        self.depth = saved_depth;
        self.max_depth = saved_max;
        if tracing {
            self.dbg_thread_exit();
        }
        let r = r?;
        // 本体 (取り込んだ値を持つクロージャ) はここで落ちる: 取り込んだ `Sender` はこの時点で閉じる。
        *t.result.borrow_mut() = Some(r);
        t.state.set(2);
        Ok(())
    }

    fn def(&self, i: usize) -> &'static Def {
        // Program は Rc で実行中ずっと生きるため、'static として扱う(参照は内部でのみ使う)。
        unsafe { &*(&self.p.defs[i] as *const Def) }
    }

    // ------------------------------------------------------------ 型による変換
    pub fn coerce(&self, v: Val, ty: &V) -> Val {
        use crate::check::{is_nil, tag, tel};
        if is_nil(ty) {
            return v;
        }
        match tag(ty).as_str() {
            "prim" => {
                let p = st(tel(ty, 1));
                if is_int_ty(&p) {
                    if let Val::Int(x, None) = v {
                        return Val::Int(x, Some(ity(&p)));
                    }
                } else if p == "f32" {
                    match v {
                        Val::Int(x, _) => return Val::F32(x as f32),
                        Val::Float(x) => return Val::F32(x as f32),
                        _ => {}
                    }
                } else if p == "f64" {
                    if let Val::Int(x, _) = v {
                        return Val::Float(x as f64);
                    }
                }
                v
            }
            // 生ポインタ: 宣言の指す型を覚える (`core.ptr.read` / `write` / `add` が幅を知るため)。
            "ptr" => {
                if let Val::Int(a, _) = &v {
                    if let Some(e) = elem_name(tel(ty, 2)) {
                        let e = self.type_fqn(&e, "").unwrap_or(e);
                        self.ptr_elem.borrow_mut().insert(*a, e);
                    }
                }
                v
            }
            "tuple" => {
                if let Val::Tuple(t) = &v {
                    let tys = items(tel(ty, 1));
                    if t.len() == tys.len() {
                        return Val::Tuple(Rc::new(t.iter().zip(tys).map(|(x, y)| self.coerce(x.clone(), y)).collect()));
                    }
                }
                v
            }
            "array" => {
                if let Val::Arr(l) = &v {
                    let items: Vec<Val> = l.borrow().iter().map(|x| self.coerce(x.clone(), tel(ty, 1))).collect();
                    return Val::Arr(new_list(items));
                }
                v
            }
            "ref" => {
                if matches!(v, Val::Ref(_)) {
                    v
                } else {
                    self.coerce(v, tel(ty, 2))
                }
            }
            "named" => {
                if st(tel(ty, 1)) == "Option" && !items(tel(ty, 2)).is_empty() {
                    if let Val::Enum(e) = &v {
                        if !e.pl.is_empty() {
                            let pl = e.pl.iter().map(|x| self.coerce(x.clone(), &items(tel(ty, 2))[0])).collect();
                            return Val::Enum(Rc::new(EnumV { en: e.en.clone(), var: e.var.clone(), pl }));
                        }
                    }
                }
                v
            }
            _ => v,
        }
    }

    pub fn is_copy(&self, tyfqn: &str) -> bool {
        self.p.types.get(tyfqn).map_or(false, |&i| self.p.defs[i].derive.iter().any(|x| x == "Copy"))
    }

    pub fn cp(&self, v: Val) -> Val {
        if let Val::Struct(s) = &v {
            if self.is_copy(&s.ty) {
                let f = s.f.borrow().iter().map(|(k, x)| (k.clone(), self.cp(x.clone()))).collect();
                return self.mk_struct(s.ty.clone(), f);
            }
        }
        v
    }

    /// 構造体の値を作る。フィールドは型定義の宣言順に並べる (4.3)。格納順を宣言順に揃えておくと、
    /// `==`・`hkey` (HashMap の鍵)・Debug の表示が、リテラルに書いた順に依らず一致する。
    /// 型定義が無いとき (標準の型) は渡された順のまま。
    pub fn mk_struct(&self, ty: String, mut fields: Vec<(String, Val)>) -> Val {
        if let Some(&i) = self.p.types.get(&ty) {
            let order = &self.p.defs[i].fields;
            fields.sort_by_key(|(k, _)| order.iter().position(|f| f.0 == *k).unwrap_or(usize::MAX));
        }
        Val::Struct(Rc::new(StructV { ty, f: RefCell::new(fields) }))
    }

    // ------------------------------------------------------------ 定数
    pub fn const_value(&mut self, di: usize) -> Res {
        let d = self.def(di);
        if let Some(v) = self.const_cache.get(&d.name) {
            return Ok(v.clone());
        }
        // 評価中の定数に戻ってきたら循環 (`static A = B` / `static B = A`、関数越しの読み戻し)。無限に再帰せず panic にする。
        if self.const_busy.iter().any(|n| *n == d.name) {
            return Err(Ctl::Panic(CONST_CYCLE_MSG.into()));
        }
        if self.const_busy.len() >= CONST_DEPTH_LIMIT {
            return Err(Ctl::Panic(CONST_DEPTH_MSG.into()));
        }
        let sc = new_scope(None, Rc::from(d.file.as_str()));
        self.const_busy.push(d.name.clone());
        let r = self.eval(d.init.as_ref().unwrap(), &sc);
        self.const_busy.pop();
        let v = self.coerce(r?, &d.ret);
        self.const_cache.insert(d.name.clone(), v.clone());
        Ok(v)
    }

    // ------------------------------------------------------------ 呼び出し
    fn poll_reload(&mut self) {
        let np = match &self.reload {
            Some(rx) => rx.try_recv().ok(),
            None => None,
        };
        if let Some(np) = np {
            let old = std::mem::replace(&mut self.p, Rc::new(np));
            self.keep.push(old);
            eprintln!("hot reload: function bodies updated");
        }
    }

    pub fn call_def(&mut self, di: usize, args: Vec<Val>) -> Res {
        self.poll_reload();
        let d = self.def(di);
        if args.len() != d.params.len() {
            return panic(format!("arity mismatch calling {}: expected {}, got {}", d.name, d.params.len(), args.len()));
        }
        let sc = new_scope(None, Rc::from(d.file.as_str()));
        for ((pn, pty), a) in d.params.iter().zip(args) {
            let a = self.coerce(a, pty);
            sc_declare(&sc, pn, a);
        }
        if self.dbg_guard {
            self.dbg_guard_def(d)?;
        }
        self.depth += 1;
        if self.depth > self.max_depth || stack_exhausted() {
            eprintln!("stack overflow");
            return Err(Ctl::Exit(102));
        }
        let tracing = self.dbg.is_some();
        if tracing {
            self.dbg_enter(d.name.clone(), &sc, true);
        }
        let r = self.eval_block_body(d.body.as_ref().unwrap(), &sc);
        if tracing {
            self.dbg_leave(&r)?;
        }
        self.depth -= 1;
        let v = match r {
            Ok(v) => v,
            Err(Ctl::Return(v)) => v,
            Err(e) => return Err(e),
        };
        let v = self.convert_err(v, &d.ret, &d.file)?;
        Ok(self.coerce(v, &d.ret))
    }

    /// 4.1: `?` のエラー変換。戻り値型が `Result<_, E2>` で `E2` が `derive [From]` の `enum` のとき、
    /// 返す値が `Err(e)` で `e` の型が `E1` (`E2` と別の型) なら、`E1` を唯一のペイロードに持つバリアントで包み直す。
    /// 検査を通ったプログラムでは、型の違う `Err` が関数から出る経路は `?` だけなので、
    /// `?` の位置で変換するのと同じ結果になる (包むのはバリアントの構築だけで、効果もpanicもない)。
    fn convert_err(&mut self, v: Val, ret: &V, file: &str) -> Res {
        use crate::check::{tag, tel};
        if self.p.from_enums.is_empty() || tag(ret) != "named" || st(tel(ret, 1)) != "Result" {
            return Ok(v);
        }
        let Val::Enum(en) = &v else { return Ok(v) };
        if en.en != "Result" || en.var != "Err" || en.pl.is_empty() {
            return Ok(v);
        }
        let Some(e2) = items(tel(ret, 2)).get(1) else { return Ok(v) };
        if tag(e2) != "named" {
            return Ok(v);
        }
        let segs: Vec<String> = st(tel(e2, 1)).split('.').map(String::from).collect();
        let e2name = self.p.expand(&segs, file).join(".");
        if !self.p.from_enums.contains(&e2name) {
            return Ok(v);
        }
        let payload = dr(&en.pl[0]);
        let key = match &payload {
            Val::Struct(s) => Some(s.ty.clone()),
            Val::Enum(e) => Some(e.en.clone()),
            Val::Int(_, Some(k)) => Some(k.to_string()),
            Val::Bool(_) => Some("bool".to_string()),
            Val::Char(_) => Some("char".to_string()),
            _ => None,
        };
        if key.as_deref() == Some(e2name.as_str()) {
            return Ok(v);
        }
        let Some(&ti) = self.p.types.get(&e2name) else { return Ok(v) };
        let cands: Vec<&String> = self.p.defs[ti]
            .variants
            .iter()
            .filter(|(_, pl)| pl.len() == 1)
            .filter(|(_, pl)| match &key {
                Some(k) => crate::types::from_v(&pl[0], &std::collections::HashSet::new()).show() == *k,
                None => true,
            })
            .map(|(vn, _)| vn)
            .collect();
        // 値の型が決まらないとき (文字列・コレクションなど) は、包めるバリアントが 1 つだけのときに限る。
        let [vn] = cands.as_slice() else { return Ok(v) };
        let vn = (*vn).clone();
        let converted = self.mk_variant(&e2name, &vn, vec![en.pl[0].clone()])?;
        Ok(mk_enum("Result", "Err", vec![converted]))
    }

    /// テスト本体など、引数なしの本体だけを実行する。
    pub fn call_body(&mut self, di: usize) -> Res {
        let d = self.def(di);
        let sc = new_scope(None, Rc::from(d.file.as_str()));
        let tracing = self.dbg.is_some();
        if tracing {
            self.dbg_enter(d.name.clone(), &sc, true);
        }
        let r = self.eval_block_body(d.body.as_ref().unwrap(), &sc);
        if tracing {
            self.dbg_leave(&r)?;
        }
        match r {
            Ok(v) => Ok(v),
            Err(Ctl::Return(v)) => Ok(v),
            Err(e) => Err(e),
        }
    }

    pub fn call_value(&mut self, f: &Val, args: Vec<Val>) -> Res {
        let f = dr(f);
        match &f {
            Val::Closure(c) => {
                let sc = new_scope(Some(&c.scope), Rc::from(""));
                for ((pn, pty), a) in c.params.iter().zip(args) {
                    let a = self.coerce(a, pty);
                    sc_declare(&sc, pn, a);
                }
                let tracing = self.dbg.is_some();
                if tracing {
                    self.dbg_enter("<closure>".into(), &sc, false);
                } else if self.dbg_guard {
                    return panic("debugger: calling a closure is not allowed in a debugger expression");
                }
                let r = self.eval(&c.body, &sc);
                if tracing {
                    self.dbg_leave(&r)?;
                }
                let r = match r {
                    Ok(v) => v,
                    Err(Ctl::Return(v)) => v,
                    Err(e) => return Err(e),
                };
                let file = c.scope.borrow().file.clone();
                let r = self.convert_err(r, &c.ret, &file)?;
                Ok(self.coerce(r, &c.ret))
            }
            Val::Func(i) => self.call_def(*i, args),
            Val::Native(n) => self.call_native(&n.clone(), args),
            Val::Ctor(c) => {
                let (en, vn) = (&c.0, &c.1);
                self.mk_variant(en, vn, args)
            }
            _ => panic("not callable"),
        }
    }

    fn mk_variant(&self, en: &str, vn: &str, args: Vec<Val>) -> Res {
        if let Some(&ei) = self.p.types.get(en) {
            let d = &self.p.defs[ei];
            if let Some((_, tys)) = d.variants.iter().find(|v| v.0 == vn) {
                let pl = args.into_iter().zip(tys).map(|(a, t)| self.coerce(a, t)).collect();
                return Ok(mk_enum(en, vn, pl));
            }
        }
        Ok(mk_enum(en, vn, args))
    }

    // ------------------------------------------------------------ 評価
    pub fn eval_block_body(&mut self, blk: &V, sc: &Scope) -> Res {
        self.run_stmts(items(&nf(blk)[0]), sc)
    }

    /// 文を順に実行する。`infer::annotate_owned_drops` が印を付けた `let` (所有する局所で、借用の文脈でしか使われないもの) の
    /// 束縛は、囲みを抜けるとき (値・`return`・`?`・`break` のどの経路でも) 解放済みにする。panic・終了は巻き戻さない。
    fn run_stmts(&mut self, stmts: &[V], sc: &Scope) -> Res {
        let mut v = Val::Unit;
        let mut dying: Vec<String> = vec![];
        for s in stmts {
            let r = self.exec_stmt(s, sc);
            match r {
                Ok(x) => v = x,
                Err(e) => {
                    if !matches!(e, Ctl::Panic(_) | Ctl::Exit(_)) {
                        Self::kill_bindings(&dying, sc);
                    }
                    return Err(e);
                }
            }
            if nname(s) == "Let" && matches!(nf(s).get(4), Some(V::S(m)) if m == "own") {
                dying.push(st(&nf(&nf(s)[0])[0]));
            }
        }
        // 末尾の式の値が束縛そのものを返す形は、印を付ける側が (借用の文脈でない使い方として) 除いている。
        Self::kill_bindings(&dying, sc);
        Ok(v)
    }

    fn kill_bindings(names: &[String], sc: &Scope) {
        for n in names {
            if let Some(v) = sc.borrow().vars.get(n) {
                mark_dead(v);
            }
        }
    }

    fn exec_stmt(&mut self, s: &V, sc: &Scope) -> Res {
        if self.dbg.is_some() {
            self.dbg_stmt(s, sc)?;
        }
        let f = nf(s);
        match nname(s) {
            "Let" => {
                let v = self.eval(&f[3], sc)?;
                self.move_out(&f[3], sc, &v);
                let mut v = self.cp(v);
                if !matches!(f[2], V::Nil) {
                    v = self.coerce(v, &f[2]);
                }
                if !self.bind(&f[0], v, sc)? {
                    return panic("refutable let pattern failed");
                }
                Ok(Val::Unit)
            }
            "Assign" => {
                let v = self.eval(&f[1], sc)?;
                let v = self.cp(v);
                self.assign(&f[0], v, sc)?;
                Ok(Val::Unit)
            }
            _ => self.eval(&f[0], sc),
        }
    }

    fn eval_block(&mut self, blk: &V, sc: &Scope) -> Res {
        let inner = new_scope(Some(sc), Rc::from(""));
        self.run_stmts(items(&nf(blk)[0]), &inner)
    }

    pub fn eval(&mut self, e: &V, sc: &Scope) -> Res {
        if let Some(l) = self.step_limit {
            self.steps += 1;
            if self.steps > l {
                return Err(Ctl::Panic(STEP_LIMIT_MSG.into()));
            }
        }
        let f = nf(e);
        match nname(e) {
            "Lit" => Ok(self.lit(e)),
            "Block" => self.eval_block(e, sc),
            "Unsafe" => self.eval_block(&f[2], sc),
            // `with 名前 = 初期化式 close 後始末式 { 本体 }` (4.4): 本体を抜けるあらゆる経路 (値・`return` /
            // `?` / `break` / `continue`) で、後始末式をちょうど 1 回、出口の値を決めたあとに実行する。
            // panic とプロセス終了は巻き戻さない (後始末なし)。初期化式が失敗したら束縛も後始末もない。
            "With" => {
                let v = self.eval(&f[2], sc)?;
                let v = self.cp(v);
                let s2 = new_scope(Some(sc), Rc::from(""));
                self.bind(&f[0], v, &s2)?;
                let r = self.eval(&f[4], &s2);
                if matches!(r, Err(Ctl::Panic(_)) | Err(Ctl::Exit(_))) {
                    return r;
                }
                self.eval(&f[3], &s2)?;
                r
            }
            "TupleE" => {
                let mut out = vec![];
                for x in items(&f[0]) {
                    let v = self.eval(x, sc)?;
                    out.push(self.cp(v));
                }
                Ok(Val::Tuple(Rc::new(out)))
            }
            "ArrayE" => {
                let mut out = vec![];
                for x in items(&f[0]) {
                    let v = self.eval(x, sc)?;
                    out.push(self.cp(v));
                }
                Ok(Val::Arr(new_list(out)))
            }
            "ArrayRep" => {
                let v = self.eval(&f[0], sc)?;
                let n = match &f[1] {
                    V::N(s) => parse_int(s) as usize,
                    V::L(xs) if xs.len() == 2 => {
                        let segs: Vec<V> = st(&xs[1]).split('.').map(|x| V::S(x.to_string())).collect();
                        let p = V::Node("Path", vec![V::L(segs), V::Nil], 0, 0);
                        match dr(&self.eval(&p, sc)?) {
                            Val::Int(x, _) => x as usize,
                            _ => 0,
                        }
                    }
                    _ => 0,
                };
                let out: Vec<Val> = (0..n).map(|_| self.cp(v.clone())).collect();
                Ok(Val::Arr(new_list(out)))
            }
            "StructLit" => {
                let name = st(&f[0]);
                let segs: Vec<String> = name.split('.').map(String::from).collect();
                let file = sc.borrow().file.to_string();
                let fq = self.p.expand(&segs, &file).join(".");
                let ftypes: Vec<(String, V)> = self
                    .p
                    .types
                    .get(&fq)
                    .map(|&i| self.p.defs[i].fields.iter().map(|x| (x.0.clone(), x.1.clone())).collect())
                    .unwrap_or_default();
                let mut fields = vec![];
                for fe in items(&f[1]) {
                    let fnm = st(&items(fe)[0]);
                    let v = self.eval(&items(fe)[1], sc)?;
                    let v = self.cp(v);
                    let v = match ftypes.iter().find(|x| x.0 == fnm) {
                        Some((_, t)) => self.coerce(v, t),
                        None => v,
                    };
                    fields.push((fnm, v));
                }
                Ok(self.mk_struct(fq, fields))
            }
            "Closure" => {
                let params = items(&f[0]).iter().map(|p| (st(&items(p)[0]), items(p)[1].clone())).collect();
                // 4.3: 取り込みは作る時点で決まる。`move x` と `x` は値の写し、`&x` は値の共有、
                // `&mut x` は外側の変数への参照 (本体の `*x = v` / `x = v` がそこへ書く)。
                // 外側のスコープを共有すると、取り込み後の外側の書き換えが見えてしまうので、取り込みだけを入れた
                // 専用のスコープを作る (取り込み漏れの名前は、従来どおり外側の scope へ届く。漏れの検査は別件)。
                let caps = items(&f[2]).iter().map(|c| (st(crate::check::tel(c, 0)), st(crate::check::tel(c, 1)))).collect();
                let cs = new_scope(Some(sc), Rc::from(""));
                for c in items(&f[2]) {
                    let (k, n) = (st(&items(c)[0]), st(&items(c)[1]));
                    let Some(owner) = sc_owner(sc, &n) else { continue };
                    let cur = owner.borrow().vars[&n].clone();
                    let v = match k.as_str() {
                        "&mut" => match cur {
                            Val::Ref(_) => cur,
                            _ => Val::Ref(Rc::new(VarPlace(owner, n.clone()))),
                        },
                        "&" => cur,
                        _ => {
                            // `move x` でチャネルの端を取り込むと、端はクロージャへ移る: 元の変数は空にする
                            // (残すと、送信側が「まだ生きている」ままになる。`move_out` と同じ近似)。
                            if k == "move" && has_chan(&cur, 0) {
                                owner.borrow_mut().vars.insert(n.clone(), Val::Unit);
                            }
                            self.cp(cur)
                        }
                    };
                    sc_declare(&cs, &n, v);
                }
                Ok(Val::Closure(Rc::new(ClosureV { params, ret: f[1].clone(), body: f[3].clone(), scope: cs, caps })))
            }
            "If" => {
                if self.truthy(&f[0], sc)? {
                    self.eval(&f[1], sc)
                } else if !matches!(f[2], V::Nil) {
                    self.eval(&f[2], sc)
                } else {
                    Ok(Val::Unit)
                }
            }
            "Match" => {
                let v = self.eval(&f[0], sc)?;
                self.move_out(&f[0], sc, &v);
                for (ai, arm) in items(&f[1]).iter().enumerate() {
                    let s2 = new_scope(Some(sc), Rc::from(""));
                    if self.bind(&items(arm)[0], v.clone(), &s2)? {
                        let r = self.eval(&items(arm)[1], &s2);
                        // `infer::annotate_owned_drops` が印を付けた腕の束縛 (値で受けた所有する値) は、腕の終わりで死ぬ。
                        if let Some(per_arm) = f.get(2) {
                            if !matches!(r, Err(Ctl::Panic(_)) | Err(Ctl::Exit(_))) {
                                if let Some(ns) = items(per_arm).get(ai) {
                                    let names: Vec<String> = items(ns).iter().map(st).collect();
                                    Self::kill_bindings(&names, &s2);
                                }
                            }
                        }
                        return r;
                    }
                }
                panic(format!("non-exhaustive match at line {}", nlineno(e)))
            }
            "Loop" => {
                loop {
                    match self.eval(&f[0], sc) {
                        Ok(_) | Err(Ctl::Continue) => {}
                        Err(Ctl::Break) => break,
                        Err(e) => return Err(e),
                    }
                }
                Ok(Val::Unit)
            }
            "While" => {
                while self.truthy(&f[0], sc)? {
                    match self.eval(&f[1], sc) {
                        Ok(_) | Err(Ctl::Continue) => {}
                        Err(Ctl::Break) => break,
                        Err(e) => return Err(e),
                    }
                }
                Ok(Val::Unit)
            }
            "For" => {
                let it = self.eval(&f[1], sc)?;
                let it = self.to_iter(&it)?;
                loop {
                    let o = self.iter_next(&it)?;
                    let Val::Enum(en) = &o else { break };
                    if en.var == "None" {
                        break;
                    }
                    let s2 = new_scope(Some(sc), Rc::from(""));
                    self.bind(&f[0], en.pl[0].clone(), &s2)?;
                    let r = self.eval(&f[2], &s2);
                    if matches!(f.get(3), Some(V::S(m)) if m == "own") && !matches!(r, Err(Ctl::Panic(_)) | Err(Ctl::Exit(_))) {
                        Self::kill_bindings(&[st(&nf(&f[0])[0])], &s2);
                    }
                    match r {
                        Ok(_) | Err(Ctl::Continue) => {}
                        Err(Ctl::Break) => break,
                        Err(e) => return Err(e),
                    }
                }
                Ok(Val::Unit)
            }
            "Break" => Err(Ctl::Break),
            "Continue" => Err(Ctl::Continue),
            "Return" => {
                let v = if matches!(f[0], V::Nil) { Val::Unit } else { self.eval(&f[0], sc)? };
                Err(Ctl::Return(v))
            }
            "ExprStmt" | "Let" | "Assign" => self.exec_stmt(e, sc),
            "Try" => {
                let v = dr(&self.eval(&f[0], sc)?);
                self.move_out(&f[0], sc, &v);
                if let Val::Enum(en) = &v {
                    if en.en == "Result" {
                        if en.var == "Ok" {
                            return Ok(en.pl[0].clone());
                        }
                        return Err(Ctl::Return(v.clone()));
                    }
                    if en.en == "Option" {
                        if en.var == "Some" {
                            return Ok(en.pl[0].clone());
                        }
                        return Err(Ctl::Return(v.clone()));
                    }
                }
                panic("? applied to non-Option/Result")
            }
            "Unary" => self.unary(e, sc),
            "Bin" => self.binary(e, sc),
            "Path" => self.e_path(e, sc),
            "Field" => {
                let b = dr(&self.eval(&f[0], sc)?);
                check_alive(&b)?;
                let name = st(&f[1]);
                if let Val::Struct(s) = &b {
                    if let Some(v) = s.get(&name) {
                        return Ok(self.cp(v));
                    }
                }
                // `String` の格納 (`self.buf`、Kasane 版の本体だけが書く): 格納を共有する `Buf<u8>` の構造体。
                if let (Val::StrBuf(sb), "buf") = (&b, name.as_str()) {
                    return Ok(str_buf_view(sb));
                }
                panic(format!("no field {}", name))
            }
            "Index" => {
                let b = dr(&self.eval(&f[0], sc)?);
                check_alive(&b)?;
                let i = match dr(&self.eval(&f[1], sc)?) {
                    Val::Int(x, _) => x,
                    _ => return panic("bad index"),
                };
                // 要素を 1 つだけ取り出す (列全体を複製しない)。
                let len = seq_len(&b).unwrap_or(0);
                if i < 0 || i as usize >= len {
                    return panic(format!("index out of bounds: the len is {} but the index is {}", len, i));
                }
                Ok(seq_get(&b, i as usize).unwrap_or(Val::Unit))
            }
            "Call" => self.e_call(e, sc),
            "MCall" => {
                let recv = self.eval(&f[0], sc)?;
                let name = st(&f[1]);
                let mut args = vec![];
                for a in items(&f[2]) {
                    let v = self.eval(a, sc)?;
                    args.push(self.cp(v));
                }
                let recv_expr = f[0].clone();
                let sc2 = sc.clone();
                self.invoke_method(recv, &name, args, Some((&recv_expr, &sc2)))
            }
            n => panic(format!("cannot evaluate node {}", n)),
        }
    }

    fn truthy(&mut self, e: &V, sc: &Scope) -> Result<bool, Ctl> {
        Ok(matches!(dr(&self.eval(e, sc)?), Val::Bool(true)))
    }

    fn lit(&self, e: &V) -> Val {
        let f = nf(e);
        match st(&f[0]).as_str() {
            "int" => {
                let sfx = match &f[2] {
                    V::S(s) => Some(ity(s)),
                    _ => None,
                };
                Val::Int(parse_int(&st(&f[1])), sfx)
            }
            "float" => {
                let x = st(&f[2]).parse::<f64>().unwrap_or(0.0);
                if matches!(&f[1], V::S(s) if s.as_str() == "f32") { Val::F32(x as f32) } else { Val::Float(x) }
            }
            "str" => Val::Str(Rc::from(unhex(&st(&f[1])).as_str())),
            "char" => Val::Char(char::from_u32(parse_int(&st(&f[1])) as u32).unwrap_or('\0')),
            "bool" => Val::Bool(matches!(f[1], V::B(true))),
            "byte" => int(parse_int(&st(&f[1])), "u8"),
            "bstr" => Val::Arr(new_list(items(&f[1]).iter().map(|x| int(parse_int(&st(x)), "u8")).collect())),
            _ => Val::Unit,
        }
    }

    fn unary(&mut self, e: &V, sc: &Scope) -> Res {
        let f = nf(e);
        let op = st(&f[0]);
        match op.as_str() {
            "&mut" => return self.eval_place(&f[1], sc),
            "&" => return self.eval(&f[1], sc),
            "*" => {
                let v = self.eval(&f[1], sc)?;
                return Ok(self.cp(dr(&v)));
            }
            _ => {}
        }
        let v = dr(&self.eval(&f[1], sc)?);
        match (op.as_str(), &v) {
            ("-", Val::Int(x, t)) => match t {
                Some(ty) if *ty == "u128" || !signed(ty) => {
                    if *x == 0 { Ok(Val::Int(0, *t)) } else { panic("attempt to negate an unsigned integer".to_string()) }
                }
                Some(ty) if -*x > tmax(ty) => panic("attempt to negate with overflow".to_string()),
                _ => Ok(Val::Int(-x, *t)),
            },
            ("-", Val::Float(x)) => Ok(Val::Float(-x)),
            ("-", Val::F32(x)) => Ok(Val::F32(-x)),
            ("!", Val::Bool(b)) => Ok(Val::Bool(!b)),
            ("!", Val::Int(x, t)) => Ok(Val::Int(wrap(!x, t.unwrap_or("i32")), *t)),
            _ => panic(format!("bad unary {}", op)),
        }
    }

    fn binary(&mut self, e: &V, sc: &Scope) -> Res {
        let f = nf(e);
        let op = st(&f[0]);
        if op == "&&" {
            return Ok(Val::Bool(self.truthy(&f[1], sc)? && self.truthy(&f[2], sc)?));
        }
        if op == "||" {
            return Ok(Val::Bool(self.truthy(&f[1], sc)? || self.truthy(&f[2], sc)?));
        }
        let a = dr(&self.eval(&f[1], sc)?);
        let b = dr(&self.eval(&f[2], sc)?);
        match op.as_str() {
            "==" | "!=" => {
                let r = self.v_eq(&a, &b)?;
                return Ok(Val::Bool(if op == "==" { r } else { !r }));
            }
            "<" | "<=" | ">" | ">=" => {
                let c = self.v_cmp(&a, &b)?;
                let r = match c {
                    None => false,
                    Some(c) => match op.as_str() {
                        "<" => c < 0,
                        "<=" => c <= 0,
                        ">" => c > 0,
                        _ => c >= 0,
                    },
                };
                return Ok(Val::Bool(r));
            }
            _ => {}
        }
        if let (Val::Bool(x), Val::Bool(y)) = (&a, &b) {
            return Ok(Val::Bool(match op.as_str() {
                "&" => x & y,
                "|" => x | y,
                _ => x ^ y,
            }));
        }
        let fl = |v: &Val| match v {
            Val::Float(x) => Some(*x),
            Val::Int(x, _) => Some(*x as f64),
            _ => None,
        };
        if matches!(a, Val::F32(_)) || matches!(b, Val::F32(_)) {
            let g = |v: &Val| match v {
                Val::F32(x) => *x,
                Val::Float(x) => *x as f32,
                Val::Int(x, _) => *x as f32,
                _ => 0.0,
            };
            let (x, y) = (g(&a), g(&b));
            return match op.as_str() {
                "+" => Ok(Val::F32(crate::fmath::nan_rule32(x, y, x + y))),
                "-" => Ok(Val::F32(crate::fmath::nan_rule32(x, y, x - y))),
                "*" => Ok(Val::F32(crate::fmath::nan_rule32(x, y, x * y))),
                "/" => Ok(Val::F32(crate::fmath::nan_rule32(x, y, x / y))),
                _ => panic(format!("bad float operator {}", op)),
            };
        }
        if matches!(a, Val::Float(_)) || matches!(b, Val::Float(_)) {
            let (x, y) = (fl(&a).unwrap_or(0.0), fl(&b).unwrap_or(0.0));
            return match op.as_str() {
                "+" => Ok(Val::Float(crate::fmath::nan_rule64(x, y, x + y))),
                "-" => Ok(Val::Float(crate::fmath::nan_rule64(x, y, x - y))),
                "*" => Ok(Val::Float(crate::fmath::nan_rule64(x, y, x * y))),
                "/" => Ok(Val::Float(crate::fmath::nan_rule64(x, y, x / y))),
                _ => panic(format!("bad float operator {}", op)),
            };
        }
        if let (Val::Int(x, ta), Val::Int(y, tb)) = (&a, &b) {
            let ty = ta.or(*tb);
            let r = match op.as_str() {
                "&" => x & y,
                "|" => x | y,
                "^" => x ^ y,
                "+" | "-" | "*" | "/" | "%" => return int_arith(op.as_str(), *x, *y, ty),
                _ => return panic(format!("operator {} is not defined for integers", op)),
            };
            return Ok(Val::Int(r, ty));
        }
        panic(format!("bad operands for {}", op))
    }

    /// 等値 (`==` / `eq`)。手書きの `eq` を持つ型はそれを呼ぶ。派生の比較 (構造体・列挙型・`Option` / `Result`・タプル・列) は
    /// 要素ごとにこの関数を再帰で呼ぶので、入れ子の利用者型の `eq` も使われる (4.3)。
    pub fn v_eq(&mut self, a: &Val, b: &Val) -> Result<bool, Ctl> {
        let (a, b) = (dr(a), dr(b));
        match (&a, &b) {
            // `Vec` の値は要素の列として比べる (格納の容量で違わないように、下の列の枝へ)。
            (Val::Struct(x), Val::Struct(y)) if x.ty != VEC_TY => {
                if let Some(&di) = self.p.funcs.get(&format!("{}.eq", x.ty)) {
                    let r = self.call_def(di, vec![a.clone(), b.clone()])?;
                    return Ok(matches!(r, Val::Bool(true)));
                }
                if x.ty != y.ty {
                    return Ok(false);
                }
                let xs: Vec<(String, Val)> = x.f.borrow().clone();
                for (k, v) in xs {
                    let Some(w) = y.get(&k) else { return Ok(false) };
                    if !self.v_eq(&v, &w)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            (Val::Enum(x), Val::Enum(y)) => {
                if x.en != "Option" && x.en != "Result" {
                    if let Some(&di) = self.p.funcs.get(&format!("{}.eq", x.en)) {
                        let r = self.call_def(di, vec![a.clone(), b.clone()])?;
                        return Ok(matches!(r, Val::Bool(true)));
                    }
                }
                if x.var != y.var || x.en != y.en || x.pl.len() != y.pl.len() {
                    return Ok(false);
                }
                for (p, q) in x.pl.iter().zip(&y.pl) {
                    if !self.v_eq(p, q)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            _ if str_of(&a).is_none() => match (seq_items(&a), seq_items(&b)) {
                (Some(x), Some(y)) => {
                    if x.len() != y.len() {
                        return Ok(false);
                    }
                    for (p, q) in x.iter().zip(&y) {
                        if !self.v_eq(p, q)? {
                            return Ok(false);
                        }
                    }
                    Ok(true)
                }
                _ => Ok(veq(&a, &b)),
            },
            _ => Ok(veq(&a, &b)),
        }
    }

    fn order_of(&self, en: &str) -> Box<dyn Fn(&str, &str) -> (usize, usize)> {
        let names: Vec<String> = match en {
            "Option" => vec!["None".into(), "Some".into()],
            "Result" => vec!["Ok".into(), "Err".into()],
            _ => self
                .p
                .types
                .get(en)
                .map(|&i| self.p.defs[i].variants.iter().map(|v| v.0.clone()).collect())
                .unwrap_or_default(),
        };
        Box::new(move |a, b| {
            (names.iter().position(|x| x == a).unwrap_or(0), names.iter().position(|x| x == b).unwrap_or(0))
        })
    }

    fn ordering(v: &Val) -> i32 {
        if let Val::Enum(e) = v {
            return match e.var.as_str() {
                "Less" => -1,
                "Equal" => 0,
                _ => 1,
            };
        }
        0
    }

    /// 演算子 `<` 等の順序 (-1 / 0 / 1、NaN は None)。`<` は `partial_cmp` から導く (4.3): 手書きの `partial_cmp` があればそれを呼び、
    /// なければ派生の順序 (列挙子は宣言順、フィールド・ペイロード・列の要素は辞書順)。
    pub fn v_cmp(&mut self, a: &Val, b: &Val) -> Result<Option<i32>, Ctl> {
        self.ord_of(a, b, false, false)
    }

    /// 順序の本体。`total` が真なら `cmp` の規則 (`min` / `max` が従う)、偽なら `partial_cmp` の規則。
    /// `derived_top` が真なら、派生の `cmp` / `partial_cmp` の入口なので、利用者の型の最上位では手書きのメソッドを引かず
    /// フィールドごとに比べる (手書きの `partial_cmp` が派生の `cmp` を呼ぶときに無限に再帰しないため)。
    /// 入れ子の要素は、どの入口でも手書きのメソッドを使う (4.3)。
    fn ord_of(&mut self, a: &Val, b: &Val, total: bool, derived_top: bool) -> Result<Option<i32>, Ctl> {
        let (a, b) = (dr(a), dr(b));
        if !derived_top {
            if let Some(r) = self.user_ord(&a, &b, total)? {
                return Ok(r);
            }
        }
        self.ord_fields(&a, &b, total)
    }

    /// 利用者の型の手書きの `partial_cmp` (`total` なら `cmp`) を呼ぶ。持たなければ `None`。
    fn user_ord(&mut self, a: &Val, b: &Val, total: bool) -> Result<Option<Option<i32>>, Ctl> {
        let ty = match a {
            Val::Struct(s) => s.ty.clone(),
            Val::Enum(e) if e.en != "Option" && e.en != "Result" => e.en.clone(),
            _ => return Ok(None),
        };
        let name = if total { "cmp" } else { "partial_cmp" };
        let Some(&di) = self.p.funcs.get(&format!("{}.{}", ty, name)) else { return Ok(None) };
        let r = self.call_def(di, vec![a.clone(), b.clone()])?;
        Ok(Some(match &r {
            Val::Enum(e) if !total => {
                if e.var == "None" {
                    None
                } else {
                    Some(Self::ordering(&e.pl[0]))
                }
            }
            _ => Some(Self::ordering(&r)),
        }))
    }

    /// 派生の順序 (手書きのメソッドは最上位で引かない)。構造体のフィールドは型定義の宣言順の辞書式 (4.3)。
    fn ord_fields(&mut self, a: &Val, b: &Val, total: bool) -> Result<Option<i32>, Ctl> {
        match (a, b) {
            (Val::Enum(x), Val::Enum(y)) => {
                if x.var != y.var {
                    let (ia, ib) = self.order_of(&x.en)(&x.var, &y.var);
                    return Ok(Some(if ia < ib { -1 } else { 1 }));
                }
                let (xs, ys) = (x.pl.clone(), y.pl.clone());
                self.cmp_seq(&xs, &ys, total)
            }
            // `Vec` は要素の列として比べる (下の列の枝)。
            (Val::Struct(x), Val::Struct(y)) if x.ty != VEC_TY => {
                // 型定義の宣言順 (`fields`) で並べる。定義が無いとき (標準の型) は格納順。
                let names: Vec<String> = match self.p.types.get(&x.ty) {
                    Some(&i) => self.p.defs[i].fields.iter().map(|f| f.0.clone()).collect(),
                    None => x.f.borrow().iter().map(|(k, _)| k.clone()).collect(),
                };
                let xs: Vec<Val> = names.iter().map(|n| x.get(n).unwrap_or(Val::Unit)).collect();
                let ys: Vec<Val> = names.iter().map(|n| y.get(n).unwrap_or(Val::Unit)).collect();
                self.cmp_seq(&xs, &ys, total)
            }
            _ if str_of(a).is_none() => match (seq_items(a), seq_items(b)) {
                (Some(x), Some(y)) => self.cmp_seq(&x, &y, total),
                _ => Ok(vcmp(a, b, &*self.order_of(""))),
            },
            _ => Ok(vcmp(a, b, &*self.order_of(""))),
        }
    }

    /// 辞書式の比較。要素は、その比較の規則 (`total`) で手書きのメソッドを使いながら比べる。
    fn cmp_seq(&mut self, x: &[Val], y: &[Val], total: bool) -> Result<Option<i32>, Ctl> {
        for (p, q) in x.iter().zip(y) {
            match self.ord_of(p, q, total, false)? {
                Some(0) => {}
                r => return Ok(r),
            }
        }
        Ok(Some((x.len() > y.len()) as i32 - (x.len() < y.len()) as i32))
    }

    /// 派生の `cmp` / `partial_cmp` のメソッド形 (利用者の型の最上位は手書きを引かない)。
    pub fn v_cmp_derived(&mut self, a: &Val, b: &Val, total: bool) -> Result<Option<i32>, Ctl> {
        self.ord_of(a, b, total, true)
    }

    /// `min` / `max` (`Ord` の提供メソッド) の比較。`cmp` の規則に従い、手書きの `cmp` があればそれを使う。
    pub fn v_cmp_total(&mut self, a: &Val, b: &Val) -> Result<Option<i32>, Ctl> {
        self.ord_of(a, b, true, false)
    }

    // ------------------------------------------------------------ 場所・代入
    pub fn eval_place(&mut self, e: &V, sc: &Scope) -> Res {
        let f = nf(e);
        match nname(e) {
            "Path" => {
                let segs = strs(&f[0]);
                if let Some(owner) = sc_owner(sc, &segs[0]) {
                    if segs.len() == 1 {
                        let cur = owner.borrow().vars[&segs[0]].clone();
                        if matches!(cur, Val::Ref(_)) {
                            return Ok(cur);
                        }
                        return Ok(Val::Ref(Rc::new(VarPlace(owner, segs[0].clone()))));
                    }
                    let mut base = dr(&owner.borrow().vars[&segs[0]].clone());
                    for s in &segs[1..segs.len() - 1] {
                        base = match &base {
                            Val::Struct(x) => dr(&x.get(s).unwrap_or(Val::Unit)),
                            _ => return panic("bad place"),
                        };
                    }
                    if let Val::Struct(x) = base {
                        return Ok(Val::Ref(Rc::new(FieldPlace(x, segs[segs.len() - 1].clone()))));
                    }
                    return panic("bad place");
                }
                let v = self.eval(e, sc)?;
                Ok(Val::Ref(Rc::new(CellPlace(RefCell::new(v)))))
            }
            "Field" => {
                let b = dr(&self.eval(&f[0], sc)?);
                match b {
                    Val::Struct(x) => Ok(Val::Ref(Rc::new(FieldPlace(x, st(&f[1]))))),
                    _ => panic("bad place"),
                }
            }
            "Index" => {
                let b = as_raw(&dr(&self.eval(&f[0], sc)?));
                let i = match dr(&self.eval(&f[1], sc)?) {
                    Val::Int(x, _) => x,
                    _ => return panic("bad index"),
                };
                match b {
                    Val::Arr(l) | Val::Vec(l) => {
                        if i < 0 || i as usize >= l.borrow().len() {
                            return panic(format!("index out of bounds: the len is {} but the index is {}", l.borrow().len(), i));
                        }
                        Ok(Val::Ref(Rc::new(IdxPlace::new(l, i as usize))))
                    }
                    Val::View(l, a, bnd) => {
                        if i < 0 || i as usize >= bnd - a {
                            return panic(format!("index out of bounds: the len is {} but the index is {}", bnd - a, i));
                        }
                        Ok(Val::Ref(Rc::new(IdxPlace::new(l, a + i as usize))))
                    }
                    _ => panic("bad place"),
                }
            }
            "Unary" if st(&f[0]) == "*" => {
                let v = self.eval(&f[1], sc)?;
                if matches!(v, Val::Ref(_)) {
                    Ok(v)
                } else {
                    Ok(Val::Ref(Rc::new(CellPlace(RefCell::new(v)))))
                }
            }
            _ => {
                let v = self.eval(e, sc)?;
                Ok(Val::Ref(Rc::new(CellPlace(RefCell::new(v)))))
            }
        }
    }

    fn assign(&mut self, target: &V, v: Val, sc: &Scope) -> Result<(), Ctl> {
        let f = nf(target);
        if nname(target) == "Unary" && st(&f[0]) == "*" {
            let r = self.eval(&f[1], sc)?;
            if let Val::Ref(p) = r {
                p.set(v);
                return Ok(());
            }
            return panic("assignment through non-reference");
        }
        if nname(target) == "Path" {
            let segs = strs(&f[0]);
            if segs.len() == 1 {
                if let Some(owner) = sc_owner(sc, &segs[0]) {
                    let cur = owner.borrow().vars[&segs[0]].clone();
                    if let Val::Ref(p) = &cur {
                        if !matches!(v, Val::Ref(_)) {
                            p.set(v);
                            return Ok(());
                        }
                    }
                    owner.borrow_mut().vars.insert(segs[0].clone(), v);
                    return Ok(());
                }
            }
        }
        if let Val::Ref(p) = self.eval_place(target, sc)? {
            p.set(v);
        }
        Ok(())
    }

    // ------------------------------------------------------------ パターン
    pub fn bind(&mut self, pat: &V, v: Val, sc: &Scope) -> Result<bool, Ctl> {
        let f = nf(pat);
        let n = nname(pat);
        if n == "PWild" {
            return Ok(true);
        }
        if n == "PBind" {
            sc_declare(sc, &st(&f[0]), v);
            return Ok(true);
        }
        // 参照越しの分解 (`&mut T` / `&T` の値を `Some(v)` などで分解) の束縛は、欄への参照にする (Rust の既定の束縛モード。
        // ネイティブと同じく `*v = ..` が元の値へ書ける)。参照の連鎖の一番内側の場所を持ち回る。
        let mut host: Option<Rc<dyn Place>> = None;
        {
            let mut cur = v.clone();
            while let Val::Ref(p) = &cur {
                host = Some(p.clone());
                let nx = p.get();
                cur = nx;
            }
        }
        let v = dr(&v);
        match n {
            "PLit" => {
                let lv = self.lit(&f[0]);
                Ok(veq(&v, &lv))
            }
            "PRef" => self.bind(&f[0], v, sc),
            "PTuple" => {
                let Val::Tuple(t) = &v else { return Ok(false) };
                let ps = items(&f[0]);
                if t.len() != ps.len() {
                    return Ok(false);
                }
                for (i, (p, x)) in ps.iter().zip(t.iter()).enumerate() {
                    let x = match &host {
                        Some(h) => Val::Ref(Rc::new(TuplePlace(h.clone(), i))),
                        None => x.clone(),
                    };
                    if !self.bind(p, x, sc)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            "PPath" => {
                let segs = strs(&f[0]);
                let file = sc.borrow().file.to_string();
                match self.p.resolve(&segs, &file) {
                    Some(r) => match &r.target {
                        Target::Variant(en, vn) => Ok(matches!(&v, Val::Enum(e) if &e.en == en && &e.var == vn)),
                        Target::Const(i) => {
                            let cv = self.const_value(*i)?;
                            Ok(veq(&v, &cv))
                        }
                        _ => panic(format!("unresolved pattern {}", segs.join("."))),
                    },
                    None => panic(format!("unresolved pattern {}", segs.join("."))),
                }
            }
            "PVariant" => {
                let segs = strs(&f[0]);
                let file = sc.borrow().file.to_string();
                let Some(r) = self.p.resolve(&segs, &file) else {
                    return panic(format!("unresolved variant pattern {}", segs.join(".")));
                };
                let Target::Variant(en, vn) = &r.target else {
                    return panic(format!("unresolved variant pattern {}", segs.join(".")));
                };
                let Val::Enum(e) = &v else { return Ok(false) };
                if &e.en != en || &e.var != vn {
                    return Ok(false);
                }
                let ps = items(&f[1]);
                if e.pl.len() != ps.len() {
                    return Ok(false);
                }
                for (i, (p, x)) in ps.iter().zip(e.pl.iter()).enumerate() {
                    let x = match &host {
                        Some(h) => Val::Ref(Rc::new(PlPlace(h.clone(), i))),
                        None => x.clone(),
                    };
                    if !self.bind(p, x, sc)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            "PStruct" => {
                let Val::Struct(s) = &v else { return Ok(false) };
                for fp in items(&f[1]) {
                    let fnm = st(&items(fp)[0]);
                    let x = match &host {
                        Some(_) => Val::Ref(Rc::new(FieldPlace(s.clone(), fnm.clone()))),
                        None => s.get(&fnm).unwrap_or(Val::Unit),
                    };
                    if !self.bind(&items(fp)[1], x, sc)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            _ => panic(format!("bad pattern {}", n)),
        }
    }

    // ------------------------------------------------------------ パス・呼び出し
    fn e_path(&mut self, e: &V, sc: &Scope) -> Res {
        let segs = strs(&nf(e)[0]);
        if let Some(owner) = sc_owner(sc, &segs[0]) {
            let mut v = owner.borrow().vars[&segs[0]].clone();
            for s in &segs[1..] {
                v = match dr(&v) {
                    Val::Struct(x) => x.get(s).unwrap_or(Val::Unit),
                    _ => return panic(format!("no field {}", s)),
                };
            }
            return Ok(if segs.len() > 1 { self.cp(v) } else { v });
        }
        let file = sc.borrow().file.to_string();
        let Some(r) = self.p.resolve(&segs, &file) else {
            return panic(format!("unresolved name {} (line {})", segs.join("."), nlineno(e)));
        };
        self.path_value(&r, 0)
    }

    fn path_value(&mut self, r: &Resolved, drop_last: usize) -> Res {
        let mut v = match &r.target {
            Target::Const(i) => self.const_value(*i)?,
            Target::Variant(en, vn) => {
                let has_payload = match en.as_str() {
                    "Option" => vn == "Some",
                    "Result" => true,
                    _ => self
                        .p
                        .types
                        .get(en)
                        .map_or(false, |&i| self.p.defs[i].variants.iter().any(|x| &x.0 == vn && !x.1.is_empty())),
                };
                if has_payload {
                    Val::Ctor(Rc::new((en.clone(), vn.clone())))
                } else {
                    mk_enum(en, vn, vec![])
                }
            }
            Target::Fn(i) => Val::Func(*i),
            Target::Native(n) => Val::Native(Rc::from(n.as_str())),
            Target::Value(n) => self.num_const_val(n),
            _ => return panic("cannot use definition as value"),
        };
        let rest = &r.rest[..r.rest.len() - drop_last];
        for s in rest {
            v = match dr(&v) {
                Val::Struct(x) => x.get(s).unwrap_or(Val::Unit),
                _ => return panic(format!("no field {}", s)),
            };
        }
        Ok(v)
    }

    fn e_call(&mut self, e: &V, sc: &Scope) -> Res {
        let f = nf(e);
        let fnn = &f[0];
        if nname(fnn) == "Path" {
            let segs = strs(&nf(fnn)[0]);
            if let Some(owner) = sc_owner(sc, &segs[0]) {
                let first = owner.borrow().vars[&segs[0]].clone();
                if segs.len() == 1 {
                    let args = self.eval_args(&f[1], sc)?;
                    return self.call_value(&first, args);
                }
                let mut base = first;
                for s in &segs[1..segs.len() - 1] {
                    base = match dr(&base) {
                        Val::Struct(x) => x.get(s).unwrap_or(Val::Unit),
                        _ => return panic(format!("no field {}", s)),
                    };
                }
                let args = self.eval_args(&f[1], sc)?;
                let place_path = V::Node(
                    "Path",
                    vec![V::L(segs[..segs.len() - 1].iter().map(|x| V::S(x.clone())).collect()), V::Nil],
                    0,
                    0,
                );
                return self.invoke_method(base, &segs[segs.len() - 1], args, Some((&place_path, sc)));
            }
            let file = sc.borrow().file.to_string();
            let Some(r) = self.p.resolve(&segs, &file) else {
                return panic(format!("unresolved function {} (line {})", segs.join("."), nlineno(e)));
            };
            if !r.rest.is_empty() {
                let base = self.path_value(&r, 1)?;
                let args = self.eval_args(&f[1], sc)?;
                return self.invoke_method(base, &r.rest[r.rest.len() - 1], args, None);
            }
            // `core.ptr.from_ref(&x)` は `x` の場所を指す (`&x` を値として評価すると複製になり、以後の書き込みが見えない)。
            let args = match (&r.target, items(&f[1]).first()) {
                (Target::Sig(i), Some(a)) if self.def(*i).name == "core.ptr.from_ref" && nname(a) == "Unary" && st(&nf(a)[0]) == "&" => {
                    vec![self.eval_place(&nf(a)[1], sc)?]
                }
                _ => self.eval_args(&f[1], sc)?,
            };
            if let Target::Sig(i) = &r.target {
                if self.def(*i).name.starts_with("core.ptr.") {
                    // 型引数 (`core.ptr.cast::<T, U>`) は呼び出し式の Path の 2 つ目の成分にある。
                    self.ptr_targs = items(&nf(fnn)[1]).to_vec();
                }
            }
            if matches!(&r.target, Target::Native(n) if n == "core.mem.drop") {
                // `core.mem.drop(tx)`: 変数の値を落とす (チャネルの端なら閉じる)。
                if let (Some(a), Some(v)) = (items(&f[1]).first(), args.first()) {
                    self.move_out(a, sc, v);
                    // 検査器が「所有する値」と決めた呼び出し (`infer::annotate_owned_drops`) は、値を解放済みにする。
                    if matches!(f.get(3), Some(V::S(m)) if m == "own") {
                        mark_dead(v);
                    }
                }
            }
            return self.call_resolved(&r, args);
        }
        let callee = self.eval(fnn, sc)?;
        let args = self.eval_args(&f[1], sc)?;
        self.call_value(&callee, args)
    }

    fn eval_args(&mut self, args: &V, sc: &Scope) -> Result<Vec<Val>, Ctl> {
        let mut out = vec![];
        for a in items(args) {
            let v = self.eval(a, sc)?;
            check_alive(&v)?;
            // チャネルの端を値で渡すのは移動: 呼び出し側の変数を空にする (残すと、送信側が閉じない)。
            self.move_out(a, sc, &v);
            out.push(self.cp(v));
        }
        Ok(out)
    }

    /// `core.ptr.*` の native。生ポインタはアドレス (整数) として持つ。要素型は、値が持つタグ (`cast::<T, U>` と
    /// `null_mut::<T>` が付ける。`Val::Int` の型名に `*i32` などを載せる) か、作った側が `ptr_elem` に覚えたものから引く
    /// (ネイティブ生成は静的型から知るが、インタプリタの値は型を持たない)。
    fn ptr_native(&mut self, name: &str, args: Vec<Val>) -> Res {
        let addr = |i: usize| match args.get(i).map(dr) {
            Some(Val::Int(a, _)) => a,
            _ => 0,
        };
        match name {
            "core.ptr.null_mut" => {
                // 型引数があれば、null にも要素型を載せる。無ければネイティブ生成と同じ既定 (`*mut u8` 相当)。
                let elem = self.ptr_targs.first().and_then(elem_name).unwrap_or_else(|| "u8".into());
                Ok(self.ptr_value(0, &elem))
            }
            "core.ptr.is_null" => Ok(Val::Bool(matches!(args.first().map(dr), Some(Val::Int(0, _))))),
            // `as_const` / `as_mut` はアドレス (と要素型) のまま。
            "core.ptr.as_const" | "core.ptr.as_mut" => Ok(args.first().map(dr).unwrap_or(Val::Unit)),
            // `cast` の指す先の型は静的型からしか分からないので、明示の型引数 (`cast::<T, U>`) を要る。
            "core.ptr.cast" | "core.ptr.cast_const" => {
                let Some(to) = self.ptr_targs.get(1).and_then(elem_name) else {
                    return panic(format!("{}: the target type needs an explicit type argument (`{}::<T, U>`)", name, name));
                };
                Ok(self.ptr_value(addr(0), &to))
            }
            "core.ptr.add" | "core.ptr.add_const" => {
                let elem = self.ptr_elem_of(name, args.first())?;
                let q = addr(0).wrapping_add(addr(1).wrapping_mul(self.elem_bytes(name, &elem)?));
                Ok(self.ptr_value(q, &elem))
            }
            "core.ptr.read" => {
                let p = addr(0);
                if p == 0 {
                    return panic(format!("{}: null pointer dereference", name));
                }
                let elem = self.ptr_elem_of(name, args.first())?;
                self.pin_check(name, p, &elem)?;
                self.pin_before_read(p);
                self.read_elem(name, p, &elem)
            }
            "core.ptr.write" => {
                let p = addr(0);
                if p == 0 {
                    return panic(format!("{}: null pointer dereference", name));
                }
                let v = args.get(1).map(dr).unwrap_or(Val::Unit);
                // ネイティブ生成と同じく、ポインタの要素型が分かればそれで、分からなければ値の型で書く。
                let elem = match self.ptr_elem_of(name, args.first()) {
                    Ok(e) => e,
                    Err(_) => match scalar_ty(&v) {
                        Some(t) => t.to_string(),
                        None => return panic(unknown_elem(name)),
                    },
                };
                self.pin_check(name, p, &elem)?;
                self.write_elem(name, p, &elem, &v)?;
                self.pin_after_write(p);
                Ok(Val::Unit)
            }
            "core.ptr.from_mut" | "core.ptr.from_ref" => {
                let a = args.first().cloned().unwrap_or(Val::Unit);
                self.make_pin(name, &a)
            }
            _ => panic(format!("{} is not supported by the interpreter (raw pointer memory); use a native build (`ksn build`)", name)),
        }
    }

    /// アドレス `addr` を指す、要素型 `elem` のポインタの値。スカラーの要素型はタグとして値に載せる (同じアドレスを別の型で
    /// 指す `cast` の結果が、元のポインタの型を変えないように)。スカラーでなければアドレスに覚える。
    pub(crate) fn ptr_value(&self, addr: i128, elem: &str) -> Val {
        match ptr_tag(elem) {
            Some(t) => Val::Int(addr, Some(t)),
            None => {
                self.ptr_elem.borrow_mut().insert(addr, elem.to_string());
                int(addr, "usize")
            }
        }
    }

    /// ポインタの値の要素型: 値のタグ、なければ作った側が覚えたもの。分からなければ実行時エラー。
    fn ptr_elem_of(&self, name: &str, p: Option<&Val>) -> Result<String, Ctl> {
        let Some(p) = p else { return panic(unknown_elem(name)) };
        if let Val::Int(a, tag) = dr(p) {
            if let Some(e) = tag.and_then(ptr_tag_elem) {
                return Ok(e.to_string());
            }
            if let Some(e) = self.ptr_elem.borrow().get(&a).cloned() {
                return Ok(e);
            }
        }
        panic(unknown_elem(name))
    }

    fn call_resolved(&mut self, r: &Resolved, args: Vec<Val>) -> Res {
        match &r.target {
            Target::Fn(i) => self.call_def(*i, args),
            Target::Native(n) => self.call_native(&n.clone(), args),
            Target::Variant(en, vn) => self.mk_variant(en, vn, args),
            Target::Type(i) => panic(format!("cannot call type {}", self.p.defs[*i].name)),
            Target::Sig(i) => {
                let d = self.def(*i);
                let parts: Vec<&str> = d.name.split('.').collect();
                if parts.len() >= 4 && parts[0] == "core" && matches!(parts[1], "num" | "bool" | "char") && PRIMS.contains(&parts[2]) {
                    if crate::natives::known_num(parts[3]) {
                        let n = format!("{}.{}", parts[2], parts[3]);
                        return self.call_native(&n, args);
                    }
                }
                if d.is_method() {
                    let mut it = args.into_iter();
                    let recv = it.next().unwrap();
                    return self.invoke_method(recv, parts[parts.len() - 1], it.collect(), None);
                }
                if d.kw == "extern" {
                    return self.extern_call(d, args);
                }
                if d.name.starts_with("core.ptr.") {
                    return self.ptr_native(&d.name, args);
                }
                if d.name == "core.ffi.trampoline" {
                    // (踏み台の関数値, user) の組。user は C が受け渡すだけの不透明なアドレス (クロージャの値の場所)。
                    let c = args.first().map(dr).unwrap_or(Val::Unit);
                    let user = match &c {
                        Val::Closure(rc) => Rc::as_ptr(rc) as usize as i128,
                        _ => 0,
                    };
                    return Ok(Val::Tuple(Rc::new(vec![Val::Tuple(Rc::new(vec![c])), self.ptr_value(user, "u8")])));
                }
                if d.name == "alloc.raw.Buf.with_capacity" || d.name == "alloc.raw.Buf.new" {
                    return self.call_native(&d.name, args);
                }
                panic(format!("function {} has no implementation in this interpreter", d.name))
            }
            _ => panic("cannot call value"),
        }
    }

    // ------------------------------------------------------------ メソッド
    fn find_method(&self, v: &Val, name: &str) -> Option<usize> {
        let ty: &str = match v {
            Val::Struct(s) => &s.ty,
            Val::Enum(e) => match e.en.as_str() {
                "Option" => "core.option.Option",
                "Result" => "core.result.Result",
                n => n,
            },
            Val::StrBuf(_) => "alloc.string.String",
            // 生の列 (`Val::Vec`: `Buf` の格納と `VecDeque`) には `Vec` の本体を引かない。`Vec` の値は構造体 (`value::VEC_TY`)。
            _ => return None,
        };
        if let Some(&i) = self.p.funcs.get(&format!("{}.{}", ty, name)) {
            return Some(i);
        }
        // トレイト側に置いた impl (3.2) のメソッド。
        if self.p.trait_side.is_empty() {
            return None;
        }
        self.p.trait_side.get(&(ty.to_string(), name.to_string())).copied()
    }

    fn prim_kind(v: &Val) -> Option<&'static str> {
        match v {
            Val::Bool(_) => Some("bool"),
            Val::Int(_, t) => Some(t.unwrap_or("i32")),
            Val::Float(_) => Some("f64"),
            Val::F32(_) => Some("f32"),
            Val::Char(_) => Some("char"),
            Val::Str(_) => Some("str"),
            _ => None,
        }
    }

    pub fn invoke_method(&mut self, recv: Val, name: &str, args: Vec<Val>, place: Option<(&V, &Scope)>) -> Res {
        let v = dr(&recv);
        check_alive(&v)?;
        let mut d = self.find_method(&v, name);
        if d.is_none() && !matches!(v, Val::Struct(_) | Val::Enum(_)) {
            if let Some(kind) = Self::prim_kind(&v) {
                if let Some(cands) = self.p.method_idx.get(name) {
                    for &ci in cands {
                        let cd = &self.p.defs[ci];
                        if cd.body.is_none() {
                            continue;
                        }
                        let parts: Vec<&str> = cd.name.split('.').collect();
                        if parts.len() >= 3 && PRIMS.contains(&parts[parts.len() - 2]) && parts[parts.len() - 2] == kind {
                            d = Some(ci);
                            break;
                        }
                    }
                }
            }
        }
        // `Box<dyn Tr>` のレシーバは中身の値で呼ぶ (自動参照外し。5.1)。`&mut self` のメソッドは中身の場所を渡す。
        if d.is_none() {
            if let Val::Struct(s) = &v {
                if s.ty == "alloc.boxed.Box" && !matches!(name, "as_ref" | "as_mut" | "into_inner" | "into_dyn" | "clone" | "try_clone") {
                    let inner = Val::Ref(Rc::new(FieldPlace(s.clone(), "value".into())));
                    return self.invoke_method(inner, name, args, None);
                }
            }
        }
        if let Some(di) = d {
            let def = self.def(di);
            let selfty = &def.params[0].1;
            let by_mut = crate::check::tag(selfty) == "ref" && matches!(crate::check::tel(selfty, 1), V::B(true));
            let first = if by_mut {
                if matches!(recv, Val::Ref(_)) {
                    recv.clone()
                } else if let Some((pe, sc)) = place {
                    self.eval_place(pe, sc)?
                } else {
                    Val::Ref(Rc::new(CellPlace(RefCell::new(v.clone()))))
                }
            } else {
                v.clone()
            };
            let mut all = vec![first];
            all.extend(args);
            return self.call_def(di, all);
        }
        // `made.map_err(f)` のように値を消費する `Option` / `Result` のメソッド: 受け手の変数はチャネルの端を手放す。
        if let (Val::Enum(e), Some((pe, psc))) = (&v, place) {
            if (e.en == "Option" || e.en == "Result")
                && !matches!(name, "is_some" | "is_none" | "is_ok" | "is_err" | "as_ref" | "as_mut" | "iter" | "clone" | "eq")
            {
                self.move_out(pe, psc, &v);
            }
        }
        if self.dbg_guard {
            self.dbg_guard_method(name)?;
        }
        self.native_method(&recv, &v, name, args, place)
    }

    // ------------------------------------------------------------ イテレータ
    pub fn to_iter(&mut self, v: &Val) -> Result<Rc<RefCell<IterV>>, Ctl> {
        let v = as_raw(&dr(v));
        match &v {
            Val::Iter(it) => Ok(it.clone()),
            Val::Arr(l) | Val::Vec(l) => Ok(Rc::new(RefCell::new(IterV::List { items: l.clone(), i: 0, end: None }))),
            Val::View(l, a, b) => Ok(Rc::new(RefCell::new(IterV::List { items: l.clone(), i: *a, end: Some(*b) }))),
            Val::Struct(s) => match self.p.funcs.get(&format!("{}.next", s.ty)) {
                Some(&di) => Ok(Rc::new(RefCell::new(IterV::User { place: Rc::new(CellPlace(RefCell::new(v.clone()))), def: di }))),
                None => panic(format!("{} is not an iterator", s.ty)),
            },
            _ => panic("not an iterator"),
        }
    }

    pub fn iter_next(&mut self, it: &Rc<RefCell<IterV>>) -> Res {
        enum Step {
            Done(Val),
            Map(Rc<RefCell<IterV>>, Val),
            Filter(Rc<RefCell<IterV>>, Val),
            Enum(Rc<RefCell<IterV>>, i128),
            Take(Rc<RefCell<IterV>>),
            User(Rc<dyn Place>, usize),
        }
        let step = {
            let mut b = it.borrow_mut();
            match &mut *b {
                IterV::List { items, i, end } => {
                    let l = items.borrow();
                    let limit = end.unwrap_or(l.len()).min(l.len());
                    if *i >= limit {
                        Step::Done(none())
                    } else {
                        let x = l[*i].clone();
                        *i += 1;
                        Step::Done(some(x))
                    }
                }
                IterV::Pre { items, i } => {
                    if *i >= items.len() {
                        Step::Done(none())
                    } else {
                        let x = items[*i].clone();
                        *i += 1;
                        Step::Done(some(x))
                    }
                }
                IterV::Range { cur, end } => {
                    let (c, e) = (cur.clone(), end.clone());
                    match (&c, &e) {
                        (Val::Int(x, t), Val::Int(y, _)) => {
                            if x >= y {
                                Step::Done(none())
                            } else {
                                *cur = Val::Int(x + 1, *t);
                                Step::Done(some(c))
                            }
                        }
                        _ => Step::Done(none()),
                    }
                }
                IterV::Map { src, f } => Step::Map(src.clone(), f.clone()),
                IterV::Filter { src, f } => Step::Filter(src.clone(), f.clone()),
                IterV::Enumerate { src, i } => {
                    let idx = *i;
                    *i += 1;
                    Step::Enum(src.clone(), idx)
                }
                IterV::Take { src, n } => {
                    if *n <= 0 {
                        Step::Done(none())
                    } else {
                        *n -= 1;
                        Step::Take(src.clone())
                    }
                }
                IterV::User { place, def } => Step::User(place.clone(), *def),
            }
        };
        match step {
            Step::Done(v) => Ok(v),
            Step::Map(src, f) => {
                let o = self.iter_next(&src)?;
                match &o {
                    Val::Enum(e) if e.var == "Some" => {
                        let r = self.call_value(&f, vec![e.pl[0].clone()])?;
                        Ok(some(r))
                    }
                    _ => Ok(o),
                }
            }
            Step::Filter(src, f) => loop {
                let o = self.iter_next(&src)?;
                match &o {
                    Val::Enum(e) if e.var == "Some" => {
                        if matches!(dr(&self.call_value(&f, vec![e.pl[0].clone()])?), Val::Bool(true)) {
                            return Ok(o);
                        }
                    }
                    _ => return Ok(o),
                }
            },
            Step::Enum(src, idx) => {
                let o = self.iter_next(&src)?;
                match &o {
                    Val::Enum(e) if e.var == "Some" => {
                        Ok(some(Val::Tuple(Rc::new(vec![int(idx, "usize"), e.pl[0].clone()]))))
                    }
                    _ => Ok(o),
                }
            }
            Step::Take(src) => self.iter_next(&src),
            Step::User(place, di) => self.call_def(di, vec![Val::Ref(place)]),
        }
    }

    // ------------------------------------------------------------ 表示
    pub fn debug_str(&mut self, v: &Val) -> Result<String, Ctl> {
        let v = as_raw(&dr(v));
        Ok(match &v {
            Val::Bool(b) => (if *b { "true" } else { "false" }).into(),
            Val::Char(c) => format!("{:?}", c),
            Val::Str(s) => py_quote(s),
            Val::StrBuf(b) => py_quote(&b.text()),
            Val::Float(f) => py_float(*f),
            Val::F32(f) => py_float32(*f),
            Val::Int(x, t) => {
                if *t == Some("u128") { (*x as u128).to_string() } else { x.to_string() }
            }
            Val::Unit => "()".into(),
            Val::Tuple(t) => {
                let parts: Result<Vec<String>, Ctl> = t.iter().map(|x| self.debug_str(x)).collect();
                let parts = parts?;
                if parts.len() == 1 { format!("({},)", parts[0]) } else { format!("({})", parts.join(", ")) }
            }
            Val::Arr(_) | Val::Vec(_) | Val::View(..) => {
                let items = seq_items(&v).unwrap_or_default();
                let parts: Result<Vec<String>, Ctl> = items.iter().map(|x| self.debug_str(x)).collect();
                format!("[{}]", parts?.join(", "))
            }
            Val::Struct(s) => {
                if let Some(&di) = self.p.funcs.get(&format!("{}.debug_fmt", s.ty)) {
                    let fm = Rc::new(RefCell::new(String::new()));
                    self.call_def(di, vec![v.clone(), Val::Fmt(fm.clone())])?;
                    let r = fm.borrow().clone();
                    return Ok(r);
                }
                let nm = s.ty.rsplit('.').next().unwrap().to_string();
                let fields: Vec<(String, Val)> = s.f.borrow().clone();
                let mut parts = vec![];
                for (k, x) in fields {
                    parts.push(format!("{}: {}", k, self.debug_str(&x)?));
                }
                format!("{} {{ {} }}", nm, parts.join(", "))
            }
            Val::Enum(e) => {
                if let Some(&di) = self.p.funcs.get(&format!("{}.debug_fmt", e.en)) {
                    let fm = Rc::new(RefCell::new(String::new()));
                    self.call_def(di, vec![v.clone(), Val::Fmt(fm.clone())])?;
                    let r = fm.borrow().clone();
                    return Ok(r);
                }
                if e.pl.is_empty() {
                    e.var.clone()
                } else {
                    let parts: Result<Vec<String>, Ctl> = e.pl.iter().map(|x| self.debug_str(x)).collect();
                    format!("{}({})", e.var, parts?.join(", "))
                }
            }
            _ => "<value>".into(),
        })
    }

    pub fn display_str(&mut self, v: &Val) -> Result<String, Ctl> {
        let v = dr(v);
        if let Some(s) = str_of(&v) {
            return Ok(s);
        }
        // `Display` の char は文字そのもの (`Debug` の `'a'` ではない)。
        if let Val::Char(c) = &v {
            return Ok(c.to_string());
        }
        if let Val::Struct(s) = &v {
            if let Some(&di) = self.p.funcs.get(&format!("{}.display_fmt", s.ty)) {
                let fm = Rc::new(RefCell::new(String::new()));
                self.call_def(di, vec![v.clone(), Val::Fmt(fm.clone())])?;
                let r = fm.borrow().clone();
                return Ok(r);
            }
        }
        self.debug_str(&v)
    }

    pub fn write_out(&mut self, s: &str) {
        let _ = self.out.write_all(s.as_bytes());
    }

    // ------------------------------------------------------------ 実行
    /// 木走査の呼び出し 1 回あたりの Rust のスタックの見積もり (主スレッド 4 GiB ÷ 既定の深さ 100000 ≒ 40 KiB)。
    /// 入口の `depth M` が既定より大きいときの別スレッドのスタックの大きさ (`M × この値`) に使う。
    const FRAME_BYTES: usize = 40 << 10;

    pub fn run_entry(&mut self, di: usize) -> i32 {
        if let Some(m) = self.def(di).depth {
            self.max_depth = (m as usize).clamp(1, crate::kir_exec::MAX_DEPTH_LIMIT);
        }
        if self.max_depth <= crate::kir_exec::MAX_DEPTH {
            return self.run_entry_here(di);
        }
        // 既定より深い上限: 主スレッドのスタック (4 GiB) では足りないので、`M × 40 KiB` のスタックの別スレッドで走らせる
        // (呼び元はここで join して待つので、`Interp` に触るのは常に 1 つのスレッドだけ)。
        let size = self.max_depth.saturating_mul(Self::FRAME_BYTES);
        let ptr = crate::task::SendPtr(self as *mut Interp);
        let spawned = std::thread::Builder::new().stack_size(size).spawn(move || {
            set_stack_floor(size);
            let p = ptr;
            // SAFETY: 呼び元のスレッドは join を待っていて、`Interp` に触らない。
            let it: &mut Interp = unsafe { &mut *p.get() };
            it.run_entry_here(di)
        });
        match spawned {
            Ok(h) => h.join().unwrap_or(2),
            Err(e) => {
                eprintln!("ksn: cannot allocate a stack for `depth {}` ({})", self.max_depth, e);
                2
            }
        }
    }

    fn run_entry_here(&mut self, di: usize) -> i32 {
        let r = self.call_def(di, vec![]);
        let code = match r {
            Err(Ctl::Panic(m)) => {
                let _ = self.out.flush();
                eprintln!("panic: {}", m);
                return 101;
            }
            Err(Ctl::Exit(c)) => c,
            Err(_) => 0,
            Ok(v) => {
                let v = dr(&v);
                if let Val::Enum(e) = &v {
                    if e.en == "Result" && e.var == "Err" {
                        let _ = self.out.flush();
                        let s = self.debug_str(&e.pl[0]).unwrap_or_default();
                        eprintln!("Error: {}", s);
                        return 1;
                    }
                }
                0
            }
        };
        let _ = self.out.flush();
        code
    }
}

pub fn py_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n"))
}

pub fn py_float32(f: f32) -> String {
    if f.is_nan() {
        return "nan".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf".into() } else { "-inf".into() };
    }
    if f == f.trunc() && f.abs() < 1e16 {
        return format!("{:.1}", f);
    }
    format!("{}", f)
}

pub fn py_float(f: f64) -> String {
    if f.is_nan() {
        return "nan".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf".into() } else { "-inf".into() };
    }
    if f == f.trunc() && f.abs() < 1e16 {
        return format!("{:.1}", f);
    }
    format!("{}", f)
}

#[allow(dead_code)]
fn unused() {
    let _ = _children;
}

/// 整数の `+ - * / %`。オーバーフローとゼロ除算は panic (4.1)。u128 はビットパターンで保持している。
fn int_arith(op: &str, x: i128, y: i128, ty: Ity) -> Res {
    let name = match op {
        "+" => "add",
        "-" => "subtract",
        "*" => "multiply",
        "/" => "divide",
        _ => "calculate the remainder",
    };
    if (op == "/" || op == "%") && y == 0 {
        return panic(format!("attempt to {} by zero", if op == "/" { "divide" } else { "calculate the remainder with a divisor of" }));
    }
    let overflow = || panic(format!("attempt to {} with overflow", name));
    if ty == Some("u128") {
        let (a, b) = (x as u128, y as u128);
        let r = match op {
            "+" => a.checked_add(b),
            "-" => a.checked_sub(b),
            "*" => a.checked_mul(b),
            "/" => a.checked_div(b),
            _ => a.checked_rem(b),
        };
        return match r {
            Some(v) => Ok(Val::Int(v as i128, ty)),
            None => overflow(),
        };
    }
    // 符号付きの `MIN % -1` は割り算が溢れるので余りも溢れ (案 A。Rust と同じ)。i128 の `checked_rem` は溢れを見ないので型で確かめる。
    if op == "%" && y == -1 && ty.is_some_and(|t| signed(t) && x == tmin(t)) {
        return overflow();
    }
    let r = match op {
        "+" => x.checked_add(y),
        "-" => x.checked_sub(y),
        "*" => x.checked_mul(y),
        "/" => x.checked_div(y),
        _ => x.checked_rem(y),
    };
    match (r, ty) {
        (Some(v), Some(t)) if v >= tmin(t) && v <= tmax(t) => Ok(Val::Int(v, ty)),
        (Some(v), None) => Ok(Val::Int(v, ty)),
        _ => overflow(),
    }
}

/// `alloc/vec.ksnl` が Kasane で書いた `Vec` の基本メソッド (木走査・KIR とも Kasane 版を使う。`Vec` の値は構造体 `{ buf }`)。
pub const KASANE_VEC_BASE: &[&str] = &["as_mut_slice", "as_slice", "capacity", "clear", "insert", "is_empty", "len", "pop", "push", "remove", "reserve", "truncate"];

/// `alloc/string.ksnl` が Kasane で書いた `String` の基本メソッド (`Buf<u8>` の上。木走査は組み込みを使う)。
pub const KASANE_STRING_BASE: &[&str] = &["clear", "is_empty", "len", "push", "push_bytes", "push_i64", "push_str", "push_u64", "reserve_bytes"];
