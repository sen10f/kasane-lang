// 起動 (修正プラン 5-3): `todo_spa.wasm` を読み、dom.mjs の import (ksn bindgen が生成) を渡して `ksn_main` を呼ぶ。
// `source` は URL (ブラウザの fetch で読む) か、バイト列 (ArrayBuffer / Uint8Array。node の試験で使う)。
// 戻り値は `ksn_main` の終了コード。標準出力・標準エラーの `ksn_write` は console に流す。
import { makeImports } from "./dom.mjs";

export async function boot(source) {
  const host = {};
  const dec = new TextDecoder();
  const env = {
    ...makeImports(host),
    ksn_write: (fd, p, n) => {
      const text = dec.decode(new Uint8Array(host.instance.exports.memory.buffer, p, n));
      (fd === 2 ? console.error : console.log)(text.replace(/\n$/, ""));
      return n;
    },
    ksn_exit: (code) => {
      throw new Error(`ksn_exit ${code}`);
    },
  };
  const bytes = typeof source === "string" ? await (await fetch(source)).arrayBuffer() : source;
  const { instance } = await WebAssembly.instantiate(bytes, { env });
  host.instance = instance;
  return instance.exports.ksn_main();
}
