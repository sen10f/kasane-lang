//! 集成体 (構造体・タプル・列挙型) の下ろし (設計_KIR.md 2.3、段階 1 の続き)。
//!
//! `kir_lower.rs` の子モジュールとして、その非公開の部品 (`Lw` の操作) を使う。
//!
//! - 型の表 (`AggTab`) は `lower_user` が 1 つ作り、全関数で共有する。添字は `Module::aggs` と同じ。
//! - 配置は clif.rs の `layout` / `enum_layout` と同じ規則で求める: 大きさと整列は clif と同じ、
//!   構造体・タプルのフィールドは宣言の順に、列挙型は先頭 4 バイトのタグ (i32、バリアントの添字) の後ろに、
//!   バリアントのペイロードを重ねて置く (整列は 4 以上)。
//! - 値は不変。フィールドの書き換えは新しい値 (`setfield`) を作り、変数へ代入し直す。
//! - `match` は変種の試験 (タグの比較) を順に並べた連鎖に下ろし、束縛は試験の後の `field` で作る。

use std::collections::{HashMap, HashSet};

use crate::check::{items, nf, nlineno as nline, nname, st, strs};
use crate::kir::{align_of, size_of, AggDef, AggKind, BoolOp, Field, Op, Pay, Pointee, Term, Ty, VId, Variant};
use crate::parser::V;
use crate::program::{Program, Target as Tgt};
use crate::types::{from_v, Ty as Src};

use super::{conv, parse_int, show, Lw, St, LR};

/// 代入先の `x.f.g` (`Field` の鎖) の名前の並び。根が名前でなければ `None`。
pub(super) fn field_segs(v: &V) -> Option<Vec<String>> {
    match nname(v) {
        "Path" => Some(strs(&nf(v)[0])),
        "Field" => {
            let f = nf(v);
            let mut s = field_segs(&f[0])?;
            s.push(st(&f[1]));
            Some(s)
        }
        _ => None,
    }
}

/// Kasane の定義 (`spec/stdlib` の `type` / `enum`) を持ち、利用者の型と同じ集成体として型表に入れる標準ライブラリの型 (設計_KIR.md 12.1)。
/// 利用者のエラー型 (`shortener.error.Error` / `http.Error` など) がペイロードや変種に持つ標準型もここに入れる (段階 2)。
pub const STD_TYPES: [&str; 45] = ["core.hash.Fnv64", "std.http.Decoder", "std.http.Error", "std.http.Framing", "std.http.Head", "std.http.Header", "std.http.Request", "std.tls.TlsAcceptor", "std.tls.Error", "std.tls.TlsStream", "std.process.Command", "std.process.Child", "std.process.ExitStatus", "std.sync.RecvError", "std.rand.Rng", "std.io.Error", "std.io.ErrorKind", "std.fs.File", "alloc.alloc.AllocError", "std.net.Error", "core.str.Utf8Error", "core.num.ParseIntError", "core.num.TryFromIntError", "std.alloc.System", "std.thread.SpawnError", "alloc.string.PushBytesError", "alloc.fmt.Spec", "std.fs.DirEntry", "std.fs.FileKind", "std.fs.Metadata", "core.cmp.Ordering", "std.time.SystemTime", "std.datetime.DateTime", "std.datetime.Error", "std.log.Level", "std.net.Shutdown", "std.net.TcpListener", "std.net.TcpStream", "std.net.UdpSocket", "core.slice.IndexError", "std.json.Doc", "std.json.Error", "std.json.Kind", "std.json.Node", "std.json.Value"];

/// Kasane で書いた標準ライブラリの総称型 (`spec/stdlib/alloc/collections.ksnl`)。利用者の型と同じ集成体として下ろす。
pub fn is_kasane_std_generic(name: &str) -> bool {
    matches!(name, "alloc.collections.HashMap" | "alloc.collections.BtreeMap" | "alloc.collections.HashCell" | "alloc.collections.MapEntry" | "alloc.collections.HashMapIter")
}

/// ソケットの型 (`std.net.TcpListener` / `TcpStream`)。型表には `lower_defs` が常に入れる。`&` の形の型も、同じ集成体として下ろす。
pub fn is_net_socket(name: &str) -> bool {
    name == "std.net.TcpListener" || name == "std.net.TcpStream" || name == "std.net.UdpSocket"
}

/// `Vec` / `String` のヘッダの型の名前 (`vec`、要素が所有物なら `vec<#N>`)。利用者の型は FQN で `.` を含むので衝突しない。
pub fn is_vec_name(n: &str) -> bool {
    n == "vec" || n.starts_with("vec<#")
}

/// 集成体の型表。`lower_user` が 1 つ作り、全関数で共有する。
#[derive(Default)]
pub struct AggTab {
    pub defs: Vec<AggDef>,
    /// 型の名前 (構造体・列挙型は FQN、タプルは `(i32,bool)`) → 添字。
    by_name: HashMap<String, u32>,
    /// 組み立て中の型 (再帰的な型を見つけるため)。
    busy: HashSet<String>,
    /// 利用者のパッケージの定義 (`Program::defs` の添字)。標準の型は clif が特別扱いする配置を持つので、ここに入るものだけ扱う。
    user: HashSet<usize>,
    /// `Box<T>` の集成体 (`box<..>`) の添字 → 中身 `T` の型 (解放のとき中身を先に解放するため。設計_KIR.md 21 章)。
    box_elems: HashMap<u32, Ty>,
    /// `Box<dyn Tr>` の変換と呼び出しの登録 (全関数で共有。最後に呼び出しの補助関数を作る)。
    pub dynr: DynReg,
    /// チャネルの端 (`std.sync.Sender<T>` / `Receiver<T>`) の型の添字 → 要素の型 `T` (設計_KIR.md 22 章)。
    pub chan_elem: HashMap<u32, Ty>,
    /// `std.thread.JoinHandle<T, A>` の型の添字 → `T`。
    pub handle_elem: HashMap<u32, Ty>,
    /// 型引数付きの集成体の添字 → (定義の FQN, 型引数)。メソッドの型引数の推論 (受け手の型) に使う。
    pub generic_args: HashMap<u32, (String, Vec<Src>)>,
    /// `derive [Drop]` の再帰する型 (添字)。`Box<Self>` を経由する再帰を許し、解放は補助関数 `drop.<FQN>` (反復) が担う。
    pub rec_drop: HashSet<u32>,
    /// 解放の補助関数が要る `rec_drop` の型 (呼び出しを下ろしたもの)。
    pub drop_need: std::collections::BTreeSet<u32>,
}

/// `Box<dyn Tr>` の vtable の登録 (設計_KIR.md 21 章)。vtable の「番地」は、登録順の番号 (`usize`)。
#[derive(Default, Clone)]
pub struct DynReg {
    /// 番号 = vtable の番号。`into_dyn` が下ろされるたびに (トレイト, 具体型) の組を 1 度だけ登録する。
    pub vts: Vec<VtEntry>,
    /// `dyn` のメソッド呼び出しの (トレイトの FQN, メソッド名) → (引数の型 (データと vtable を除く), 戻り値の型)。
    pub calls: std::collections::BTreeMap<(String, String), (Vec<Ty>, Ty)>,
    /// `dyn.drop` を使ったか。
    pub drop_used: bool,
}

#[derive(Clone)]
pub struct VtEntry {
    /// トレイトの FQN。
    pub tr: String,
    /// 具体型の FQN (メソッドの定義 `<具体型>.<メソッド>` を引く)。
    pub concrete_name: String,
    /// 具体型の KIR の型。
    pub concrete: Ty,
    /// 具体型の名前を解決するファイル。
    pub file: String,
}

/// `dyn Tr` の `Tr` (書かれた名前) を、トレイト定義の FQN にする。解決できなければ書かれた名前のまま。
pub fn trait_fqn(p: &Program, file: &str, written: &str) -> String {
    let segs: Vec<String> = written.split('.').map(String::from).collect();
    match p.resolve(&segs, file).map(|r| r.target) {
        Some(Tgt::Type(di)) | Some(Tgt::Sig(di)) if p.defs[di].kw == "trait" => p.defs[di].name.clone(),
        _ => written.to_string(),
    }
}

fn align_up(n: u32, a: u32) -> u32 {
    (n + a - 1) / a * a
}

/// 構造体・タプル: フィールドを宣言の順に並べる。大きさは `max(末尾, 1)` を整列に切り上げる (clif と同じ)。
fn layout_fields(defs: &[AggDef], fs: &[(String, Ty)]) -> (u32, u32, Vec<Field>) {
    let mut off = 0u32;
    let mut align = 1u32;
    let mut out = vec![];
    for (name, t) in fs {
        let (sz, al) = (size_of(defs, *t), align_of(defs, *t));
        off = align_up(off, al);
        out.push(Field { name: name.clone(), ty: *t, off });
        off += sz;
        align = align.max(al);
    }
    (align_up(off.max(1), align), align, out)
}

/// 列挙型: 整列の最大 (4 以上) を求めてから、タグの後ろ (`start`) にバリアントごとのペイロードを重ねる。
fn layout_enum(defs: &[AggDef], vs: &[(String, Vec<Ty>)]) -> (u32, u32, Vec<Variant>) {
    let mut max_align = 4u32;
    let mut sizes = vec![];
    let mut rel: Vec<Vec<(Ty, u32)>> = vec![];
    for (_, tys) in vs {
        let mut cur = 0u32;
        let mut fs = vec![];
        for t in tys {
            let (sz, al) = (size_of(defs, *t), align_of(defs, *t));
            cur = align_up(cur, al);
            fs.push((*t, cur));
            cur += sz;
            max_align = max_align.max(al);
        }
        sizes.push(cur);
        rel.push(fs);
    }
    let start = align_up(4, max_align);
    let max_payload = sizes.iter().copied().max().unwrap_or(0);
    let variants = vs
        .iter()
        .zip(rel)
        .map(|((vn, _), fs)| Variant {
            name: vn.clone(),
            fields: fs.into_iter().map(|(ty, off)| Field { name: String::new(), ty, off: start + off }).collect(),
        })
        .collect();
    (align_up(start + max_payload, max_align), max_align, variants)
}

impl AggTab {
    /// `user` は利用者のパッケージの定義の添字 (`lower_user` の引数と同じ)。
    pub fn new(user: &[usize]) -> AggTab {
        AggTab { defs: vec![], by_name: HashMap::new(), busy: HashSet::new(), user: user.iter().copied().collect(), box_elems: HashMap::new(), dynr: DynReg::default(), chan_elem: HashMap::new(), handle_elem: HashMap::new(), generic_args: HashMap::new(), rec_drop: HashSet::new(), drop_need: Default::default() }
    }

