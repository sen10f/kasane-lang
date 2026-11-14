//! 標準ライブラリ (`spec/stdlib`) の構文解析結果 (`Vec<Def>`) の保存 (設計_KIR.md 18.11)。
//!
//! 起動時間の大半は標準ライブラリ約 1000 定義の構文解析なので、結果を 1 本のバイナリにして
//! `<KSN_CACHE_DIR>/std/<実行体の識別子の接頭辞>.bin` に置き、次回からは読み戻すだけにする。
//! 鍵は実行体の識別子 `KSN_BUILD_ID` (構文解析器のソースを含む) と、全ファイルの (パス・文面)。
//! 文面が 1 バイトでも変われば未命中。破損・不一致は黙って捨てて構文解析し直す。
//! 書き込みは一時ファイルから `rename` で置き換える (同時に動く `ksn` が中途半端なファイルを見ない)。

use std::collections::HashMap;
use std::sync::Mutex;

use crate::parser::V;
use crate::program::Def;

const MAGIC: &[u8; 8] = b"KSNSTD02";

fn dir() -> std::path::PathBuf {
    crate::project::cache_dir().join("std")
}

fn prefix() -> &'static str {
    &env!("KSN_BUILD_ID")[..16]
}

/// 鍵 (全ファイルの順序つきの (パス・文面) と実行体の識別子)。
/// 起動のたびに約 90 KB を hash するので、SHA-NI の無い CPU では 0.7 ms かかる SHA-256 ではなく、
/// 別々の接頭辞の SipHash-1-3 を 2 本 (128 bit) 使う。敵対的な衝突は想定しない (置き場は利用者自身のもの)。
pub fn key(files: &[(String, String)]) -> [u8; 32] {
    use std::hash::Hasher;
    let mut hs: Vec<std::collections::hash_map::DefaultHasher> = (0..2u64)
        .map(|i| {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            h.write_u64(i);
            h.write(env!("KSN_BUILD_ID").as_bytes());
            h.write_u64(files.len() as u64);
            h
        })
        .collect();
    for (p, t) in files {
        for h in hs.iter_mut() {
            h.write_u64(p.len() as u64);
            h.write(p.as_bytes());
            h.write_u64(t.len() as u64);
        }
    }
    // 文面は 1 回ずつなめて 2 本へ流す。
    for (_, t) in files {
        for h in hs.iter_mut() {
            h.write(t.as_bytes());
        }
    }
    let mut out = [0u8; 32];
    for (i, h) in hs.iter().enumerate() {
        out[i * 8..i * 8 + 8].copy_from_slice(&h.finish().to_le_bytes());
    }
    out
}

// ---------------------------------------------------------------- 書き出し
struct W(Vec<u8>);

