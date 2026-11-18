#!/usr/bin/env node
// `ksn build --target wasm32-unknown-unknown` が作った素の `.wasm` を走らせる小さな実行器 (修正プラン 5-3)。
//
// 与える import は `env` の 2 つだけ (仕様の「WASM ABI」の節):
//   - ksn_write(fd: i32, ptr: i32, len: i32) -> i32   線形メモリの [ptr, ptr+len) を fd (1 = 標準出力、2 = 標準エラー) へ書く。書けた数、失敗は -1。
//   - ksn_exit(code: i32)                             プログラムの終了。戻らない (ここでは例外で巻き戻す)。
// 入口は export の `ksn_main() -> i32`。戻り値が終了コード (正常 0、Err の終了 1、panic 101、スタック超過 102)。
//
// 使い方: node tools/wasm/run.mjs <file.wasm>
// 出力はバイトのまま書き、終了コードは `ksn_main` の戻り値 (`ksn_exit` で終わったときはその値)。

import fs from 'node:fs';

class Exit extends Error {
    constructor(code) {
        super(`exit ${code}`);
        this.code = code;
    }
}

// `--abi <file.wasm>`: import と export の一覧を JSON で出して終わる (WASM ABI の検査に使う。実行はしない)。
if (process.argv[2] === '--abi') {
    const mod = new WebAssembly.Module(fs.readFileSync(process.argv[3]));
    const imports = WebAssembly.Module.imports(mod).map((i) => `${i.module}.${i.name}:${i.kind}`);
    const exports = WebAssembly.Module.exports(mod).map((e) => `${e.name}:${e.kind}`);
    console.log(JSON.stringify({ imports, exports }));
    process.exit(0);
}

// 深いスタック (呼び出しの深さの上限 100000 まで。仕様 5.3) のために、実行は大きなスタックの Worker スレッドで行う。
// 主スレッドの JS のスタックは約 1 MB で、`trap: Maximum call stack size exceeded` が先に起きるため。
import { Worker, isMainThread, workerData } from 'node:worker_threads';
if (isMainThread) {
    const w = new Worker(new URL(import.meta.url), { argv: process.argv.slice(2), workerData: 1, resourceLimits: { stackSizeMb: 512 } });
    w.on('error', (e) => {
        console.error(e);
        process.exitCode = 2;
    });
    w.on('exit', (c) => {
        process.exitCode = process.exitCode || c;
    });
    await new Promise(() => {});
}

const file = process.argv[2];
if (!file) {
    console.error('usage: node tools/wasm/run.mjs <file.wasm> | --abi <file.wasm>');
    process.exit(2);
}

let memory = null;

const env = {
    ksn_write(fd, ptr, len) {
        // 呼ぶたびにビューを作り直す (メモリが伸びると古いバッファは使えない)。
        const view = new Uint8Array(memory.buffer, ptr >>> 0, len >>> 0);
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
};

const { instance } = await WebAssembly.instantiate(fs.readFileSync(file), { env });
memory = instance.exports.memory;

let code;
try {
    code = instance.exports.ksn_main();
} catch (e) {
    if (e instanceof Exit) {
        code = e.code;
    } else {
        // 素の wasm の trap (`unreachable` など) は、ネイティブの異常終了に当たる。理由を出して 134 (SIGABRT 相当) にする。
        console.error(`trap: ${e && e.message ? e.message : e}`);
        code = 134;
    }
}
process.exitCode = code & 0xff;