    /// 推論済みの型 `t` を段階 1 の型にする。`file` は名前を解決するファイル。対象外なら理由付きで `Err`。
    pub fn lower_ty(&mut self, p: &Program, file: &str, t: &Src) -> Result<Ty, String> {
        if let Some(x) = conv(t) {
            return Ok(x);
        }
        match t {
            // `Option<&T>` (T は所有物を持つ型): ペイロードは要素の番地を 1 つ持つ集成体 `bref` (設計_KIR.md 17.18)。
            Src::Named(name, args) if name == "Option" && args.len() == 1 && matches!(&args[0], Src::Ref(m, i) if self.bref_ref(p, file, *m, i)) => {
                let b = self.bref();
                Ok(Ty::Opt(Pay::of(b).expect("集成体はペイロードになる")))
            }
            // 組み込みの Option / Result。ペイロードは集成体・unit・入れ子も持てる (設計_KIR.md 11.5)。
            Src::Named(name, args) if name == "Option" && args.len() == 1 => Ok(Ty::Opt(self.pay_of(p, file, &args[0])?)),
            Src::Named(name, args) if name == "Result" && args.len() == 2 => {
                let t0 = self.pay_of(p, file, &args[0])?;
                let e0 = self.pay_of(p, file, &args[1])?;
                Ok(Ty::Res(t0, e0))
            }
            // 値渡しの `Vec` / `String` は 24 バイトのヘッダ `vec` の値 (所有の移動。設計_KIR.md 13 章)。
            // 参照 (`&Vec` など) は下の腕に入れず、借用のヘッダのアドレス (`ptr`) として `sig_ty` が受ける。
            Src::Named(name, args) if name == "alloc.vec.Vec" || name == "alloc.raw.Buf" => {
                let elem = args.first().and_then(|a| self.lower_ty(p, file, a).ok());
                Ok(self.vec_of(elem))
            }
            Src::Named(name, _) if name == "alloc.string.String" => Ok(self.vec_hdr()),
            // `core.str.Chars` (局所に持つ `s.chars()`) は、まだ読んでいない残りの `&str` (`fat`)。`next` が先頭の文字を読んで進める。
            Src::Named(name, _) if name == "core.str.Chars" => Ok(self.fat()),
            // `core.str.Split` (局所に持つ `s.split(sep)`): 残りと区切りと終わりの印 (`split_it`)。`next` が進める。
            Src::Named(name, _) if name == "core.str.Split" => Ok(self.split_it()),
            Src::Named(name, _) if name == "alloc.alloc.AllocError" => Ok(self.alloc_err()),
            Src::Named(name, _) if name == "alloc.vec.InsertError" => Ok(self.insert_err()),
            // `std.thread.JoinHandle<T, A>` = `{A, usize, *mut T}`。`T` はポインタの向こうにあるので配置には現れない。
            Src::Named(name, args) if name == "std.thread.JoinHandle" && args.len() == 2 => {
                let elem_ty = self.lower_ty(p, file, &args[0])?;
                let alloc_ty = self.lower_ty(p, file, &args[1])?;
                Ok(self.join_handle(elem_ty, alloc_ty))
            }
            // `std.task.Task<T>` = `{usize}` (実行器の表のタスクの番号)。`T` は名前に埋め込み、`join_task` で読む型として持つ (設計_KIR.md 26 章)。
            Src::Named(name, args) if name == "std.task.Task" && args.len() == 1 => {
                let elem_ty = self.lower_ty(p, file, &args[0])?;
                Ok(self.task_handle(elem_ty))
            }
            // `Box<T, System>` は 8 バイトの所有ポインタ (`box<..>`)、`Box<dyn Tr, System>` はデータと vtable の組 (`dynbox`)。設計_KIR.md 21 章。
            Src::Named(name, args) if name == "alloc.boxed.Box" && args.len() == 2 => {
                if !matches!(&args[1], Src::Named(a, _) if a == "std.alloc.System") {
                    return Err("Box のアロケータは std.alloc.System だけ対象".to_string());
                }
                match &args[0] {
                    Src::Dyn(tr, _) => {
                        let fq = trait_fqn(p, file, tr);
                        Ok(self.dynbox(&fq))
                    }
                    inner => {
                        let e = self.lower_ty(p, file, inner)?;
                        if Pointee::of(e).is_none() {
                            return Err(format!("Box の中身の型 {} は対象外", crate::kir::fmt_ty(e)));
                        }
                        Ok(self.box_of(e))
                    }
                }
            }
            // チャネルの端 `Sender<T>` / `Receiver<T>` (`{ state: ptr }`) と `SendError<T>` (`{ value: T }`)。T は 8 バイト以下のスカラー。
            Src::Named(name, args) if (name == "std.sync.Sender" || name == "std.sync.Receiver" || name == "std.sync.SendError") && args.len() == 1 => {
                let elem = self.lower_ty(p, file, &args[0])?;
                if !(matches!(elem, Ty::Agg(_)) || (elem.is_scalar() && elem != Ty::Unit && !matches!(elem, Ty::I128 | Ty::U128))) {
                    return Err(format!("{}<{}> は段階 1 の対象外 (要素は 64 ビット以下のスカラーか集成体だけ)", name, show(&args[0])));
                }
                Ok(if name == "std.sync.SendError" { self.send_err(elem) } else { self.chan_end(name == "std.sync.Sender", elem) })
            }
            Src::Named(name, args) => {
                let segs: Vec<String> = name.split('.').map(String::from).collect();
                match p.resolve(&segs, file).map(|r| r.target) {
                    // ソケットの型 (`std.net.TcpListener` / `TcpStream`) は `&` の形でも現れるので、型表に入れる (設計_KIR.md 12 章)。
                    Some(Tgt::Type(di)) if !args.is_empty() && self.user.contains(&di) => self.intern_generic(p, di, args),
                    // 段 2 の `HashMap` などは、利用者の型と同じ集成体として下ろす (`spec/stdlib/alloc/collections.ksnl`)。
                    Some(Tgt::Sig(di) | Tgt::Type(di)) if !args.is_empty() && is_kasane_std_generic(name) => self.intern_generic(p, di, args),
                    _ if !args.is_empty() => Err(format!("型 {} は型引数付きで段階 1 の対象外", name)),
                    Some(Tgt::Sig(di)) if p.defs[di].kw == "type" && STD_TYPES.contains(&p.defs[di].name.as_str()) => self.intern_def(p, di),
                    Some(Tgt::Type(di)) if self.user.contains(&di) || STD_TYPES.contains(&p.defs[di].name.as_str()) || is_net_socket(&p.defs[di].name) => self.intern_def(p, di),
                    // 上の一覧にない標準ライブラリの型は、KIR の型表に入れない (値は組み込みの実装が作るので、下ろせない)。
                    _ => Err(format!("{} は未対応 (標準ライブラリの型は KIR に組み込みがない)", name)),
                }
            }
            Src::Tuple(xs) => {
                let mut elems = vec![];
                for x in xs {
                    elems.push(self.field_ty(p, file, x)?);
                }
                self.intern_tuple(elems)
            }
            // `&dyn Tr` / `&mut dyn Tr`: データの番地と vtable の番号の組 (`dynref<Tr>`。所有しない借用で、`Box<dyn>` と同じ呼び出しの補助関数を使う)。
            Src::Ref(_, inner) if matches!(&**inner, Src::Dyn(..)) => {
                let Src::Dyn(tr, _) = &**inner else { unreachable!() };
                let fq = trait_fqn(p, file, tr);
                Ok(self.dynbox_named("dynref", &fq))
            }
            // 集成体への参照 `&S` / `&mut S` (スカラーへの参照は上の `conv` が済ませている)。
            Src::Ref(_, inner) if matches!(&**inner, Src::Named(n, _) if n == "alloc.vec.Vec" || n == "alloc.raw.Buf" || n == "alloc.string.String") => {
                Err("Vec / String への参照はヘッダのアドレスで渡す".to_string())
            }
            Src::Ref(m, inner) if matches!(&**inner, Src::Named(..) | Src::Tuple(_)) => match self.lower_ty(p, file, inner)? {
                Ty::Agg(i) => Ok(Ty::Ref(*m, Pointee::Agg(i))),
                // `&mut Option<Box<Node>>` (連結リスト・木の付け替え): `Option` の値をメモリに置いた先への参照。
                t @ Ty::Opt(_) => Ok(Ty::Ref(*m, Pointee::of(t).expect("Option は参照の指す先になれる"))),
                _ => Err(format!("参照の指す先 {} は段階 1 の対象外", show(inner))),
            },
            // `&[T; N]` / `&mut [T; N]`: 配列の値は `{ptr, len}` の `fat` なので、参照は `ref<fat>` / `mutref<fat>`。
            // 要素の先頭ポインタは `fat` の 0 番フィールド (呼び出し側の `coerce_args` で ptr に調整する)。
            Src::Ref(m, inner) if matches!(&**inner, Src::Array(_, _)) => {
                let Ty::Agg(fat_ai) = self.fat() else { unreachable!() };
                Ok(Ty::Ref(*m, Pointee::Agg(fat_ai)))
            }
            // `&&str` (`&K` の K が `&str` の型) は、`&str` の値 (`fat`) の番地 (`ref<fat>`)。
            Src::Ref(m, inner) if matches!(&**inner, Src::Ref(_, i) if matches!(&**i, Src::Prim(s) if s == "str")) => {
                let fat = self.fat();
                let Ty::Agg(fi) = fat else { unreachable!() };
                Ok(Ty::Ref(*m, Pointee::Agg(fi)))
            }
            // 集成体の要素のスライス `&[S]` (`HashMapIter` の欄など) も `fat` (`{ptr, len}`)。要素の型が下ろせるときだけ。
            Src::Ref(_, inner) if matches!(&**inner, Src::Slice(e) if matches!(&**e, Src::Named(..))) => {
                let Src::Slice(e) = &**inner else { unreachable!() };
                self.lower_ty(p, file, e)?;
                Ok(self.fat())
            }
            // `&str` とスライスの値は `fat` (`{ptr, len}`)。要素はスカラーのものだけ (`sig_ty` と同じ条件)。Option のペイロードにも置ける。
            Src::Ref(_, inner) if matches!(&**inner, Src::Prim(s) if s == "str") || matches!(&**inner, Src::Slice(e) if conv(&**e).is_some()) => Ok(self.fat()),
            // 生ポインタ `*mut T` / `*const T` (指す先がスカラーのもの) は番地 (`ptr`)。extern の引数と同じ規則 (`kir_lower_ffi`)。
            Src::Ptr(_, inner) if matches!(&**inner, Src::Prim(s) if Ty::parse(s).and_then(crate::kir::Pointee::of).is_some()) => Ok(Ty::Ptr),
            other => Err(format!("{} は未対応", show(other))),
        }
    }

    /// 型 `t` (`&T` の `T`) が所有物を持つ型として下ろせるか。
    fn owning_src(&mut self, p: &Program, file: &str, t: &Src) -> bool {
        matches!(t, Src::Named(..) | Src::Tuple(_)) && self.lower_ty(p, file, t).is_ok_and(|x| self.owns_heap(x))
    }

    /// `Option<&T>` のペイロード: 要素の番地 (`ptr`) 1 つの構造体。借用なので `owns_heap` は偽 (解放しない)。名前は `bref`。
    pub fn bref(&mut self) -> Ty {
        if let Some(&i) = self.by_name.get("bref") {
            return Ty::Agg(i);
        }
        let fields = vec![Field { name: "ptr".to_string(), ty: Ty::Ptr, off: 0 }];
        self.push("bref".to_string(), 8, 8, AggKind::Struct(fields))
    }

    /// `std.io.Error` を型表に入れる (`write` の結果と `print_line` の戻り値の型)。
    pub fn io_error(&mut self, p: &Program) -> Result<Ty, String> {
        match p.defs.iter().position(|d| d.name == "std.io.Error") {
            Some(di) => self.intern_def(p, di),
            None => Err("std.io.Error の定義がない".to_string()),
        }
    }

    /// `eprint` (panic の文言と Debug 表示) の引数の型。`LLVM` の `write` の実体は `std.io.ErrorKind` の表を要るので、先に入れる。
    pub fn stderr_fat(&mut self, p: &Program) -> Result<Ty, String> {
        self.io_error(p)?;
        Ok(self.fat())
    }

    /// `&str` / スライスの値 (`{ptr, len}` の 16 バイトの構造体。設計_KIR.md 10.10)。名前は `fat` (利用者の型は FQN で `.` を含むので衝突しない)。
    pub fn fat(&mut self) -> Ty {
        if let Some(&i) = self.by_name.get("fat") {
            return Ty::Agg(i);
        }
        let fields = vec![
            Field { name: "ptr".to_string(), ty: Ty::Ptr, off: 0 },
            Field { name: "len".to_string(), ty: Ty::Usize, off: 8 },
        ];
        self.push("fat".to_string(), 16, 8, AggKind::Struct(fields))
    }

