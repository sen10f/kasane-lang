#!/usr/bin/env node
// テスト用: `args_sizes_get` が 32 ビットを溢れさせる個数を返す WASI のホスト (review2 12)。
// 使い方: node tools/wasm/hostile_args.mjs <file.wasm>。終了コードは `_start` が `proc_exit` した値。
import fs from 'node:fs';
import { WASI } from 'node:wasi';

process.removeAllListeners('warning');
const wasi = new WASI({ version: 'preview1', args: [], env: {}, returnOnExit: true });
const imp = { ...wasi.wasiImport };
let mem = null;
imp.args_sizes_get = (pc, pb) => {
    const v = new DataView(mem.buffer);
    v.setUint32(pc, 0x40000000, true);
    v.setUint32(pb, 0xffffffff, true);
    return 0;
};
const mod = new WebAssembly.Module(fs.readFileSync(process.argv[2]));
const inst = new WebAssembly.Instance(mod, { wasi_snapshot_preview1: imp });
mem = inst.exports.memory;
process.exit(wasi.start(inst));
