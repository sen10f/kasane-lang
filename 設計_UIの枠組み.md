# 設計: Kasane UI の枠組み (N4 試作・N5 framework)

2026-11-19。前提は `設計_ネイティブUI.md` の決定 (SDL3 / wgpu-native / FreeType + HarfBuzz、対象 Linux・Windows・macOS) と
`Kasane 簡易仕様書.md` 0.10.32・`チートシート.md`。gpui の方針: 状態は保持し、元素木は毎フレーム組み直す
(retained-state + immediate-style element tree)。この文書は N4 (試作) と N5 (`kasane-ui`) の設計とする。

## 1. 全体像

```
利用者の app (純粋な組み立て + 表示の関数)
  └ ui.tree / ui.view / ui.event / ui.state ......... 元素木・差分・当たり判定・フォーカス (全部純粋)
      ├ ui.layout ....... flexbox のサブセット、位置と大きさだけを計算 (純粋・決定性)
      ├ ui.text ......... FreeType ラスタ + HarfBuzz 成形・行分割・字送り (純粋・決定性)
      └ ui.plat ......... SDL3: 窓・入力・IME・クリップボード・DPI (uses [display])
  └ ui.gpu ............. wgpu-native: 四角・角丸・影・グリフ・画像の GPU (uses [gpu])
      └ ui/native/ ..... C の薄い橋 (libksnui): SDL3 / wgpu / FT / HB を平坦な C の API に包む
```

所有は 1 方向で、共有はしない。**`ui.Ctx` が窓・GPU の装置・アリーナ・状態を唯一の持ち主として持ち、
利用者は Copy の取っ手 (u32 の番号) だけを触る**。`Rc` 相当 (N3 の共有の取っ手) は設計に要らない
(複数窓で device を共有する場合も `Ctx` が 1 つ持ち、窓は取っ手)。窓は 1 つに限らないが、所有の根は常に 1 個。

純粋な境界: `Ctx.pump` の出口で生の SDL の事象は Kasane の Copy の `ui.Event` 列に写す。その上 (木・配置・
成形・当たり判定・描画リスト) は `uses [] panics never` で動き、入力と資源の出入りだけが効果を持つ。これが
「インタプリタとネイティブの一致」を安くする。

## 2. 層ごとの設計

### 2.1 platform (`ui.plat`、SDL3)

- 中身: 窓の生成と大きさ、`SDL_PollEvent` の一巡の列 (キー・ポインタ・ホイール・窓のリサイズ・
  文字入力・編集 (IME の変換中)・窓を閉じる)、IME の開始と終了と編集の位置、クリップボードの読み書き、
  表示の縮尺 (`SDL_GetDisplayContentScale`) を論理 px へ。**SDL のコールバックは登録しない** (事象は
  飲み込み (polling) で回す)、macOS の主スレッド規制も守る。
- 効果: 新しい葉 `display` を提案 (下 6-1)。暫定では `io` に読み替えても動く。
- C の橋: `ksnui_window_*` / `ksnui_poll_event(ptr, cap) -> usize` / `ksnui_text_input_start(rect)` /
  `ksnui_clipboard_read(buf, cap) -> usize` / `ksnui_display_scale() -> f32`。スカラーとポインタだけで書く
  (構造体の値渡しに頼らない: N0 前でも動く)。
- Kasane の特徴: 事象を Copy の `enum ui.Event` に写すので、上は全部純粋になり再現できる。参照は二級なので
  窓の取っ手は `type ui.Window` (中身は非 pub の u32) にして、`*mut c_void` は直接持ち出させない。

### 2.2 GPU (`ui.gpu`、wgpu-native)

- 中身: 橋 (`ksnui_gpu_*`) が (i) `requestAdapter`・`requestDevice`・swapchain の作成を**同期風**に包む
  (捕獲のないトップレベル関数をコールバックに登録し、結果は橋のグローバルに置いて `wgpuDevicePoll(dev, true)` で
  待つ。捕獲のないコールバックは今の仕様 3.8 で渡せるので N3 は要らない)、(ii) 装置の喪失と捕まえられなかった
  誤りのコールバックも静的関数で理由をグローバルに書き、Kasane 側は毎フレーム `check_lost() -> Option<&str>` で
  拾う、(iii) 読み戻し (`mapAsync` の結果を同期でコピー)。
