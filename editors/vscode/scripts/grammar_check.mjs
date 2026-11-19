import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(here, '../../..');
const grammarPath = path.join(
    root,
    'editors/vscode/syntaxes/kasane.tmLanguage.json',
);
const grammarText = fs.readFileSync(grammarPath, 'utf8');
const grammar = JSON.parse(grammarText);

if (typeof grammar.scopeName !== 'string' || !grammar.patterns) {
    throw new Error('Kasane grammar に scopeName か patterns がありません');
}

let textmate;
let oniguruma;
try {
    textmate = await import('vscode-textmate');
    oniguruma = await import('vscode-oniguruma');
} catch {
    console.log('Kasane grammar: JSON OK（vscode-textmate 未導入のためトークン化は省略）');
    process.exit(0);
}

const wasmPath = path.join(
    root,
    'editors/vscode/node_modules/vscode-oniguruma/release/onig.wasm',
);
const wasm = fs.readFileSync(wasmPath);
await oniguruma.loadWASM(
    wasm.buffer.slice(wasm.byteOffset, wasm.byteOffset + wasm.byteLength),
);

const registry = new textmate.Registry({
    onigLib: Promise.resolve({
        createOnigScanner: (sources) => new oniguruma.OnigScanner(sources),
        createOnigString: (value) => new oniguruma.OnigString(value),
    }),
    loadGrammar: async (scopeName) =>
        scopeName === grammar.scopeName
            ? textmate.parseRawGrammar(grammarText, grammarPath)
            : null,
});
const loaded = await registry.loadGrammar(grammar.scopeName);
if (!loaded) {
    throw new Error(`grammar ${grammar.scopeName} を読み込めません`);
}

const exampleDir = path.join(root, 'examples/demo/src');
const files = fs
    .readdirSync(exampleDir)
    .filter((name) => name.endsWith('.ksnl'))
    .sort();
if (files.length === 0) {
    throw new Error('examples/demo/src に .ksnl がありません');
}

for (const name of files) {
    const lines = fs.readFileSync(path.join(exampleDir, name), 'utf8').split(/\r?\n/);
    let ruleStack = textmate.INITIAL;
    let tokenCount = 0;
    for (const line of lines) {
        const result = loaded.tokenizeLine(line, ruleStack);
        ruleStack = result.ruleStack;
        tokenCount += result.tokens.length;
    }
    if (tokenCount === 0) {
        throw new Error(`${name} のトークンがありません`);
    }
    console.log(`Kasane grammar: ${name} (${tokenCount} tokens)`);
}
