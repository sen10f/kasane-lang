### 2.10?? 標準ライブラリの場所に作業ディレクトリを使わない (sec2-X2)
`project::stdlib_dir()` は作業ディレクトリとその親の `stdlib/`・`spec/stdlib` を探していた。落としてきたリポジトリの `stdlib/` が標準ライブラリになり、`ksn check` だけで副作用が起きた。今は `KSN_STDLIB`、実行体の隣の `stdlib/`、実行体の親をたどった先の `spec/stdlib` (開発の配置)、埋め込みの順だけ。試験は `tests/sec2_stdlib_dir.rs`。

実行体の親をたどる探索は、開発の配置そのものだけに絞った: 実行体が `<root>/stage0/target/<profile>/ksn` か `<root>/stage0/target/<三つ組>/<profile>/ksn` で、その `<root>/spec/stdlib` があるときだけ使う。ほかの祖先の `spec/stdlib` は見ない。理由: 実行体を落としてきた木の中に置くと、その木の `spec/stdlib` が標準ライブラリになり、作業ディレクトリと同じ乗っ取りが残るため。実行体の隣の `stdlib/` (導入した配置) は残す。試験は単体で置いた ksn が、`stdlib/` と `spec/stdlib/` に悪意のある定義を置いた作業ディレクトリの下で `check` / `run` しても、副作用のファイルが作られないことを確かめる。
