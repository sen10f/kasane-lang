#!/usr/bin/env node
// TODO の SPA (apps/todo_spa) の確認 (修正プラン 5-3 段 4)。node の上で、偽の DOM (fake_dom.mjs) に SPA を読み込み、
// 「追加 → 一覧に出る → 完了を切り替える → 削除 → localStorage に残る → 読み直すと localStorage の一覧が先に出る」を確かめる。
// サーバは `apps/todo` (実物。node の fetch で話す)。サーバを起動するのは呼び出し側 (テストは必ず kill する)。
//
// 使い方: node tools/wasm/spa_check.mjs <web ディレクトリ> <todo_spa.wasm> <API の接頭辞 (例 http://127.0.0.1:18080)>
// 成功は終了コード 0 と `spa_check: ok` を出す。失敗は理由を標準エラーに出して 1。

import fs from 'node:fs';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { FakeStorage, installCookieJar, installGlobals, textOf } from './fake_dom.mjs';

const [webDir, wasmPath, apiBase] = process.argv.slice(2);
if (!webDir || !wasmPath || apiBase === undefined) {
    console.error('usage: node tools/wasm/spa_check.mjs <web dir> <todo_spa.wasm> <API base>');
    process.exit(2);
}

const TIMEOUT_MS = 8000;
const steps = [];
const step = (name) => steps.push(name);

function fail(msg) {
    console.error(`spa_check: 失敗: ${msg}`);
    process.exitCode = 1;
    throw new Error(msg);
}

// 条件が成り立つまで待つ (タイムアウトで失敗)。
async function waitFor(pred, what) {
    const t0 = Date.now();
    for (;;) {
        if (pred()) return;
        if (Date.now() - t0 > TIMEOUT_MS) fail(`待ちがタイムアウト: ${what}`);
        await new Promise((r) => setTimeout(r, 20));
    }
}

const storage = new FakeStorage();
storage.setItem('todo_spa.api', apiBase);
let restoreGlobals = installGlobals(storage);
// サーバは利用者の認証を要る (session の cookie)。ブラウザの代わりに cookie を受け渡す (起動のたびに同じ jar)。
const cookies = installCookieJar();

// 一覧の行: { span, toggle, del } (ul の子の li)。
function rows() {
    const ul = globalThis.document.getElementById('todo-list');
    return ul.children.map((li) => ({ li, span: li.children[0], toggle: li.children[1], del: li.children[2] }));
}
const statusText = () => textOf(globalThis.document.getElementById('todo-status'));
const input = () => globalThis.document.getElementById('todo-input');
const addButton = () => globalThis.document.getElementById('todo-add');

async function main() {
    const { boot } = await import(pathToFileURL(path.join(path.resolve(webDir), 'boot.mjs')).href);
    const bytes = fs.readFileSync(wasmPath);

    // 1. 起動: 画面が出て、サーバの一覧 (空) を取り込む。
    const code = await boot(bytes);
    if (code !== 0) fail(`ksn_main の終了コードが 0 でない: ${code}`);
    step('起動');
    await waitFor(() => statusText() === '同期しました', '最初の同期');
    if (rows().length !== 0) fail(`最初の一覧は空のはず: ${rows().length} 件`);

    // 2. 追加: 引用符と円記号を含む題 (JSON の逃がしを確かめる)。
    input().value = '買い物 "A" \\';
    addButton().dispatchClick();
    await waitFor(() => rows().length === 1, '1 件目が一覧に出る');
    if (rows()[0].span.textContent !== '買い物 "A" \\') fail(`1 件目の題: ${JSON.stringify(rows()[0].span.textContent)}`);
    if (input().value !== '') fail('追加のあと入力欄は空のはず');
    step('追加 → 一覧に出る');

    input().value = 'B';
    addButton().dispatchClick();
    await waitFor(() => rows().length === 2, '2 件目が一覧に出る');
    step('2 件目の追加');

    // 3. 完了の切り替え: 1 件目を完了に、表示が変わる。
    rows()[0].toggle.dispatchClick();
    await waitFor(() => rows()[0].span.textContent.startsWith('✓ '), '完了の表示');
    if (rows()[0].toggle.textContent !== '未完了に戻す') fail(`切り替えのボタン: ${rows()[0].toggle.textContent}`);
    step('完了の切り替え');

    // 4. 切り替えを戻す。
    rows()[0].toggle.dispatchClick();
    await waitFor(() => !rows()[0].span.textContent.startsWith('✓ '), '未完了に戻る');
    step('完了を戻す');
    rows()[0].toggle.dispatchClick();
    await waitFor(() => rows()[0].span.textContent.startsWith('✓ '), '再び完了');
    step('再び完了');

    // 5. 削除: 2 件目を消す。
    rows()[1].del.dispatchClick();
    await waitFor(() => rows().length === 1, '2 件目が消える');
    step('削除');

    // 6. localStorage に残る: 1 件 (完了、JSON の逃がしのまま)。
    const saved = storage.getItem('todo_spa.items');
    const m = saved && saved.match(/^(\d+)\t1\t(.*)\n$/s);
    if (!m) fail(`localStorage の内容: ${JSON.stringify(saved)}`);
    if (m[2] !== '買い物 \\"A\\" \\\\') fail(`localStorage の題: ${JSON.stringify(m[2])}`);
    step('localStorage に残る');

    // 7. 読み直し: 同じ localStorage で起動し直すと、同期の前に保存した一覧が出る。
    restoreGlobals();
    restoreGlobals = installGlobals(storage);
    await boot(bytes);
    if (rows().length !== 1 || !rows()[0].span.textContent.startsWith('✓ ')) {
        fail(`読み直しの直後に保存した一覧が出ない: ${rows().length} 件`);
    }
    await waitFor(() => statusText() === '同期しました', '読み直しの同期');
    step('読み直すと localStorage の一覧が先に出る');
}

try {
    await main();
    console.log(`spa_check: ok (${steps.length} 手順: ${steps.join(' → ')})`);
} catch (e) {
    if (!process.exitCode) {
        console.error(`spa_check: 失敗: ${e && e.stack ? e.stack : e}`);
        process.exitCode = 1;
    }
} finally {
    restoreGlobals();
    cookies.restore();
}
