### 2.10?? 標準ライブラリの場所に作業ディレクトリを使わない (sec2-X2)
`project::stdlib_dir()` は作業ディレクトリとその親の `stdlib/`・`spec/stdlib` を探していた。落としてきたリポジトリの `stdlib/` が標準ライブラリになり、`ksn check` だけで副作用が起きた。今は `KSN_STDLIB`、実行体の隣の `stdlib/`、実行体の親をたどった先の `spec/stdlib` (開発の配置)、埋め込みの順だけ。試験は `tests/sec2_stdlib_dir.rs`。
