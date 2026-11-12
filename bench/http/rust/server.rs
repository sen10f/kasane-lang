// URL 短縮 API サーバ (Rust 標準ライブラリのみ)。examples/shortener (Kasane) と同じ HTTP/1.1 の部分集合を話す。
//   POST /shorten (本文 = URL) -> 201 {"code":"1000","url":"..."} / 400 {"error":"invalid url"}
//   GET /<code>               -> 302 + Location / 404 {"error":"not found"}
//   GET /stats                -> 200 {"links":N}
// 1 接続 1 リクエスト (Connection: close)。固定数のワーカースレッドが、受け付けた接続を共有のキューから取り合う。
// 保存先はメモリのみ (Kasane は保存先に "" を渡すと同じ動作)。
// 使い方: server <アドレス> <ワーカー数>
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;

const MAX_HEAD: usize = 8192;
const MAX_BODY: usize = 4096;
const OFFSET: u64 = 46656;

fn encode(mut n: u64) -> String {
    let mut digits = Vec::new();
    loop {
        let d = (n % 36) as u8;
        digits.push(if d < 10 { b'0' + d } else { b'a' + d - 10 });
        n /= 36;
        if n == 0 {
            break;
        }
    }
    digits.reverse();
    String::from_utf8(digits).unwrap()
}

fn decode(text: &str) -> Option<u64> {
    if text.is_empty() || text.len() > 12 || (text.len() > 1 && text.starts_with('0')) {
        return None;
    }
    let mut n: u64 = 0;
    for c in text.chars() {
        let d = match c {
            '0'..='9' => c as u64 - '0' as u64,
            'a'..='z' => c as u64 - 'a' as u64 + 10,
            _ => return None,
        };
        n = n.checked_mul(36)?.checked_add(d)?;
    }
    n.checked_sub(OFFSET)
}

fn valid_url(url: &str) -> bool {
    if url.len() > 2048 {
        return false;
    }
    let rest = match url.strip_prefix("https://") {
        Some(r) => r,
        None => match url.strip_prefix("http://") {
            Some(r) => r,
            None => return false,
        },
    };
    !rest.is_empty() && url.chars().all(|c| (c as u32) > 32 && (c as u32) != 127)
}

fn escape(out: &mut String, text: &str) {
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 32 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
}

fn link_body(code: &str, url: &str) -> String {
    let mut out = String::from("{\"code\":\"");
    out.push_str(code);
    out.push_str("\",\"url\":\"");
    escape(&mut out, url);
    out.push_str("\"}");
    out
}

fn error_body(message: &str) -> String {
    let mut out = String::from("{\"error\":\"");
    escape(&mut out, message);
    out.push_str("\"}");
    out
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        302 => "Found",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        413 => "Payload Too Large",
        _ => "Internal Server Error",
    }
}

struct Response {
    status: u16,
    location: String,
    body: String,
}

fn json(status: u16, body: String) -> Response {
    Response { status, location: String::new(), body }
}

fn route(store: &Mutex<Vec<String>>, method: &str, path: &str, body: &str) -> Response {
    match method {
        "GET" => {
            if path == "/stats" {
                let n = store.lock().unwrap().len();
                return json(200, format!("{{\"links\":{}}}", n));
            }
            if path == "/shorten" {
                return json(405, error_body("method not allowed"));
            }
            let code = &path[1..];
            let found = decode(code).and_then(|i| store.lock().unwrap().get(i as usize).cloned());
            match found {
                Some(url) => Response { status: 302, body: link_body(code, &url), location: url },
                None => json(404, error_body("not found")),
            }
        }
        "POST" if path == "/shorten" => {
            let url = body.trim();
            if !valid_url(url) {
                return json(400, error_body("invalid url"));
            }
            let index = {
                let mut v = store.lock().unwrap();
                v.push(url.to_string());
                (v.len() - 1) as u64
            };
            json(201, link_body(&encode(index + OFFSET), url))
        }
        _ => json(405, error_body("method not allowed")),
    }
}

fn find_head_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
}

// 要求を読む。Err(status) は読めなかった理由 (400 / 413)。接続が途中で切れたときは Err(0)。
fn read_request(stream: &mut TcpStream) -> Result<(String, String, String), u16> {
    let mut data: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 1024];
    let head_end = loop {
        let n = stream.read(&mut chunk).map_err(|_| 0u16)?;
        if n == 0 {
            return Err(0);
        }
        data.extend_from_slice(&chunk[..n]);
        if let Some(e) = find_head_end(&data) {
            break e;
        }
        if data.len() > MAX_HEAD {
            return Err(413);
        }
    };
    let (method, path, content_length) = {
        let head = std::str::from_utf8(&data[..head_end]).map_err(|_| 400u16)?;
        let (line, headers) = head.split_once("\r\n").ok_or(400u16)?;
        let (method, after) = line.split_once(' ').ok_or(400u16)?;
        let (path, version) = after.split_once(' ').ok_or(400u16)?;
        if (version != "HTTP/1.1" && version != "HTTP/1.0") || !path.starts_with('/') {
            return Err(400);
        }
        if method.is_empty() || !method.chars().all(|c| c.is_ascii_uppercase()) {
            return Err(400);
        }
        let mut content_length = 0usize;
        for h in headers.split("\r\n") {
            if let Some((name, value)) = h.split_once(':') {
                if name.eq_ignore_ascii_case("content-length") {
                    content_length = value.trim().parse().map_err(|_| 400u16)?;
                }
            }
        }
        (method.to_string(), path.to_string(), content_length)
    };
    if content_length > MAX_BODY {
        return Err(413);
    }
    let total = head_end + content_length;
    while data.len() < total {
        let n = stream.read(&mut chunk).map_err(|_| 0u16)?;
        if n == 0 {
            return Err(0);
        }
        data.extend_from_slice(&chunk[..n]);
    }
    let body = String::from_utf8(data[head_end..total].to_vec()).map_err(|_| 400u16)?;
    Ok((method, path, body))
}

fn write_response(stream: &mut TcpStream, r: &Response) {
    let mut out = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n",
        r.status,
        reason(r.status),
        r.body.len()
    );
    if !r.location.is_empty() {
        out.push_str("Location: ");
        out.push_str(&r.location);
        out.push_str("\r\n");
    }
    out.push_str("Connection: close\r\n\r\n");
    out.push_str(&r.body);
    let _ = stream.write_all(out.as_bytes());
}

fn handle(mut stream: TcpStream, store: &Mutex<Vec<String>>) {
    let response = match read_request(&mut stream) {
        Ok((method, path, body)) => route(store, &method, &path, &body),
        Err(0) => return,
        Err(413) => json(413, error_body("payload too large")),
        Err(_) => json(400, error_body("bad request")),
    };
    write_response(&mut stream, &response);
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: {} <addr> <workers>", args[0]);
        std::process::exit(2);
    }
    let workers: usize = args[2].parse().expect("workers");
    let listener = TcpListener::bind(&args[1]).expect("bind");
    let store: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let (tx, rx) = mpsc::sync_channel::<TcpStream>(64);
    let rx = Arc::new(Mutex::new(rx));
    for _ in 0..workers {
        let (rx, store) = (rx.clone(), store.clone());
        thread::spawn(move || loop {
            let job = rx.lock().unwrap().recv();
            match job {
                Ok(stream) => handle(stream, &store),
                Err(_) => break,
            }
        });
    }
    for stream in listener.incoming() {
        if let Ok(stream) = stream {
            if tx.send(stream).is_err() {
                break;
            }
        }
    }
}
