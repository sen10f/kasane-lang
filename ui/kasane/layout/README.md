# ui.layout — フレックスボックス部分実装のレイアウトエンジン

Kasane で書かれた、フレックスボックスの部分集合を実装する純粋レイアウトエンジンです。

パッケージ: `ui_layout` / モジュール: `ui.layout`

## 特徴

- 平らなノード配列とインデックス（ハンドル）による木構造
- `flex_direction`, `justify_content`, `align_items`
- `flex_grow` / `flex_shrink`
- 固定 / 最小 / 最大 幅・高さ
- `padding`, `margin`, `gap`
- ネスト（子ノードによる再帰レイアウト）
- 全長を `f32` で扱い、`uses [] panics never`（再帰関数は `stack 4096`）
- 配列の確保を行う操作のみ `uses [alloc] panics may`

## モジュール構成

```
ui.layout
├─ FlexDirection          Row | Column
├─ JustifyContent         Start | End | Center | SpaceBetween
├─ AlignItems             Start | End | Center | Stretch
├─ Rect                   x, y, width, height (f32)
├─ Size                   w, h, mnw, mxw, mnh, mxh (Option<f32>)
├─ Space                  pad, mar, gap (f32)
├─ Style                  dir, jus, ali, gr, sr, size, space
├─ Node                   style, rect
└─ Tree                   nodes + kids, レイアウトの入り口
```

## API

### `ui.layout.Tree`

レイアウト対象の木。ノードは `push_node` で追加し、`push_child` で親子関係を結びます。

- `fn ui.layout.Tree.new() -> ui.layout.Tree uses [] panics never pub` — 空の木を作る
- `fn ui.layout.Tree.push_node(self: &mut ui.layout.Tree, style: ui.layout.Style) -> usize uses [alloc] panics may pub` — ノードを追加し、そのインデックスを返す
- `fn ui.layout.Tree.push_child(self: &mut ui.layout.Tree, parent: usize, child: usize) -> () uses [alloc] panics may pub` — `child` を `parent` の子として登録する
- `fn ui.layout.Tree.child_count(self: &ui.layout.Tree, node: usize) -> usize uses [] panics never` — 子の数
- `fn ui.layout.Tree.child_at(self: &ui.layout.Tree, node: usize, i: usize) -> Option<usize> uses [] panics never` — i 番目の子のインデックス
- `fn ui.layout.Tree.node_at(self: &ui.layout.Tree, index: usize) -> Option<ui.layout.Node> uses [] panics never` — ノードを取得
- `fn ui.layout.Tree.rect(self: &ui.layout.Tree, index: usize) -> ui.layout.Rect uses [] panics never pub` — レイアウト後の矩形
- `fn ui.layout.Tree.layout(self: &mut ui.layout.Tree, root: usize, avail_w: f32, avail_h: f32) -> () uses [] panics never pub` — `root` を根に `avail_w × avail_h` でレイアウトを行う

### `ui.layout.Style`

ノードのレイアウト設定。`ui.layout.Style.default()` で初期値（`Row`, `Start`, `Start`, grow/shrink 0, すべて未指定, pad/mar/gap 0）を得ます。

- `dir: ui.layout.FlexDirection` — 主軸の方向
- `jus: ui.layout.JustifyContent` — 主軸沿いの配置
- `ali: ui.layout.AlignItems` — 交差軸沿いの配置
- `gr: f32` — `flex_grow`（余りを按分する割合）
- `sr: f32` — `flex_shrink`（はみ出しを按分する割合）
- `size: ui.layout.Size` — 幅/高さとその最小・最大
- `space: ui.layout.Space` — `padding`, `margin`, `gap`

### `ui.layout.Size`

- `w, h: Option<f32>` — 固定幅/固定高さ（`None` で未指定）
- `mnw, mxw, mnh, mxh: Option<f32>` — 最小/最大幅・高さ

### `ui.layout.Space`

- `pad: f32` — パディング（利用可能領域を縮小し、子ノードを `pad` 分オフセットする）
- `mar: f32` — マージン（各子の周り）
- `gap: f32` — 子ノード間の隙間

### 列挙型

- `ui.layout.FlexDirection`: `Row`, `Column`
- `ui.layout.JustifyContent`: `Start`, `End`, `Center`, `SpaceBetween`
- `ui.layout.AlignItems`: `Start`, `End`, `Center`, `Stretch`

### `ui.layout.Rect`

レイアウト結果の矩形: `x, y, width, height: f32`。

## 使い方の例

```kasane
let mut tree = ui.layout.Tree.new()
let mut root_style = ui.layout.Style.default()
root_style.ali = ui.layout.AlignItems.Stretch
let root = tree.push_node(root_style)

let mut child = ui.layout.Style.default()
child.size.w = Some(100.0f32)
child.gr = 1.0f32
let c = tree.push_node(child)

tree.push_child(root, c)
tree.layout(root, 300.0f32, 200.0f32)
let rect = tree.rect(c) // ui.layout.Rect { x: 0.0, y: 0.0, width: 300.0, height: 200.0 }
```

## テスト

`src/ui/layout.ksnl` に `test` ブロックを 16 件同梱し、`ksn test --both` でインタープリタとネイティブバンドルの両方で一致を確認しています。

```
ksn test --both
```

## 注意

- 構造体リテラルは 1 行に収める必要があり（100 文字以内）、ネストした `Size` / `Space` を介して `Style` を構成しています。
- `Tree` の再帰レイアウトは `stack 4096` で深さを宣言しています（深さは保証されず、超過は終了コード 102）。
- `padding` はコンテンツボックスを縮小する**と同時に子ノードを `pad` 分オフセット**する（子は `x + pad, y + pad` から始まる）。`margin` は各子自身をコンテンツボックス内でオフセットする。
- 確保を伴う `push_node` / `push_child` は `uses [alloc] panics may` で、メモリ不足は `panic` します。