    /// 局所に持つ `s.split(sep)` の反復子 `core.str.Split` (sig の欄 `rest` / `sep` / `done` を平らに並べたもの。40 バイト)。
    /// `next` が読んで進める (`split_next`)。名前は `split_it`。
    pub fn split_it(&mut self) -> Ty {
        if let Some(&i) = self.by_name.get("split_it") {
            return Ty::Agg(i);
        }
        let fields = vec![
            Field { name: "rest_ptr".to_string(), ty: Ty::Ptr, off: 0 },
            Field { name: "rest_len".to_string(), ty: Ty::Usize, off: 8 },
            Field { name: "sep_ptr".to_string(), ty: Ty::Ptr, off: 16 },
            Field { name: "sep_len".to_string(), ty: Ty::Usize, off: 24 },
            Field { name: "done".to_string(), ty: Ty::Bool, off: 32 },
        ];
        self.push("split_it".to_string(), 40, 8, AggKind::Struct(fields))
    }

    /// `Vec<T>` / `String` のヘッダ `{ptr, len, cap}` (24 バイト)。構造体のフィールドに埋め込む形 (clif の配置と同じ)。名前は `vec`。
    pub fn vec_hdr(&mut self) -> Ty {
        if let Some(&i) = self.by_name.get("vec") {
            return Ty::Agg(i);
        }
        let fields = vec![
            Field { name: "ptr".to_string(), ty: Ty::Ptr, off: 0 },
            Field { name: "len".to_string(), ty: Ty::Usize, off: 8 },
            Field { name: "cap".to_string(), ty: Ty::Usize, off: 16 },
        ];
        self.push("vec".to_string(), 24, 8, AggKind::Struct(fields))
    }

    /// `std.thread.JoinHandle<T, A>` = `{A, usize, *mut T}`。`result` フィールドはオフセット 16。
    /// `T` は名前に埋め込む（レイアウトには現れないが、`join` で読み出す型として必要）。
    pub fn join_handle(&mut self, elem_ty: Ty, alloc_ty: Ty) -> Ty {
        let name = format!("std.thread.JoinHandle<{},{}", elem_ty.name(), alloc_ty.name());
        if let Some(&i) = self.by_name.get(&name) {
            return Ty::Agg(i);
        }
        let alloc_sz = size_of(&self.defs, alloc_ty);
        let alloc_al = align_of(&self.defs, alloc_ty);
        let align = alloc_al.max(8);
        let off_native = align_up(alloc_sz, 8);
        let off_result = align_up(off_native + 8, 8);
        let size = align_up(off_result + 8, align);
        let fields = vec![
            Field { name: "allocator".to_string(), ty: alloc_ty, off: 0 },
            Field { name: "native".to_string(), ty: Ty::Usize, off: off_native },
            Field { name: "result".to_string(), ty: Ty::Ptr, off: off_result },
        ];
        let t = self.push(name, size, align, AggKind::Struct(fields));
        if let Ty::Agg(i) = t {
            self.handle_elem.insert(i, elem_ty);
        }
        t
    }

    /// `std.task.Task<T>` = `{id: usize, gen: usize}`。`T` は名前に埋め込み、`handle_elem` に持つ (`join_task` の結果の型)。
    pub fn task_handle(&mut self, elem_ty: Ty) -> Ty {
        let name = format!("std.task.Task<{}", elem_ty.name());
        if let Some(&i) = self.by_name.get(&name) {
            return Ty::Agg(i);
        }
        let fields = vec![
            Field { name: "id".to_string(), ty: Ty::Usize, off: 0 },
            Field { name: "gen".to_string(), ty: Ty::Usize, off: 8 },
        ];
        let t = self.push(name, 16, 8, AggKind::Struct(fields));
        if let Ty::Agg(i) = t {
            self.handle_elem.insert(i, elem_ty);
        }
        t
    }

    /// 要素が所有物 (`Vec<String>`、`Vec<構造体>`) の `Vec` の型。配置は `vec` と同じで、名前 `vec<#要素の型の添字>` で要素の型を持つ
    /// (所有物の解放は型だけを見るので、Vec を解放するとき各要素も解放できる。設計_KIR.md 13.8)。それ以外の要素は `vec`。
    pub fn vec_of(&mut self, elem: Option<Ty>) -> Ty {
        let Some(Ty::Agg(i)) = elem.filter(|e| self.owns_heap(*e)) else { return self.vec_hdr() };
        let name = format!("vec<#{}>", i);
        if let Some(&k) = self.by_name.get(&name) {
            return Ty::Agg(k);
        }
        let fields = vec![
            Field { name: "ptr".to_string(), ty: Ty::Ptr, off: 0 },
            Field { name: "len".to_string(), ty: Ty::Usize, off: 8 },
            Field { name: "cap".to_string(), ty: Ty::Usize, off: 16 },
        ];
        self.push(name, 24, 8, AggKind::Struct(fields))
    }

    /// `Box<T>` の型 (所有するヒープの 1 要素へのポインタ `{ptr}`)。名前は `box<#N>` (`T` が集成体) か `box<型名>`。
    pub fn box_of(&mut self, elem: Ty) -> Ty {
        let key = match elem {
            Ty::Agg(i) => format!("box<#{}>", i),
            t => format!("box<{}>", crate::kir::fmt_ty(t)),
        };
        if let Some(&i) = self.by_name.get(&key) {
            return Ty::Agg(i);
        }
        let fields = vec![Field { name: "ptr".to_string(), ty: Ty::Ptr, off: 0 }];
        let t = self.push(key, 8, 8, AggKind::Struct(fields));
        if let Ty::Agg(i) = t {
            self.box_elems.insert(i, elem);
        }
        t
    }

    /// `Box<dyn Tr>` の型 `{ptr, vt}` (データの番地と vtable の番号)。トレイトごとに別の型 (名前 `dynbox<トレイトの FQN>`)。
    pub fn dynbox(&mut self, tr: &str) -> Ty {
        self.dynbox_named("dynbox", tr)
    }

    /// `dynbox<Tr>` (所有) か `dynref<Tr>` (`&dyn Tr` の借用。配置は同じ)。
    pub fn dynbox_named(&mut self, kind: &str, tr: &str) -> Ty {
        let name = format!("{}<{}>", kind, tr);
        if let Some(&i) = self.by_name.get(&name) {
            return Ty::Agg(i);
        }
        let fields = vec![
            Field { name: "ptr".to_string(), ty: Ty::Ptr, off: 0 },
            Field { name: "vt".to_string(), ty: Ty::Usize, off: 8 },
        ];
        self.push(name, 16, 8, AggKind::Struct(fields))
    }

    /// `Box<dyn Tr>` の型なら、トレイトの FQN。
    pub fn dynbox_trait(&self, t: Ty) -> Option<String> {
        let Ty::Agg(i) = t else { return None };
        let n = &self.defs[i as usize].name;
        n.strip_prefix("dynbox<").or_else(|| n.strip_prefix("dynref<"))?.strip_suffix('>').map(String::from)
    }

    /// `derive [Drop]` の再帰する型か。
    pub fn is_rec_drop(&self, t: Ty) -> bool {
        matches!(t, Ty::Agg(i) if self.rec_drop.contains(&i))
    }

    /// `Box<T>` (`box<..>`) の中身の型。`Box<dyn>` と他の型は `None`。
    pub fn box_elem(&self, t: Ty) -> Option<Ty> {
        let Ty::Agg(i) = t else { return None };
        self.box_elems.get(&i).copied()
    }

    /// `Box<dyn Tr>` の型か。
    pub fn is_dynbox(&self, t: Ty) -> bool {
        self.dynbox_trait(t).is_some()
    }

    /// `Vec` / `String` のヘッダ型 (`vec` と `vec<#N>`) か。
    pub fn is_vec(&self, t: Ty) -> bool {
        matches!(t, Ty::Agg(i) if is_vec_name(&self.defs[i as usize].name))
    }

    /// 名前付きの集成体の添字。
    pub(super) fn agg_index(&self, name: &str) -> Option<u32> {
        self.by_name.get(name).copied()
    }

    /// 所有物を要素に持つ Vec の型 `t` の、要素の型。
    pub fn vec_elem(&self, t: Ty) -> Option<Ty> {
        let Ty::Agg(i) = t else { return None };
        let n = self.defs[i as usize].name.strip_prefix("vec<#")?.strip_suffix('>')?;
        n.parse::<u32>().ok().map(Ty::Agg)
    }

    /// `alloc.alloc.AllocError` (仕様どおり `{ code: u32 }`。Kasane で書いた `Vec` が `AllocError { code: .. }` を作れるように)。`Vec::push` などの `Result` の E。
    pub fn alloc_err(&mut self) -> Ty {
        const NAME: &str = "alloc.alloc.AllocError";
        if let Some(&i) = self.by_name.get(NAME) {
            return Ty::Agg(i);
        }
        let fields = vec![Field { name: "code".to_string(), ty: Ty::U32, off: 0 }];
        self.push(NAME.to_string(), 4, 4, AggKind::Struct(fields))
    }

    /// `alloc.vec.InsertError` 列挙型 (OutOfBounds / Alloc)。`insert` の `Err(InsertError::OutOfBounds)` を作るためだけに用いる。
    pub fn insert_err(&mut self) -> Ty {
        const NAME: &str = "alloc.vec.InsertError";
        if let Some(&i) = self.by_name.get(NAME) {
            return Ty::Agg(i);
        }
        let alloc_err = self.alloc_err();
        let variants = vec![
            ("OutOfBounds".to_string(), vec![]),
            ("Alloc".to_string(), vec![alloc_err]),
        ];
        let (size, align, variants) = layout_enum(&self.defs, &variants);
        self.push(NAME.to_string(), size, align, AggKind::Enum(variants))
    }

    /// `std.sync.Sender<T>` / `Receiver<T>` = `{ state: ptr }` (チャネルの状態は実メモリ。設計_KIR.md 22 章)。
    pub fn chan_end(&mut self, sender: bool, elem: Ty) -> Ty {
        let name = format!("std.sync.{}<{}>", if sender { "Sender" } else { "Receiver" }, elem.name());
        if let Some(&i) = self.by_name.get(&name) {
            return Ty::Agg(i);
        }
        let fields = vec![Field { name: "state".to_string(), ty: Ty::Ptr, off: 0 }];
        let t = self.push(name, 8, 8, AggKind::Struct(fields));
        if let Ty::Agg(i) = t {
            self.chan_elem.insert(i, elem);
        }
        t
    }

    /// `std.sync.SendError<T>` = `{ value: T }`。
    pub fn send_err(&mut self, elem: Ty) -> Ty {
        let name = format!("std.sync.SendError<{}>", elem.name());
        if let Some(&i) = self.by_name.get(&name) {
            return Ty::Agg(i);
        }
        let (size, align, fields) = layout_fields(&self.defs, &[("value".to_string(), elem)]);
        self.push(name, size, align, AggKind::Struct(fields))
    }

    /// チャネルの端の型なら `Some(送り手か)`。
    pub fn chan_kind(&self, t: Ty) -> Option<bool> {
        let Ty::Agg(i) = t else { return None };
        let n = &self.defs[i as usize].name;
        if n.starts_with("std.sync.Sender<") {
            Some(true)
        } else if n.starts_with("std.sync.Receiver<") {
            Some(false)
        } else {
            None
        }
    }