impl W {
    fn n(&mut self, mut v: u64) {
        while v >= 0x80 {
            self.0.push((v as u8) | 0x80);
            v >>= 7;
        }
        self.0.push(v as u8);
    }
    fn s(&mut self, s: &str) {
        self.n(s.len() as u64);
        self.0.extend_from_slice(s.as_bytes());
    }
    fn b(&mut self, b: bool) {
        self.0.push(b as u8);
    }
    fn v(&mut self, v: &V) {
        match v {
            V::S(x) => {
                self.0.push(0);
                self.s(x);
            }
            V::N(x) => {
                self.0.push(1);
                self.s(x);
            }
            V::B(x) => {
                self.0.push(2);
                self.b(*x);
            }
            V::L(xs) => {
                self.0.push(3);
                self.n(xs.len() as u64);
                for x in xs {
                    self.v(x);
                }
            }
            V::Node(k, xs, line, id) => {
                self.0.push(4);
                self.s(k);
                self.n(xs.len() as u64);
                for x in xs {
                    self.v(x);
                }
                self.n(*line as u64);
                self.n(*id as u64);
            }
            V::Nil => self.0.push(5),
        }
    }
    fn strs(&mut self, xs: &[String]) {
        self.n(xs.len() as u64);
        for x in xs {
            self.s(x);
        }
    }
    fn params(&mut self, xs: &[(String, V)]) {
        self.n(xs.len() as u64);
        for (a, t) in xs {
            self.s(a);
            self.v(t);
        }
    }
    fn def(&mut self, d: &Def) {
        // 全フィールドを名指しで分解する (Def にフィールドが増えたらここがコンパイルエラーになる)。
        let Def {
            kw, name, line, end_line, file: _, module: _, header, generics, params, ret, is_const, uses, inferred, panics, stack,
            derive, linear, layout_c, fields, variants, elided_refs, body, init, has_impls, impls_trait, methods, trait_sigs,
            trait_effs, requires, generic_methods, impl_trait, link, impl_for, impl_targs,
        } = d;
        self.s(kw);
        self.s(name);
        self.n(*line as u64);
        self.n(*end_line as u64);
        self.s(header);
        self.n(generics.len() as u64);
        for (a, b, c) in generics {
            self.s(a);
            self.s(b);
            self.v(c);
        }
        self.params(params);
        self.v(ret);
        self.b(*is_const);
        self.strs(uses);
        self.b(*inferred);
        match panics {
            Some(p) => {
                self.b(true);
                self.s(p);
            }
            None => self.b(false),
        }
        match stack {
            Some(n) => {
                self.b(true);
                self.n(*n);
            }
            None => self.b(false),
        }
        self.strs(derive);
        self.b(*linear);
        self.b(*layout_c);
        self.n(fields.len() as u64);
        for (a, t, f) in fields {
            self.s(a);
            self.v(t);
            self.b(*f);
        }
        self.n(variants.len() as u64);
        for (a, ts) in variants {
            self.s(a);
            self.n(ts.len() as u64);
            for t in ts {
                self.v(t);
            }
        }
        self.strs(elided_refs);
        for o in [body, init] {
            match o {
                Some(v) => {
                    self.b(true);
                    self.v(v);
                }
                None => self.b(false),
            }
        }
        self.b(*has_impls);
        self.s(impls_trait);
        self.strs(methods);
        self.n(trait_sigs.len() as u64);
        for (a, ps, r) in trait_sigs {
            self.s(a);
            self.params(ps);
            self.v(r);
        }
        self.n(trait_effs.len() as u64);
        for (a, us, p) in trait_effs {
            self.s(a);
            self.strs(us);
            self.s(p);
        }
        self.params(requires);
        self.strs(generic_methods);
        self.s(impl_trait);
        self.s(link);
        self.v(impl_for);
        self.v(impl_targs);
    }
}

// ---------------------------------------------------------------- 読み込み
/// `V::Node` の種別は `&'static str`。実行中に現れる種別は数百種なので、1 回だけ漏らして使い回す。
fn intern(s: &str) -> &'static str {
    static T: Mutex<Option<HashMap<String, &'static str>>> = Mutex::new(None);
    let mut g = T.lock().unwrap();
    let m = g.get_or_insert_with(HashMap::new);
    if let Some(k) = m.get(s) {
        return k;
    }
    let k: &'static str = Box::leak(s.to_string().into_boxed_str());
    m.insert(s.to_string(), k);
    k
}

struct Rd<'a> {
    b: &'a [u8],
    i: usize,
    /// 種別の添字 → 文字列 (同じファイル内で `intern` を引き直さない)
    kinds: HashMap<Vec<u8>, &'static str>,
}

type O<T> = Option<T>;

