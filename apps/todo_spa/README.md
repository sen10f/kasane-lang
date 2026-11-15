# todo_spa: TODO の SPA (Kasane → wasm32)

修正プラン 5-3 の成果物。`apps/todo` のサーバ (`GET/POST /todos`、`GET/PATCH/DELETE /todos/<id>`) を、ブラウザから使う画面。
本体は Kasane で、`ksn build --target wasm32-unknown-unknown` の `todo_spa.wasm`。ブラウザの API は `ksn bindgen` が生成した
`web/dom.mjs` を通して呼ぶ (定義は `bindgen/dom.bindgen`、生成先は `src/todo_spa/dom.ksnl` と `web/dom.mjs`)。

## 構成

- `src/todo_spa.ksnl`: 入口 `todo_spa.main` と、JS から呼ばれる `ksn_event` (クリックなど) と `ksn_fetch_done` (fetch の完了)。
- `src/todo_spa/app.ksnl`: 画面の組み立て、操作、サーバとの同期。状態は持たない。
- `src/todo_spa/model.ksnl`: サーバの JSON から一覧の行への変換、行の読み書き、JSON の文字の逃がし。
- `src/todo_spa/dom.ksnl`: 生成物 (`ksn bindgen bindgen/dom.bindgen`)。直接編集しない。
- `web/boot.mjs`、`web/index.html`: 読み込みと起動。

## 動き

- 正はサーバ。localStorage の `todo_spa.items` は最後に受け取った一覧の写し (1 行 1 件: `id<TAB>完了(0/1)<TAB>題`)。
  起動のときは写しをすぐ出し、そのあとサーバの一覧で取り直す。
- 追加・完了の切り替え・削除は、サーバへ送ってから一覧を取り直す (`ksn_fetch_done` で完了を受ける)。
- サーバの接頭辞は localStorage の `todo_spa.api` (空なら同じ場所。ブラウザで別の場所のサーバを使うときに設定する)。
  apps/todo のサーバは CORS の見出しを返さないので、ブラウザでは同じ場所 (リバースプロキシなど) から配る。
- 上限: 一覧の本体と保存は 16 KiB まで。題の入力は 512 バイトまで (超えると空として扱われる)。

## 作り方と確かめ方

```
cd apps/todo_spa
ksn check && ksn bindgen bindgen/dom.bindgen        # 生成物を更新 (通常は不要)
ksn build --target wasm32-unknown-unknown --release  # target/wasm32-unknown-unknown/release/todo_spa.wasm
```

`todo_spa.wasm` と `web/` を同じ場所に置いて配る (`index.html` を開く)。サイズは `.wasm` と JS・HTML を合わせて 500 KB 以下。

node の上での確かめ (`tools/wasm/spa_check.mjs`、偽の DOM は `tools/wasm/fake_dom.mjs`) は `stage0/tests/wasm_spa.rs` が
`apps/todo` のサーバを起動して行う。
