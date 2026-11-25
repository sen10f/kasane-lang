//! 各サブコマンドの使い方 (`ksn <cmd> --help` / `ksn help <cmd>`)。
//! (コマンド, 使い方, 1 行の説明, 例)

pub const HELP: &[(&str, &str, &str, &str)] = &[
    ("new", "ksn new <name>", "Create a new package directory <name> with kasane.toml and a hello-world entry.", "ksn new hello"),
    ("check", "ksn check [--json] [--native] [--signatures-only] [--target <triple>]", "Type-check the package (run in the package or any subdirectory).", "ksn check"),
    ("check-stdlib", "ksn check-stdlib [--json]", "Check the standard library sources.", "ksn check-stdlib"),
    ("run", "ksn run [-- <program args>...]", "Check and run the package entry (VM by default; falls back to the tree walker; --engine=tree|vm).", "ksn run -- 1 2"),
    ("test", "ksn test [--both] [--json] [--debug <FQN>] [<filter>]", "Run the package tests (--both: also compare with the native backend).", "ksn test --both"),
    ("build", "ksn build [--release] [--allow-bundle] [--target <triple>] [--backend llvm|c]", "Build target/<debug|release>/<name>.native; the last line says which file was made.", "ksn build --release"),
    ("fmt", "ksn fmt [--check]", "Rewrite sources to the one canonical format (--check: only report).", "ksn fmt"),
    ("fix", "ksn fix", "Apply the mechanical fixes for known diagnostics and the canonical-form rewrites.", "ksn fix"),
    ("strip", "ksn strip", "Make the comment-free .ksnl from .ksnh files.", "ksn strip"),
    ("sync", "ksn sync", "Update .ksnh files to match the .ksnl files.", "ksn sync"),
    ("explain", "ksn explain <code>", "Explain a diagnostic code.", "ksn explain E2020"),
    ("context", "ksn context <FQN>", "Print the definition and what it needs, for writing code that uses <FQN>.", "ksn context std.vec.Vec"),
    ("get", "ksn get [--derived] <FQN>", "Print the source of a definition.", "ksn get demo.main"),
    ("sig", "ksn sig <FQN>", "Print the signature of a definition.", "ksn sig demo.main"),
    ("doc", "ksn doc [--std] [--private]", "Print the package documentation.", "ksn doc --std"),
    ("index", "ksn index [--std]", "List the definitions of the package.", "ksn index"),
    ("unsafe", "ksn unsafe [--json]", "List [unsafe] modules, manual Send/Sync impls, unsafe blocks, externs and raw-pointer types of the package and its dependencies.", "ksn unsafe --json"),
    ("refs", "ksn refs <FQN>", "List the uses of a definition.", "ksn refs demo.main"),
    ("rename", "ksn rename [--dry-run] <old> <new>", "Rename a definition everywhere.", "ksn rename --dry-run demo.a demo.b"),
    ("bindgen", "ksn bindgen <file> | ksn bindgen --c <header.h> [--prefix NAME] [--link LIB] -o <out.ksnh>", "Generate FFI bindings.", "ksn bindgen --c x.h -o x.ksnh"),
    ("lsp", "ksn lsp", "Run the language server on stdin/stdout.", "ksn lsp"),
    ("debug", "ksn debug <args>", "Run the interpreter debugger.", "ksn debug"),
    ("dap", "ksn dap", "Run the debug adapter on stdin/stdout.", "ksn dap"),
    ("kir", "ksn kir <FQN> [--opt] [--run|--native <args>... | --llvm]", "Show or run the KIR of a function.", "ksn kir demo.main --llvm"),
    ("update", "ksn update [<dep>]", "Update dependencies.", "ksn update"),
    ("lock", "ksn lock --accept <dep>... | --accept --all | --diff | --upgrade", "Manage kasane.lock.", "ksn lock --diff"),
    ("vendor", "ksn vendor", "Copy dependencies into the package.", "ksn vendor"),
    ("git", "ksn git tags <url>", "Git helpers.", "ksn git tags https://example.com/x.git"),
    ("registry", "ksn registry index <registry> <name> [--offline]", "Registry helpers.", "ksn registry index ./reg demo"),
    ("publish", "ksn publish --registry <url-or-path>", "Publish the package.", "ksn publish --registry ./reg"),
    ("yank", "ksn yank [--undo] --registry <url-or-path> <name> <version>", "Yank (or un-yank) a version.", "ksn yank --registry ./reg demo 0.1.0"),
];

pub fn overview() -> String {
    let names: Vec<&str> = HELP.iter().map(|h| h.0).collect();
    format!(
        "usage: ksn <command> [args]\ncommands: {}\n`ksn help <command>` or `ksn <command> --help` shows one command. Package commands run in the package directory or any subdirectory (kasane.toml is searched upward).",
        names.join(" ")
    )
}

pub fn text(cmd: &str) -> Option<String> {
    HELP.iter().find(|h| h.0 == cmd).map(|(_, usage, desc, ex)| format!("usage: {}\n{}\nexample: {}", usage, desc, ex))
}
