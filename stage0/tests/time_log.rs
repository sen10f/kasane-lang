//! 実時刻 (`std.time.SystemTime`)・暦 (`std.datetime`)・構造化ログ (`std.log`) のテスト。
//! 既知の UNIX 秒と暦の往復、RFC 3339 の往復と不正な入力の位置、ログの 1 行の形を、インタプリタと
//! LLVM (デバッグ・リリース) の出力で確かめる。`now()` は値の範囲だけを確かめる。

#![allow(dead_code, unused_imports)]

mod common;

use std::process::Command;

use common::*;

/// インタプリタと LLVM (デバッグ・リリース) の実行ファイルの標準出力が一致することを確かめ、インタプリタの出力を返す。
/// この試験の形はすべて LLVM に下りるので、バンドルへの退避は許さない (退避したら失敗する)。
fn agree_native(name: &str, src: &str) -> String {
    let t = Tmp::new(&format!("{}_interp", name), src);
    let interp = run(&["run"], &t.0);
    assert_eq!(interp.status.code(), Some(0), "{}{}", text(&interp.stdout), text(&interp.stderr));
    for release in [false, true] {
        let (bt, found, out) = build_native_strict(&format!("{}_{}", name, release), src, release);
        let p = found.unwrap_or_else(|| panic!("LLVM に下りない (release: {}): {}", release, out));
        let o = Command::new(&p).current_dir(&bt.0).output().unwrap();
        assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
        assert_eq!(text(&o.stdout), text(&interp.stdout), "native and interpreter outputs differ (release: {})", release);
    }
    text(&interp.stdout)
}

/// 標準エラーのログ行から、時刻の `"ts":"..."` の欄を取り除く (両者の比較用)。
fn strip_ts(line: &str) -> String {
    match line.find("\",\"level\"") {
        Some(i) if line.starts_with("{\"ts\":\"") => format!("{{{}", &line[i + 2..]),
        _ => line.to_string(),
    }
}

#[test]
fn known_unix_seconds_print_rfc3339_in_run_and_native() {
    let src = "fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    t.show(0i64)?
    t.show(951782400i64)?
    t.show(4102444800i64)?
    t.show(-1i64)?
    t.show(-62135596800i64)?
    t.show(253402300799i64)?
    t.show(1709164800i64)?
    t.show(4107542400i64)?
    t.show(-2208988800i64)?
    t.show(1794651630i64)?
    Ok(())
end t.main

fn t.show(secs: i64) -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let d = std.datetime.from_unix_seconds(secs)
    match d.to_rfc3339() {
        Ok(s) => {
            std.io.print_line(s.as_str())?
        }
        Err(_e) => return Err(std.io.Error { kind: std.io.ErrorKind.OutOfMemory, code: 0i32 })
    }
    Ok(())
end t.show
";
    let out = agree_native("time_known", src);
    assert_eq!(
        out,
        "1970-01-01T00:00:00Z\n\
         2000-02-29T00:00:00Z\n\
         2100-01-01T00:00:00Z\n\
         1969-12-31T23:59:59Z\n\
         0001-01-01T00:00:00Z\n\
         9999-12-31T23:59:59Z\n\
         2024-02-29T00:00:00Z\n\
         2100-03-01T00:00:00Z\n\
         1900-01-01T00:00:00Z\n\
         2026-11-14T10:20:30Z\n"
    );
}

