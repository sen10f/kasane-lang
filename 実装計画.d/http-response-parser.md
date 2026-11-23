### 2.10?? std.http の上流応答の解析 (有界・ストリーミング)

- `std.http.ResponseDecoder` を足した (`Decoder` と同じ使い方)。頭は 1 バイトずつ読み、解析は 1 度だけ。本体は取り出しながら読み、上限 (応答 1 件の本体の合計) は超えた時点で止める。
- 枠組み: Content-Length / chunked / 閉じるまで (`finish`)。1xx は途中の応答として読み捨て (101 は Unsupported)。204・304 は本体なし。
- 拒むもの: CL と TE の両方、CL・TE の二重、`+` や符号・空白の付いた長さ、CR・NUL・単独の LF・制御文字、折り返し行、3 桁でない状態、HTTP/2.0 など。
- 限界: HEAD への応答の本体は読めない (要求の方法を知らない。呼び手が頭だけで止める)。
- 試験: `stage0/tests/http_response.rs` (入力 `data/http_tls/response.ksnl`)。木走査とネイティブが同じ出力。256 KiB の上限で閉じるまでの本体を止め、保持は 16 KiB のまま。