- 資源は**線形トークン + 資源表**: `type ui.gpu.Buffer linear { id: u32 }` のような薄い線形の型が Kasane 側の
  所有の札で、`close(self)` が札を消費して橋の資源表の番号を解放する。生ポインタは表の中 (Vec) にだけ置く。
  札を二重に close する・close 後で使うプログラムは借用と線形の検査 (E3002・E3003) で拒たれる。命令の符号化は
  `with enc = cb.begin(..) close core.mem.drop(queue.submit(enc.finish())) { ... }` の形で 1 回の提出にする。
- 描画: 1 フレームの四角 (場所・半径・色・影) とグリフ (場所・atlas の uv・色) を Copy の構造体の `Vec` に集め、
  1 本の頂点の列にまとめて 2 本のパイプライン (色付き角丸四角 / テクスチャ) に回す。WGSL は `static` の
  `&'static str` で埋め込む。パイプラインと atlas は作り直さず持ち回る。
- 表示: 生の窓の経路 (swapchain) とヘッドレスの経路 (画面を持たないテクスチャ) に同じ描画リストを流す。
- Kasane の特徴: 線形が資源の「ちょうど 1 回」を保証する。提出 (`gpu`) と符号化 (`uses []`) を分けるので
  描画リストの構築は純粋で、ネイティブも木走査も同じ結果を印字できる。`unsafe` を使うモジュールは
  `kasane.toml` の `[unsafe] modules = ["ui.plat", "ui.gpu", "ui.text"]` に閉じる。

### 2.3 文字 (`ui.text`、FreeType + HarfBuzz)

- 中身: 字の出現 (font id, 大きさ) をキーに HarfBuzz に成形させ、グリフの id・前進・位置の列を得る。FreeType で
  同じ大きさを CPU 側にラスタ化し 1024² の 8 bit 灰色の atlas (1 チャネル) に入れる。不足の字は候補の chain
  (既定・日本語・等幅) から探す。計測と行分割は**純粋**なので、成形結果は (font id, 大きさ, 文字) でキャッシュ
  する (`alloc.collections.HashMap`、`&str` の検索は `get_str` の形)。
- 行分割: UAX #14 のサブセットを Kasane に書く。日本語は「かな・漢字の間で割れる」「開き括弧の次・閉じ括弧の
  前・小さいかな・長音・禁則の後ろでは割れない」程度に留める (N4)。ラテンは空白と連字符で割る。
- Kasane の特徴: 成形・計測が純粋なので、ネイティブと木走査が同じ FreeType/HarfBuzz を見ている限り結果は一致
  する。ラスタは生のバイト列を `Vec<u8>` / `alloc.raw.Buf` に写してから atlas にする (FreeType が
  貸すメモリを指す参照は置けない)。
- N2 (bindgen) の効く所: FreeType/HarfBuzz の定数の書き出し。C の橋の見出しを bindgen で `extern` に
  下ろす (2.1・2.2・2.3 の `ksnui_*` を 1 回まとめて)。

### 2.4 配置 (`ui.layout`、flexbox のサブセット)

- 中身: Taffy のように node 列 (親・第 1 子・兄弟の番号) と `Style` (Copy の構造体) を持ち、
  `measure(子の列) -> Size` と `layout(root, 制約) -> ()` を反復で回す。対応は direction (row / column)・
  main と cross の配置 (start / center / end / spaceBetween)・子の割り (固定・flex grow)・gap・padding・
  `fitContent`・境界 (min / max)。絶対位置と重ね (absolute の overlay / popup) も番号で扱う。
  アスペクト比は採らない。
- 再帰は避ける (深さの上限と `stack N` を避ける)、禁則の判定は番号の走査で書く。座標は `f32` のまま
  (整数の演算は panic しうるので `panics never` の熱い道では `wrapping_*` / `checked_*` を使うか f32 で通す)。
