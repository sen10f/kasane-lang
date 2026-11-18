//! `ksn bindgen <定義ファイル>` (修正プラン 5-3 段 2): ブラウザの DOM などの小さな部分集合を、Kasane から呼ぶ形にする。
//!
//! 定義ファイルは 1 行に 1 つの指定 (`#` で始まる行と空行は無視)。パスはカレントディレクトリ (パッケージ直下) からの相対:
//! ```text
//! module todo_spa.dom          # 生成する Kasane のモジュール (ファイルは src/ 以下の対応するパス)
//! ksnl src/todo_spa/dom.ksnl   # 生成する Kasane のファイル
//! js web/dom.mjs               # 生成する JS の接着コード
//! prefix ksn_dom_              # import の名前の接頭辞 (省略時は ksn_<モジュールの末尾>_)
//! op create                    # 組み込みの操作 (下の表)。Kasane の関数名も同じ
//! ```
//!
//! 生成物:
//! - Kasane: 各 `op` について、JS の import を呼ぶ `extern <module>.raw_<op>(...) link "env:<prefix><op>"` と、
//!   `unsafe` の中で呼ぶ安全な関数 `<module>.<op>(...)` (`pub`)。文字列は `(ptr, len)` で渡る (`&str`)、出力の領域は `(ptr, cap)` (`&mut [u8]`)。
//!   生成したモジュールは `kasane.toml` の `[unsafe] modules` に入れる。
//! - JS: `makeImports(host)` が import の表を返す。`host.instance` に `instantiate` の結果を入れてから使う。
//!   JS から Kasane へは `ksn_event(tag)` (`listen` の発火) と `ksn_fetch_done(tag, status)` (`fetch` の完了。status 0 は通信の失敗) を呼ぶ。
//!   これらは Kasane 側で `ksn_` の関数として export する (WASM ABI)。

use std::fs;
use std::path::Path;

/// 引数の型 (Kasane の側の表し方と、wasm の引数への割り当て)。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum K {
    /// `u32`: そのまま。
    U32,
    /// `&str`: `(ptr: *const u8, len: usize)`。UTF-8 のバイト列を JS で読む。
    Str,
    /// `&mut [u8]`: `(ptr: *mut u8, cap: usize)`。JS が線形メモリへ書き、書いた長さを返す (領域が足りなければ何も書かず長さだけ返す)。
    Buf,
}

/// 戻り値の型。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum R {
    U32,
    I32,
    Unit,
}

/// 組み込みの操作: 名前、引数、戻り値、JS の実装 (引数は引数の表し方に従って並ぶ。`Str` は文字列、`Buf` は (ptr, cap))。
struct Op {
    name: &'static str,
    /// 引数の名前 (Kasane の wrapper の引数名)。`params` と同じ長さ。
    names: &'static [&'static str],
    params: &'static [K],
    ret: R,
    js: &'static str,
}

use K::{Buf, Str, U32};

