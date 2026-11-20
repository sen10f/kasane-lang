# bench: LLM ベンチ

LLM が Kasane を書けるかを測る。`run_bench.py` が OpenAI 互換 API (`/v1/chat/completions`) でモデルに課題 (`tasks.py`, `tasks_hard.py`) を解かせ、`ksn check` と評価側が追記した `ksn test` で採点する。失敗したら診断を返して `--repairs` 回まで書き直させる。

`results/`・`agent_prompts*`・`*.log`・`work/` は gitignore 済み。ハーネス自体のテストは `python -I bench/test_harness.py` (外部に接続しない)。

## 実行

ローカル (LM Studio など。鍵なし。既定は `http://127.0.0.1:1234` と `gemma-4-12b-it`):

```
cd stage0 && cargo build --release && cd ..
python bench/run_bench.py --docs full --repairs 3
```

NVIDIA NIM (モデルは NVIDIA 側で動く。クラウド開発環境でもローカルでも同じ):

```
export NVIDIA_API_KEY=...   # クラウドならシークレットの環境変数として登録する
python bench/run_bench.py --provider nim
```

`--provider nim` は、ベース `https://integrate.api.nvidia.com`、モデル `nvidia/nemotron-3.5-lightning-30b-a3b`、鍵の環境変数 `NVIDIA_API_KEY`、並列数 2 を既定にする。個別に上書きできる。

| オプション | 意味 |
| --- | --- |
| `--provider {local,nim}` | 既定値の組。省略は `local` |
| `--base URL` / `--model NAME` | ベース URL とモデルの上書き |
| `--api-key-env NAME` | 鍵を読む環境変数の名前。指定すると `Authorization: Bearer <鍵>` を送る |
| `--jobs N` | 課題の並列数 (`local` 1、`nim` 2) |
| `--max-attempts N` | 1 リクエストの最大試行回数 (初回 + リトライ、既定 6) |
| `--retry-base SEC` | バックオフの基準秒 (既定 2) |
| `--out-dir DIR` | 結果の出力先 (既定 `bench/results`) |
| `--docs`, `--repairs`, `--tasks`, `--max-tokens`, `--timeout`, `--tag` | 従来どおり |
| `--large` | `bench/tasks_large` の大規模課題を対象にする。`verify_reference.py` / `run_bench.py` で指定。既定は `bench/tasks` |

## API キーの扱い

- キーは必ず環境変数で渡す (クラウドではシークレット)。コマンドライン引数・ファイル・リポジトリに書かない。コミットしない。
- ハーネスはキーを標準出力・結果 JSON・ログ・エラーメッセージのどこにも出さない。結果に残るのは環境変数の名前だけ。HTTP エラーはステータスコードだけを報告する。
- `--api-key-env` / `nim` で指定した環境変数が未設定なら、API を呼ぶ前に停止する。

## リトライ

HTTP 429・500・502・503・504、接続エラー、タイムアウト、壊れた応答をリトライする。待ち時間は `--retry-base * 2^n` (上限 60 秒) に 0.5〜1.0 倍のジッタを掛けたもの。`Retry-After` ヘッダ (秒数または HTTP 日付) があればそれを優先する (上限 120 秒)。それ以外の 4xx (401 など) はリトライしない。1 リクエストの試行は最大 `--max-attempts` 回で、尽きたら API エラーとして扱う。

## `--large`: 大規模課題を対象にする

`--large` を指定すると、通常の `bench/tasks/*` の代わりに `bench/tasks_large/*` (レベル `large` の 5 課題: discount, expr, inventory, table, vending) を対象にする。`python3 -I bench/verify_reference.py --large` で全件 pass を確かめ、`python bench/run_bench.py --large` でベンチを実行する。既定動作は変更しない。

## 整形の扱い

