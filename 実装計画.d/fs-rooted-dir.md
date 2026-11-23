### 2.10?? std.fs の根つきのディレクトリ (B6)

Mythos-scale の報告 B6 への対応。生成したサーバが `..` と symlink で根の外のファイルを配り、読みの上限を TOCTOU で超えた問題を、標準ライブラリの形で塞ぐ。

- `std.fs.Dir` (`linear`、`descriptor: i32`。`File` と同じ設計) を追加。`Dir.open(path)` はディレクトリだけ開く (違えば ENOTDIR)。`Dir.close(self)`。
- `Dir.open_file_beneath(self: &Dir, rel: &str)`: `rel` は相対だけ。空・`/` 始まり・NUL・`..` の成分は InvalidInput。途中・最後のどの成分の symlink も拒む (ELOOP)。Linux (x86_64・aarch64) は `openat2` の `RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS`、カーネルに無い (ENOSYS) ときと他の Unix は成分ごとの `openat(O_NOFOLLOW)`。Windows は Unsupported (黙ってリンクを辿らない)。実装は `src/fs_beneath.rs` (木走査・KIR の参照実行器・VM が共有) と `src/sysrt.c` (LLVM の C 実行系)、`src/wasm/wasi.c` は ENOSYS で失敗。
- `File.metadata(self: &File)`: `fstat` (開いた記述子から。パスを引き直さない)。木走査・ネイティブとも同じ (本体は Kasane、`stat_fd_raw` と `read` の下ろしを使う)。
- `File.read_to_end_limited(self: &mut File, max: usize) -> Result<Vec<u8>, _>`: `max` を超える分があれば Err。読むのは最大 `max+1` バイト。木走査・ネイティブとも同じ (本体は Kasane、`stat_fd_raw` と `read` の下ろしを使う)。
- 効果は `fs.read` 相当 (`io.fs`)。
- 試験: `stage0/tests/fs_rooted.rs` (木走査で全件、ネイティブ debug で開く・拒む範囲の一致)、`fs_beneath.rs` の単体試験 (成分ごとの経路の symlink 拒否)。