/// 組み込みの操作の表 (DOM・localStorage・fetch の小さな部分集合)。
static OPS: &[Op] = &[
    Op { name: "body", names: &[], params: &[], ret: R::U32, js: "() => push(document.body)" },
    Op { name: "by_id", names: &["id"], params: &[Str], ret: R::U32, js: "(id) => push(document.getElementById(id))" },
    Op { name: "create", names: &["tag"], params: &[Str], ret: R::U32, js: "(tag) => push(document.createElement(tag))" },
    Op { name: "create_text", names: &["text"], params: &[Str], ret: R::U32, js: "(text) => push(document.createTextNode(text))" },
    Op { name: "set_attr", names: &["node", "name", "value"], params: &[U32, Str, Str], ret: R::Unit, js: "(node, name, value) => { N(node).setAttribute(name, value); }" },
    Op { name: "set_value", names: &["node", "text"], params: &[U32, Str], ret: R::Unit, js: "(node, text) => { N(node).value = text; }" },
    Op { name: "set_text", names: &["node", "text"], params: &[U32, Str], ret: R::Unit, js: "(node, text) => { N(node).textContent = text; }" },
    Op { name: "append", names: &["parent", "child"], params: &[U32, U32], ret: R::Unit, js: "(parent, child) => { N(parent).appendChild(N(child)); }" },
    Op { name: "remove", names: &["node"], params: &[U32], ret: R::Unit, js: "(node) => { N(node).remove(); }" },
    Op { name: "clear", names: &["node"], params: &[U32], ret: R::Unit, js: "(node) => { N(node).replaceChildren(); }" },
    Op { name: "listen", names: &["node", "event", "tag"], params: &[U32, Str, U32], ret: R::Unit, js: "(node, event, tag) => { N(node).addEventListener(event, () => ex().ksn_event(tag)); }" },
    Op { name: "input_value", names: &["node", "buf"], params: &[U32, Buf], ret: R::I32, js: "(node, p, cap) => put(p, cap, enc.encode(N(node).value ?? \"\"))" },
    Op {
        name: "storage_get",
        names: &["key", "buf"],
        params: &[Str, Buf],
        ret: R::I32,
        js: "(key, p, cap) => { const v = localStorage.getItem(key); return v === null ? -1 : put(p, cap, enc.encode(v)); }",
    },
    Op { name: "storage_set", names: &["key", "value"], params: &[Str, Str], ret: R::Unit, js: "(key, value) => { localStorage.setItem(key, value); }" },
    Op {
        name: "fetch",
        names: &["method", "url", "body", "tag"],
        params: &[Str, Str, Str, U32],
        ret: R::Unit,
        js: "(method, url, body, tag) => { const init = { method }; if (body !== \"\") { init.body = body; init.headers = { \"content-type\": \"application/json\" }; } fetch(url, init).then(async (r) => { fetched = enc.encode(await r.text()); ex().ksn_fetch_done(tag, r.status); }, () => { fetched = new Uint8Array(0); ex().ksn_fetch_done(tag, 0); }); }",
    },
    Op { name: "fetch_body", names: &["buf"], params: &[Buf], ret: R::I32, js: "(p, cap) => put(p, cap, fetched)" },
];

/// 定義ファイルを読んだ結果。
struct Def {
    module: String,
    ksnl: String,
    js: String,
    prefix: String,
    ops: Vec<&'static Op>,
}

fn is_ident(s: &str) -> bool {
    let mut cs = s.chars();
    matches!(cs.next(), Some('a'..='z')) && cs.all(|c| matches!(c, 'a'..='z' | '0'..='9' | '_'))
}

fn is_module(s: &str) -> bool {
    !s.is_empty() && s.split('.').all(is_ident)
}

/// 定義ファイルの文字列を読む。行ごとに `行番号: 理由` で失敗を返す。
fn parse(src: &str) -> Result<Def, String> {
    let mut module = None;
    let mut ksnl = None;
    let mut js = None;
    let mut prefix = None;
    let mut ops: Vec<&'static Op> = Vec::new();
    for (i, raw) in src.lines().enumerate() {
        let n = i + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, val) = line.split_once(char::is_whitespace).map(|(k, v)| (k, v.trim())).unwrap_or((line, ""));
        let bad = |why: &str| format!("{}: {}", n, why);
        match key {
            "module" => {
                if !is_module(val) {
                    return Err(bad("module は `a.b_c` の形 (成分は小文字で始まる識別子)"));
                }
                if module.replace(val.to_string()).is_some() {
                    return Err(bad("module が二度ある"));
                }
            }
            "ksnl" | "js" if val.is_empty() => return Err(bad(&format!("{} にパスがない", key))),
            "ksnl" => ksnl = Some(val.to_string()),
            "js" => js = Some(val.to_string()),
            "prefix" => {
                if !val.ends_with('_') || !val.chars().all(|c| matches!(c, 'a'..='z' | '0'..='9' | '_')) {
                    return Err(bad("prefix は小文字・数字・_ で、_ で終わる"));
                }
                prefix = Some(val.to_string());
            }
            "op" => {
                let Some(op) = OPS.iter().find(|o| o.name == val) else {
                    let names: Vec<&str> = OPS.iter().map(|o| o.name).collect();
                    return Err(bad(&format!("未知の操作 `{}` (使えるもの: {})", val, names.join(", "))));
                };
                if ops.iter().any(|o| o.name == op.name) {
                    return Err(bad(&format!("操作 `{}` が二度ある", val)));
                }
                ops.push(op);
            }
            _ => return Err(bad(&format!("未知の指定 `{}`", key))),
        }
    }
    let module = module.ok_or("module が要る")?;
    let ksnl = ksnl.ok_or("ksnl が要る")?;
    let js = js.ok_or("js が要る")?;
    if ops.is_empty() {
        return Err("op が一つもない".into());
    }
    let prefix = prefix.unwrap_or_else(|| format!("ksn_{}_", module.rsplit('.').next().unwrap_or(&module)));
    ops.sort_by_key(|o| o.name);
    Ok(Def { module, ksnl, js, prefix, ops })
}

