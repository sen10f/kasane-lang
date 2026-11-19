//! `ksn bindgen --c <header.h>` (設計_ネイティブUI.md の N2) の前半: libclang の実行時ロード。
//!
//! libclang は `ksn bindgen --c` のときだけ `dlopen` で開く (ksn 本体にはリンクしない)。
//! 見つからなければ明確な誤りを出す。

use std::ffi::{c_char, c_int, c_uint, c_void, CStr};

/// libclang C API の hand-written FFI (実行時ロード)。
pub(crate) mod lc {
    use super::*;

    pub type CXIndex = *mut c_void;
    pub type CXTranslationUnit = *mut c_void;
    pub type CXClientData = *mut c_void;

    #[derive(Clone, Copy)]
    #[repr(C)]
    pub struct CXString {
        pub data: *const c_void,
        pub private_flags: c_uint,
    }
    #[derive(Clone, Copy)]
    #[repr(C)]
    pub struct CXCursor {
        pub kind: c_int,
        pub xdata: c_int,
        pub data: [*const c_void; 3],
    }
    #[derive(Clone, Copy)]
    #[repr(C)]
    pub struct CXType {
        pub kind: c_int,
        pub data: [*const c_void; 2],
    }
    #[derive(Clone, Copy)]
    #[repr(C)]
    pub struct CXSourceLocation {
        pub ptr_data: [*const c_void; 2],
        pub int_data: c_uint,
    }
    #[derive(Clone, Copy)]
    #[repr(C)]
    pub struct CXSourceRange {
        pub ptr_data: [*const c_void; 2],
        pub begin_int_data: c_uint,
        pub end_int_data: c_uint,
    }
    #[derive(Clone, Copy)]
    #[repr(C)]
    pub struct CXToken {
        pub int_data: [c_uint; 4],
        pub ptr_data: *mut c_void,
    }

    pub type CXCursorVisitor = extern "C" fn(CXCursor, CXCursor, CXClientData) -> c_int;

    /// CXCursorKind のうち使うもの。
    pub mod ck {
        use super::c_int;
        pub const STRUCT_DECL: c_int = 2;
        pub const UNION_DECL: c_int = 3;
        pub const ENUM_DECL: c_int = 5;
        pub const FIELD_DECL: c_int = 6;
        pub const ENUM_CONSTANT_DECL: c_int = 7;
        pub const FUNCTION_DECL: c_int = 8;
        pub const TYPEDEF_DECL: c_int = 20;
        pub const MACRO_DEFINITION: c_int = 501;
    }
    /// CXTypeKind のうち使うもの。
    pub mod tk {
        use super::c_int;
        pub const VOID: c_int = 2;
        pub const BOOL: c_int = 3;
        pub const CHAR_U: c_int = 4;
        pub const UCHAR: c_int = 5;
        pub const CHAR16: c_int = 6;
        pub const CHAR32: c_int = 7;
        pub const USHORT: c_int = 8;
        pub const UINT: c_int = 9;
        pub const ULONG: c_int = 10;
        pub const ULONG_LONG: c_int = 11;
        pub const CHAR_S: c_int = 13;
        pub const SCHAR: c_int = 14;
        pub const WCHAR: c_int = 15;
        pub const SHORT: c_int = 16;
        pub const INT: c_int = 17;
        pub const LONG: c_int = 18;
        pub const LONG_LONG: c_int = 19;
        pub const FLOAT: c_int = 21;
        pub const DOUBLE: c_int = 22;
        pub const LONG_DOUBLE: c_int = 23;
        pub const POINTER: c_int = 101;
        pub const RECORD: c_int = 105;
        pub const ENUM: c_int = 106;
        pub const TYPEDEF: c_int = 107;
        pub const FUNCTION_NO_PROTO: c_int = 110;
        pub const FUNCTION_PROTO: c_int = 111;
        pub const CONSTANT_ARRAY: c_int = 112;
        pub const INCOMPLETE_ARRAY: c_int = 114;
    }

