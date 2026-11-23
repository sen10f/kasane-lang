# 構造体リテラル・パターンの 1 行 1 欄 (2026-11-23、spec/struct-literal-lines)

- 正規形: `T {` の行末で改行、欄は字下げ +4 で 1 行ずつ・`,` 無し、`}` は `{` の行の字下げ (後ろに `)` `?` `=>` が続いてよい)。
- 実装: `parser.rs` (リテラル・パターンとも改行区切りを読む。旧い `,` も読める = fmt が直す)、`fmt_struct.rs` (`ksn fmt` が旧い形を書き直す)、`selfhost/parse` (expr・pat)。
- E0053 は廃止 (rustisms の検出・`surface_fix` の畳み・diag ケースを削除。explain に廃止と書く)。`fmt_hoist` の構造体括り出しも削除。
