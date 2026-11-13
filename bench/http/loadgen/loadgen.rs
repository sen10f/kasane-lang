// URL 短縮サーバ用の負荷生成器 (Rust 標準ライブラリのみ)。
// N 本のスレッドが、それぞれ「接続 -> 1 リクエスト -> 閉じるまで読む」を決められた時間だけ繰り返し、
// 毎秒のリクエスト数 (RPS) と遅延の p50 / p99 を出す。リクエストの割合は決め打ち (10 回に 1 回が POST /shorten、
// 1 回が GET /stats、残りは事前に作った 100 件への GET /<code>)。
// 使い方: loadgen <アドレス> <並行接続数> <計測秒数> [ウォームアップ秒数=1]
// 出力 (標準出力の 1 行): rps=<> p50_us=<> p99_us=<> max_us=<> requests=<> errors=<>
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

const PRELOAD: u64 = 100;
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

// 1 回のやり取り。返り値は (ステータス, 成功か)。
fn exchange(addr: &str, request: &[u8]) -> Option<u16> {
    let mut s = TcpStream::connect(addr).ok()?;
    let _ = s.set_nodelay(true);
    s.write_all(request).ok()?;
    let mut raw = Vec::with_capacity(512);
    s.read_to_end(&mut raw).ok()?;
    // "HTTP/1.1 NNN "
    if raw.len() < 12 || &raw[..9] != b"HTTP/1.1 " {
        return None;
    }
    std::str::from_utf8(&raw[9..12]).ok()?.parse().ok()
}

fn post(url: &str) -> Vec<u8> {
    format!(
        "POST /shorten HTTP/1.1\r\nHost: bench\r\nContent-Length: {}\r\n\r\n{}",
        url.len(),
        url
    )
    .into_bytes()
}

fn get(path: &str) -> Vec<u8> {
    format!("GET {} HTTP/1.1\r\nHost: bench\r\n\r\n", path).into_bytes()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        eprintln!("usage: {} <addr> <connections> <seconds> [warmup_seconds]", args[0]);
        std::process::exit(2);
    }
    let addr = args[1].clone();
    let conns: usize = args[2].parse().expect("connections");
    let secs: f64 = args[3].parse().expect("seconds");
    let warm: f64 = args.get(4).map(|s| s.parse().expect("warmup")).unwrap_or(1.0);

    // 事前に PRELOAD 件を作る (コードは 1000 から連番)。
    for i in 0..PRELOAD {
        let st = exchange(&addr, &post(&format!("http://bench.test/preload/{}", i)));
        if st != Some(201) {
            eprintln!("preload failed: {:?}", st);
            std::process::exit(1);
        }
    }
    let codes: Arc<Vec<String>> = Arc::new((0..PRELOAD).map(|i| encode(OFFSET + i)).collect());

    let stop = Arc::new(AtomicBool::new(false));
    let measuring = Arc::new(AtomicBool::new(false));
    let mut handles = Vec::new();
    for tid in 0..conns {
        let (addr, codes, stop, measuring) = (addr.clone(), codes.clone(), stop.clone(), measuring.clone());
        handles.push(thread::spawn(move || {
            let mut lat: Vec<u32> = Vec::new();
            let mut errors = 0u64;
            let mut k = tid as u64; // スレッドごとに位相をずらす
            while !stop.load(Ordering::Relaxed) {
                let (req, want): (Vec<u8>, u16) = match k % 10 {
                    0 => (post(&format!("http://bench.test/p/{}/{}", tid, k)), 201),
                    1 => (get("/stats"), 200),
                    _ => (get(&format!("/{}", codes[(k * 7 + tid as u64) as usize % codes.len()])), 302),
                };
                k += 1;
                let t0 = Instant::now();
                let st = exchange(&addr, &req);
                let us = t0.elapsed().as_micros().min(u32::MAX as u128) as u32;
                if measuring.load(Ordering::Relaxed) {
                    lat.push(us);
                    if st != Some(want) {
                        errors += 1;
                    }
                }
            }
            (lat, errors)
        }));
    }
    thread::sleep(Duration::from_secs_f64(warm));
    let t0 = Instant::now();
    measuring.store(true, Ordering::Relaxed);
    thread::sleep(Duration::from_secs_f64(secs));
    measuring.store(false, Ordering::Relaxed);
    let elapsed = t0.elapsed().as_secs_f64();
    stop.store(true, Ordering::Relaxed);
    let mut all: Vec<u32> = Vec::new();
    let mut errors = 0u64;
    for h in handles {
        let (l, e) = h.join().unwrap();
        all.extend(l);
        errors += e;
    }
    all.sort_unstable();
    let pct = |p: f64| -> u32 {
        if all.is_empty() {
            0
        } else {
            all[(((all.len() - 1) as f64) * p).round() as usize]
        }
    };
    println!(
        "rps={:.0} p50_us={} p99_us={} max_us={} requests={} errors={}",
        all.len() as f64 / elapsed,
        pct(0.50),
        pct(0.99),
        all.last().copied().unwrap_or(0),
        all.len(),
        errors
    );
}