    /// ロードした libclang の関数ポインタの表。
    pub struct Clang {
        pub create_index: unsafe extern "C" fn(c_int, c_int) -> CXIndex,
        pub dispose_index: unsafe extern "C" fn(CXIndex),
        pub parse_tu2: unsafe extern "C" fn(CXIndex, *const c_char, *const *const c_char, c_int, *mut c_void, c_uint, c_uint, *mut CXTranslationUnit) -> c_int,
        pub dispose_tu: unsafe extern "C" fn(CXTranslationUnit),
        pub tu_cursor: unsafe extern "C" fn(CXTranslationUnit) -> CXCursor,
        pub visit_children: unsafe extern "C" fn(CXCursor, CXCursorVisitor, CXClientData) -> c_uint,
        pub cursor_kind: unsafe extern "C" fn(CXCursor) -> c_int,
        pub cursor_spelling: unsafe extern "C" fn(CXCursor) -> CXString,
        pub cursor_usr: unsafe extern "C" fn(CXCursor) -> CXString,
        pub cursor_location: unsafe extern "C" fn(CXCursor) -> CXSourceLocation,
        pub location_from_main_file: unsafe extern "C" fn(CXSourceLocation) -> c_int,
        pub cursor_type: unsafe extern "C" fn(CXCursor) -> CXType,
        pub cursor_argument: unsafe extern "C" fn(CXCursor, c_uint) -> CXCursor,
        pub cursor_extent: unsafe extern "C" fn(CXCursor) -> CXSourceRange,
        pub is_definition: unsafe extern "C" fn(CXCursor) -> c_int,
        pub is_bitfield: unsafe extern "C" fn(CXCursor) -> c_int,
        pub is_variadic: unsafe extern "C" fn(CXCursor) -> c_int,
        pub cursor_linkage: unsafe extern "C" fn(CXCursor) -> c_int,
        pub type_spelling: unsafe extern "C" fn(CXType) -> CXString,
        pub canonical_type: unsafe extern "C" fn(CXType) -> CXType,
        pub pointee_type: unsafe extern "C" fn(CXType) -> CXType,
        pub is_const_type: unsafe extern "C" fn(CXType) -> c_int,
        pub result_type: unsafe extern "C" fn(CXType) -> CXType,
        pub num_arg_types: unsafe extern "C" fn(CXType) -> c_int,
        pub arg_type: unsafe extern "C" fn(CXType, c_uint) -> CXType,
        pub array_element_type: unsafe extern "C" fn(CXType) -> CXType,
        pub array_size: unsafe extern "C" fn(CXType) -> i64,
        pub type_declaration: unsafe extern "C" fn(CXType) -> CXCursor,
        pub enum_const_value: unsafe extern "C" fn(CXCursor) -> i64,
        pub enum_const_uvalue: unsafe extern "C" fn(CXCursor) -> u64,
        pub enum_integer_type: unsafe extern "C" fn(CXCursor) -> CXType,
        pub typedef_underlying: unsafe extern "C" fn(CXCursor) -> CXType,
        pub tokenize: unsafe extern "C" fn(CXTranslationUnit, CXSourceRange, *mut *mut CXToken, *mut c_uint),
        pub get_token_spelling: unsafe extern "C" fn(CXTranslationUnit, CXToken) -> CXString,
        pub dispose_tokens: unsafe extern "C" fn(CXTranslationUnit, *mut CXToken, c_uint),
        pub get_cstring: unsafe extern "C" fn(CXString) -> *const c_char,
        pub dispose_string: unsafe extern "C" fn(CXString),
    }

    /// libclang を探して開き、関数ポインタを集める。見つからなければ理由つきの誤り。
    pub fn load() -> Result<Clang, String> {
        let candidates: &[&str] = if cfg!(windows) {
            &["libclang.dll", "libclang-20.dll"]
        } else if cfg!(target_os = "macos") {
            &["libclang.dylib", "/usr/lib/llvm-20/lib/libclang.dylib"]
        } else {
            &[
                "libclang-20.so.20",
                "libclang-20.so.1",
                "libclang.so.20",
                "libclang.so.1",
                "libclang.so",
                "/usr/lib/llvm-20/lib/libclang-20.so.20",
            ]
        };
        let mut last = String::new();
        for cand in candidates {
            match crate::ffi::lookup(Some(cand), "clang_createIndex") {
                Ok(_) => return Clang::load_from(cand),
                Err(e) => last = e,
            }
        }
        Err(format!(
            "libclang が見つからない ({})。`ksn bindgen --c` には libclang (libclang-20.so / libclang.so。例 /usr/lib/llvm-20/lib/) が要る",
            last
        ))
    }

