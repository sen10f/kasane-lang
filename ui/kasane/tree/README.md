# ui.tree — 要素木の構築・差分更新、状態、ビュー合成

Kasane で書かれた UI 要素木（`ui.tree`）とその差分更新（diffing）、状態管理（`ui.state`）、`Box<dyn ui.state.ViewState>` / `Box<dyn ui.tree.View>` によるビュー合成のパッケージです。

パッケージ: `ui_tree` / モジュール: `ui.tree` + `ui.state` / 依存: `ui_layout` (`ui.layout`)

## 特徴

- アリーナ + プレーンインデックスの `Handle` による再帰的な要素木（`Vec<Box<Self>>` は `derive [Drop]` で拒否されるため不使用）
- `ui.tree.View` トレイトと `Box<dyn ui.tree.View>` によるビューの型消去合成（`into_dyn` で `Box<dyn>` へ）
- `ui.tree.Element`（描画対象の要素）と `ui.tree.Node`（木に保持される実ノード）の分離
- `ui.tree.Tree.commit` によるキー付き差分更新（挿入・削除・移動・キー付き再順序）
- `ui.state.Arena` + `ui.state.Handle` による per-node 状態のプレースホルダ管理（`remove` は穴を残し、`len` は確保済みスロット数のまま）
- `ui.state.ViewState` トレイトと `Box<dyn ui.state.ViewState>` によるユーザー状態の型消去
- 再帰関数は `stack 4096` で深さを宣言（深さは保証されず、超過は終了コード 102）
- 配列の確保を行う操作のみ `uses [alloc] panics may`；それ以外は `panics never`

## モジュール構成

```
ui.tree
├─ Element                  kind, style, key, children (Vec<usize>)
├─ Node                     kind, style, key, state: Option<Handle>
├─ StateEntry               value: Box<dyn ViewState + 'static>
├─ Tree                     nodes, kids, state: Arena<StateEntry>
├─ View                     render(sink) -> usize   (トレイト)
├─ ViewKind                 Text | Block | Leaf
└─ commit / new / push_node / push_child / child_count / node_at / state_ref / state_tick

ui.state
├─ Arena[T]                 slots: Vec<Option<T>>  (プレースホルダ・アリーナ)
├─ Handle                   index: usize pub
├─ State                    Count(u32) | Focused(bool)
├─ ViewState                tick(self: &mut Self)   (トレイト)
└─ Arena::new / insert / get / get_mut / remove / contains / is_empty / len
```

## API

### `ui.tree.Tree`

要素木そのもの。`new` で作り、`push_node` でノードを追加し、`push_child` で親子関係を結びます。`commit` で `ui.tree.Element` の列を木に反映（差分更新）します。

- `fn ui.tree.Tree.new() -> ui.tree.Tree uses [] panics never pub` — 空の木を作る
- `fn ui.tree.Tree.push_node(self: &mut ui.tree.Tree, kind: ui.tree.ViewKind, style: ui.layout.Style, key: Option<u64>) -> Result<usize, alloc.alloc.AllocError> uses [alloc] panics never pub` — ノードを追加し、そのインデックスを返す
- `fn ui.tree.Tree.push_child(self: &mut ui.tree.Tree, parent: usize, child: usize) -> bool uses [alloc] panics never pub` — `child` を `parent` の子として登録する（親が不明または確保失敗なら `false`）
- `fn ui.tree.Tree.child_count(self: &ui.tree.Tree, node: usize) -> usize uses [] panics never pub` — 子の数
- `fn ui.tree.Tree.node_at(self: &ui.tree.Tree, index: usize) -> Option<&ui.tree.Node> uses [] panics never pub` — ノードを取得
- `fn ui.tree.Tree.state_ref(self: &ui.tree.Tree) -> &ui.state.Arena<ui.tree.StateEntry, std.alloc.System> uses [] panics never pub` — 状態アリーナへの参照
- `fn ui.tree.Tree.state_tick(self: &mut ui.tree.Tree, h: ui.state.Handle) -> () uses [] panics never pub` — ハンドル指定の状態を `tick` する
- `fn ui.tree.Tree.commit(self: &mut ui.tree.Tree, root: usize, elements: &alloc.vec.Vec<ui.tree.Element, std.alloc.System>, root_elem: usize) -> Result<(), alloc.alloc.AllocError> uses [alloc] panics never stack 4096 pub` — `root` を根に `elements` の差分を木に反映する

### `ui.tree.View`

ビューの型消去インターフェース。`render` は `ui.tree.Element` を `sink` に書き込み、書き込み開始位置のインデックスを返します。

- `fn ui.tree.View.render(self: &Self, sink: &mut alloc.vec.Vec<ui.tree.Element, std.alloc.System>) -> usize uses [E] panics P pub`

`alloc.boxed.Box.into_dyn` で `Box<dyn ui.tree.View<[alloc], may> + 'static>` に包み、ビューを合成できます（`ui.tree_test.ViewBox` 参照）。

### `ui.tree.Element` / `ui.tree.Node`