    /// 型 `t` の値が (入れ子を含めて) ヒープの所有物 (Vec / String のヘッダ) を持つか。持つ値は移動と解放の対象になる。
    pub fn owns_heap(&self, t: Ty) -> bool {
        match t {
            Ty::Agg(i) => {
                let d = &self.defs[i as usize];
                if is_vec_name(&d.name) || d.name.starts_with("dynbox<") || self.box_elems.contains_key(&i) || self.chan_kind(t).is_some() {
                    return true;
                }
                match &d.kind {
                    AggKind::Struct(fs) | AggKind::Tuple(fs) => fs.iter().any(|f| self.owns_heap(f.ty)),
                    AggKind::Enum(vs) => vs.iter().any(|v| v.fields.iter().any(|f| self.owns_heap(f.ty))),
                }
            }
            Ty::Opt(p) => self.owns_heap(p.ty()),
            Ty::Res(a, e) => self.owns_heap(a.ty()) || self.owns_heap(e.ty()),
            _ => false,
        }
    }

    /// `Option<&T>` / `Option<&mut T>` を番地 1 つの `bref` にする型か: 所有物を持つ型、集成体 (名前・タプル)、`&mut` のスカラー。
    /// `&str` と `&[T]` (fat) と共有のスカラーは値のまま (`Option<&スカラー>` の決まり、設計_KIR.md 10.10)。
    fn bref_ref(&mut self, p: &Program, file: &str, m: bool, inner: &Src) -> bool {
        self.owning_src(p, file, inner) || matches!(inner, Src::Named(..) | Src::Tuple(_)) || (m && matches!(inner, Src::Prim(s) if s != "str"))
    }

    /// `Option` / `Result` のペイロードの型。スカラー・unit・集成体・入れ子は `Pay` になる (128 ビット整数・参照は拒否)。
    fn pay_of(&mut self, p: &Program, file: &str, t: &Src) -> Result<Pay, String> {
        // `Option<&[T; N]>` は `Option<&[T]>` と同じ値 (`fat`) で持つ。長さ N は型検査が両辺を揃えるので、値には要らない。
        // (`lower_ty` の `&[T; N]` は参照 `ref<fat>` で、Option のペイロードには置けないため。)
        if let Src::Ref(_, inner) = t {
            if matches!(&**inner, Src::Array(e, _) if conv(e).is_some()) {
                return Pay::of(self.fat()).ok_or_else(|| "&[T; N] の fat は Option のペイロードに使えない".to_string());
            }
        }
        // `&Vec` / `&String` / `&HashMap` は借用のヘッダのアドレス (`ptr`)。
        if let Src::Ref(_, inner) = t {
            if matches!(&**inner, Src::Named(n, _) if n == "alloc.vec.Vec" || n == "alloc.raw.Buf" || n == "alloc.string.String") {
                return Ok(Pay::Ptr);
            }
        }
        // `Option<&T>` / `Result<&T, E>` (T は所有物を持つ型): 番地 1 つの `bref` (設計_KIR.md 17.18)。Option と Result で同じ形にする。
        if let Src::Ref(_, inner) = t {
            // 集成体への参照 (所有物を持つか否か) は番地 1 つの `bref`: 値の複製では `&mut` の書き込みが残らない。
            if self.bref_ref(p, file, matches!(t, Src::Ref(true, _)), inner) {
                let b = self.bref();
                return Pay::of(b).ok_or_else(|| "bref は Option / Result のペイロードに使えない".to_string());
            }
        }
        let x = self.lower_ty(p, file, t)?;
        Pay::of(x).ok_or_else(|| format!("型 {} は Option / Result のペイロードに使えない", x.name()))
    }

    /// フィールド・要素の型。ユニット型は値を置けないので拒否する。
    fn field_ty(&mut self, p: &Program, file: &str, t: &Src) -> Result<Ty, String> {
        if let Src::Named(n, _) = t {
            if n == "alloc.vec.Vec" || n == "alloc.raw.Buf" || n == "alloc.string.String" {
                return self.lower_ty(p, file, t);
            }
        }
        // `&Vec` / `&String` / `&HashMap` のフィールドは、借用のヘッダのアドレス (`ptr`)。
        if let Src::Ref(_, inner) = t {
            if matches!(&**inner, Src::Named(n, _) if n == "alloc.vec.Vec" || n == "alloc.raw.Buf" || n == "alloc.string.String") {
                return Ok(Ty::Ptr);
            }
        }
        match self.lower_ty(p, file, t)? {
            Ty::Unit => Err("ユニット型のフィールドは段階 1 の対象外".to_string()),
            // `Option<P>` のフィールド (P は 64 ビット以下のスカラー・unit・集成体。所有物を持つ `Vec` / `String` も含む)。
            // 値はペイロードの枠とタグの整数で、バイト列の枠の後ろにタグのバイトを置く (設計_KIR.md 26 章)。
            Ty::Opt(q) if !matches!(q.ty(), Ty::Opt(_) | Ty::Res(..)) => Ok(Ty::Opt(q)),
            Ty::Opt(_) => Err("入れ子の Option のフィールドは段階 1 の対象外".to_string()),
            Ty::Res(..) => Err("Result のフィールドは段階 1 の対象外".to_string()),
            x => Ok(x),
        }
    }

    fn name_of(&self, t: Ty) -> String {
        match t {
            Ty::Agg(i) => self.defs[i as usize].name.clone(),
            t => t.name().to_string(),
        }
    }

    fn push(&mut self, name: String, size: u32, align: u32, kind: AggKind) -> Ty {
        let i = self.defs.len() as u32;
        self.by_name.insert(name.clone(), i);
        self.defs.push(AggDef { name, size, align, kind });
        Ty::Agg(i)
    }

    pub(super) fn intern_tuple(&mut self, elems: Vec<Ty>) -> Result<Ty, String> {
        let name = format!("({})", elems.iter().map(|t| self.name_of(*t)).collect::<Vec<_>>().join(","));
        if let Some(&i) = self.by_name.get(&name) {
            return Ok(Ty::Agg(i));
        }
        let fs: Vec<(String, Ty)> = elems.into_iter().map(|t| (String::new(), t)).collect();
        let (size, align, fields) = layout_fields(&self.defs, &fs);
        Ok(self.push(name, size, align, AggKind::Tuple(fields)))
    }

    /// 定義 `p.defs[di]` (構造体 `type` または列挙型 `enum`) を型表に登録する。
    pub(super) fn intern_def(&mut self, p: &Program, di: usize) -> Result<Ty, String> {
        let d = &p.defs[di];
        if let Some(&i) = self.by_name.get(&d.name) {
            return Ok(Ty::Agg(i));
        }
        if (d.kw != "type" && d.kw != "enum") || !d.generics.iter().all(|g| g.0 == "life") {
            return Err(format!("型 {} は段階 1 の対象外 (構造体・列挙型でない、またはジェネリクス)", d.name));
        }
        if !self.busy.insert(d.name.clone()) {
            return Err(format!("再帰的な型 {} は段階 1 の対象外", d.name));
        }
        // `derive [Drop]` の型は、先に添字を予約して、`Box<Self>` のフィールドから自分を指せるようにする (箱は 8 バイトなので
        // 中身の配置は要らない)。検査器が、再帰の出現を `Box<Self>` / `Option<Box<Self>>` に限っている (E2051)。
        let reserved = if d.derive.iter().any(|x| x == "Drop") {
            let t = self.push(d.name.clone(), 0, 1, AggKind::Struct(vec![]));
            let Ty::Agg(i) = t else { unreachable!() };
            self.rec_drop.insert(i);
            Some(i)
        } else {
            None
        };
        let r = self.build_def(p, di, reserved);
        self.busy.remove(&d.name);
        if r.is_err() {
            if let Some(i) = reserved {
                // 失敗した予約は取り消す (型表の末尾にあるとき)。
                self.rec_drop.remove(&i);
                self.by_name.remove(&d.name);
                if i as usize + 1 == self.defs.len() {
                    self.defs.pop();
                }
            }
        }
        r
    }

    /// 型引数付きの利用者の構造体・列挙型 (`p2.Pair<u32>`) を、型パラメータを具体型で置き換えた別の集成体として登録する。
    /// 名前は `FQN<引数,..>`。`type` の型パラメータだけを持つ定義が対象 (寿命の引数は無視する)。
    fn intern_generic(&mut self, p: &Program, di: usize, args: &[Src]) -> Result<Ty, String> {
        let d = &p.defs[di];
        let names: Vec<String> = d.generics.iter().filter(|g| g.0 == "type").map(|g| g.1.clone()).collect();
        if (d.kw != "type" && d.kw != "enum") || d.generics.iter().any(|g| g.0 != "life" && g.0 != "type") || names.len() != args.len() {
            return Err(format!("型 {} は段階 1 の対象外 (型パラメータ以外のジェネリクス、または型引数の個数が違う)", d.name));
        }
        let key = format!("{}<{}>", d.name, args.iter().map(show).collect::<Vec<_>>().join(","));
        if let Some(&i) = self.by_name.get(&key) {
            return Ok(Ty::Agg(i));
        }
        if !self.busy.insert(key.clone()) {
            return Err(format!("再帰的な型 {} は段階 1 の対象外", key));
        }
        let bind: std::collections::HashMap<String, Src> = names.into_iter().zip(args.iter().cloned()).collect();
        let r = self.build_def_with(p, di, &bind, key.clone(), None);
        self.busy.remove(&key);
        if let Ok(Ty::Agg(i)) = r {
            self.generic_args.insert(i, (d.name.clone(), args.to_vec()));
        }
        r
    }

    fn build_def(&mut self, p: &Program, di: usize, reserved: Option<u32>) -> Result<Ty, String> {
        let name = p.defs[di].name.clone();
        self.build_def_with(p, di, &std::collections::HashMap::new(), name, reserved)
    }

    fn build_def_with(&mut self, p: &Program, di: usize, bind: &std::collections::HashMap<String, Src>, name: String, reserved: Option<u32>) -> Result<Ty, String> {
        let d = &p.defs[di];
        let tn: HashSet<String> = bind.keys().cloned().collect();
        let sub = |v: &V| super::mono::subst_ty(&from_v(v, &tn), bind);
        let (size, align, kind) = if d.kw == "type" {
            let mut fs = vec![];
            for (fname, fv, _) in &d.fields {
                let t = self.field_ty(p, &d.file, &sub(fv))?;
                fs.push((fname.clone(), t));
            }
            let (size, align, fields) = layout_fields(&self.defs, &fs);
            (size, align, AggKind::Struct(fields))
        } else {
            let mut vs = vec![];
            for (vn, tys) in &d.variants {
                let mut ts = vec![];
                for tv in tys {
                    ts.push(self.field_ty(p, &d.file, &sub(tv))?);
                }
                vs.push((vn.clone(), ts));
            }
            let (size, align, variants) = layout_enum(&self.defs, &vs);
            (size, align, AggKind::Enum(variants))
        };
        if let Some(i) = reserved {
            self.defs[i as usize] = AggDef { name: name.clone(), size, align, kind };
            return Ok(Ty::Agg(i));
        }
        Ok(self.push(name, size, align, kind))
    }
}

impl<'a> Lw<'a> {
    /// 推論済みの型を段階 1 の型にする (集成体は型表に登録する)。対象外なら `None`。
    pub(super) fn conv_t(&self, t: &Src) -> Option<Ty> {
        self.conv_t_why(t).ok()
    }

    /// `conv_t` と同じ。対象外の理由 (`std.io.Error は未対応` など) も返す。
    pub(super) fn conv_t_why(&self, t: &Src) -> Result<Ty, String> {
        self.aggs.borrow_mut().lower_ty(self.p, &self.def.file, t)
    }

    pub(super) fn agg_def(&self, ai: u32) -> AggDef {
        self.aggs.borrow().defs[ai as usize].clone()
    }

