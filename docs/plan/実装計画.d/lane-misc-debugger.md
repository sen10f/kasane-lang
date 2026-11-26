# 線 6: VM の単発の退避理由とデバッガの制限

- (a) 退避理由を 1 件ずつ潰す。試験は `stage0/tests/kir_native_gaps9.rs` (木走査 == VM == ネイティブ)。
  - 済: `with` の束縛に種類の手がかりを付ける (String の `clear`)。
- (b) デバッガの制限 (docs/design/設計_VM一本化.md 5 章): メソッド呼び出しの式・Vec/String の中身・unsafe・スレッド。未着手 (残り)。
