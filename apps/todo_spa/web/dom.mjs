// 生成: ksn bindgen (修正プラン 5-3)。直接編集しない。module todo_spa.dom
// 使い方: const host = {}; const env = { ...makeImports(host), ksn_write, ksn_exit };
//            host.instance = (await WebAssembly.instantiate(bytes, { env })).instance;
// Kasane へは ksn_event(tag)（listen）と ksn_fetch_done(tag, status)（fetch。status 0 は通信の失敗）を呼ぶ。

const enc = new TextEncoder();
const dec = new TextDecoder();

export function makeImports(host) {
  const mem = () => new Uint8Array(host.instance.exports.memory.buffer);
  const ex = () => host.instance.exports;
  const S = (p, n) => dec.decode(mem().subarray(p, p + n));
  const put = (p, cap, bytes) => { if (bytes.length <= cap) mem().set(bytes, p); return bytes.length; };
  const nodes = [null];
  const push = (x) => nodes.push(x) - 1;
  const N = (h) => { if (!nodes[h]) throw new Error(`dom: 不正なハンドル ${h}`); return nodes[h]; };
  let fetched = new Uint8Array(0);

  const ops = {
    append: (parent, child) => { N(parent).appendChild(N(child)); },
    body: () => push(document.body),
    by_id: (id) => push(document.getElementById(id)),
    clear: (node) => { N(node).replaceChildren(); },
    create: (tag) => push(document.createElement(tag)),
    create_text: (text) => push(document.createTextNode(text)),
    fetch: (method, url, body, tag) => { const init = { method }; if (body !== "") { init.body = body; init.headers = { "content-type": "application/json" }; } fetch(url, init).then(async (r) => { fetched = enc.encode(await r.text()); ex().ksn_fetch_done(tag, r.status); }, () => { fetched = new Uint8Array(0); ex().ksn_fetch_done(tag, 0); }); },
    fetch_body: (p, cap) => put(p, cap, fetched),
    input_value: (node, p, cap) => put(p, cap, enc.encode(N(node).value ?? "")),
    listen: (node, event, tag) => { N(node).addEventListener(event, () => ex().ksn_event(tag)); },
    remove: (node) => { N(node).remove(); },
    set_attr: (node, name, value) => { N(node).setAttribute(name, value); },
    set_text: (node, text) => { N(node).textContent = text; },
    set_value: (node, text) => { N(node).value = text; },
    storage_get: (key, p, cap) => { const v = localStorage.getItem(key); return v === null ? -1 : put(p, cap, enc.encode(v)); },
    storage_set: (key, value) => { localStorage.setItem(key, value); },
  };

  return {
    ksn_dom_append: (a0, a1) => ops.append(a0, a1),
    ksn_dom_body: () => ops.body(),
    ksn_dom_by_id: (p0, n0) => ops.by_id(S(p0, n0)),
    ksn_dom_clear: (a0) => ops.clear(a0),
    ksn_dom_create: (p0, n0) => ops.create(S(p0, n0)),
    ksn_dom_create_text: (p0, n0) => ops.create_text(S(p0, n0)),
    ksn_dom_fetch: (p0, n0, p1, n1, p2, n2, a3) => ops.fetch(S(p0, n0), S(p1, n1), S(p2, n2), a3),
    ksn_dom_fetch_body: (p0, n0) => ops.fetch_body(p0, n0),
    ksn_dom_input_value: (a0, p1, n1) => ops.input_value(a0, p1, n1),
    ksn_dom_listen: (a0, p1, n1, a2) => ops.listen(a0, S(p1, n1), a2),
    ksn_dom_remove: (a0) => ops.remove(a0),
    ksn_dom_set_attr: (a0, p1, n1, p2, n2) => ops.set_attr(a0, S(p1, n1), S(p2, n2)),
    ksn_dom_set_text: (a0, p1, n1) => ops.set_text(a0, S(p1, n1)),
    ksn_dom_set_value: (a0, p1, n1) => ops.set_value(a0, S(p1, n1)),
    ksn_dom_storage_get: (p0, n0, p1, n1) => ops.storage_get(S(p0, n0), p1, n1),
    ksn_dom_storage_set: (p0, n0, p1, n1) => ops.storage_set(S(p0, n0), S(p1, n1)),
  };
}