/// Kasane の `extern` の引数の並び (`raw_` の側): 各引数が wasm の引数になる。
fn raw_params(o: &Op) -> Vec<String> {
    let mut out = Vec::new();
    for (i, k) in o.params.iter().enumerate() {
        let n = o.names[i];
        match k {
            K::U32 => out.push(format!("{}: u32", n)),
            K::Str => {
                out.push(format!("{}_ptr: *const u8", n));
                out.push(format!("{}_len: usize", n));
            }
            K::Buf => {
                out.push(format!("{}_ptr: *mut u8", n));
                out.push(format!("{}_cap: usize", n));
            }
        }
    }
    out
}

fn ret_ty(r: R) -> &'static str {
    match r {
        R::U32 => "u32",
        R::I32 => "i32",
        R::Unit => "()",
    }
}

/// Kasane のファイルを作る。`extern` の宣言を先に、そのあと安全な関数を置く (`ksn fmt` の並び順: 名前順、定義と定義の間に空行 1 行)。
fn gen_ksnl(d: &Def) -> String {
    let mut out = String::new();
    out.push_str(&format!("// 生成: ksn bindgen (修正プラン 5-3)。直接編集しない。module {}\n", d.module));
    for o in &d.ops {
        out.push_str(&format!(
            "extern {}.raw_{}({}) -> {} link \"env:{}{}\"\n\n",
            d.module,
            o.name,
            raw_params(o).join(", "),
            ret_ty(o.ret),
            d.prefix,
            o.name
        ));
    }
    for o in &d.ops {
        // 安全な関数: 引数は Kasane の型で受け、長さは `unsafe` の外で取り、ポインタは `unsafe` の中で作る (E5003)。
        let mut sig = Vec::new();
        let mut pre = Vec::new();
        let mut inner = Vec::new();
        let mut call = Vec::new();
        for (i, k) in o.params.iter().enumerate() {
            let n = o.names[i];
            match k {
                K::U32 => {
                    sig.push(format!("{}: u32", n));
                    call.push(n.to_string());
                }
                K::Str => {
                    sig.push(format!("{}: &str", n));
                    pre.push(format!("    let {}_len = {}.len()\n", n, n));
                    inner.push(format!("        let {0}_p = core.ptr.from_ref({0}.as_bytes())\n", n));
                    call.push(format!("{}_p", n));
                    call.push(format!("{}_len", n));
                }
                K::Buf => {
                    sig.push(format!("{}: &mut [u8]", n));
                    pre.push(format!("    let {}_cap = {}.len()\n", n, n));
                    inner.push(format!("        let {0}_p = core.ptr.from_mut({0})\n", n));
                    call.push(format!("{}_p", n));
                    call.push(format!("{}_cap", n));
                }
            }
        }
        let mut body = pre.concat();
        body.push_str(&format!("    unsafe \"{}\" \"call the JS import {}{}\" {{\n", d.module, d.prefix, o.name));
        body.push_str(&inner.concat());
        body.push_str(&format!("        {}.raw_{}({})\n    }}\n", d.module, o.name, call.join(", ")));
        out.push_str(&format!(
            "fn {0}.{1}({2}) -> {3} uses [] panics never pub\n{4}end {0}.{1}\n\n",
            d.module,
            o.name,
            sig.join(", "),
            ret_ty(o.ret),
            body
        ));
    }
    // 末尾は改行 1 つ (`ksn fmt` の規則)。
    while out.ends_with("\n\n") {
        out.pop();
    }
    out
}