    /// 値 `v` の集成体の型 (番号と定義)。集成体でなければ止める。
    fn agg_of(&self, v: VId, line: usize) -> LR<(u32, AggDef)> {
        match self.vty(v) {
            Ty::Agg(ai) => Ok((ai, self.agg_def(ai))),
            t => self.err(line, format!("型 {} は集成体でない", crate::kir::fmt_ty(t))),
        }
    }

    /// 集成体の式の振り分け (`expr` から呼ぶ)。
    pub(super) fn agg_expr(&mut self, e: &V) -> LR<Option<VId>> {
        match nname(e) {
            "StructLit" => self.struct_lit(e),
            "TupleE" => self.tuple_e(e),
            "Field" => {
                let f = nf(e);
                let Some(b) = self.expr_peek(&f[0])? else { return Ok(None) };
                let name = st(&f[1]);
                let v = self.field_of(b, &name, nline(e))?;
                if super::coll::owns(self, self.vty(v)) {
                    // 局所変数から始まるフィールドの連なりなら、所有物を移し出す (部分的な移動)。それ以外 (一時の値のフィールド) は未対応。
                    if let Some((s, names)) = super::own::chain_of(self, e) {
                        if self.coll.elem_slots.values().any(|x| *x == s) {
                            return self.err(nline(e), "添字で借りた要素のフィールドの所有物を値で使う (移す) 形は段階 1 の対象外");
                        }
                        return Ok(Some(super::coll::field_move(self, s, &names, nline(e))?));
                    }
                    return self.err(nline(e), "一時の値のフィールドからの所有物の移動は段階 1 の対象外");
                }
                if super::coll::owns(self, self.vty(b)) && super::coll::val_slot(self, &f[0]).is_none() {
                    let bt = self.vty(b);
                    super::coll::drop_value(self, b, bt);
                }
                Ok(Some(v))
            }
            other => self.err(nline(e), format!("式 {} は段階 1 の対象外", other)),
        }
    }

    /// 構造体リテラル `S { f: e, ... }`。式は書かれた順に評価し、値は宣言の順に並べる。
    fn struct_lit(&mut self, e: &V) -> LR<Option<VId>> {
        let f = nf(e);
        let line = nline(e);
        let segs: Vec<String> = st(&f[0]).split('.').map(String::from).collect();
        let Some(Tgt::Type(_)) = self.p.resolve(&segs, &self.def.file).map(|r| r.target) else {
            return self.err(line, "構造体リテラルの名前が段階 1 の対象外");
        };
        let ty = self.ty_of(e)?;
        let Ty::Agg(ai) = ty else {
            return self.err(line, "構造体リテラルの型が集成体でない");
        };
        let d = self.agg_def(ai);
        let AggKind::Struct(decl) = &d.kind else {
            return self.err(line, "構造体リテラルの型が構造体でない");
        };
        let mut vals: Vec<Option<VId>> = vec![None; decl.len()];
        for it in items(&f[1]) {
            let V::L(xs) = it else {
                return self.err(line, "構造体リテラルの形が不正");
            };
            let fname = st(&xs[0]);
            let Some(k) = decl.iter().position(|x| x.name == fname) else {
                return self.err(line, format!("フィールド {} がない (型 {} の欄: {:?})", fname, d.name, decl.iter().map(|x| x.name.clone()).collect::<Vec<_>>()));
            };
            if vals[k].is_some() {
                return self.err(line, format!("フィールド {} が二度書かれている", fname));
            }
            let Some(v) = self.expr(&xs[1])? else {
                let held: Vec<VId> = vals.iter().flatten().copied().collect();
                super::coll::tmp_release(self, &held);
                return Ok(None);
            };
            super::coll::tmp_hold(self, v);
            if self.vty(v) != decl[k].ty {
                return self.err(line, format!("フィールド {} の型が違う", fname));
            }
            vals[k] = Some(v);
        }
        let held: Vec<VId> = vals.iter().flatten().copied().collect();
        super::coll::tmp_release(self, &held);
        let Some(fields) = vals.into_iter().collect::<Option<Vec<VId>>>() else {
            return self.err(line, "構造体リテラルにフィールドが足りない");
        };
        Ok(Some(self.emit(ty, Op::Make { variant: None, fields })))
    }

    /// タプル `(a, b, ...)`。
    fn tuple_e(&mut self, e: &V) -> LR<Option<VId>> {
        let line = nline(e);
        let xs = items(&nf(e)[0]);
        let ty = self.ty_of(e)?;
        let Ty::Agg(ai) = ty else {
            return self.err(line, "タプルの型が集成体でない");
        };
        let d = self.agg_def(ai);
        let AggKind::Tuple(decl) = &d.kind else {
            return self.err(line, "タプルの型がタプルでない");
        };
        if decl.len() != xs.len() {
            return self.err(line, "タプルの要素数が型と合わない");
        }
        let mut fields = vec![];
        for (x, fd) in xs.iter().zip(decl) {
            // 参照の要素 (`(&K, &V)`) は、読んで値にせず参照のまま渡す (`expr` は参照を読むことがある)。
            let got = if matches!(fd.ty, Ty::Ref(..)) { self.expr_peek(x)? } else { self.expr(x)? };
            let Some(v) = got else {
                super::coll::tmp_release(self, &fields);
                return Ok(None);
            };
            // 欄が `&スカラー` で、要素が値 (束縛が値のとき): 値を一時の枠に置き、その番地を渡す (共有の参照は読むだけなので複製と同じ)。
            let v = match fd.ty {
                Ty::Ref(false, p) if self.vty(v) == p.ty() && p.ty().is_scalar() => {
                    let slot = self.alloca(p);
                    self.emit(Ty::Unit, Op::Store { ptr: slot, val: v });
                    self.emit(fd.ty, Op::Bitcast { src: self.vty(slot), a: slot })
                }
                _ => v,
            };
            let v = match self.vty(v) {
                Ty::Ref(_, p) if p.ty() == fd.ty && fd.ty.is_scalar() => self.emit(fd.ty, Op::Load(v)),
                // 借用のヘッダ (`&String` の欄は `ptr`): 集成体の番地をヘッダのポインタとして渡す。
                Ty::Ref(_, Pointee::Agg(_)) if fd.ty == Ty::Ptr => self.emit(Ty::Ptr, Op::Bitcast { src: self.vty(v), a: v }),
                // 参照の集成体 (`&&str` の番地) を、値の欄 (`fat`) へ読む。
                Ty::Ref(_, Pointee::Agg(i)) if fd.ty == Ty::Agg(i) => self.emit(fd.ty, Op::Load(v)),
                _ => v,
            };
            super::coll::tmp_hold(self, v);
            if self.vty(v) != fd.ty {
                return self.err(line, format!("タプルの要素の型が違う ({} / {})", crate::kir::fmt_ty(self.vty(v)), crate::kir::fmt_ty(fd.ty)));
            }
            fields.push(v);
        }
        super::coll::tmp_release(self, &fields);
        Ok(Some(self.emit(ty, Op::Make { variant: None, fields })))
    }

    /// 構造体のフィールドの読み出し (`field`)。
    pub(super) fn field_of(&mut self, base: VId, name: &str, line: usize) -> LR<VId> {
        // 参照を通した読み出し `r.f`: フィールドのアドレスを取って読む。
        if let Ty::Ref(_, Pointee::Agg(_)) = self.vty(base) {
            // 参照型のフィールド (`r: &'a mut u32`): 番地の中身は ptr (8 バイト)。読んで参照に戻す。
            if let Some((addr, fty)) = self.ref_field_ptr(base, name, line)? {
                let raw = self.emit(Ty::Ptr, Op::MemLoad { p: addr, off: 0 });
                return Ok(self.emit(fty, Op::Bitcast { src: Ty::Ptr, a: raw }));
            }
            let addr = self.field_addr(base, name, line)?;
            let Ty::Ref(_, p) = self.vty(addr) else {
                return self.err(line, "フィールドのアドレスが参照でない");
            };
            return Ok(self.emit(p.ty(), Op::Load(addr)));
        }
        let (_, d) = self.agg_of(base, line)?;
        let AggKind::Struct(fs) = &d.kind else {
            return self.err(line, format!("{} は構造体でないのでフィールド {} を読めない", d.name, name));
        };
        let Some(fd) = fs.iter().find(|x| x.name == name) else {
            return self.err(line, format!("フィールド {} がない", name));
        };
        Ok(self.emit(fd.ty, Op::Field { a: base, off: fd.off }))
    }

    /// 集成体への参照 `base` の構造体のフィールド `name` が参照型のとき、その番地 (ptr) と欄の型を返す。
    /// 参照は指す先を持たない 8 バイトの ptr として置くので、番地は `base` の ptr に欄の位置を足したもの。それ以外は `None`。
    pub(super) fn ref_field_ptr(&mut self, base: VId, name: &str, line: usize) -> LR<Option<(VId, Ty)>> {
        let bty = self.vty(base);
        let Ty::Ref(_, Pointee::Agg(ai)) = bty else { return Ok(None) };
        let d = self.agg_def(ai);
        let AggKind::Struct(fs) = &d.kind else { return Ok(None) };
        let Some(fd) = fs.iter().find(|x| x.name == name) else { return Ok(None) };
        if !matches!(fd.ty, Ty::Ref(..)) {
            return Ok(None);
        }
        let (off, fty) = (fd.off, fd.ty);
        let _ = line;
        let bp = self.emit(Ty::Ptr, Op::Bitcast { src: bty, a: base });
        let k = self.emit(Ty::Usize, Op::Const(off as i128));
        Ok(Some((self.emit(Ty::Ptr, Op::PtrAdd { p: bp, off: k }), fty)))
    }

    /// 集成体への参照 `base` の構造体のフィールド `name` へのアドレス (`fieldaddr`)。可変性は `base` と同じ。
    pub(super) fn field_addr(&mut self, base: VId, name: &str, line: usize) -> LR<VId> {
        let Ty::Ref(m, Pointee::Agg(ai)) = self.vty(base) else {
            return self.err(line, "フィールドのアドレスの対象が集成体への参照でない");
        };
        let d = self.agg_def(ai);
        let AggKind::Struct(fs) = &d.kind else {
            return self.err(line, format!("{} は構造体でないのでフィールド {} のアドレスを取れない", d.name, name));
        };
        let Some(fd) = fs.iter().find(|x| x.name == name) else {
            return self.err(line, format!("フィールド {} がない", name));
        };
        let Some(p) = Pointee::of(fd.ty) else {
            return self.err(line, format!("フィールド {} の型 {} への参照は段階 1 の対象外", name, crate::kir::fmt_ty(fd.ty)));
        };
        Ok(self.emit(Ty::Ref(m, p), Op::FieldAddr { a: base, off: fd.off }))
    }

    /// `x.f.v.len()` / `x.f.v.is_empty()` (`v` は Vec / String のフィールド): ヘッダの `len` を読む。扱えない形なら `None`。
    pub(super) fn vec_field_len(&mut self, s: usize, segs: &[String], line: usize) -> LR<Option<VId>> {
        let base = self.slots[s].0;
        let mut cur = match self.vty(base) {
            Ty::Ref(_, Pointee::Agg(_)) => base,
            _ if self.is_mem_val(base) => base,
            _ => return Ok(None),
        };
        for name in &segs[1..segs.len() - 1] {
            cur = self.field_addr(cur, name, line)?;
        }
        let Ty::Ref(_, Pointee::Agg(k)) = self.vty(cur) else { return Ok(None) };
        if !is_vec_name(&self.agg_def(k).name) {
            return Ok(None);
        }
        let la = self.field_addr(cur, "len", line)?;
        let len = self.emit(Ty::Usize, Op::Load(la));
        if segs[segs.len() - 1] == "is_empty" {
            let zero = self.emit(Ty::Usize, Op::Const(0));
            return Ok(Some(self.emit(Ty::Bool, Op::Cmp { cc: crate::kir::Cc::Eq, ty: Ty::Usize, a: len, b: zero })));
        }
        Ok(Some(len))
    }

