# rt-lto
- 原因: build.rs の実行時ビットコードは clang -O0 で作られ全関数に optnone+noinline が付き、L2 のリンクを効かせてもユーザ側へインライン展開されなかった。`-Xclang -disable-O0-optnone` で解消 (ビットコードは未最適化のまま、ユーザと同じパイプラインで最適化)。
- ホストの ELF では -ffreestanding/-fno-builtin は元から付いていない。wasm/darwin のビットコードと従来の分離経路の -O1 には残す (libc 無し・memcpy 実装が自分を呼ぶのを避けるため)。
- null_pointer_is_valid 9 か所: 全部残す (core.ptr.read(null) を本物の fault にする方針。実行時の C 側は変更なし)。生ポインタ/FFI を通る可能性のない手書きの入口・包みは外せるが利得未測定。
- 計測: HTTP 例 91224→84856 B。micro は load が高く ±。
