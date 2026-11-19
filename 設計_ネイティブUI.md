# 設計: ネイティブで動く UI (gpui の方向)

2026-11-19。利用者の決定: Linux・macOS・Windows のすべてで、Kasane で書いた UI がネイティブ (インタプリタへの退避なし) で動くようにする。gpui (Zed の GPU で描く UI) を想定する。

## 1. 今の状態 (2026-11-19 の調査)

- ネイティブのビルドは単体の `ksn` (LLVM と lld を組み込み、実行時の C はビットコードで埋め込み) で、利用者の側に clang は要らない (2.10da・2.10db)。対象は x86_64 / aarch64 の Linux と wasm32 / wasm32-wasi。
- 全 53 パッケージのうち 49 がネイティブになる (`~/kasane-verify/agents/lower-survey/report.md`)。本当に下ろせないのは `examples/ffi/shapes` だけで、理由は UI の C の API に必須のもの:
  - `layout c` の構造体をポインタ・値で渡す `extern` の型、構造体へのポインタ (`*mut T`)
  - 固定長の配列 (`[u8; 256]`。`core.fmt.Formatter` も同じ理由で下りない)
- FFI: `extern` + `link "lib:sym"` (dlopen)、`layout c`、x86_64 SysV の値渡し、取り込みの無いコールバック。
- 線形型 (`linear`) は GPU の資源 (必ず解放する取っ手) に向く。
- 欠けているもの: 共有の持ち主 (`Rc` 相当)、利用者のデータを添えたコールバック (取り込みのある閉包 → C の関数ポインタ + `void*`)、C の見出しからの `extern` の生成、macOS / Windows の対象、C のライブラリを実行ファイルに静的につなぐ手段。

## 2. 段

| 段 | 中身 | 依存 |
| --- | --- | --- |
| N0 | ネイティブの FFI を完成: `layout c` の構造体の値渡し・ポインタ渡し・戻り値、固定長の配列、構造体へのポインタ、`aarch64` の AAPCS64 の値渡し。`examples/ffi/shapes` と `core.fmt.Formatter` がネイティブになる | なし |
| N1w | Windows 対象 `x86_64-windows-gnu` (mingw-w64 の import ライブラリ、lld の COFF、`sysrt.c` の Windows の枝)。試験は wine | なし (N0 と並行) |
| N1m | macOS 対象 `aarch64-apple-darwin` / `x86_64-apple-darwin` (lld の Mach-O、SDK の `.tbd`)。試験は Mac の実機 (利用者が貸す。i9 の検証機と同じく ssh で入る) | 利用者の Mac |
| N2 | `ksn bindgen --c <header.h>`: libclang (bindgen のときだけ dlopen) で C の見出しから `extern`・`layout c`・定数を生成 | N0 |
| N3 | 利用者のデータを添えたコールバック (閉包 → 踏み台 + `void*`)、`Rc` 相当の共有の取っ手 (またはアリーナの標準ライブラリ)、C のライブラリの静的なつなぎ (`kasane.toml` の `[native]`) | N0 |
| N4 | 試作: SDL3 (窓・入力、3 つの OS) + wgpu-native (GPU、Vulkan / Metal / D3D12) + FreeType / HarfBuzz (文字) で、四角と文字とボタンを描く。インタプリタへの退避が 0 であることを確かめる | N0・N2・N3 |
| N5 | UI の枠組み (`kasane-ui`): 取っ手と状態、配置の計算 (flexbox を Kasane で)、描画のリスト、文字の入力、IME | N4 |

## 3. 選んだもの (2026-11-19、利用者の決定)

- GPU は wgpu-native (WebGPU の C の API)。1 つの API で Vulkan / Metal / D3D12 を覆える。OS ごとに書くより桁で小さい。
- 窓と入力は SDL3 (C、3 つの OS、IME と高 DPI に対応)。後で OS ごとの層に替えられる形にする。
- 文字は FreeType + HarfBuzz (Linux / Windows)、macOS も当面は同じ (Core Text は後)。

## 4. 確かめ

- 各段で `ksn run` とネイティブの出力の一致 (既存の試験の方針)。N4 以降は描画の結果を画像で比べる (ヘッドレスの描画)。
- Windows は wine、macOS は実機 (または利用者の CI)。


> 2026-11-19 (N1w): Windows ターゲット `x86_64-windows-gnu` を追加した (組み込みの lld の MinGW 版でリンク、wine で `ksn run` と出力が一致)。詳細は `実装計画.md` の N1w の節。
>
> 2026-11-19 (試験先行): Win64 の構造体の値渡し (`stage0/tests/windows_ffi_struct.rs`) と C からの利用者のデータつきコールバック・捕まえたクロージャ (`stage0/tests/ffi_userdata.rs`・`stage0/tests/data/ffi_userdata/`) の試験を先に置いた。どちらも未実装なので `#[ignore]` (実装されたら外す)。