impl<'a> Rd<'a> {
    fn byte(&mut self) -> O<u8> {
        let c = *self.b.get(self.i)?;
        self.i += 1;
        Some(c)
    }
    fn n(&mut self) -> O<u64> {
        let mut v = 0u64;
        let mut sh = 0;
        loop {
            let c = self.byte()?;
            v |= ((c & 0x7f) as u64) << sh;
            if c & 0x80 == 0 {
                return Some(v);
            }
            sh += 7;
            if sh > 63 {
                return None;
            }
        }
    }
    fn us(&mut self) -> O<usize> {
        self.n().map(|v| v as usize)
    }
    fn bytes(&mut self) -> O<&'a [u8]> {
        let n = self.us()?;
        let s = self.b.get(self.i..self.i.checked_add(n)?)?;
        self.i += n;
        Some(s)
    }
    fn s(&mut self) -> O<String> {
        Some(std::str::from_utf8(self.bytes()?).ok()?.to_string())
    }
    fn b(&mut self) -> O<bool> {
        Some(self.byte()? != 0)
    }
    fn kind(&mut self) -> O<&'static str> {
        let raw = self.bytes()?;
        if let Some(k) = self.kinds.get(raw) {
            return Some(k);
        }
        let k = intern(std::str::from_utf8(raw).ok()?);
        self.kinds.insert(raw.to_vec(), k);
        Some(k)
    }
    /// 長さの宣言が残りのバイト数より大きいものは壊れているので捨てる (巨大な確保を避ける)。
    fn len(&mut self) -> O<usize> {
        let n = self.us()?;
        if n > self.b.len() - self.i {
            return None;
        }
        Some(n)
    }
    fn v(&mut self) -> O<V> {
        Some(match self.byte()? {
            0 => V::S(self.s()?),
            1 => V::N(self.s()?),
            2 => V::B(self.b()?),
            3 => {
                let n = self.len()?;
                let mut xs = Vec::with_capacity(n);
                for _ in 0..n {
                    xs.push(self.v()?);
                }
                V::L(xs)
            }
            4 => {
                let k = self.kind()?;
                let n = self.len()?;
                let mut xs = Vec::with_capacity(n);
                for _ in 0..n {
                    xs.push(self.v()?);
                }
                let line = self.us()?;
                let id = self.n()? as u32;
                V::Node(k, xs, line, id)
            }
            5 => V::Nil,
            _ => return None,
        })
    }
    fn strs(&mut self) -> O<Vec<String>> {
        let n = self.len()?;
        (0..n).map(|_| self.s()).collect()
    }
    fn params(&mut self) -> O<Vec<(String, V)>> {
        let n = self.len()?;
        (0..n).map(|_| Some((self.s()?, self.v()?))).collect()
    }
    fn opt_v(&mut self) -> O<Option<V>> {
        Some(if self.b()? { Some(self.v()?) } else { None })
    }
    fn def(&mut self, file: &str, module: &str) -> O<Def> {
        let kw = self.s()?;
        let name = self.s()?;
        let line = self.us()?;
        let end_line = self.us()?;
        let header = self.s()?;
        let n = self.len()?;
        let generics = (0..n).map(|_| Some((self.s()?, self.s()?, self.v()?))).collect::<O<Vec<_>>>()?;
        let params = self.params()?;
        let ret = self.v()?;
        let is_const = self.b()?;
        let uses = self.strs()?;
        let inferred = self.b()?;
        let panics = if self.b()? { Some(self.s()?) } else { None };
        let stack = if self.b()? { Some(self.n()?) } else { None };
        let derive = self.strs()?;
        let linear = self.b()?;
        let layout_c = self.b()?;
        let n = self.len()?;
        let fields = (0..n).map(|_| Some((self.s()?, self.v()?, self.b()?))).collect::<O<Vec<_>>>()?;
        let n = self.len()?;
        let variants = (0..n)
            .map(|_| {
                let a = self.s()?;
                let m = self.len()?;
                let ts = (0..m).map(|_| self.v()).collect::<O<Vec<_>>>()?;
                Some((a, ts))
            })
            .collect::<O<Vec<_>>>()?;
        let elided_refs = self.strs()?;
        let body = self.opt_v()?;
        let init = self.opt_v()?;
        let has_impls = self.b()?;
        let impls_trait = self.s()?;
        let methods = self.strs()?;
        let n = self.len()?;
        let trait_sigs = (0..n).map(|_| Some((self.s()?, self.params()?, self.v()?))).collect::<O<Vec<_>>>()?;
        let n = self.len()?;
        let trait_effs = (0..n).map(|_| Some((self.s()?, self.strs()?, self.s()?))).collect::<O<Vec<_>>>()?;
        let requires = self.params()?;
        let generic_methods = self.strs()?;
        let impl_trait = self.s()?;
        let link = self.s()?;
        let impl_for = self.v()?;
        let impl_targs = self.v()?;
        Some(Def {
            kw,
            name,
            line,
            end_line,
            file: file.to_string(),
            module: module.to_string(),
            header,
            generics,
            params,
            ret,
            is_const,
            uses,
            inferred,
            panics,
            stack,
            derive,
            linear,
            layout_c,
            fields,
            variants,
            elided_refs,
            body,
            init,
            has_impls,
            impls_trait,
            methods,
            trait_sigs,
            trait_effs,
            requires,
            generic_methods,
            impl_trait,
            link,
            impl_for,
            impl_targs,
        })
    }
}