    impl Clang {
        fn load_from(lib: &str) -> Result<Clang, String> {
            macro_rules! sym {
                ($name:literal, $ty:ty) => {
                    match crate::ffi::lookup(Some(lib), $name) {
                        Ok(p) => unsafe { std::mem::transmute::<*mut c_void, $ty>(p) },
                        Err(e) => return Err(format!("libclang ({}) に {} がない: {}", lib, $name, e)),
                    }
                };
            }
            Ok(Clang {
                create_index: sym!("clang_createIndex", unsafe extern "C" fn(c_int, c_int) -> CXIndex),
                dispose_index: sym!("clang_disposeIndex", unsafe extern "C" fn(CXIndex)),
                parse_tu2: sym!("clang_parseTranslationUnit2", unsafe extern "C" fn(CXIndex, *const c_char, *const *const c_char, c_int, *mut c_void, c_uint, c_uint, *mut CXTranslationUnit) -> c_int),
                dispose_tu: sym!("clang_disposeTranslationUnit", unsafe extern "C" fn(CXTranslationUnit)),
                tu_cursor: sym!("clang_getTranslationUnitCursor", unsafe extern "C" fn(CXTranslationUnit) -> CXCursor),
                visit_children: sym!("clang_visitChildren", unsafe extern "C" fn(CXCursor, CXCursorVisitor, CXClientData) -> c_uint),
                cursor_kind: sym!("clang_getCursorKind", unsafe extern "C" fn(CXCursor) -> c_int),
                cursor_spelling: sym!("clang_getCursorSpelling", unsafe extern "C" fn(CXCursor) -> CXString),
                cursor_usr: sym!("clang_getCursorUSR", unsafe extern "C" fn(CXCursor) -> CXString),
                cursor_location: sym!("clang_getCursorLocation", unsafe extern "C" fn(CXCursor) -> CXSourceLocation),
                location_from_main_file: sym!("clang_Location_isFromMainFile", unsafe extern "C" fn(CXSourceLocation) -> c_int),
                cursor_type: sym!("clang_getCursorType", unsafe extern "C" fn(CXCursor) -> CXType),
                cursor_argument: sym!("clang_Cursor_getArgument", unsafe extern "C" fn(CXCursor, c_uint) -> CXCursor),
                cursor_extent: sym!("clang_getCursorExtent", unsafe extern "C" fn(CXCursor) -> CXSourceRange),
                is_definition: sym!("clang_isCursorDefinition", unsafe extern "C" fn(CXCursor) -> c_int),
                is_bitfield: sym!("clang_Cursor_isBitField", unsafe extern "C" fn(CXCursor) -> c_int),
                is_variadic: sym!("clang_Cursor_isVariadic", unsafe extern "C" fn(CXCursor) -> c_int),
                cursor_linkage: sym!("clang_getCursorLinkage", unsafe extern "C" fn(CXCursor) -> c_int),
                type_spelling: sym!("clang_getTypeSpelling", unsafe extern "C" fn(CXType) -> CXString),
                canonical_type: sym!("clang_getCanonicalType", unsafe extern "C" fn(CXType) -> CXType),
                pointee_type: sym!("clang_getPointeeType", unsafe extern "C" fn(CXType) -> CXType),
                is_const_type: sym!("clang_isConstQualifiedType", unsafe extern "C" fn(CXType) -> c_int),
                result_type: sym!("clang_getResultType", unsafe extern "C" fn(CXType) -> CXType),
                num_arg_types: sym!("clang_getNumArgTypes", unsafe extern "C" fn(CXType) -> c_int),
                arg_type: sym!("clang_getArgType", unsafe extern "C" fn(CXType, c_uint) -> CXType),
                array_element_type: sym!("clang_getArrayElementType", unsafe extern "C" fn(CXType) -> CXType),
                array_size: sym!("clang_getArraySize", unsafe extern "C" fn(CXType) -> i64),
                type_declaration: sym!("clang_getTypeDeclaration", unsafe extern "C" fn(CXType) -> CXCursor),
                enum_const_value: sym!("clang_getEnumConstantDeclValue", unsafe extern "C" fn(CXCursor) -> i64),
                enum_const_uvalue: sym!("clang_getEnumConstantDeclUnsignedValue", unsafe extern "C" fn(CXCursor) -> u64),
                enum_integer_type: sym!("clang_getEnumDeclIntegerType", unsafe extern "C" fn(CXCursor) -> CXType),
                typedef_underlying: sym!("clang_getTypedefDeclUnderlyingType", unsafe extern "C" fn(CXCursor) -> CXType),
                tokenize: sym!("clang_tokenize", unsafe extern "C" fn(CXTranslationUnit, CXSourceRange, *mut *mut CXToken, *mut c_uint)),
                get_token_spelling: sym!("clang_getTokenSpelling", unsafe extern "C" fn(CXTranslationUnit, CXToken) -> CXString),
                dispose_tokens: sym!("clang_disposeTokens", unsafe extern "C" fn(CXTranslationUnit, *mut CXToken, c_uint)),
                get_cstring: sym!("clang_getCString", unsafe extern "C" fn(CXString) -> *const c_char),
                dispose_string: sym!("clang_disposeString", unsafe extern "C" fn(CXString)),
            })
        }

        pub fn cstring(&self, s: CXString) -> String {
            unsafe {
                let p = (self.get_cstring)(s);
                let r = if p.is_null() { String::new() } else { CStr::from_ptr(p).to_string_lossy().into_owned() };
                (self.dispose_string)(s);
                r
            }
        }

