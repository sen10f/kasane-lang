#!/usr/bin/env node
// `ksn build --target wasm32-wasi` が作った WASI preview 1 の `.wasm` を `node:wasi` で走らせる実行器 (修正プラン 5-3)。
//
// 使い方: node tools/wasm/run_wasi.mjs [--dir <ホストのディレクトリ>] <file.wasm> [引数...]
//   --dir: 前開きのディレクトリ (ゲストの `/`)。省略は今のカレントディレクトリ。std.fs の相対パスはここからの相対になる。
//   引数: プログラムの引数 (`std.env.arg` で読める。先頭の名前は .wasm のパス)。
//   環境変数: 今のプロセスのものを渡す (`std.env.var` で読める)。
//   標準入出力は本物の fd 0・1・2 につなぐ。終了コードは `_start` が `proc_exit` した値 (正常 0、Err 1、panic 101、スタック超過 102)。
// `--abi <file.wasm>`: import と export の一覧を JSON で出して終わる (実行はしない)。
//
// 素の wasm (`run.mjs`) との違い: import は `wasi_snapshot_preview1` の関数だけ。入口は export の `_start` (`ksn_main` も残る)。

import fs from 'node:fs';
import path from 'node:path';
import { WASI } from 'node:wasi';

const argv = process.argv.slice(2);

if (argv[0] === '--abi') {
    const mod = new WebAssembly.Module(fs.readFileSync(argv[1]));
    const imports = WebAssembly.Module.imports(mod).map((i) => `${i.module}.${i.name}:${i.kind}`);
    const exports = WebAssembly.Module.exports(mod).map((e) => `${e.name}:${e.kind}`);
    console.log(JSON.stringify({ imports, exports }));
    process.exit(0);
}

// 深いスタック (呼び出しの深さの上限 100000 まで。仕様 5.3) のために、実行は大きなスタックの Worker スレッドで行う。
// 主スレッドの JS のスタックは約 1 MB で、`trap: Maximum call stack size exceeded` が先に起きるため。
import { Worker, isMainThread, workerData } from 'node:worker_threads';
if (isMainThread) {
    // 実験的の警告 (`node:wasi`) は Worker から主スレッドへ転送されて出るので、ここでも捨てる (実行の標準エラーを汚さない)。
    process.removeAllListeners('warning');
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

let dir = process.cwd();
if (argv[0] === '--dir') {
    if (!argv[1]) {
        console.error('usage: node tools/wasm/run_wasi.mjs [--dir <dir>] <file.wasm> [args...]');
        process.exit(2);
    }
    dir = path.resolve(argv[1]);
    argv.splice(0, 2);
}

const file = argv.shift();
if (!file) {
    console.error('usage: node tools/wasm/run_wasi.mjs [--dir <dir>] <file.wasm> [args...] | --abi <file.wasm>');
    process.exit(2);
}

// `node:wasi` は作るときに実験的の警告を標準エラーへ出す。実行の出力 (標準エラー) を素の実行と比べるので、これは捨てる。
process.removeAllListeners('warning');
const wasi = new WASI({
    version: 'preview1',
    args: [file, ...argv],
    env: process.env,
    preopens: { '/': dir },
    // `proc_exit` で戻らずに終わらせ、終了コードを `start` の戻り値で受け取る。
    returnOnExit: true,
});

const { instance } = await WebAssembly.instantiate(fs.readFileSync(file), {
    wasi_snapshot_preview1: wasi.wasiImport,
});

let code;
try {
    code = wasi.start(instance);
} catch (e) {
    // 素の wasm の trap (`unreachable` など) は、ネイティブの異常終了に当たる。理由を出して 134 (SIGABRT 相当) にする。
    console.error(`trap: ${e && e.message ? e.message : e}`);
    code = 134;
}
process.exitCode = code & 0xff;