- Kasane の特徴: 純粋で決定性がある。入出力は番号と構造体だけなので、一致試験は「配置の表を印字して比べる」で
  済む。依存がゼロなので、N0 / N2 / N3 を待たずに着手できる (下 5 の Z6)。

### 2.5 元素木・表示・状態 (`ui.tree`・`ui.view`・`ui.state`)

- 中身: 毎フレーム、利用者は `ui.Element` (種別・`ui.Style`・Copy の属性・子の列) を純粋に組み立て、
  `Ctx.commit(根)` でアリーナへ差分を適用する。同一性は「親の道 + 位置」と「利用者が付ける Copy の u64 のキー」
  のどちらかを 64 bit に折る (`shortcut: 衝突は N5 で世代つきの番号に直す。衝突が問題になるまで放置`)。
  `Box<dyn ui.View>` (dyn は `'static` 明示・`into_dyn`) で拡張の受け口を作る。線形は `Box` に入らないので
  資源の札は持てない (持つのは Copy の取っ手だけ)。
- 状態: 窓 1 つに 1 個の `Ctx` のアリーナに、
  (a) 組込の状態 (入力の文字列・変換中の文字列・選択の位置・稼働中の入力の取っ手) を enum で、
  (b) 利用者の表示の状態は `Box<dyn ui.ViewState>` で持つ。キーは Copy なので、扱いの関数は
  `fn(..) -> .. capture [move key] { .. }` の形で取っ手を値で捕まえられる (参照の捕獲はフィールドに
  入れられない制約と両立)。
- Kasane の特徴: 参照が二級なので「木の子を指す」は全て番号。状態の保存が面倒になる場所は、
  素の enum のバリアントに納めるか `Box<dyn>` に逃がす。

### 2.6 入力の配線とフォーカス (`ui.event`)

- 中身: 事象列を (1) 稼働中の入力 (テキスト・変換・キー) に渡す、(2) 残りは当たり判定に回す (塗りの逆順で
  layout の矩形と切り抜きを比べる)、(3) 稼働中の入力が変わったら focus の取っ手を更新する、の順で処理する。
  フォーカスは 1 個の番号と順序 (アリーナ順)。IME の候補の窓は focus された入力の矩形を使う。
  ポインタの捕え (スクロールバーを引く) は「捕えた取っ手と最初の位置」を `Ctx` が持ち、離すまで配線する。
- 事象は `layout c` にせず、橋の出口で Copy の enum に写す (再現のため)。

### 2.7 非同期 (`std.task`)

- 中身: UI の中身は主スレッドの 1 本で回し、非同期は**結果の配送だけ**。`std.task.spawn_local` と `block_on` は
  使わず、重い仕事は `std.thread.spawn` したスレッドが素のデータを `std.sync.channel` で送り、主ループは
  毎フレーム `recv` を空にして状態へ畳む。spawn の閉包は `'static` で参照を捕まえられないので、メッセージは
  所有のデータか Copy (文字列・番号・derive FromJson/ToJson の列挙) に限る。
- 将来: HTTP の読み込みを `uses [suspend]` の関数にして `block_on` に入れる形は、枠に `Result` を置けるので
  動く見込み。ただし UI の再入を防ぐため、中断してよいのは主ループの外 (起動前の読み込み) だけにまず限る。

## 3. Kasane の機能の利点と制約 (要約)

| 層 | 利点 | 制約 (設計での受け) |
| --- | --- | --- |
| platform | 事象を Copy の enum に写せて上は純粋 | コールバック型の API を避けて飲み込みで回す、macOS は主スレッド規制 |
| GPU | 線形が資源のちょうど 1 回の解放、`gpu` と `[]` で提出と構築を分離 | 資源表は橋の側、共有は `Ctx` の単一所有、装置の喪失は毎フレーム覗く |
| text | 成形が純粋で一致試験が安い | FreeType の貸すメモリを写す、フォント依存の決定性 |
| layout | 純粋・依存ゼロ・並列着手可 | 木は平坦な番号、再帰は `stack N` か反復 |
| tree / state | `Box<dyn ui.View>` の拡張、Copy キーの捕獲 | 参照を持てないので取っ手、線形を持てない |
| events / focus | 事象が素のデータなので再現と配線が単純 | 同期の取り回し、捕えの管理は `Ctx` |

