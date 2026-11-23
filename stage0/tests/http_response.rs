//! `std.http.ResponseDecoder` (上流の応答の読み取り: 頭と本体の上限・枠組み・1xx・不正な形の拒否・有界の本体)。
//! 入力は `data/http_tls/response.ksnl`。木走査 (`ksn run`) とネイティブ (`ksn build`) の出力が、同じ期待値と一致することを確かめる。
//! 268 MB 相当の閉じるまでの本体は、上限 256 KiB で止まる。本体は取り出しながら読むので、保持は 1 片 (16 KiB) のまま (`peak=16384`)。

mod common;

use common::*;
use std::process::Command;

const RESP_SRC: &str = include_str!("data/http_tls/response.ksnl");

/// `response.ksnl` の期待出力。最後の行の `peak` は、読んだ後に decoder が保持したバイトの最大 (上限ではなく保持量)。
const RESP_OUT: &str = "cl 200 OK body=hello done=1
chunked 200 OK body=Wikipedia done=1
close 200 OK body=abc done=1
interim 200 OK body=ok done=1
interim2 201 Created body= done=1
nobody204 204 No Content body= done=1
nobody304 304 Not Modified body= done=1
empty 200 OK body= done=1
noreason 200  body=x done=1
partial Truncated
nohead Truncated
slow 200 OK body=hello done=1
split 200 OK body=ok done=1
pipe1 200 OK body=A done=1
pipe2 404 Not Found body= done=1
clte BadRequest
twocl BadRequest
clplus BadRequest
clminus BadRequest
clempty BadRequest
clspace BadRequest
clbig BodyTooLarge
twote BadRequest
gzip Unsupported
v10chunk BadRequest
valcr BadRequest
valnul BadRequest
reasoncr BadRequest
reasonnul BadRequest
barelf BadRequest
fold BadRequest
status2 BadRequest
statusabc BadRequest
status099 BadRequest
http2 BadRequest
status101 Unsupported
noline BadRequest
chunkplus BadRequest
chunkdata BadRequest
chunkhuge BodyTooLarge
trailerlf BadRequest
headbig HeadTooLarge
big BodyTooLarge fed=262144 got=262144 peak=16384
";

/// `ksn run` (木走査) の標準出力。
fn interp_stdout(name: &str, src: &str) -> String {
    let t = Tmp::new(name, src);
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    text(&o.stdout)
}

/// ネイティブの実行ファイルを作って走らせ、標準出力を返す (退避は許さない)。
fn native_stdout(name: &str, src: &str) -> String {
    let (t, found, out) = build_native_strict(name, src, true);
    let Some(p) = found else { panic!("ネイティブの実行ファイルがない (退避は許さない): {}", out) };
    assert!(!out.contains("skipped"), "ネイティブに下ろせず退避した: {}", out);
    let o = Command::new(&p).current_dir(&t.0).output().expect("run native");
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    text(&o.stdout)
}

#[test]
fn response_decoder_interp_matches_expected() {
    assert_eq!(interp_stdout("http_response_interp", RESP_SRC), RESP_OUT);
}

#[test]
fn response_decoder_native_matches_expected() {
    assert_eq!(native_stdout("http_response_native", RESP_SRC), RESP_OUT);
}
