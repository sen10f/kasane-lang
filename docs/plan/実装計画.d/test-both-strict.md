### 2.10?? `ksn test --both --native-required` と E9040 の利用者向けの文

- `ksn test --both --native-required`: ネイティブに下ろせずバンドルへ退避するテストを、退避して走らせずに失敗にする (理由つき)。`--both` が要る。フラグなしは従来どおり。
- E9040 の本文 (`ksn build --release` / `--emit`・`format_reasons`) を利用者の言葉に直す (`user_reason_text`)。内部の文言 (段階 1・型表・KIR) は残さず、構文名と関数・行を出し、直し方が決まっている形 (参照の受け手・連鎖した `stdout().flush()`) には回避策を添える。コードは E9040 のまま。
- 試験: `stage0/tests/test_both_strict.rs`。チートシートに 2 行。