        /// 見出しを解析する。失敗したら誤り。
        pub fn parse(&self, header: &str) -> Result<CXTranslationUnit, String> {
            let index = unsafe { (self.create_index)(0, 0) };
            if index.is_null() {
                return Err("clang_createIndex に失敗した".into());
            }
            let dir = std::path::Path::new(header).parent().map(|p| p.to_path_buf()).unwrap_or_default();
            let args: Vec<String> = ["ksn-bindgen", "-x", "c"]
                .iter()
                .map(|s| s.to_string())
                .chain(std::iter::once(format!("-I{}", dir.display())))
                .collect();
            let c_args: Vec<Vec<u8>> = args.iter().map(|a| a.bytes().chain(std::iter::once(0)).collect()).collect();
            let c_ptrs: Vec<*const c_char> = c_args.iter().map(|a| a.as_ptr() as *const c_char).collect();
            let src = std::ffi::CString::new(header).map_err(|_| "見出しのパスに NUL がある".to_string())?;
            // CXTranslationUnit_DetailedPreprocessingRecord (0x01) | SkipFunctionBodies (0x40)
            let mut tu: CXTranslationUnit = std::ptr::null_mut();
            let rc = unsafe {
                (self.parse_tu2)(index, src.as_ptr(), c_ptrs.as_ptr(), c_ptrs.len() as c_int, std::ptr::null_mut(), 0, 0x01 | 0x40, &mut tu)
            };
            unsafe { (self.dispose_index)(index) };
            if rc != 0 || tu.is_null() {
                return Err(format!("C の見出しを解析できなかった: {} (rc={})", header, rc));
            }
            Ok(tu)
        }

        pub fn top(&self, tu: CXTranslationUnit) -> Vec<(c_int, CXCursor)> {
            let root = unsafe { (self.tu_cursor)(tu) };
            let mut out: Vec<(c_int, CXCursor)> = Vec::new();
            extern "C" fn collect(cursor: CXCursor, _parent: CXCursor, data: CXClientData) -> c_int {
                let v = unsafe { &mut *(data as *mut Vec<(c_int, CXCursor)>) };
                v.push((cursor.kind, cursor));
                1 // CXChildVisit_Continue
            }
            unsafe { (self.visit_children)(root, collect, &mut out as *mut _ as CXClientData) };
            out
        }

        pub fn kind(&self, cur: CXCursor) -> c_int {
            unsafe { (self.cursor_kind)(cur) }
        }
        pub fn spelling(&self, cur: CXCursor) -> String {
            let s = unsafe { (self.cursor_spelling)(cur) };
            self.cstring(s)
        }
        pub fn usr(&self, cur: CXCursor) -> String {
            let s = unsafe { (self.cursor_usr)(cur) };
            self.cstring(s)
        }
        pub fn in_main_file(&self, cur: CXCursor) -> bool {
            unsafe { (self.location_from_main_file)((self.cursor_location)(cur)) != 0 }
        }
        pub fn ty(&self, cur: CXCursor) -> CXType {
            unsafe { (self.cursor_type)(cur) }
        }
        pub fn canon(&self, t: CXType) -> CXType {
            unsafe { (self.canonical_type)(t) }
        }
        /// 子カーソルを全部集める。
        pub fn children(&self, parent: CXCursor) -> Vec<CXCursor> {
            let mut out: Vec<CXCursor> = Vec::new();
            extern "C" fn collect(cursor: CXCursor, _parent: CXCursor, data: CXClientData) -> c_int {
                let v = unsafe { &mut *(data as *mut Vec<CXCursor>) };
                v.push(cursor);
                1
            }
            unsafe { (self.visit_children)(parent, collect, &mut out as *mut _ as CXClientData) };
            out
        }
        /// マクロの定義域をトークン列にする。
        pub fn macro_tokens(&self, tu: CXTranslationUnit, cur: CXCursor) -> Vec<String> {
            let ext = unsafe { (self.cursor_extent)(cur) };
            let mut toks: *mut CXToken = std::ptr::null_mut();
            let mut n: c_uint = 0;
            unsafe { (self.tokenize)(tu, ext, &mut toks, &mut n) };
            let mut out = Vec::new();
            for i in 0..n as isize {
                let t = unsafe { *toks.offset(i) };
                out.push(self.cstring(unsafe { (self.get_token_spelling)(tu, t) }));
            }
            unsafe { (self.dispose_tokens)(tu, toks, n) };
            out
        }
    }
}

// ---------------------------------------------------------------- 名前の整形