    /// `x.f.g` の形の名前 (局所変数が先頭) のフィールドをたどる。
    pub(super) fn field_path(&mut self, base: VId, names: &[String], line: usize) -> LR<VId> {
        let mut v = base;
        for n in names {
            v = self.field_of(v, n, line)?;
        }
        Ok(v)
    }

    /// 列挙型のバリアントを作る (引数は宣言の順)。`args` が空なら、ペイロードのない変種。
    pub(super) fn variant_make(&mut self, en: &str, vn: &str, args: &[V], e: Option<&V>, line: usize) -> LR<Option<VId>> {
        // 型引数のある列挙型 (`HashCell<K, V>`) の変種は、型の名前だけでは決まらない: 式の型 (型検査が決めたもの) から取る。
        let ty = match self.conv_t(&Src::Named(en.to_string(), vec![])) {
            Some(t) => t,
            None => match e.and_then(|e| self.src_ty(e).ok()).and_then(|s| self.conv_t(&s)) {
                Some(t) => t,
                None => return self.err(line, format!("列挙型 {} は段階 1 の対象外", en)),
            },
        };
        let Ty::Agg(ai) = ty else {
            return self.err(line, format!("列挙型 {} は集成体でない", en));
        };
        let d = self.agg_def(ai);
        let AggKind::Enum(vs) = &d.kind else {
            return self.err(line, format!("{} は列挙型でない", en));
        };
        let Some(idx) = vs.iter().position(|v| v.name == vn) else {
            return self.err(line, format!("列挙子 {} がない", vn));
        };
        let decl = vs[idx].fields.clone();
        if decl.len() != args.len() {
            return self.err(line, format!("列挙子 {} の引数の個数が違う", vn));
        }
        let mut vals = vec![];
        for (a, fd) in args.iter().zip(&decl) {
            let Some(v) = self.expr(a)? else {
                super::coll::tmp_release(self, &vals);
                return Ok(None);
            };
            super::coll::tmp_hold(self, v);
            if self.vty(v) != fd.ty {
                return self.err(line, format!("列挙子 {} の引数の型が違う", vn));
            }
            vals.push(v);
        }
        super::coll::tmp_release(self, &vals);
        Ok(Some(self.emit(ty, Op::Make { variant: Some(idx as u32), fields: vals })))
    }

    /// 呼び出し形 `E.V(args)` (`call` から呼ぶ)。
    pub(super) fn variant_call(&mut self, e: &V, en: &str, vn: &str) -> LR<Option<VId>> {
        let args = items(&nf(e)[1]);
        self.variant_make(en, vn, args, Some(e), nline(e))
    }

    /// 値の形 `E.V` (ペイロードのない列挙子、`path_val` から呼ぶ)。
    pub(super) fn variant_value(&mut self, en: &str, vn: &str, e: Option<&V>, line: usize) -> LR<VId> {
        match self.variant_make(en, vn, &[], e, line)? {
            Some(v) => Ok(v),
            None => self.err(line, "列挙子の値が作れない"),
        }
    }

    /// `let` の束縛 (名前だけでない形: タプル・構造体・列挙型の反駁できない形)。
    pub(super) fn let_pat(&mut self, pat: &V, init: &V) -> LR<St> {
        let Some(v) = self.expr(init)? else { return Ok(St::Div) };
        let t = self.vty(v);
        let binds = self.pat(pat, v, t, None)?;
        if super::coll::owns(self, t) {
            // 所有物を持つ値の分解: 束縛した部分は変数が所有し、束縛しなかった所有物は分解の時点で解放する (設計_KIR.md 13.7)。
            let mut srcs = std::collections::BTreeMap::new();
            if let Ok(ss) = self.src_ty(init) {
                super::coll::pat_srcs(self, pat, &ss, &mut srcs);
            }
            for (n, bv, bt) in binds {
                self.coll.hint = srcs.get(&n).cloned();
                super::coll::bind_owned(self, &n, bv, bt);
                self.coll.hint = None;
            }
            super::coll::drop_unbound(self, pat, v, t)?;
            return Ok(St::Unit);
        }
        // 束縛の型表の型から種類 (`&str` の `split_once` など) を結び付ける (設計_KIR.md 23 章)。
        let mut srcs = std::collections::BTreeMap::new();
        if let Ok(ss) = self.src_ty(init) {
            super::coll::pat_srcs(self, pat, &ss, &mut srcs);
        }
        for (n, bv, bt) in binds {
            self.bind(&n, bv, bt);
            let Some(src) = srcs.get(&n).cloned() else { continue };
            let Some(slot) = self.lookup(&n) else { continue };
            if let Some(k) = super::coll::kind_of_src(self, &src) {
                self.coll.kinds.insert(slot, k);
                if matches!(src, crate::types::Ty::Ref(..)) {
                    self.coll.borrowed.insert(slot);
                }
            }
        }
        Ok(St::Unit)
    }

    /// `p.f = e` / `p.f.g = e` (局所変数を起点とする名前への代入)。値を作り直して変数に入れ直す。
    pub(super) fn assign_path(&mut self, target: &V, rhs: &V, line: usize) -> LR<St> {
        let segs = strs(&nf(target)[0]);
        self.assign_segs(segs, rhs, line)
    }

    /// `segs` (局所の名前と、その後のフィールド) への代入。`assign_path` と `self.f = e` (フィールドの式の形) が使う。
    pub(super) fn assign_segs(&mut self, segs: Vec<String>, rhs: &V, line: usize) -> LR<St> {
        let Some(s) = self.lookup(&segs[0]) else {
            return self.err(line, format!("代入先 {} が見つからない", segs[0]));
        };
        let Some(v) = self.expr(rhs)? else { return Ok(St::Div) };
        self.store_segs(s, &segs, v, line, true)
    }

    /// `segs` (局所変数 `s` から始まる) のフィールドへ値 `v` を書く。`drop_old` なら所有物の古い値を先に解放する
    /// (メソッドが書き換えたヘッダを戻すときは、同じバッファなので解放しない)。
    pub(super) fn store_segs(&mut self, s: usize, segs: &[String], v: VId, line: usize, drop_old: bool) -> LR<St> {
        let base0 = self.read_slot(s);
        // 参照を通した代入 `r.f.g = v`: フィールドのアドレスをたどって書く (参照は可変のものだけ)。
        if let Ty::Ref(m, Pointee::Agg(_)) = self.vty(base0) {
            if !m {
                return self.err(line, "共有参照 &T を通した代入は段階 1 の対象外");
            }
            let mut cur = base0;
            for name in &segs[1..segs.len() - 1] {
                cur = self.field_addr(cur, name, line)?;
            }
            // 最後の欄が参照型: 番地の ptr に参照を ptr として書く (参照は所有物を持たないので古い値の解放はない)。
            if let Some((addr, fty)) = self.ref_field_ptr(cur, &segs[segs.len() - 1], line)? {
                if fty != self.vty(v) {
                    return self.err(line, "フィールドへの代入の型が違う");
                }
                let bits = self.emit(Ty::Ptr, Op::Bitcast { src: fty, a: v });
                self.emit(Ty::Unit, Op::MemStore { ty: Ty::Ptr, p: addr, off: 0, v: bits });
                return Ok(St::Unit);
            }
            let last = self.field_addr(cur, &segs[segs.len() - 1], line)?;
            let Ty::Ref(_, p) = self.vty(last) else {
                return self.err(line, "フィールドのアドレスが参照でない");
            };
            if p.ty() != self.vty(v) {
                return self.err(line, "フィールドへの代入の型が違う");
            }
            // 所有物のフィールドは、古い値を解放してから書く。
            if drop_old && super::coll::owns(self, p.ty()) {
                let old = self.emit(p.ty(), Op::Load(last));
                super::coll::drop_value(self, old, p.ty());
            }
            self.emit(Ty::Unit, Op::Store { ptr: last, val: v });
            return Ok(St::Unit);
        }
        // chain[j] は segs[..=j] の値 (最後の名前の手前まで)
        let mut chain = vec![base0];
        for name in &segs[1..segs.len() - 1] {
            let p = *chain.last().expect("鎖");
            let nv = self.field_of(p, name, line)?;
            chain.push(nv);
        }
        // 所有物のフィールドは、古い値を解放してから書き換える。
        let last_old = {
            let parent = *chain.last().expect("鎖");
            let old = self.field_of(parent, &segs[segs.len() - 1], line)?;
            (drop_old && super::coll::owns(self, self.vty(old))).then_some(old)
        };
        if let Some(old) = last_old {
            let ot = self.vty(old);
            super::coll::drop_value(self, old, ot);
        }
        let mut newv = v;
        for j in (1..segs.len()).rev() {
            newv = self.set_field(chain[j - 1], &segs[j], newv, line)?;
        }
        if self.vty(newv) != self.slots[s].1 {
            return self.err(line, "代入の型が変数の型と違う");
        }
        self.assign_slot(s, newv);
        Ok(St::Unit)
    }

    fn set_field(&mut self, parent: VId, name: &str, newv: VId, line: usize) -> LR<VId> {
        let pty = self.vty(parent);
        let (_, d) = self.agg_of(parent, line)?;
        let AggKind::Struct(fs) = &d.kind else {
            return self.err(line, format!("{} は構造体でないのでフィールド {} に代入できない", d.name, name));
        };
        let Some(fd) = fs.iter().find(|x| x.name == name) else {
            return self.err(line, format!("フィールド {} がない", name));
        };
        if self.vty(newv) != fd.ty {
            return self.err(line, format!("フィールド {} への代入の型が違う", name));
        }
        Ok(self.emit(pty, Op::SetField { a: parent, off: fd.off, v: newv }))
    }

    /// 試験 `c` が偽なら `fail` へ、真なら新しいブロック (現在のブロックになる) へ進む。
    fn test(&mut self, c: VId, fail: usize) {
        let (cont, _) = self.new_block(&[]);
        self.branch(c, (cont, vec![]), (fail, vec![]));
        self.cur = Some(cont);
    }

    /// 列挙型の変種の試験 (タグの比較)。ペイロードのフィールドを返す。
    fn variant_test(&mut self, segs: &[String], val: VId, ty: Ty, fail: Option<usize>, line: usize) -> LR<Vec<Field>> {
        let Some(Tgt::Variant(en, vn)) = self.p.resolve(segs, &self.def.file).map(|r| r.target) else {
            return self.err(line, format!("パターン {} は段階 1 の対象外", segs.join(".")));
        };
        let Ty::Agg(ai) = ty else {
            return self.err(line, "列挙型のパターンの対象が集成体でない");
        };
        let d = self.agg_def(ai);
        let AggKind::Enum(vs) = &d.kind else {
            return self.err(line, "列挙型のパターンの対象が列挙型でない");
        };
        if d.name.split('<').next() != Some(en.as_str()) {
            return self.err(line, format!("パターン {} の型が対象の型 {} と違う", vn, d.name));
        }
        let Some(idx) = vs.iter().position(|v| v.name == vn) else {
            return self.err(line, format!("列挙子 {} がない", vn));
        };
        let Some(fl) = fail else {
            return self.err(line, "let の束縛は反駁可能でない");
        };
        let tg = self.emit(Ty::I32, Op::Tag(val));
        let k = self.emit(Ty::I32, Op::Const(idx as i128));
        let c = self.emit(Ty::Bool, Op::Cmp { cc: crate::kir::Cc::Eq, ty: Ty::I32, a: tg, b: k });
        self.test(c, fl);
        Ok(vs[idx].fields.clone())
    }

