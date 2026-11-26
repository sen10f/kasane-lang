# 設計: macOS 交叉ビルド (aarch64-apple-darwin / x86_64-apple-darwin) の準備

2026-11-20。Linux から macOS 向けのネイティブビルドを準備する。実際の Mac は無いので、目的ファイル (`.o`) までの確認とリンク失敗の理由だけを確かめ、実行は行わない。macOS SDK (`KSN_SYSROOT`) はこの機械に無いので、リンクを伴う試験は SDK があるときだけ動き、無ければ黙って飛ばす。

## 1. 対象ターゲット (`stage0/src/kir_llvm.rs`)

- `TARGETS` に `aarch64-apple-darwin` と `x86_64-apple-darwin` を追加 (計 7 件)。
  - `aarch64-apple-darwin`: datalayout `e-m:e-p:64:64-i64:64-i128:128-n32:64-S128`、ptr_bits 64、sysroot 空。
  - `x86_64-apple-darwin`: datalayout `e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128`、ptr_bits 64、sysroot 空。
- `is_macos(t)`: `t.triple.contains("darwin")`。
- `target_of("aarch64-apple-darwin")` / `target_of("x86_64-apple-darwin")` で `Some(Target)` を返す (`TARGETS` の `name` と一致)。
- `--emit obj <ファイル>` や `ksn build --target <triple>` で受け付ける (`cli.rs` の `target_of` 分岐)。

## 2. C の ABI (`stage0/src/kir_cabi.rs`)

- `Arch::Aarch64Apple` 追加。構造体の値渡しは AAPCS64 と同じ (`Arch::Aarch64` と同一の `classify` 分岐)。`x86_64-apple-darwin` は `Arch::X86_64` (SysV) を使う。
- `emit_program` の arch 選択: macOS かつ `aarch64` で始まる三つ組なら `Arch::Aarch64Apple`。

## 3. 組み込み lld の Mach-O ドライバ (`stage0/src/native/lld.rs`、`lld_shim.cpp`)

- `Flavor::MachO` 追加 (`ld64.lld`)。`lld_link` の flavor マッチに `Flavor::MachO => 3` を追加。
- `darwin_args(name, root, objs, exe, nat)`: `aarch64-apple-darwin` → `arm64`、`x86_64-apple-darwin` → `x86_64`。SDK (`root`、`KSN_SYSROOT`) が無い、または `usr/lib` / `crt1.o` / `crti.o` / `crtn.o` が無ければ `KSN_SYSROOT` を名指しして失敗。`-arch`・`-platform_version macos 14.0 14.0`・`-syslibroot`・`crt1.o`/`crti.o`・`-lSystem`・`crtn.o` を組む。
- `lld_shim.cpp`: `LLD_HAS_DRIVER(macho)` を追加し、`flavor == 3` で `lld::DriverDef{lld::Darwin, &lld::macho::link}` を返す (`lld::MachO` は LLVM 20 には無く `lld::Darwin` が正)。
- `build.rs`: `liblldMachO.a` を `/usr/lib/llvm-20/lib/` から直接リンク (`--libs` の `machodriver` component を入れるとリンク順序が変わり既存ドライバが未解決になるため、パス指定に留める)。

## 4. ビルド (`stage0/src/cli.rs`、`stage0/build.rs`)

- `cmd_build` の `--emit obj <ファイル>`: ネイティブを作らずリンクも行わず、LLVM IR から目的ファイル (`.o`) だけを書き出す (`embed_llvm::ir_to_object`)。macOS 向けは Mach-O (`cf fa ed fe`)、Linux 向けは ELF。entry 点が必要。
- `clang_ll_embedded` の macOS 分岐: `is_macos` なら `darwin_args` + `lld_link(Flavor::MachO)`。リンク前に `KSN_SYSROOT` が無いと早々に失敗し、SDK の設定を促す。
- `build.rs` `gen_assets()`: macOS 用の実行時の C ビットコードジョブを追加 (`aarch64-apple-darwin` / `x86_64-apple-darwin` × `kir_rt` / `sysrt`、`KSN_SYSROOT` の `include` があるときだけ)。SDK が無ければ飛ばす。

## 5. 実行時の C (`stage0/src/sysrt.c`)

- `#if defined(__APPLE__)`:
  - `ksn_rt_sys_wall_ns`: `gettimeofday` (macOS は `CLOCK_REALTIME` が古い版で無いことがある)。
  - 単調時計: `mach_absolute_time` + `mach_timebase_info` (`mach/mach_time.h` を include)。

## 6. 試験 (`stage0/tests/macos_target.rs`)

- `macos_obj_is_macho`: `--target <t> --emit obj <file>` で作った `.o` の先頭 4 バイトが `cf fa ed fe` (Mach-O `MH_MAGIC_64`) であることを両ターゲットで確かめる (SDK 不要)。
- `macos_link_needs_sdk`: `KSN_SYSROOT` なしで `ksn build --target x86_64-apple-darwin` すると失敗し、標準エラーに `KSN_SYSROOT` を含む。
- `macos_link_with_sdk_when_present`: `KSN_SYSROOT` (macOS SDK の `usr/lib` があるパス) が設定されていればリンクまで通し、実行ファイルが Mach-O であることを確かめる。SDK が無ければ黙って飛ばす。
- 本物の Mac での実行確認は行わない (docs/design/設計_macOS.md の本項・実行は Mac 実機 / 利用者の CI)。

## 7. 回帰基準

- `examples/` の全パッケージと `bench/tasks/*/reference.ksnl` の参照解が診断 0 件のまま。
- `cargo check --release --tests` が通ること。
- `cargo test --release --test macos_target` が通ること (SDK なしでも 3 テストが pass / skip)。

## 8. まだやっていないこと (本物の Mac が必要)

- macOS 実機での `ksn run` と出力の一致 (設計プランの N1m)。利用者が貸す Mac / ssh で入る。
- `Xcode` の SDK (Command Line Tools) のパスを `KSN_SYSROOT` に指定したリンクの実行確認。
