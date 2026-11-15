#!/usr/bin/env node
// JS の Promise を待つ wasm32 の例を走らせる (仕様 5.6 の第 5 段、WASM ABI の「待ち」の節)。
//
// 入口 `ksn_main() -> i32` は、プログラムが終わったときは終了コード (正常 0、Err 1、panic 101 など)、
// JS の完了を待つときは -1 を返す。待ちの Promise が解決したら、JS は `ksn_resume(h, 値) -> i32` を呼ぶ。
// その戻り値も同じ決まりで、-1 のあいだは待ちが続く。
//
// 与える import は `env` の 3 つ (`ksn_write`・`ksn_exit`・例の `ksn_timer_start`)。`ksn_timer_start(ms) -> u32` は、
// ms 後に値 7 で解決するタイマーを作り、その番号 (ハンドル) を返す。
//
// 使い方: node tools/wasm/run_await.mjs <file.wasm>
// 標準出力は `ksn_write` のまま、終了コードは最後に得た `ksn_main` / `ksn_resume` の戻り値 (`ksn_exit` で終わったときはその値)。

import fs from 'node:fs';

class Exit extends Error {
    constructor(code) {
        super(`exit ${code}`);
        this.code = code;
    }
}

const file = process.argv[2];
if (!file) {
    console.error('usage: node tools/wasm/run_await.mjs <file.wasm>');
    process.exit(2);
}

let instance = null;
let nextHandle = 1;

const env = {
    ksn_write(fd, ptr, len) {
        // 呼ぶたびにビューを作り直す (メモリが伸びると古いバッファは使えない)。
        const view = new Uint8Array(instance.exports.memory.buffer, ptr >>> 0, len >>> 0);
        let off = 0;
        try {
            while (off < view.length) {
                off += fs.writeSync(fd, view, off, view.length - off);
            }
        } catch (e) {
            return -1;
        }
        return view.length;
    },
    ksn_exit(code) {
        throw new Exit(code);
    },
    // 例の JS の操作: ms 後に値 7 で解決する (Promise の代わりにタイマーを使う)。戻り値は番号。
    ksn_timer_start(ms) {
        const h = nextHandle++;
        setTimeout(() => settle(() => instance.exports.ksn_resume(h, 7)), ms);
        return h;
    },
};

// 入口・再開の結果を受ける。-1 は「まだ待つ」、-2 は「待つプログラムがない (無視)」、それ以外は終了コード。
function settle(call) {
    let code;
    try {
        code = call();
    } catch (e) {
        if (e instanceof Exit) {
            code = e.code;
        } else {
            // 素の wasm の trap (`unreachable` など) は、ネイティブの異常終了に当たる。理由を出して 134 (SIGABRT 相当) にする。
            console.error(`trap: ${e && e.message ? e.message : e}`);
            code = 134;
        }
    }
    if (code !== -1 && code !== -2) {
        process.exitCode = code & 0xff;
    }
}

const { instance: inst } = await WebAssembly.instantiate(fs.readFileSync(file), { env });
instance = inst;
settle(() => instance.exports.ksn_main());