/// C の名前を Kasane の snake_case識別子にする (ASCII のみ。`foo_2` の形は作らない)。
pub(crate) fn to_snake(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    let mut out = String::new();
    let mut prev_lower = false;
    for (i, &c) in chars.iter().enumerate() {
        if c.is_ascii_alphanumeric() {
            if c.is_ascii_uppercase() {
                if prev_lower {
                    out.push('_');
                }
                out.push(c.to_ascii_lowercase());
                prev_lower = false;
            } else {
                // `_` + 数字 は名前の規約違反なので `_` を落とす
                if c.is_ascii_digit() && out.ends_with('_') && chars.get(i.wrapping_sub(1)).map(|p| *p == '_').unwrap_or(false) {
                    out.pop();
                }
                out.push(c);
                prev_lower = true;
            }
        } else {
            out.push('_');
            prev_lower = false;
        }
    }
    while out.contains("__") {
        out = out.replace("__", "_");
    }
    let out = out.trim_matches('_').to_string();
    let mut out = if out.is_empty() { "x".to_string() } else { out };
    if out.chars().next().unwrap().is_ascii_digit() {
        out.insert(0, '_');
    }
    if crate::grammar::RESERVED.contains(&out.as_str()) {
        out.push('_');
    }
    out
}

/// C の名前を UpperCamelCase の型名にする (連続する大文字2文字以上は作らない)。
pub(crate) fn to_upper_camel(name: &str) -> String {
    let chars: Vec<char> = name.chars().filter(|c| c.is_ascii()).collect();
    let mut words: Vec<String> = Vec::new();
    let mut cur = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if !c.is_ascii_alphanumeric() {
            if !cur.is_empty() {
                words.push(std::mem::take(&mut cur));
            }
            continue;
        }
        if c.is_ascii_uppercase() {
            let prev = cur.chars().last();
            let next_lower = chars.get(i + 1).map(|n| n.is_ascii_lowercase()).unwrap_or(false);
            if !cur.is_empty() && (prev.map(|p| p.is_ascii_lowercase() || p.is_ascii_digit()).unwrap_or(false) || (prev.unwrap().is_ascii_uppercase() && next_lower)) {
                words.push(std::mem::take(&mut cur));
            }
            cur.push(c.to_ascii_lowercase());
        } else {
            cur.push(c);
        }
    }
    if !cur.is_empty() {
        words.push(cur);
    }
    let mut out = String::new();
    for w in &words {
        let mut cs = w.chars();
        if let Some(f) = cs.next() {
            out.push(f.to_ascii_uppercase());
        }
        out.extend(cs);
    }
    // 連続する大文字が残っていたら後ろを小文字にする (HTTPServer → HttpServer)
    let mut fixed = String::new();
    let prev_upper = false;
    for c in out.chars() {
        if c.is_ascii_uppercase() && prev_upper {
            fixed.push(c.to_ascii_lowercase());
        } else {
            fixed.push(c);
        }
    }
    let mut out = if fixed.is_empty() { "X".to_string() } else { fixed };
    if out.chars().next().unwrap().is_ascii_digit() {
        out.insert(0, 'X');
    }
    out
}

/// C の名前を SCREAMING_SNAKE_CASE の const 名にする。
pub(crate) fn to_screaming(name: &str) -> String {
    to_snake(name).to_uppercase()
}

/// 既存の名前と衝突しないように数字を付ける。
fn uniq(used: &mut std::collections::HashSet<String>, name: String) -> String {
    if used.insert(name.clone()) {
        return name;
    }
    let mut n = 2u32;
    loop {
        let cand = format!("{}{}", name, n);
        if used.insert(cand.clone()) {
            return cand;
        }
        n += 1;
    }
}

/// C の整数型 (CXTypeKind) を Kasane の型名にする。対応外は None。
fn int_type(kind: c_int) -> Option<&'static str> {
    use lc::tk::*;
    Some(match kind {
        CHAR_S | SCHAR => "i8",
        CHAR_U | UCHAR => "u8",
        SHORT => "i16",
        USHORT | CHAR16 => "u16",
        INT => "i32",
        UINT => "u32",
        LONG | LONG_LONG | WCHAR => "i64",
        ULONG | ULONG_LONG | CHAR32 => "u64",
        _ => return None,
    })
}

// ---------------------------------------------------------------- 内部モデル

pub(crate) struct Rec {
    pub name: String,
    pub fields: Vec<(String, String)>,
}
pub(crate) struct Const {
    pub name: String,
    pub ty: &'static str,
    pub val: i128,
}
pub(crate) struct Ext {
    pub name: String,
    pub c_name: String,
    pub params: Vec<(String, String)>,
    pub ret: String,
}
pub(crate) struct Model {
    pub consts: Vec<Const>,
    pub recs: Vec<Rec>,
    pub exts: Vec<Ext>,
    pub notes: Vec<String>,
}