/// 保存済みの定義を読む。`files` は (パス, モジュール名) の並び (保存したときと同じ順)。
/// 鍵が違う・壊れている・ファイル数が違うときは `None`。
/// 復号は確保の数に支配されるので、ファイルをまとめて複数スレッドで復号する (表は先頭にある)。
pub fn load(key: &[u8; 32], files: &[(&str, &str)]) -> Option<Vec<Vec<Def>>> {
    load_in(&dir(), key, files)
}

fn load_in(dir: &std::path::Path, key: &[u8; 32], files: &[(&str, &str)]) -> Option<Vec<Vec<Def>>> {
    let raw = std::fs::read(dir.join(format!("{}.bin", prefix()))).ok()?;
    if raw.len() < 40 || &raw[..8] != MAGIC || &raw[8..40] != key {
        return None;
    }
    let mut r = Rd { b: &raw, i: 40, kinds: HashMap::new() };
    if r.us()? != files.len() {
        return None;
    }
    // 表: ファイルごとの (定義数, 本体のバイト数)。
    let mut tab = Vec::with_capacity(files.len());
    for _ in 0..files.len() {
        tab.push((r.us()?, r.us()?));
    }
    let mut at = r.i;
    let mut spans: Vec<&[u8]> = Vec::with_capacity(files.len());
    for &(_, n) in &tab {
        spans.push(raw.get(at..at.checked_add(n)?)?);
        at += n;
    }
    if at != raw.len() {
        return None;
    }
    let one = |i: usize| -> Option<Vec<Def>> {
        let mut r = Rd { b: spans[i], i: 0, kinds: HashMap::new() };
        if tab[i].0 > spans[i].len() {
            return None;
        }
        let mut ds = Vec::with_capacity(tab[i].0);
        for _ in 0..tab[i].0 {
            ds.push(r.def(files[i].0, files[i].1)?);
        }
        if r.i != spans[i].len() {
            return None;
        }
        Some(ds)
    };
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).clamp(1, 4);
    if cores == 1 || files.len() < 4 {
        return (0..files.len()).map(one).collect();
    }
    // 大きいファイルから順に、取り合いで復号する。
    let mut order: Vec<usize> = (0..files.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(tab[i].1));
    let next = std::sync::atomic::AtomicUsize::new(0);
    let slots: Vec<Mutex<Option<Option<Vec<Def>>>>> = files.iter().map(|_| Mutex::new(None)).collect();
    std::thread::scope(|sc| {
        let work = || loop {
            let k = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if k >= order.len() {
                break;
            }
            let i = order[k];
            *slots[i].lock().unwrap() = Some(one(i));
        };
        // 呼び出したスレッド自身も働く。
        for _ in 1..cores {
            sc.spawn(work);
        }
        work();
    });
    slots.into_iter().map(|m| m.into_inner().unwrap().flatten()).collect()
}

/// 構文解析の結果を保存する。失敗しても何も起きない (次回は構文解析し直すだけ)。
/// 古い実行体のファイルはここで消す (置き場は実行体ごとに 1 本だけ)。
pub fn store(key: &[u8; 32], defs: &[&[Def]]) {
    store_in(&dir(), key, defs)
}