## 4. N4 試作の仕様

### 4.1 作るもの (1 プログラム、1 窓)

- 窓 800×600 を開き、中の 1 フレームを描く (Vulkan 優先、Metal / D3D12 はそのまま)。`--headless` では画面を
  作らず後ろのテクスチャへ描き、N フレーム後に終了する (`--frames`、既定 10)。終了は ESC か窓を閉じる。
- 画面: 上に題 (日本語と ASCII 混在)、中央の札にテキスト入力 (日本語 IME・ラベル「名前」)・
  ボタン「クリック」とカウントの表示・下に 200 行の混在のリスト (ホイールとスクロールバー・切り抜き)。
- 操作: ポインタでボタンを押すとカウント、入力の確定文が即座に題の下に出る、リストが動く。`--script ファイル`
  で事象を順に与えられる (テキスト 1 行 1 命令、中身は `ui.Event` の印字)。乱れと時刻の依存は N4 では作らない
  (金色画像の比較のため)。

### 4.2 確かめること (すべて試験で)

1. **退避が 0**: `examples/ui` を `ksn check` (診断 0 件)・`ksn fmt --check`・`ksn test --both`・
   `ksn build --release` が全て通り、E9040・同梱バンドルにならない。ビルドした実行ファイルが同じ画像を出す。
2. **ヘッドレスの描画を画像で比べる**: 同じ script で 3 フレーム (初期・入力確定後・スクロール後) を読み戻して
   金色の PNG と比べる (全同一を目標、後端差のため許容誤差つきの比較にする)。Linux は lavapipe (ソフトの
   Vulkan) で生成、実 GPU も走らせて壊れないことを見る。Windows は wine で headless を 1 回、macOS は ssh の実機。
3. **インタプリタとネイティブの一致**: 同じ木・同じ script を `ksn run` (木走査) とネイティブで回し、
   配置の表 (番号・矩形)・切り抜きの結果・成形した字の列と前進・事象の後で印字した状態を比べてバイト一致。
   画像の一致は GPU の側の話なので、論理の一致は配置と描画リストのハッシュで取る。
4. **線形の資源**: Buffer / Texture / Pipeline を作って 1 回だけ close する經路が検査を通り、`stage0/tests/data/
   unsound/` に「二重に close する・close 後で使う」を 1 件ずつ足して拒まれることを確かめる。
5. **効果の裏付け**: `ui.plat` が `display`・`ui.gpu` が `gpu` を宣言し、木・配置・成形は `uses [] panics never`
   のまま (E4025 が増えすぎを黙らせない)。効果を書く前に暫定の `io` を使う場合は 6-1 の追従を残す。
6. **対象**: Linux 実機・wine (headless)・macOS 実機 (貸与マシン、ssh)。

## 5. タスク分解 (並列可能な単位)

規模: S = 1 日以内、M = 2〜3 日、L = 1 週間前後。「N2」「N3」はその仕事が要る段 (bindgen / 捕獲つきコールバック・
静的なつなぎ・共有の取っ手)。「N0 前でも動く」は壊れ・直しの値を避ける形なら着手できるという意味。