/// C の型を Kasane の型の文字列にする。関数ポインタは `fn(..) -> .. uses [] panics never`。
fn render_type(c: &lc::Clang, rec_names: &std::collections::HashMap<String, String>, t: lc::CXType, depth: usize) -> Result<String, String> {
    use lc::tk::*;
    if depth > 32 {
        return Err("型が深すぎる".into());
    }
    let canon = c.canon(t);
    match canon.kind {
        VOID => Ok("()".into()),
        BOOL => Ok("bool".into()),
        FLOAT => Ok("f32".into()),
        DOUBLE => Ok("f64".into()),
        k if int_type(k).is_some() => Ok(int_type(k).unwrap().to_string()),
        POINTER => {
            let pointee = c.canon(unsafe { (c.pointee_type)(canon) });
            if pointee.kind == FUNCTION_PROTO {
                // 関数ポインタは Kasane では関数型そのもの (C ABI で同一)
                return render_type(c, rec_names, pointee, depth + 1);
            }
            if pointee.kind == VOID {
                return Ok(if unsafe { (c.is_const_type)(pointee) } != 0 { "*const u8".into() } else { "*mut u8".into() });
            }
            let inner = render_type(c, rec_names, pointee, depth + 1)?;
            Ok(if unsafe { (c.is_const_type)(pointee) } != 0 { format!("*const {}", inner) } else { format!("*mut {}", inner) })
        }
        CONSTANT_ARRAY => {
            let elem = unsafe { (c.array_element_type)(canon) };
            let n = unsafe { (c.array_size)(canon) };
            if n < 0 {
                return Err("配列の長さが負".into());
            }
            Ok(format!("[{}; {}]", render_type(c, rec_names, elem, depth + 1)?, n))
        }
        INCOMPLETE_ARRAY => Err("可変長配列 (flexible array member)".into()),
        RECORD => {
            let decl = unsafe { (c.type_declaration)(canon) };
            if c.kind(decl) == lc::ck::UNION_DECL {
                return Err("union".into());
            }
            let u = c.usr(decl);
            rec_names.get(&u).cloned().ok_or_else(|| format!("構造体 {} はこの見出しで定義されていない", c.spelling(decl)))
        }
        ENUM => {
            let decl = unsafe { (c.type_declaration)(canon) };
            let it = c.canon(unsafe { (c.enum_integer_type)(decl) });
            int_type(it.kind).map(|s| s.to_string()).ok_or_else(|| "enum の整数型に対応がない".to_string())
        }
        FUNCTION_PROTO => {
            let n = unsafe { (c.num_arg_types)(canon) };
            let mut args = Vec::new();
            for i in 0..n.max(0) as c_uint {
                let a = unsafe { (c.arg_type)(canon, i) };
                args.push(render_type(c, rec_names, a, depth + 1)?);
            }
            let ret = render_type(c, rec_names, unsafe { (c.result_type)(canon) }, depth + 1)?;
            Ok(format!("fn({}) -> {} uses [] panics never", args.join(", "), ret))
        }
        FUNCTION_NO_PROTO => Err("プロトタイプのない関数 (K&R 形式)".into()),
        _ => Err(format!("Kasane で表現できない型 (CXTypeKind {})", canon.kind)),
    }
}

/// `#define` の値のトークン列 (先頭のマクロ名を除く) が整数リテラルならその値を返す。
fn parse_int_macro(toks: &[String]) -> Option<i128> {
    let mut toks: &[String] = toks;
    // (式) の形は外側の括弧を剥く
    while toks.len() >= 2 && toks.first().map(|s| s == "(").unwrap_or(false) && toks.last().map(|s| s == ")").unwrap_or(false) {
        toks = &toks[1..toks.len() - 1];
    }
    let (neg, lits) = match toks {
        [l] => (false, l.as_str()),
        [s, l] if s == "-" => (true, l.as_str()),
        [s, l] if s == "+" => (false, l.as_str()),
        _ => return None,
    };
    let lit = lits;
    let (radix, start) = if lit.len() >= 2 && (lit.starts_with("0x") || lit.starts_with("0X")) {
        (16, 2)
    } else if lit.len() > 1 && lit.starts_with('0') {
        (8, 1)
    } else {
        (10, 0)
    };
    let rest = &lit[start..];
    let body_len = rest.chars().take_while(|c| c.is_digit(radix)).count();
    let body = &rest[..body_len];
    let suffix = &rest[body_len..];
    if body.is_empty() || !suffix.chars().all(|c| matches!(c, 'u' | 'U' | 'l' | 'L')) {
        return None;
    }
    let v = i128::from_str_radix(body, radix).ok()?;
    Some(if neg { -v } else { v })
}

/// 値に合う smallest の整数型を選ぶ。
fn const_type(v: i128) -> &'static str {
    if v >= 0 {
        if v <= i32::MAX as i128 {
            "i32"
        } else if v <= u32::MAX as i128 {
            "u32"
        } else if v <= i64::MAX as i128 {
            "i64"
        } else {
            "u64"
        }
    } else if v >= i32::MIN as i128 {
        "i32"
    } else {
        "i64"
    }
}