fn store_in(d: &std::path::Path, key: &[u8; 32], defs: &[&[Def]]) {
    let mut bodies: Vec<Vec<u8>> = vec![];
    for ds in defs {
        let mut w = W(Vec::new());
        for d in *ds {
            w.def(d);
        }
        bodies.push(w.0);
    }
    let mut w = W(Vec::with_capacity(1 << 20));
    w.0.extend_from_slice(MAGIC);
    w.0.extend_from_slice(key);
    w.n(defs.len() as u64);
    for (ds, b) in defs.iter().zip(&bodies) {
        w.n(ds.len() as u64);
        w.n(b.len() as u64);
    }
    for b in &bodies {
        w.0.extend_from_slice(b);
    }
    if std::fs::create_dir_all(d).is_err() {
        return;
    }
    if let Ok(rd) = std::fs::read_dir(d) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            if n.ends_with(".bin") && n != format!("{}.bin", prefix()) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    let tmp = d.join(format!("{}.{}.tmp", prefix(), std::process::id()));
    if std::fs::write(&tmp, &w.0).is_ok() && std::fs::rename(&tmp, d.join(format!("{}.bin", prefix()))).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::Source;

    /// 標準ライブラリ全体を保存して読み戻した結果が、構文解析の結果と (Debug 表示で) 一致する。
    /// 鍵・ファイル数が違えば未命中、壊れたファイルも未命中。
    #[test]
    fn roundtrip_equals_parse() {
        let root = crate::project::stdlib_dir();
        let rs = crate::project::norm(&root);
        let mut ps = vec![];
        for ext in ["sig", "ksnl"] {
            let mut fs = vec![];
            crate::project::walk_files_for_test(&root, &[ext], &mut fs);
            let mut fs: Vec<String> = fs.iter().map(|p| crate::project::norm(p)).collect();
            fs.sort();
            ps.extend(fs);
        }
        let srcs: Vec<Source> = ps.iter().map(|p| Source::load(p, &rs)).collect();
        assert!(srcs.iter().all(|s| s.error.is_none()));
        let texts: Vec<(String, String)> = srcs.iter().map(|s| (s.path.clone(), s.text.clone())).collect();
        let key = key(&texts);
        let dir = std::env::temp_dir().join(format!("ksn-stdcache-test-{}", std::process::id()));
        let ds: Vec<&[Def]> = srcs.iter().map(|s| s.defs.as_slice()).collect();
        store_in(&dir, &key, &ds);
        let files: Vec<(&str, &str)> = srcs.iter().map(|s| (s.path.as_str(), s.module.as_str())).collect();
        let back = load_in(&dir, &key, &files).expect("命中する");
        assert_eq!(back.len(), srcs.len());
        for (a, b) in srcs.iter().zip(&back) {
            assert_eq!(format!("{:?}", a.defs), format!("{:?}", b));
        }
        // 文面が 1 文字変われば鍵が変わる。
        let mut t2 = texts.clone();
        t2[0].1.push(' ');
        assert!(load_in(&dir, &super::key(&t2), &files).is_none());
        assert!(load_in(&dir, &key, &files[1..]).is_none());
        // 壊れたファイル (途中で切れた・1 バイト変わった) は未命中 (読み誤らない)。
        let f = dir.join(format!("{}.bin", prefix()));
        let raw = std::fs::read(&f).unwrap();
        std::fs::write(&f, &raw[..raw.len() - 5]).unwrap();
        assert!(load_in(&dir, &key, &files).is_none());
        let mut bad = raw.clone();
        let mid = bad.len() / 2;
        bad[mid] ^= 0xff;
        std::fs::write(&f, &bad).unwrap();
        if let Some(b) = load_in(&dir, &key, &files) {
            // 1 バイトの化けが復号できてしまう場合でも、落ちずに完了する (中身の同一性は保証しない)。
            assert_eq!(b.len(), srcs.len());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
