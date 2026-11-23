### 2.10?? std.http の要求の厳格化 (RFC 9112)

`std.http.parse_head` (`spec/stdlib/std/http.ksnl`) で次を `BadRequest` にする。木走査とネイティブは同じ `.ksnl` を通るので両方で同じ。
- 頭部の中の NUL・単独の CR (`std.http.has_bare_ctl`)
- `Host` の欠落 (HTTP/1.1) と 2 つ以上の `Host`
- `Transfer-Encoding` の 2 行以上
- HTTP/1.0 での chunked

試験: `stage0/tests/data/http_tls/parse.ksnl` に 6 件を足し、既存の入力に `Host` を足した (HTTP/1.1 は `Host` 必須になったため。期待の出力は変えていない)。