/// JS の接着コード。`makeImports(host)` が `env` の import の表を返す (`host.instance` が要る)。
fn gen_js(d: &Def) -> String {
    let mut out = String::new();
    out.push_str(&format!("// 生成: ksn bindgen (修正プラン 5-3)。直接編集しない。module {}\n", d.module));
    out.push_str(
        "// 使い方: const host = {}; const env = { ...makeImports(host), ksn_write, ksn_exit };\n\
         //            host.instance = (await WebAssembly.instantiate(bytes, { env })).instance;\n\
         // Kasane へは ksn_event(tag)（listen）と ksn_fetch_done(tag, status)（fetch。status 0 は通信の失敗）を呼ぶ。\n\n",
    );
    out.push_str(
        "const enc = new TextEncoder();\n\
         const dec = new TextDecoder();\n\n\
         export function makeImports(host) {\n\
         \x20 const mem = () => new Uint8Array(host.instance.exports.memory.buffer);\n\
         \x20 const ex = () => host.instance.exports;\n\
         \x20 const S = (p, n) => dec.decode(mem().subarray(p, p + n));\n\
         \x20 const put = (p, cap, bytes) => { if (bytes.length <= cap) mem().set(bytes, p); return bytes.length; };\n\
         \x20 const nodes = [null];\n\
         \x20 const push = (x) => nodes.push(x) - 1;\n\
         \x20 const N = (h) => { if (!nodes[h]) throw new Error(`dom: 不正なハンドル ${h}`); return nodes[h]; };\n\
         \x20 let fetched = new Uint8Array(0);\n\n\
         \x20 const ops = {\n",
    );
    for o in &d.ops {
        out.push_str(&format!("    {}: {},\n", o.name, o.js));
    }
    out.push_str("  };\n\n  return {\n");
    for o in &d.ops {
        // wasm の引数 (JS の並び) と、操作の引数 (`Str` は文字列に直す、`Buf` は (ptr, cap) のまま)。
        let mut args = Vec::new();
        let mut call = Vec::new();
        for (i, k) in o.params.iter().enumerate() {
            match k {
                K::U32 => {
                    args.push(format!("a{}", i));
                    call.push(format!("a{}", i));
                }
                K::Str => {
                    args.push(format!("p{0}, n{0}", i));
                    call.push(format!("S(p{}, n{})", i, i));
                }
                K::Buf => {
                    args.push(format!("p{0}, n{0}", i));
                    call.push(format!("p{0}, n{0}", i));
                }
            }
        }
        out.push_str(&format!("    {}{}: ({}) => ops.{}({}),\n", d.prefix, o.name, args.join(", "), o.name, call.join(", ")));
    }
    out.push_str("  };\n}\n");
    out
}

/// `ksn bindgen <定義ファイル>`。成功 0、定義の誤り・書けない・未知の引数は 2。
pub fn cmd_bindgen(rest: &[String]) -> i32 {
    let Some(path) = rest.first().filter(|a| !a.starts_with('-')) else {
        eprintln!("usage: ksn bindgen <定義ファイル>");
        return 2;
    };
    let src = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("ksn bindgen: {} を読めない: {}", path, e);
            return 2;
        }
    };
    let d = match parse(&src) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("ksn bindgen: {}: {}", path, e);
            return 2;
        }
    };
    // 生成の注意書きのコメントは `.ksnh` が持ち、`.ksnl` はそれを `strip` したもの (`.ksnl` にコメントは置けない)。
    let (ksnh, js) = (gen_ksnl(&d), gen_js(&d));
    let ksnh_path = format!("{}.ksnh", d.ksnl.trim_end_matches(".ksnl"));
    let ksnl = crate::ksnh::strip(&ksnh);
    for (p, text) in [(&ksnh_path, ksnh), (&d.ksnl, ksnl), (&d.js, js)] {
        let p = Path::new(p);
        if let Some(dir) = p.parent().filter(|x| !x.as_os_str().is_empty()) {
            if let Err(e) = fs::create_dir_all(dir) {
                eprintln!("ksn bindgen: {} を作れない: {}", dir.display(), e);
                return 2;
            }
        }
        if let Err(e) = fs::write(p, text) {
            eprintln!("ksn bindgen: {} を書けない: {}", p.display(), e);
            return 2;
        }
        println!("wrote {}", p.display());
    }
    println!(
        "module {}: {} 操作。kasane.toml の [unsafe] modules に `{}` を入れる (extern と unsafe の呼び出しがあるため)",
        d.module,
        d.ops.len(),
        d.module
    );
    0
}