#[test]
fn calendar_round_trips_over_the_whole_range() {
    // 0001-01-01 から 9999-12-31 まで、約 11 か月ごとに刻む (閏年の 2 月・月末・年末をまたぐ)。
    // 各値で `from_unix_seconds` → `unix_seconds` が元に戻り、各欄が暦の範囲にあることを確かめる。
    let src = "fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let mut s = -62135596800i64
    let last = 253402300799i64
    let step = 28508400i64
    let mut checked = 0i64
    let mut bad = 0i64
    while s <= last {
        let d = std.datetime.from_unix_seconds(s)
        if d.unix_seconds() != s {
            bad = bad + 1i64
        }
        if d.month < 1i64 || d.month > 12i64 {
            bad = bad + 1i64
        }
        if d.day < 1i64 || d.day > std.datetime.days_in_month(d.year, d.month) {
            bad = bad + 1i64
        }
        if d.hour > 23i64 || d.minute > 59i64 || d.second > 59i64 || d.nanos != 0i64 {
            bad = bad + 1i64
        }
        checked = checked + 1i64
        s = s + step
    }
    match t.summary(checked, bad) {
        Ok(line) => {
            std.io.print_line(line.as_str())?
        }
        Err(_e) => return Err(std.io.Error { kind: std.io.ErrorKind.OutOfMemory, code: 0i32 })
    }
    Ok(())
end t.main

fn t.summary(checked: i64, bad: i64) -> Result<alloc.string.String<std.alloc.System>, alloc.alloc.AllocError> uses [alloc] panics never
    let mut line = alloc.string.String.new(std.alloc.SYSTEM)
    line.push_str(\"checked=\")?
    line.push_i64(checked)?
    line.push_str(\" bad=\")?
    line.push_i64(bad)?
    line.push_str(\" leap_day_days=\")?
    line.push_i64(std.datetime.days_from_civil(2000i64, 2i64, 29i64))?
    Ok(line)
end t.summary
";
    let out = agree_native("time_roundtrip", src);
    assert_eq!(out, "checked=11069 bad=0 leap_day_days=11016\n");
}

#[test]
fn rfc3339_parse_and_write_with_positioned_errors() {
    let inputs = [
        "2026-11-14T10:20:30Z",
        "2000-02-29t00:00:00.5+09:00",
        "1970-01-01T00:00:00.000000001z",
        "1969-12-31T23:59:59.120-01:30",
        "2028-02-29T00:00:00Z",
        "2026-13-01T00:00:00Z",
        "2026-02-29T00:00:00Z",
        "2026-11-14 10:20:30Z",
        "2026-11-14T10:20:30",
        "2026-11-14T10:20:30Zx",
        "2026-11-14T10:20:30.Z",
        "2026-11-14T10:20:30+0900",
        "2026-11-14T24:00:00Z",
        "2026-11-14T10:20:60Z",
    ];
    let mut src = String::from("fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may\n");
    for s in inputs {
        src.push_str(&format!("    t.emit(\"{}\")?\n", s));
    }
    src.push_str(
        "    Ok(())
end t.main

fn t.describe(text: &str) -> Result<alloc.string.String<std.alloc.System>, alloc.alloc.AllocError> uses [alloc] panics may
    let mut line = alloc.string.String.new(std.alloc.SYSTEM)
    line.push_str(text)?
    match std.datetime.parse_rfc3339(text) {
        Ok(d) => {
            line.push_str(\" -> \")?
            line.push_i64(d.unix_seconds())?
            line.push_str(\" \")?
            let back = d.to_rfc3339()?
            line.push_str(back.as_str())?
            line.push_str(\" ns=\")?
            line.push_i64(d.nanos)?
        }
        Err(e) => {
            line.push_str(\" !! \")?
            alloc.fmt.debug(&mut line, &e)?
        }
    }
    Ok(line)
end t.describe

fn t.emit(text: &str) -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    match t.describe(text) {
        Ok(line) => {
            std.io.print_line(line.as_str())?
        }
        Err(_e) => return Err(std.io.Error { kind: std.io.ErrorKind.OutOfMemory, code: 0i32 })
    }
    Ok(())
end t.emit
",
    );
    let out = agree_native("time_parse", &src);
    assert_eq!(
        out,
        "2026-11-14T10:20:30Z -> 1794651630 2026-11-14T10:20:30Z ns=0\n\
         2000-02-29t00:00:00.5+09:00 -> 951750000 2000-02-28T15:00:00.5Z ns=500000000\n\
         1970-01-01T00:00:00.000000001z -> 0 1970-01-01T00:00:00.000000001Z ns=1\n\
         1969-12-31T23:59:59.120-01:30 -> 5399 1970-01-01T01:29:59.12Z ns=120000000\n\
         2028-02-29T00:00:00Z -> 1835395200 2028-02-29T00:00:00Z ns=0\n\
         2026-13-01T00:00:00Z !! OutOfRange(5)\n\
         2026-02-29T00:00:00Z !! OutOfRange(8)\n\
         2026-11-14 10:20:30Z !! Unexpected(10)\n\
         2026-11-14T10:20:30 !! Eof(19)\n\
         2026-11-14T10:20:30Zx !! Trailing(20)\n\
         2026-11-14T10:20:30.Z !! Unexpected(20)\n\
         2026-11-14T10:20:30+0900 !! Unexpected(22)\n\
         2026-11-14T24:00:00Z !! OutOfRange(11)\n\
         2026-11-14T10:20:60Z !! OutOfRange(17)\n"
    );
}

#[test]
fn log_lines_have_one_json_object_per_line_on_stderr() {
    let src = "fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console, time] panics may
    std.log.info(\"started\", &[(\"port\", \"8080\"), (\"mode\", \"dev\\\"x\")])?
    std.log.warn(\"odd\\nline\", &[(\"n\", \"1\")])?
    std.log.error(\"bad\", &[(\"k\", \"v\")])?
    Ok(())
end t.main
";
    let want: Vec<String> = [
        "{\"level\":\"info\",\"msg\":\"started\",\"port\":\"8080\",\"mode\":\"dev\\\"x\"}",
        "{\"level\":\"warn\",\"msg\":\"odd\\nline\",\"n\":\"1\"}",
        "{\"level\":\"error\",\"msg\":\"bad\",\"k\":\"v\"}",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    // 1 行ごとに: `{"ts":"<UTC の RFC 3339>",` で始まり `}` で終わる。時刻を除いた残りは `want` と同じ。
    let shape = |s: &str| -> Vec<String> {
        let lines: Vec<&str> = s.lines().collect();
        assert_eq!(lines.len(), 3, "{}", s);
        for l in &lines {
            assert!(l.starts_with("{\"ts\":\"") && l.ends_with('}'), "{}", l);
            let rest = &l["{\"ts\":\"".len()..];
            let ts = &rest[..rest.find('"').unwrap()];
            assert!(ts.len() >= 20 && ts.as_bytes()[10] == b'T' && ts.ends_with('Z'), "ts: {}", ts);
        }
        lines.iter().map(|l| strip_ts(l)).collect()
    };
    // インタプリタ。
    let t = Tmp::new("log_run", src);
    let r = run(&["run"], &t.0);
    assert_eq!(r.status.code(), Some(0), "{}{}", text(&r.stdout), text(&r.stderr));
    assert_eq!(text(&r.stdout), "");
    assert_eq!(shape(&text(&r.stderr)), want);
    // LLVM (デバッグ・リリース): 標準エラーの行も同じ形でなければならない。
    for release in [false, true] {
        let (bt, found, out) = build_native_strict(&format!("log_native_{}", release), src, release);
        let p = found.unwrap_or_else(|| panic!("LLVM に下りない (release: {}): {}", release, out));
        let o = Command::new(&p).current_dir(&bt.0).output().unwrap();
        assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
        assert_eq!(text(&o.stdout), "", "release: {}", release);
        assert_eq!(shape(&text(&o.stderr)), want, "native stderr differs (release: {})", release);
    }
}

#[test]
fn system_time_and_date_time_now_are_in_range() {
    let src = "fn t.main() -> Result<(), std.io.Error> uses [io.console, time] panics may
    let now = std.time.SystemTime.now()
    let s = now.unix_seconds()
    let ns = now.unix_nanos()
    let in_range = s > 1767225600i64 && s < 4102444800i64
    let next = s.wrapping_add(1i64)
    let scale = 1000000000i64
    let consistent = ns >= s.wrapping_mul(scale) && ns < next.wrapping_mul(scale)
    let d = std.datetime.DateTime.now()
    let year_ok = d.year >= 2026i64 && d.year < 2300i64
    if in_range && consistent && year_ok {
        std.io.print_line(\"now_ok=true\")?
    } else {
        std.io.print_line(\"now_ok=false\")?
    }
    Ok(())
end t.main
";
    let out = agree_native("time_now", src);
    assert_eq!(out, "now_ok=true\n");
}

#[test]
fn reading_the_clock_needs_the_time_effect() {
    // `std.time.SystemTime.now` は `uses [time]`。宣言のない `main` からは呼べない (E4001)。
    let (codes, out) = check_codes("time_effect", &["core.mem.drop(std.time.SystemTime.now())"]);
    assert!(codes.iter().any(|c| c == "E4001"), "{}", out);
}

#[test]
fn stderr_writes_are_allowed_under_io_console() {
    // `std.io.write_stderr` は `uses [io.console]` の関数から呼べる (E4001 にならない)。
    let (codes, out) = check_codes("stderr_effect_ok", &["core.mem.drop(std.io.write_stderr(\"x\".as_bytes()))"]);
    assert!(!codes.iter().any(|c| c == "E4001"), "{}", out);
}

#[test]
fn system_time_values_compare_in_clock_order() {
    // `SystemTime` は値で `Ord` を持つ。普通の走行では、後に読んだ時刻が前の時刻より小さくならない。
    let src = "fn t.main() -> Result<(), std.io.Error> uses [io.console, time] panics may
    let a = std.time.SystemTime.now()
    let b = std.time.SystemTime.now()
    if a.unix_nanos() <= b.unix_nanos() {
        std.io.print_line(\"ordered\")?
    } else {
        std.io.print_line(\"reversed\")?
    }
    Ok(())
end t.main
";
    let out = agree_native("time_order", src);
    assert_eq!(out, "ordered\n");
}