    /// パターン `p` を値 `val` (型 `ty`) に当てる。試験が偽なら `fail` へ飛ぶ (`None` は反駁できない形のみ)。
    /// 返すのは束縛 (名前、値、型) の列。試験は現在のブロックから連鎖で作る。
    pub(super) fn pat(&mut self, p: &V, val: VId, ty: Ty, fail: Option<usize>) -> LR<Vec<(String, VId, Ty)>> {
        let line = nline(p);
        let mut out = vec![];
        match nname(p) {
            "PWild" => {}
            "PBind" => out.push((st(&nf(p)[0]), val, ty)),
            "PLit" => {
                let lit = &nf(p)[0];
                let lf = nf(lit);
                match st(&lf[0]).as_str() {
                    "unit" => {}
                    "bool" => {
                        let Some(fl) = fail else {
                            return self.err(line, "let の束縛は反駁可能でない");
                        };
                        if ty != Ty::Bool {
                            return self.err(line, "bool のパターンの型が違う");
                        }
                        let b = matches!(lf[1], V::B(true)) as i128;
                        let k = self.emit(Ty::Bool, Op::Const(b));
                        let c = self.emit(Ty::Bool, Op::Cmp { cc: crate::kir::Cc::Eq, ty: Ty::Bool, a: val, b: k });
                        self.test(c, fl);
                    }
                    "int" | "byte" => {
                        let Some(fl) = fail else {
                            return self.err(line, "let の束縛は反駁可能でない");
                        };
                        if !ty.is_int() {
                            return self.err(line, "整数のパターンの型が整数でない");
                        }
                        let v = parse_int(&lf[1]).ok_or_else(|| format!("行 {}: 整数パターンが扱えない", line))?;
                        if v < ty.min() || v > ty.max() {
                            return self.err(line, format!("整数パターンが {} の範囲にない", ty.name()));
                        }
                        let k = self.emit(ty, Op::Const(v));
                        let c = self.emit(Ty::Bool, Op::Cmp { cc: crate::kir::Cc::Eq, ty, a: val, b: k });
                        self.test(c, fl);
                    }
                    // 文字リテラル (`'é'`): `char` は u32 のコード。
                    "char" => {
                        let Some(fl) = fail else {
                            return self.err(line, "let の束縛は反駁可能でない");
                        };
                        if ty != Ty::U32 {
                            return self.err(line, "char のパターンの型が違う");
                        }
                        let v = parse_int(&lf[1]).ok_or_else(|| format!("行 {}: 文字パターンが扱えない", line))?;
                        let k = self.emit(Ty::U32, Op::Const(v));
                        let c = self.emit(Ty::Bool, Op::Cmp { cc: crate::kir::Cc::Eq, ty: Ty::U32, a: val, b: k });
                        self.test(c, fl);
                    }
                    // 文字列リテラル: 値 (`&str` の fat) とバイト列が等しいか
                    "str" => {
                        let Some(fl) = fail else {
                            return self.err(line, "let の束縛は反駁可能でない");
                        };
                        if ty != self.fat_ty() {
                            return self.err(line, "文字列のパターンの対象が &str でない");
                        }
                        let k = super::coll::str_lit(self, lit)?;
                        let c = self.str_eq(val, k)?;
                        self.test(c, fl);
                    }
                    other => return self.err(line, format!("リテラルのパターン {} は段階 1 の対象外", other)),
                }
            }
            // `let (a, b) = r` (r は集成体のタプルへの参照): フィールドを借りて束縛する。Vec / String のフィールドは借用のヘッダ、
            // スカラーのフィールドは値の複製 (`Copy`)。
            "PTuple" if matches!(ty, Ty::Ref(_, Pointee::Agg(_))) => {
                let Ty::Ref(m, Pointee::Agg(ai)) = ty else { unreachable!() };
                let d = self.agg_def(ai);
                let AggKind::Tuple(fs) = &d.kind else {
                    return self.err(line, "タプルのパターンの対象がタプルでない");
                };
                let subs = items(&nf(p)[0]);
                if subs.len() != fs.len() {
                    return self.err(line, "タプルのパターンの要素数が型と合わない");
                }
                for (sp, fd) in subs.iter().zip(fs) {
                    match nname(sp) {
                        "PWild" => continue,
                        "PBind" => {}
                        _ => return self.err(line, "参照越しのタプルのパターンは名前と _ だけ段階 1 の対象"),
                    }
                    let name = st(&nf(sp)[0]);
                    let Some(fp) = Pointee::of(fd.ty) else {
                        return self.err(line, "参照越しのタプルのフィールドの型は段階 1 の対象外");
                    };
                    let addr = self.emit(Ty::Ref(m, fp), Op::FieldAddr { a: val, off: fd.off });
                    if let Ty::Agg(fi) = fd.ty {
                        if is_vec_name(&self.agg_def(fi).name) {
                            let hp = self.emit(Ty::Ptr, Op::Bitcast { src: Ty::Ref(m, fp), a: addr });
                            let kind = match self.aggs.borrow().vec_elem(fd.ty) {
                                Some(e) => super::coll::Kind::Vec(Some(e)),
                                None => super::coll::Kind::String,
                            };
                            self.bind(&name, hp, Ty::Ptr);
                            let sl = self.lookup(&name).expect("直前に束縛した名前");
                            self.coll.kinds.insert(sl, kind);
                            self.coll.borrowed.insert(sl);
                            continue;
                        }
                        return self.err(line, "参照越しのタプルの集成体のフィールドは段階 1 の対象外");
                    }
                    let v = self.emit(fd.ty, Op::Load(addr));
                    out.extend(self.pat(sp, v, fd.ty, fail)?);
                }
            }
            "PTuple" => {
                let Ty::Agg(ai) = ty else {
                    return self.err(line, "タプルのパターンの対象が集成体でない");
                };
                let d = self.agg_def(ai);
                let AggKind::Tuple(fs) = &d.kind else {
                    return self.err(line, "タプルのパターンの対象がタプルでない");
                };
                let subs = items(&nf(p)[0]);
                if subs.len() != fs.len() {
                    return self.err(line, "タプルのパターンの要素数が型と合わない");
                }
                for (sp, fd) in subs.iter().zip(fs) {
                    let fv = self.emit(fd.ty, Op::Field { a: val, off: fd.off });
                    out.extend(self.pat(sp, fv, fd.ty, fail)?);
                }
            }
            // `match n { t.Node { val: v, child: c } => .. }` (`n: &Node`): 欄を借りて束縛する (`bind_struct_pat`)。
            "PStruct" if matches!(ty, Ty::Ref(_, Pointee::Agg(_))) => {
                super::own::bind_struct_pat(self, line, p, val, None)?;
            }
            "PStruct" => {
                let Ty::Agg(ai) = ty else {
                    return self.err(line, "構造体のパターンの対象が集成体でない");
                };
                let d = self.agg_def(ai);
                let AggKind::Struct(fs) = &d.kind else {
                    return self.err(line, "構造体のパターンの対象が構造体でない");
                };
                for it in items(&nf(p)[1]) {
                    let V::L(xs) = it else {
                        return self.err(line, "構造体のパターンの形が不正");
                    };
                    let fname = st(&xs[0]);
                    let Some(fd) = fs.iter().find(|x| x.name == fname) else {
                        return self.err(line, format!("フィールド {} がない", fname));
                    };
                    let fv = self.emit(fd.ty, Op::Field { a: val, off: fd.off });
                    out.extend(self.pat(&xs[1], fv, fd.ty, fail)?);
                }
            }
            // `match r { V(a, b) => .. }` (r は列挙型への参照): 名前はペイロードへの参照 (既定の参照の束縛)。
            // 木走査と同じく、スカラーのペイロードは指す先の番地で持つ。Vec / String のペイロードはまだ対象外。
            "PVariant" | "PPath" if matches!(ty, Ty::Ref(_, Pointee::Agg(ai)) if matches!(self.agg_def(ai).kind, AggKind::Enum(_))) => {
                let Ty::Ref(m, Pointee::Agg(ai)) = ty else { unreachable!() };
                let segs = strs(&nf(p)[0]);
                let subs = if nname(p) == "PVariant" { items(&nf(p)[1]).to_vec() } else { vec![] };
                let cur = self.emit(Ty::Agg(ai), Op::Load(val));
                let fields = self.variant_test(&segs, cur, Ty::Agg(ai), fail, line)?;
                if nname(p) == "PVariant" && subs.len() != fields.len() {
                    return self.err(line, "列挙子のパターンの要素数が違う");
                }
                for (sp, fd) in subs.iter().zip(&fields) {
                    match nname(sp) {
                        "PWild" => continue,
                        "PBind" => {}
                        _ => return self.err(line, "参照越しの列挙子のパターンは名前と _ だけ段階 1 の対象"),
                    }
                    let name = st(&nf(sp)[0]);
                    let Some(fp) = Pointee::of(fd.ty) else {
                        return self.err(line, "参照越しの列挙子のペイロードの型は段階 1 の対象外");
                    };
                    // 列挙型のペイロードは `fieldaddr` ではなく番地の加算で指す (`fieldaddr` は構造体・タプルだけ)。
                    let base = self.emit(Ty::Ptr, Op::Bitcast { src: Ty::Ref(m, Pointee::Agg(ai)), a: val });
                    let k = self.emit(Ty::Usize, Op::Const(fd.off as i128));
                    let pa = self.emit(Ty::Ptr, Op::PtrAdd { p: base, off: k });
                    let addr = self.emit(Ty::Ref(m, fp), Op::Bitcast { src: Ty::Ptr, a: pa });
                    out.push((name, addr, Ty::Ref(m, fp)));
                }
            }
            "PVariant" => {
                let segs = strs(&nf(p)[0]);
                let subs = items(&nf(p)[1]);
                let fields = self.variant_test(&segs, val, ty, fail, line)?;
                if subs.len() != fields.len() {
                    return self.err(line, "列挙子のパターンの要素数が違う");
                }
                for (sp, fd) in subs.iter().zip(&fields) {
                    let fv = self.emit(fd.ty, Op::Field { a: val, off: fd.off });
                    out.extend(self.pat(sp, fv, fd.ty, fail)?);
                }
            }
            "PPath" => {
                let segs = strs(&nf(p)[0]);
                self.variant_test(&segs, val, ty, fail, line)?;
            }
            other => return self.err(line, format!("パターン {} は段階 1 の対象外", other)),
        }
        Ok(out)
    }