モデルのコードは、採点の前に `ksn fmt` を通す (人がエディタで保存するのと同じ。定義の並びや空白を LLM が正規形に打つ必要はない)。整形の前後でファイルが変わらなければ `canonical: true` (1 回目から正規形)、変わったら `false`、構文の誤りで `fmt` が失敗したら `false` (その出力は `check` の段の失敗として返す)。集計の `first_canonical` / `first_answers` は、1 回目がコードブロックだった答案のうち正規形だった数と、その総数 (表示の「1回目の答案が正規形」)。
2026-11-20 から、`ksn fmt` の後に `ksn fix` (効果の宣言の書き足し・冗長な注釈・`get` から `get_str` などの機械的な直し) も当て、もう一度 `ksn fmt` してから検査する (利用者の決定。Kasane は道具が直せるものを道具に任せる考え方)。記録の「1 回目の答案が正規形」は、fmt と fix の前後でファイルが変わらなかった数。

## 結果の分類

課題ごとの `final`:

- `pass`: どれかの試行がテストに通った。
- `fail`: モデルの失敗 (診断が残る、コードブロックが無い、テスト不合格、`ksn` のタイムアウト)。
- `invalid`: インフラの失敗で結論が出なかった。リトライ後も API がエラー (`api-error`)、または `ksn` がシグナルで落ち続けた (`infra`。同じ呼び出しを 3 回までやり直してから判定)。モデルの失敗には数えない。

集計の `pass_at_1` / `pass_at_n` は `invalid` を除いた課題数 (`valid_tasks`) を分母にする。`invalid` が 1 件でもあれば、実行の最後に `INVALID RUN` を表示して終了コード 3 で終わる。invalid の課題だけ `--tasks` で再実行する。

試行ごとに、所要秒・prompt / completion トークン・推論トークン (API が返す場合)・リトライ回数を記録し、集計にも合計 (`retries`, `prompt_tokens`, `completion_tokens`, `reasoning_tokens`) が入る。

## 推論モデル

`reasoning_content` / `reasoning` フィールドと、本文中の `<think>...</think>` (閉じ忘れ・開始タグ無しも) は推論として分け、コードブロックの抽出は最終回答だけから行う。推論トークン数は `usage.completion_tokens_details.reasoning_tokens` から記録する。推論で `--max-tokens` を使い切ると最終回答が空になり `no-code` になるので、そのときは `--max-tokens` を上げる。

## ローカル機の注意

開発機の CPU コアの 1 つが不安定で、負荷時に無関係なプロセスが segfault することがある (`実装計画.md` 7 章)。回避として、不安定なコアを避けて実行する:

```
taskset -c 0-9,12-31 python bench/run_bench.py --provider nim
```

それでも `ksn` が落ちた場合は `infra` / `invalid` として分類されるので、モデルの成績には混ざらない。

## その他のスクリプト

- `eval_files.py`, `verify_reference.py`: 保存済みの解・参照解の採点。
- `run_selfdriven.py`: `claude -p` に `ksn` を回させる自走型。OpenAI 互換 API は使わないので、上のオプションは関係ない。

## 課題の追加

新しい課題は `bench/tasks/<id>/` ディレクトリに置く:

- `task.json`: `{"id", "title", "spec", "decl", "level": "basic" | "hard", "order"}`。
  `order` は `TASKS` 全体での位置 (0 始まり)。`bench/tasks.py` のローダが
  `bench/tasks/*/task.json` を読んで `order` 順に `TASKS` (各辞書のキーは
  `id, title, spec, decl, tests` の 5 つ) を組み立てる。
- `tests.ksnl`: Kasane の `test` 定義の文字列 (課題があるものだけ)。
- `reference.ksnl`: 参照解 (あるものだけ)。`python3 -I bench/verify_reference.py` が、課題のあるものはテストで採点し、回帰用だけのものは `ksn check` の診断 0 件と `ksn fmt --check` を確かめる (フル検証 `tests/run_tests.py` にも入っている)。

回帰用だけの課題 (テストも無く、回帰検査だけの参照解)は
`{"id", "title", "level": "reference-only"}` の `task.json` と `reference.ksnl` のみ。
`bench/tasks.py` は回帰用 id の一覧 `REFERENCE_ONLY` も出す。
