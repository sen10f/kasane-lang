# fix/syntax-all-errors

構文エラーを定義ごとに全部報告する。`program::load_file_all` が定義ごとの誤りを集め、`Source.all_errors` に持つ。check/fmt は全件を出し、rustisms の指摘は誤りのある定義の行に絞る (正しい定義の偽の E0057/E0055 を消す)。試験は `tests/syntax_all.rs`。