    /// `Option` / `Result` の中の集成体に当てるパターンの判定 (bool の値)。`pat_test` が集成体の型の部分パターンへ回す。
    /// 束縛は `pat_test` が現在のスコープに入れる。変種の判定はタグの比較、フィールドは判定の前に読む (値は不変なので害はない)。
    pub(super) fn agg_pat_test(&mut self, p: &V, v: VId, t: Ty) -> LR<VId> {
        let line = nline(p);
        let Ty::Agg(ai) = t else {
            return self.err(line, "集成体のパターンの対象が集成体でない");
        };
        // 文字列リテラル (`Some("fn")`): `&str` の fat とバイト列が等しいか
        if nname(p) == "PLit" && st(&nf(&nf(p)[0])[0]) == "str" && t == self.fat_ty() {
            let k = super::coll::str_lit(self, &nf(p)[0])?;
            return self.str_eq(v, k);
        }
        let d = self.agg_def(ai);
        match nname(p) {
            "PPath" | "PVariant" => {
                let segs = strs(&nf(p)[0]);
                let Some(Tgt::Variant(en, vn)) = self.p.resolve(&segs, &self.def.file).map(|r| r.target) else {
                    return self.err(line, format!("パターン {} は段階 1 の対象外", segs.join(".")));
                };
                let AggKind::Enum(vs) = &d.kind else {
                    return self.err(line, format!("列挙子のパターンの対象 {} が列挙型でない", d.name));
                };
                if d.name.split('<').next() != Some(en.as_str()) {
                    return self.err(line, format!("パターン {} の型が対象の型 {} と違う", vn, d.name));
                }
                let Some(idx) = vs.iter().position(|x| x.name == vn) else {
                    return self.err(line, format!("列挙子 {} がない", vn));
                };
                let tg = self.emit(Ty::I32, Op::Tag(v));
                let k = self.emit(Ty::I32, Op::Const(idx as i128));
                let mut c = self.emit(Ty::Bool, Op::Cmp { cc: crate::kir::Cc::Eq, ty: Ty::I32, a: tg, b: k });
                if nname(p) == "PVariant" {
                    let subs = items(&nf(p)[1]).to_vec();
                    let fields = vs[idx].fields.clone();
                    if subs.len() != fields.len() {
                        return self.err(line, "列挙子のパターンの要素数が違う");
                    }
                    for (sp, fd) in subs.iter().zip(&fields) {
                        let fv = self.emit(fd.ty, Op::Field { a: v, off: fd.off });
                        let sc = self.pat_test(sp, fv, fd.ty)?;
                        c = self.emit(Ty::Bool, Op::Bool(BoolOp::And, c, sc));
                    }
                }
                Ok(c)
            }
            "PTuple" => {
                let AggKind::Tuple(fs) = &d.kind else {
                    return self.err(line, "タプルのパターンの対象がタプルでない");
                };
                let subs = items(&nf(p)[0]).to_vec();
                if subs.len() != fs.len() {
                    return self.err(line, "タプルのパターンの要素数が型と合わない");
                }
                let mut c = self.emit(Ty::Bool, Op::Const(1));
                for (sp, fd) in subs.iter().zip(fs) {
                    let fv = self.emit(fd.ty, Op::Field { a: v, off: fd.off });
                    let sc = self.pat_test(sp, fv, fd.ty)?;
                    c = self.emit(Ty::Bool, Op::Bool(BoolOp::And, c, sc));
                }
                Ok(c)
            }
            "PStruct" => {
                let AggKind::Struct(fs) = &d.kind else {
                    return self.err(line, "構造体のパターンの対象が構造体でない");
                };
                let mut c = self.emit(Ty::Bool, Op::Const(1));
                for it in items(&nf(p)[1]) {
                    let V::L(xs) = it else {
                        return self.err(line, "構造体のパターンの形が不正");
                    };
                    let fname = st(&xs[0]);
                    let Some(fd) = fs.iter().find(|x| x.name == fname) else {
                        return self.err(line, format!("フィールド {} がない", fname));
                    };
                    let fv = self.emit(fd.ty, Op::Field { a: v, off: fd.off });
                    let sc = self.pat_test(&xs[1], fv, fd.ty)?;
                    c = self.emit(Ty::Bool, Op::Bool(BoolOp::And, c, sc));
                }
                Ok(c)
            }
            other => self.err(line, format!("パターン {} は集成体の型に対して段階 1 の対象外", other)),
        }
    }

    /// `match e { p => body ... }`。腕は順に試し、最初に当たった腕の値が結果になる。
    /// 代入された変数は `if` と同じく合流のブロック引数で渡す。
    pub(super) fn match_e(&mut self, e: &V) -> LR<Option<VId>> {
        let f = nf(e);
        let line = nline(e);
        let Some(sv) = self.expr_peek(&f[0])? else { return Ok(None) };
        let sty = self.vty(sv);
        let res: Option<Ty> = match self.src_ty(e)? {
            Src::Never => None,
            t => Some(self.conv_t(&t).ok_or_else(|| format!("行 {}: match の型 {} は段階 1 の対象外", line, show(&t)))?),
        };
        let carry = matches!(res, Some(t) if t != Ty::Unit);
        let arms: Vec<(&V, &V)> = items(&f[1])
            .iter()
            .map(|a| match a {
                V::L(xs) if xs.len() == 2 => Ok((&xs[0], &xs[1])),
                _ => Err(format!("行 {}: match の腕の形が不正", line)),
            })
            .collect::<Result<_, _>>()?;
        let bodies: Vec<&V> = arms.iter().map(|(_, b)| *b).collect();
        let pats: Vec<&V> = arms.iter().map(|(p, _)| *p).collect();
        let scrut_src = self.src_ty(&f[0]).ok();
        let sc = super::coll::scrut_begin(self, &f[0], sv, &pats)?;
        let slots = self.assigned(&bodies);
        let slot_tys: Vec<Ty> = slots.iter().map(|s| self.slots[*s].1).collect();
        let mut params: Vec<Ty> = if carry { res.into_iter().collect() } else { vec![] };
        params.extend(slot_tys.iter().copied());
        let (jb, jp) = self.new_block(&params);
        let saved = self.slots.clone();
        for (i, (pat, body)) in arms.into_iter().enumerate() {
            let next = self.new_block(&[]).0;
            self.slots = saved.clone();
            // 腕の束縛の入れ物は、判定の前に作る (参照越しの構造体パターンは `pat` の中でこの腕の名前を直接束縛する)。
            self.scopes.push(vec![]);
            let binds = self.pat(pat, sv, sty, Some(next))?;
            let mut srcs = std::collections::BTreeMap::new();
            if let Some(ss) = &scrut_src {
                super::coll::pat_srcs(self, pat, ss, &mut srcs);
            }
            for (n, v, t) in binds {
                self.coll.hint = srcs.get(&n).cloned();
                super::coll::bind_owned(self, &n, v, t);
                self.coll.hint = None;
            }
            super::coll::scrut_arm(self, &sc, i, pat, sv)?;
            if let Some(v) = self.expr(body)? {
                if let Some(t) = res {
                    if self.vty(v) != t && carry {
                        return self.err(line, "match の腕の型が結果の型と違う");
                    }
                }
                super::coll::block_end(self);
                let mut args: Vec<VId> = if carry { vec![v] } else { vec![] };
                args.extend(self.vals_of(&slots));
                self.goto(jb, args);
            } else {
                super::coll::block_end(self);
            }
            self.scopes.pop();
            self.cur = Some(next);
        }
        // 網羅されていれば、最後の試験は到達しない。
        self.terminate(Term::Unreachable);
        self.slots = saved;
        if self.blocks[jb].preds == 0 {
            return Ok(None);
        }
        self.cur = Some(jb);
        let off = carry as usize;
        for (i, s) in slots.iter().enumerate() {
            self.rebind_slot(*s, jp[off + i], slot_tys[i]);
        }
        Ok(Some(if carry { jp[0] } else { self.unit() }))
    }
}

#[cfg(test)]
mod tests {
    use crate::kir::AggKind;

    /// 配置の照合用の定義。C の配置規則 (旧 clif と同じ) と KIR の型表の大きさ・整列・オフセットが一致することを確かめる。
    /// 入れ子の構造体、整列の違うフィールド、タグだけの列挙型、バリアントごとに違う大きさを含む。
    const SRC: &str = "type t.A
    a: u8
    b: u64
    c: bool
end t.A

type t.B
    x: i16
    inner: t.A
    y: i32
end t.B

type t.C
    p: t.B
    q: bool
end t.C

enum t.E
    One(u8, u32)
    Two(i64)
    Three(bool, i16, u8)
    Zero
end t.E

fn t.keep(_a: t.A, _b: t.B, _c: t.C, _e: t.E, _p: (u8, i64), _q: (bool, i16, u8, i32)) -> i32 uses [] panics never
    0i32
end t.keep

fn t.main() -> Result<(), std.io.Error> uses [] panics never
    Ok(())
end t.main
";

    #[test]
    fn kir_agg_layout_matches_c() {
        let root = std::env::temp_dir().join(format!("ksn_kir_agg_layout_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("kasane.toml"), "[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = 1\nentry = \"t.main\"\n\n[dependencies]\n").unwrap();
        std::fs::write(root.join("kasane.lock"), "version = 1\n").unwrap();
        std::fs::write(root.join("src").join("t.ksnl"), SRC).unwrap();
        let checked = crate::cli::checked_program_for_test(&root);
        let _ = std::fs::remove_dir_all(&root);
        let (prog, user) = checked.expect("試験の入力は検査を通るはず");
        let lo = crate::kir_lower::lower_user(&prog, &user);
        assert!(lo.module.find("t.keep").is_some(), "{:?}", lo.unsupported);
        let names: Vec<&str> = lo.module.aggs.iter().map(|d| d.name.as_str()).collect();
        for want in ["t.A", "t.B", "t.C", "t.E", "(u8,i64)", "(bool,i16,u8,i32)"] {
            assert!(names.contains(&want), "型表に {} がない: {:?}", want, names);
        }
        // `fat` / `vec` は、利用者の型ではなくスライス・`Vec` のヘッダ (他の module の `fn` (`std.json` など) を下ろすときに入る)。`(fat,fat)` は `std.path` の `split_at` の結果。
        for d in lo.module.aggs.iter().filter(|d| !super::STD_TYPES.contains(&d.name.as_str()) && d.name != "fat" && d.name != "split_it" && d.name != "bref" && !super::is_vec_name(&d.name) && d.name != "(fat,fat)") {
            // 期待値は、削除した clif.rs の `layout` / `enum_layout` が出していた配置 (C と同じ規則) を固定したもの。
            let (size, align, offs, vars): (u32, u32, Vec<u32>, Vec<Vec<u32>>) = match d.name.as_str() {
                "t.A" => (24, 8, vec![0, 8, 16], vec![]),
                "t.B" => (40, 8, vec![0, 8, 32], vec![]),
                "t.C" => (48, 8, vec![0, 40], vec![]),
                "t.E" => (16, 8, vec![], vec![vec![8, 12], vec![8], vec![8, 10, 12], vec![]]),
                "(u8,i64)" => (16, 8, vec![0, 8], vec![]),
                "(bool,i16,u8,i32)" => (12, 4, vec![0, 2, 4, 8], vec![]),
                // 列挙子の番地の借用 (`Option<&T>` の `Some(E::V(..))`) が入れる 2 つのタプル。ヘッダ `vec` は 24 バイト・整列 8。
                "(usize,usize)" => (16, 8, vec![0, 8], vec![]),
                "(vec,usize)" => (32, 8, vec![0, 24], vec![]),
                // `std.http.take_request` の結果 (`Request` は 104 バイト・整列 8、続けて `usize`)。
                "(std.http.Request,usize)" => (112, 8, vec![0, 104], vec![]),

                // `Option<&str>` (枠 128 ビット + タグ = 17 バイトを 8 の倍数に広げて 24 バイト) を持つ組。`fat` は 16 バイト。
                n if n.starts_with("(fat,Option<") => (40, 8, vec![0, 16], vec![]),
                other => panic!("期待値のない型: {}", other),
            };
            assert_eq!((d.size, d.align), (size, align), "大きさ・整列: {}", d.name);
            match &d.kind {
                AggKind::Struct(fs) | AggKind::Tuple(fs) => {
                    let mine: Vec<u32> = fs.iter().map(|f| f.off).collect();
                    assert_eq!(mine, offs, "フィールドのオフセット: {}", d.name);
                }
                AggKind::Enum(vs) => {
                    let mine: Vec<Vec<u32>> = vs.iter().map(|v| v.fields.iter().map(|f| f.off).collect()).collect();
                    assert_eq!(mine, vars, "バリアントのオフセット: {}", d.name);
                }
            }
        }
    }
}