| 番号 | 内容 | 規模 | 依存 | N2 | N3 | メモ |
| --- | --- | --- | --- | --- | --- | --- |
| Z0 | C のライブラリの準備: SDL3・wgpu-native・FreeType・HarfBuzz を入れて橋 (library) を作る | M | なし |  |  | 機材の面。脚本を 1 個 (`ui/native/build.sh`) |
| Z1 | C の橋 `libksnui`: `ksnui.h` を書く (平坦な API・捕獲なしコールバック・同期の読み戻し・PNG) | M | Z0 |  |  | 全 API はスカラー/ポインタ・`*mut ksnui_Rect` |
| Z2 | `extern` と `layout c` の宣言 (`ui.plat`・`ui.gpu`・`ui.text` の 3 モジュール) | S | Z1 | N2 |  | bindgen ができれば原則 N2 で下ろす。手書きでも S |
| Z3 | `ui.plat`: 窓・飲み込みの事象・IME・クリップボード・DPI | M | Z2 |  |  | 出口で `ui.Event` に写す |
| Z4 | `ui.gpu`: 資源表・線形トークン・頂点の束ね・2 本のパイプライン・swapchain と画面なしの経路 | L | Z2 |  | N3 (静的なつなぎ) | headless の経路もここ。開発中は dlopen で回す |
| Z5 | `ui.text`: HarfBuzz 成形・FreeType のラスタ・atlas の出現・行分割 | M | Z2 | N2 |  | 純粋の部分に單体試験 |
| Z6 | `ui.layout`: flexbox のサブセット (平坦な node 列) | M | **なし** |  |  | 純粋なので N0 / N2 / N3 を待たず着手 |
| Z7 | `ui.tree`・`ui.state`: Element の組み立てと差分・状態の enum・`Box<dyn ui.View>` | M | Z6 |  |  | 取っ手と Copy のキー |
| Z8 | `ui.event`: 配線・当たり判定・フォーカス・捕え | M | Z7 |  |  | script の再生はここの副産物 |
| Z9 | `ui.textinput`・`ui.button`・`ui.list` の見た目の部品 | M | Z7, Z5, Z8 |  |  | 部品は元素を作るだけで GPU に触らない |
| Z10 | headless の固定 (読み戻し・PNG・金色画像・比較) と CI のつなぎ | M | Z4 |  | N3 (つなぎ) | Linux の lavapipe・wine・macOS の並び |
| Z11 | 一致試験 (配置・成形・状態の印字の比較、unsound 2 件) | S | Z6, Z7, Z3, Z8 |  |  | `stage0/tests/` に新しい分野のファイル |
| Z12 | N4 のデモ `examples/ui` を組む (4.1) と文書の整頓 | M | 全部 |  | N3 | `チートシート.md`・`Kasane 簡易仕様書.md`・`実装計画.md` 2 章を更新 |

依存の連なり: Z6 は単独で始められる (最大の並列)。Z7 ← Z6。Z8 ← Z7。Z9 ← Z7 / Z5 / Z8。
Z4・Z5・Z3 は Z2 の後に平行。Z10 ← Z4。Z11 ← Z6 / Z7 / Z3 / Z8。Z12 ← その他全部。
N5 (framework): 見た目の体系 (色と字の大きさのトークン)、切り抜きと絶対位置の一般化、順番 (tab) の制御、
動き (time の効果)、列表の仮想化、SDF の文字、複数窓、`kasane-ui` のパッケージ化と文書 — ここには見積もらない。

## 6. 未決・仕様の要請・リスク

1. **効果の葉を 2 つ足す** (仕様 5.1 の改): `display` (窓・入力・IME・クリップボード) と `gpu` (GPU の提出・
   表示・up 読み) を標準の階級に足す案。暫定は `io` に読み替えても動くが、意味が悪いので N4 の最初に決めたい
   (5.1 の表の改と `explain`・`実装計画.md` 2 章の追従)。自作の効果は許せないので足せない。
2. 資源表の取っ手の使い回し (close 後のハンドルを使う) は検査で塞げない。世代カウンタか N3 の共有の取っ手で
   後で直す (`shortcut` を 2.4 と 2.5 のコードに残す)。
3. headless の描く経路: Linux は lavapipe を要する (CI の準備)。macOS の headless は画面を作らない
   Metal の装置で試す (未確かめ、最初の課題)。wine と Vulkan は劣るので wine は起動の確認だけにする。
4. 金色画像はフォントの版に依存する。CI ではフォントの道を固定し、フォントを変えたら取り直す。
5. IME の検証は headless では難しい: script で「文字編集・確定」の事象を注ぎ、変換中の状態 (composition の
   文字列・下線・キャレット) と確定後の流れを印字で確認する。実機 (Linux の ibus/fcitx と macOS) は人の手で確認。
6. N1w / N1m (Windows・macOS の対象) と N3 の静的なつなぎが N4 の配布の前提。開発中は `examples/ffi/shapes`
   と同じようにローカルの `.so` で回せるので、対象の段を待たずに Z1〜Z6 は進められる。