/// 見出しを解析して内部モデルにする。`prefix` は Kasane のモジュール名。
pub(crate) fn extract(header: &str, prefix: &str) -> Result<Model, String> {
    let c = lc::load()?;
    let tu = c.parse(header)?;
    let top = c.top(tu);
    let decls: Vec<(c_int, lc::CXCursor)> = top.into_iter().filter(|(k, cur)| c.in_main_file(*cur) && matches!(*k, lc::ck::STRUCT_DECL | lc::ck::UNION_DECL | lc::ck::ENUM_DECL | lc::ck::TYPEDEF_DECL | lc::ck::FUNCTION_DECL | lc::ck::MACRO_DEFINITION)).collect();

    let mut notes: Vec<String> = Vec::new();
    let mut rec_names: std::collections::HashMap<String, String> = Default::default();
    let mut used_type: std::collections::HashSet<String> = Default::default();
    let mut used_const: std::collections::HashSet<String> = Default::default();
    let mut used_fn: std::collections::HashSet<String> = Default::default();

    // struct の名前を登録する (無名の struct は typedef 名を使う)。map には FQN を入れる
    for (k, cur) in &decls {
        if *k == lc::ck::STRUCT_DECL && unsafe { (c.is_definition)(*cur) } != 0 {
            let sp = c.spelling(*cur);
            if !sp.is_empty() {
                let name = uniq(&mut used_type, to_upper_camel(&sp));
                rec_names.insert(c.usr(*cur), format!("{}.{}", prefix, name));
            }
        }
    }
    for (k, cur) in &decls {
        if *k == lc::ck::TYPEDEF_DECL {
            let canon = c.canon(unsafe { (c.typedef_underlying)(*cur) });
            if canon.kind == lc::tk::RECORD {
                let decl = unsafe { (c.type_declaration)(canon) };
                let u = c.usr(decl);
                if !rec_names.contains_key(&u) {
                    let name = uniq(&mut used_type, to_upper_camel(&c.spelling(*cur)));
                    rec_names.insert(u, format!("{}.{}", prefix, name));
                }
            }
        }
    }

    let mut model = Model { consts: Vec::new(), recs: Vec::new(), exts: Vec::new(), notes };

    // struct
    for (k, cur) in &decls {
        if *k != lc::ck::STRUCT_DECL || unsafe { (c.is_definition)(*cur) } == 0 {
            continue;
        }
        let c_name = c.spelling(*cur);
        let Some(full) = rec_names.get(&c.usr(*cur)).cloned() else { continue };
        let name = full[prefix.len() + 1..].to_string();
        let mut fields: Vec<(String, String)> = Vec::new();
        let mut used_field: std::collections::HashSet<String> = Default::default();
        let mut bad: Option<String> = None;
        for ch in c.children(*cur) {
            if c.kind(ch) != lc::ck::FIELD_DECL {
                continue;
            }
            if unsafe { (c.is_bitfield)(ch) } != 0 {
                bad = Some("ビットフィールド".into());
                break;
            }
            match render_type(&c, &rec_names, c.ty(ch), 0) {
                Ok(t) => {
                    let fname = uniq(&mut used_field, to_snake(&c.spelling(ch)));
                    fields.push((fname, t));
                }
                Err(e) => {
                    bad = Some(e);
                    break;
                }
            }
        }
        match bad {
            Some(e) => model.notes.push(format!("構造体 {} は飛ばした: {}", if c_name.is_empty() { name.as_str() } else { c_name.as_str() }, e)),
            None if fields.is_empty() => model.notes.push(format!("構造体 {} は飛ばした: フィールドがない", c_name)),
            None => model.recs.push(Rec { name, fields }),
        }
    }
    // union は丸ごと飛ばす
    for (k, cur) in &decls {
        if *k == lc::ck::UNION_DECL {
            model.notes.push(format!("union {} は飛ばした: Kasane に union はない", c.spelling(*cur)));
        }
    }
    // enum → const
    for (k, cur) in &decls {
        if *k != lc::ck::ENUM_DECL {
            continue;
        }
        let it = c.canon(unsafe { (c.enum_integer_type)(*cur) });
        let Some(ty) = int_type(it.kind) else {
            model.notes.push(format!("enum {} は飛ばした: 整数型 {} に対応がない", c.spelling(*cur), c.cstring(unsafe { (c.type_spelling)(it) })));
            continue;
        };
        let unsigned = matches!(it.kind, lc::tk::UINT | lc::tk::ULONG | lc::tk::ULONG_LONG | lc::tk::USHORT | lc::tk::UCHAR | lc::tk::CHAR_U | lc::tk::CHAR32);
        for ch in c.children(*cur) {
            if c.kind(ch) != lc::ck::ENUM_CONSTANT_DECL {
                continue;
            }
            let sv = unsafe { (c.enum_const_value)(ch) } as i128;
            let uv = unsafe { (c.enum_const_uvalue)(ch) } as i128;
            let v = if sv < 0 && unsigned { uv } else { sv };
            let name = uniq(&mut used_const, to_screaming(&c.spelling(ch)));
            model.consts.push(Const { name: format!("{}.{}", prefix, name), ty, val: v });
        }
    }
    // #define → const (整数のみ)
    for (k, cur) in &decls {
        if *k != lc::ck::MACRO_DEFINITION {
            continue;
        }
        let name = c.spelling(*cur);
        let toks = c.macro_tokens(tu, *cur);
        // トークン列の先頭はマクロ名そのもの
        let body = if toks.is_empty() { &toks[..] } else { &toks[1..] };
        match parse_int_macro(body) {
            Some(v) => {
                let ty = const_type(v);
                let name = uniq(&mut used_const, to_screaming(&name));
                model.consts.push(Const { name: format!("{}.{}", prefix, name), ty, val: v });
            }
            None => model.notes.push(format!("#define {} は飛ばした: 整数定数でない ({})", name, body.join(" "))),
        }
    }
    // 関数 → extern
    for (k, cur) in &decls {
        if *k != lc::ck::FUNCTION_DECL {
            continue;
        }
        let c_name = c.spelling(*cur);
        // 外部リンケージのみ (static / inline はシンボルを持たない)
        if unsafe { (c.cursor_linkage)(*cur) } < 3 {
            model.notes.push(format!("関数 {} は飛ばした: 外部リンケージでない (static / inline)", c_name));
            continue;
        }
        if unsafe { (c.is_variadic)(*cur) } != 0 {
            model.notes.push(format!("関数 {} は飛ばした: 可変長引数 (varargs)", c_name));
            continue;
        }
        let fty = c.canon(c.ty(*cur));
        if fty.kind != lc::tk::FUNCTION_PROTO {
            model.notes.push(format!("関数 {} は飛ばした: プロトタイプがない", c_name));
            continue;
        }
        let n = unsafe { (c.num_arg_types)(fty) }.max(0) as c_uint;
        let mut params: Vec<(String, String)> = Vec::new();
        let mut used_param: std::collections::HashSet<String> = Default::default();
        let mut bad = None;
        for i in 0..n {
            match render_type(&c, &rec_names, unsafe { (c.arg_type)(fty, i) }, 0) {
                Ok(t) => {
                    let arg = unsafe { (c.cursor_argument)(*cur, i) };
                    let raw = c.spelling(arg);
                    let pname = if raw.is_empty() { format!("a{}", i) } else { to_snake(&raw) };
                    params.push((uniq(&mut used_param, pname), t));
                }
                Err(e) => {
                    bad = Some(e);
                    break;
                }
            }
        }
        let ret = match &bad {
            None => match render_type(&c, &rec_names, unsafe { (c.result_type)(fty) }, 0) {
                Ok(r) => r,
                Err(e) => {
                    bad = Some(e);
                    String::new()
                }
            },
            Some(_) => String::new(),
        };
        match bad {
            Some(e) => model.notes.push(format!("関数 {} は飛ばした: {}", c_name, e)),
            None => {
                let name = uniq(&mut used_fn, to_snake(&c_name));
                model.exts.push(Ext { name, c_name, params, ret });
            }
        }
    }

    unsafe { (c.dispose_tu)(tu) };
    Ok(model)
}

/// 隠しの debug 引数: 見出しを libclang で解析して内部モデルを出す。
pub(crate) fn cmd_bindgen_c_debug_kinds(rest: &[String]) -> i32 {
    let Some(header) = rest.iter().position(|a| a == "--c").and_then(|i| rest.get(i + 1)) else {
        eprintln!("usage: ksn bindgen --c <header.h> --debug-kinds");
        return 2;
    };
    let prefix = rest.iter().position(|a| a == "--prefix").and_then(|i| rest.get(i + 1)).cloned().unwrap_or_else(|| "x".into());
    let m = match extract(header, &prefix) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("ksn bindgen --c: {}", e);
            return 2;
        }
    };
    for cn in &m.consts {
        println!("const {}: {} = {}", cn.name, cn.ty, cn.val);
    }
    for r in &m.recs {
        println!("type {}", r.name);
        for (f, t) in &r.fields {
            println!("    {}: {}", f, t);
        }
    }
    for e in &m.exts {
        println!("extern {}({}) -> {}", e.name, e.params.iter().map(|(n, t)| format!("{}: {}", n, t)).collect::<Vec<_>>().join(", "), e.ret);
    }
    for n in &m.notes {
        println!("note: {}", n);
    }
    0
}
