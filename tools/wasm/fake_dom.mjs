// node の上で動かす最小の DOM の偽物 (修正プラン 5-3 段 4)。ブラウザの代わりに、要素の木・属性・テキスト・value・
// イベント (`dispatchClick` で発火) と、`localStorage` (Map の写し)、`getElementById` を持つ。
// `ksn bindgen` の生成物 (apps/todo_spa/web/dom.mjs) が使う API だけを作る。
//
// 使い方: installGlobals() で `document` と `localStorage` を大域に置いてから、生成物と boot.mjs を読む。
// 木の見方: `textOf(el)` は要素の中の文字 (子の要素の文字を連ねる)、`find(pred)` は木を前から探す。

export class FakeElement {
    constructor(tag, doc) {
        this.tagName = String(tag).toUpperCase();
        this.ownerDocument = doc;
        this.parent = null;
        this.children = [];
        this.attributes = new Map();
        this.listeners = new Map();
        this.text = '';
        this.value = '';
    }

    get id() {
        return this.attributes.get('id') ?? '';
    }

    setAttribute(name, value) {
        this.attributes.set(String(name), String(value));
    }

    getAttribute(name) {
        return this.attributes.has(name) ? this.attributes.get(name) : null;
    }

    // ブラウザの textContent の設定: 子を外して、文字だけにする。
    set textContent(s) {
        for (const c of this.children) c.parent = null;
        this.children = [];
        this.text = String(s);
    }

    get textContent() {
        return this.text + this.children.map((c) => c.textContent).join('');
    }

    appendChild(child) {
        if (child.parent) child.remove();
        child.parent = this;
        this.children.push(child);
        return child;
    }

    replaceChildren() {
        for (const c of this.children) c.parent = null;
        this.children = [];
        this.text = '';
    }

    remove() {
        if (this.parent) {
            this.parent.children = this.parent.children.filter((c) => c !== this);
            this.parent = null;
        }
    }

    addEventListener(type, fn) {
        if (!this.listeners.has(type)) this.listeners.set(type, []);
        this.listeners.get(type).push(fn);
    }

    // イベントを発火させる (ブラウザの利用者の操作の代わり)。戻り値は呼んだ数。
    dispatch(type) {
        const fns = this.listeners.get(type) ?? [];
        for (const fn of [...fns]) fn({ type, target: this });
        return fns.length;
    }

    dispatchClick() {
        return this.dispatch('click');
    }
}

export class FakeDocument {
    constructor() {
        this.body = new FakeElement('body', this);
    }

    createElement(tag) {
        return new FakeElement(tag, this);
    }

    createTextNode(text) {
        const el = new FakeElement('#text', this);
        el.text = String(text);
        return el;
    }

    getElementById(id) {
        return this.find((el) => el.id === id);
    }

    // 木を前から (深さ優先) 探す。見つからなければ null。
    find(pred, root = this.body) {
        if (pred(root)) return root;
        for (const c of root.children) {
            const hit = this.find(pred, c);
            if (hit) return hit;
        }
        return null;
    }
}

export class FakeStorage {
    constructor() {
        this.map = new Map();
    }

    getItem(key) {
        return this.map.has(key) ? this.map.get(key) : null;
    }

    setItem(key, value) {
        this.map.set(String(key), String(value));
    }

    removeItem(key) {
        this.map.delete(key);
    }
}

// 大域の `document` と `localStorage` を置く。既存のものがあれば、戻すための関数を返す。
export function installGlobals(storage = new FakeStorage()) {
    const prev = { document: globalThis.document, localStorage: globalThis.localStorage };
    globalThis.document = new FakeDocument();
    globalThis.localStorage = storage;
    return () => {
        globalThis.document = prev.document;
        globalThis.localStorage = prev.localStorage;
    };
}

// 木の文字 (子の文字を連ねる)。
export function textOf(el) {
    return el ? el.textContent : null;
}

// node の fetch には cookie の入れ物がない。ブラウザが自動でしていること (Set-Cookie を保存し、以後の要求に付ける。
// `Max-Age=0` で消す) を、1 つの接続先について真似る。戻りの `restore` で元の fetch に戻す。
export function installCookieJar() {
    const jar = new Map();
    const realFetch = globalThis.fetch;
    globalThis.fetch = async (url, init = {}) => {
        const headers = new Headers(init.headers ?? {});
        if (jar.size > 0) {
            headers.set('cookie', [...jar].map(([k, v]) => `${k}=${v}`).join('; '));
        }
        const res = await realFetch(url, { ...init, headers });
        for (const setCookie of res.headers.getSetCookie()) {
            const pair = setCookie.split(';')[0];
            const eq = pair.indexOf('=');
            const name = pair.slice(0, eq).trim();
            const value = pair.slice(eq + 1).trim();
            if (/max-age=0/i.test(setCookie)) jar.delete(name);
            else jar.set(name, value);
        }
        return res;
    };
    return {
        jar,
        restore: () => {
            globalThis.fetch = realFetch;
        },
    };
}
