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
    }
}

/// 隠しの debug 引数: 見出しを libclang で解析して top-level の cursor kind を出す。
pub(crate) fn cmd_bindgen_c_debug_kinds(rest: &[String]) -> i32 {
    let Some(header) = rest.iter().position(|a| a == "--c").and_then(|i| rest.get(i + 1)) else {
        eprintln!("usage: ksn bindgen --c <header.h> --debug-kinds");
        return 2;
    };
    let c = match lc::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("ksn bindgen --c: {}", e);
            return 2;
        }
    };
    let tu = match c.parse(header) {
        Ok(tu) => tu,
        Err(e) => {
            eprintln!("ksn bindgen --c: {}", e);
            return 2;
        }
    };
    for (kind, cursor) in c.top(tu) {
        println!("kind={} spelling={}", kind, c.cstring(unsafe { (c.cursor_spelling)(cursor) }));
    }
    unsafe { (c.dispose_tu)(tu) };
    0
}
