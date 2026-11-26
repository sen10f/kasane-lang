# 実装計画メモ: docs/spec-provisional

担当: サブエージェント (ドキュメントのみ)
ブランチ: docs/spec-provisional
作業ツリー: /home/kasane/kasane-wt/spec-prov

## やること

1. `docs/spec-provisional.md` を作成。`Kasane 簡易仕様書.md` 8 章の方針表のうち「暫定」の 15 行それぞれについて、現在の文言（引用）、実装の現状（/home/kasane/kasane-verify/bin/current/ksn で動作確認、KSN_CACHE_DIR=/home/kasane/coord_tmp/specprov_cache）、AGENTS.md のオーナー決定、未解決の問い、2〜3 つの選択肢と帰結、推奨を記す。実装済みでオーナー決定済みと見なせる行は「確定してよい」と明記。仕様表自体は変更しない。
2. 仕様に「Edition 1 の範囲（凍結対象・実験的対象）」の短い節を edition 文言の近くに追加。BETA.md にも「何が入る / 何が実験的か」を追加。
3. `docs/plan/Edition1_凍結の点検.md` を 0.10.45 向けに書き直し。範囲は checker + VM + LLVM（言語、コア std、check/run/test/build/fmt/fix/LSP）。ui/・WASM SPA・レジストリ/publish・セルフホスト・bindgen は実験的で範囲外。各項目を done / open / 暫定行で blocked のいずれかに分類。
4. `python3 -I spec/tools/check_all.py` と `python3 tests/doc_examples.py --ksn /home/kasane/kasane-verify/bin/current/ksn` を実行。
5. 変更をコミット。

## 制約

- コードは変更しない（ドキュメントのみ）。stage0 はビルドしない。
- コミットは仕様ツリー内のみ。/home/kasane/kasane にはコミットしない。
- `git stash` は使わない。プロセスを名前で一括停止しない。