- `Element`: 描画側が生成する一時的な要素。`kind: ViewKind`, `style: ui.layout.Style`, `key: Option<u64>`, `children: Vec<usize>`。
- `Node`: 木に保持される実ノード。`kind`, `style`, `key`, `state: Option<ui.state.Handle>`。

### `ui.tree.ViewKind`

- `Text`, `Block`, `Leaf`

### `ui.state.Arena[T]`

プレースホルダ・アリーナ。`insert` でスロットを確保し `Handle` を返し、`remove` でそのスロットを `None`（穴）にします。`remove` 後もスロットは再利用されず、`len` は確保済みスロット総数のままです。

- `fn ui.state.Arena.new(allocator: A) -> ui.state.Arena<T, A> uses [] panics never pub`
- `fn ui.state.Arena.insert(self: &mut ui.state.Arena<T, A>, value: T) -> Result<ui.state.Handle, alloc.alloc.AllocError> uses [alloc] panics never pub`
- `fn ui.state.Arena.get(self: &ui.state.Arena<T, A>, h: ui.state.Handle) -> Option<&T> uses [] panics never pub`
- `fn ui.state.Arena.get_mut(self: &mut ui.state.Arena<T, A>, h: ui.state.Handle) -> Option<&mut T> uses [] panics never pub`
- `fn ui.state.Arena.remove(self: &mut ui.state.Arena<T, A>, h: ui.state.Handle) -> Option<T> uses [] panics never pub`
- `fn ui.state.Arena.contains(self: &ui.state.Arena<T, A>, h: ui.state.Handle) -> bool uses [] panics never pub`
- `fn ui.state.Arena.is_empty(self: &ui.state.Arena<T, A>) -> bool uses [] panics never pub`
- `fn ui.state.Arena.len(self: &ui.state.Arena<T, A>) -> usize uses [] panics never pub`

### `ui.state.Handle`

プレーンインデックスのハンドル。`index: usize pub`。

### `ui.state.ViewState`

ユーザー状態のトレイト。`Box<dyn ui.state.ViewState + 'static>` として `StateEntry.value` に納めます。

- `fn ui.state.ViewState.tick(self: &mut Self) -> () uses [] panics never pub`

## 使い方の例

```kasane
let mut tree = ui.tree.Tree.new()
let root = tree.push_node(ui.tree.ViewKind.Block, ui.layout.Style.default(), None).unwrap()

// 子要素を構築
let mut elements = alloc.vec.Vec.new(std.alloc.SYSTEM)
// ... elements に ui.tree.Element を積む ...

// 差分更新（初回は構築、2回目以降は diff）
match tree.commit(root, &elements, 0usize) {
    Ok(()) => ()
    Err(_e) => ()
}

// 状態をハンドルで更新
let h = tree.state_ref().insert(ui.tree.StateEntry { value: alloc.boxed.Box.into_dyn(box_state) }).unwrap()
tree.state_tick(h)
```

## テスト

`src/ui/tree_test.ksnl` に 13 件の `test` を同梱し、`ksn test --both` でインタープリタとネイティブバンドルの両方で一致を確認しています。

カバー項目:
- 木の構築: `test_new`, `test_push_node_indices`, `test_push_child`, `test_commit_build`
- 差分更新: 挿入 `test_commit_insert`、削除 `test_commit_remove`、キー付き再順序 `test_keyed_reorder`
- 状態: ハンドル経由の更新 `test_state_insert` / `test_state_insert_two` / `test_state_remove` / `test_state_tick`
- ビュー合成: `Box<dyn ui.tree.View>` 経由 `test_view_box_dyn`, `test_composite_render`

```
ksn test --both
```

## 注意

- `Tree` の再帰 diff は `stack 4096` で深さを宣言しています（深さは保証されず、超過は終了コード 102）。
- `Node.state` は `Option<ui.state.Handle>` で、per-node の状態を持ちます。
- `push_node` / `push_child` / `commit` は確保を伴うため `uses [alloc]` で、メモリ不足は `Result::Err` として伝播します（`panic` しません）。
- `Arena.remove` はスロットを `None` にするだけで詰め直さず、`len` は確保済みスロット数のまま減りません。

## ハンドルの置き換え計画（Handle swap plan）

現在 `ui.state.Handle` はプレーンインデックス（`index: usize`）のプレースホルダ実装です。将来的に `std/arena-handle` ブランチで整備予定のジェネレーティブ `Handle<T>`（世代番号付きハンドル、ダングリング参照の検出）に置き換える予定です。

置き換えの方針:
1. `ui.state.Handle` を `Handle<T>`（ジェネレーティブ、型パラメータ `T` でアリーナの要素型と結びつける）に変更する
2. `Arena.insert` / `get` / `get_mut` / `remove` / `contains` のシグネチャを `Handle<T>` に合わせる
3. `Tree` 内の `Option<ui.state.Handle>` を `Option<ui.state.Handle<StateEntry>>` に合わせる
4. 呼び出し側（`tree_test` 等）の `Handle.index` アクセスを `Handle` 自体の操作に置き換える

この変更は互換を破るため、リリース単位でまとめて行います。
