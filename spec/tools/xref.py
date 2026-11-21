#!/usr/bin/env python3
"""Kasane 相互参照検査: 複数ファイルにまたがる整合性 (ksn check の名前解決・impl 検査の参照実装)。

使い方: python -I xref.py <root> <file>...   (root は ksnlint.py と同じ)
検査項目:
  ref        ヘッダ中の core./alloc./std. の型・トレイト参照が、与えたファイル群で定義されている
  impl       `impl Tr for X` に対し、`impls Tr` のメソッドが trait の必須メソッドと過不足なく揃っている (3.2)
  dup        同一 (トレイト名, 型) の impl が複数ある (型引数違いを含む) (3.2)
  method     同一の型に同名メソッドが複数ある (トレイト間を含む) (3.2)
  field      型のフィールド名とメソッド名が衝突する (3.2)
  dyn        `dyn Tr` を使うトレイトが dyn 安全でない (ジェネリックメソッド、Self の値渡し/戻り値) (5.1)
  effect     impl のメソッドの効果がトレイト宣言の効果を超える (uses の包含、トレイトが never なら panics) (4.3 / 5.1, E4021)
"""
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from ksnlint import Lint  # noqa: E402

PRIM_HOME = {}
for p in ('i8', 'i16', 'i32', 'i64', 'i128', 'isize', 'u8', 'u16', 'u32', 'u64', 'u128', 'usize', 'f32', 'f64'):
    PRIM_HOME[p] = 'core.num.' + p
PRIM_HOME['bool'] = 'core.bool.bool'
PRIM_HOME['char'] = 'core.char.char'
PRIM_HOME['str'] = 'core.str'
ROOTS = ('core', 'alloc', 'std')
TYPEREF = re.compile(r'(?<![A-Za-z0-9_])((?:core|alloc|std)(?:\.[a-z_][a-z0-9_]*)*(?:\.[A-Z][a-z][A-Za-z0-9]*))')

# デフォルト確保器の短縮名 (std.x.Y) -> 正式名 (alloc.x.Y)。xref は .sig/.ksnl の
# 定義 (alloc.x.Y) を known に入れるので、std.* 参照も受け付ける (types.rs の
# ALLOC_ALIASES と対応)。
ALLOC_ALIASES = {
    'std.vec.Vec': 'alloc.vec.Vec',
    'std.string.String': 'alloc.string.String',
    'std.collections.HashMap': 'alloc.collections.HashMap',
    'std.collections.BtreeMap': 'alloc.collections.BtreeMap',
    'std.collections.HashSet': 'alloc.collections.HashSet',
    'std.collections.VecDeque': 'alloc.collections.VecDeque',
    'std.boxed.Box': 'alloc.boxed.Box',
}


def base_of(text):
    """`core.iter.Iterator<T, E, P>` -> core.iter.Iterator"""
    return re.split(r'[<\[ ]', text.strip(), 1)[0]


def split_list(text):
    """`io.fs, time` -> ['io.fs', 'time']（効果集合の要素）"""
    return [x.strip() for x in text.split(',') if x.strip()]


def covered(caller, eff):
    """ksn check の covers と同じ: 効果変数 (大文字) は覆う扱い。具体名は階層の部分型 (io ⊇ io.fs)。"""
    if eff[:1].isupper():
        return True
    return any(eff == c or eff.startswith(c + '.') for c in caller)


def main():
    sys.stdout.reconfigure(encoding='utf-8')
    root, files = sys.argv[1], sys.argv[2:]
    errs = []
    types, traits, fields, trait_methods = set(), set(), {}, {}
    trait_requires, trait_requires_raw = {}, {}
    trait_gen = {}       # トレイト -> [(型パラメータ名, 種別)]。種別は uses / panics / type
    trait_effs = {}      # トレイト -> {メソッド: (uses の要素, panics)} (宣言された効果だけ)
    all_items = []
    for f in files:
        lint = Lint(f, root)
        its = lint.items()
        all_items.append((f, its))
        for it in its:
            kw, nm = it['kw'], it.get('name')
            if kw in ('type', 'enum'):
                types.add(nm)
                if kw == 'type':
                    fields[nm] = {l.strip().split(':')[0] for l in it['body'] if l.strip()}
            elif kw == 'trait':
                traits.add(nm)
                mr = re.search(r' requires (.+?)(?: pub)?$', it['text'])
                trait_requires[nm] = [base_of(x) for x in lint.split_top(mr.group(1))] if mr else []
                trait_requires_raw[nm] = lint.split_top(mr.group(1)) if mr else []
                trait_methods[nm] = {}
                mg = re.search(r'\[(.*)\]', it['text'])
                trait_gen[nm] = [(x.split(':')[0].strip(), x.split(':')[1].strip() if ':' in x else 'type')
                                 for x in (lint.split_top(mg.group(1)) if mg else [])]
                for l in it['body']:
                    m = re.match(r'\s+fn (\w+)(\[.*?\])?\((.*)$', l)
                    if m:
                        trait_methods[nm][m.group(1)] = l.strip()
                        me = re.search(r' uses \[(.*?)\] panics (\w+)\s*$', l)
                        if me:
                            trait_effs.setdefault(nm, {})[m.group(1)] = (split_list(me.group(1)), me.group(2))
    known = types | traits

    def err(code, f, n, msg):
        errs.append((code, f, n, msg))

    impl_pairs = {}      # (trait_base, owner) -> set(method names)
    impl_where = {}      # (trait_base, owner) -> (file, line) 最初のメソッド
    impl_lines = []      # (file, line, trait_base, target_base, owner)
    owner_methods = {}   # owner -> {method: trait_base or None}
    impl_args = {}       # (trait_base, owner) -> impl 行の実引数 (文字列のリスト)
    impl_fn_effs = {}    # (trait_base, owner) -> {method: (uses, panics, file, line)}  宣言された効果だけ
    for f, its in all_items:
        for it in its:
            n = it['line'] + 1
            text = it['text']
            for m in TYPEREF.finditer(text):
                ref = m.group(1)
                alloc_ref = ALLOC_ALIASES.get(ref)
                if ref not in known and ref.rsplit('.', 1)[0] not in known and (alloc_ref is None or alloc_ref not in known):
                    err('ref', f, n, f'unknown type/trait {ref}')
            if it['kw'] == 'impl':
                mm = re.match(r'impl(?:\[.*?\])?\s+(.*?) for (.*)$', text)
                # generics 内の ] を含む場合に備え、 " for " で分割
                body = re.sub(r'^impl(\[(?:[^\[\]]|\[[^\]]*\])*\])?\s+', '', text)
                tr, tg = body.split(' for ', 1)
                tb, gb = base_of(tr), base_of(tg)
                owner = PRIM_HOME.get(gb, gb)
                impl_lines.append((f, n, tb, gb, owner))
                # 実引数 (`Read<[io.fs]>` の `[io.fs]`)。効果の規則 (4.3 / 5.1) に使う。
                targs = []
                if '<' in tr and '>' in tr:
                    targs = lint.split_top(tr[tr.index('<') + 1:tr.rindex('>')])
                impl_args[(tb, owner)] = targs
                impl_args[(tb, f'{tb}.{gb}')] = targs
            if it['kw'] == 'fn':
                nm = it['name']
                owner, meth = nm.rsplit('.', 1)
                mi = re.search(r' impls (.+?)(?: pub)?$', text)
                trait = base_of(mi.group(1)) if mi else None
                owner_methods.setdefault(owner, {})
                if meth in owner_methods[owner] and owner in types:
                    err('method', f, n, f'duplicate method {owner}.{meth}')
                owner_methods[owner][meth] = trait
                if trait:
                    impl_pairs.setdefault((trait, owner), set()).add(meth)
                    impl_where.setdefault((trait, owner), (f, n))
                    me = re.search(r' uses \[(.*?)\] panics (\w+)', text)
                    if me:
                        impl_fn_effs.setdefault((trait, owner), {})[meth] = (
                            split_list(me.group(1)), me.group(2), f, n)
                if owner in fields and meth in fields[owner]:
                    err('field', f, n, f'method {meth} collides with field of {owner}')
                if mi and trait not in traits:
                    err('ref', f, n, f'impls unknown trait {trait}')
    seen = {}
    for f, n, tb, gb, owner in impl_lines:
        if tb not in traits:
            err('ref', f, n, f'impl of unknown trait {tb}')
            continue
        key = (tb, gb)
        if key in seen:
            err('dup', f, n, f'duplicate impl of {tb} for {gb}')
        seen[key] = n
        want = set(trait_methods.get(tb, {}))
        have = impl_pairs.get((tb, owner), set())
        # 孤児規則のトレイト側配置 (<トレイト>.<型名>.<メソッド>) も許す
        alt = impl_pairs.get((tb, f'{tb}.{gb}'), set())
        have = have | alt
        if want != have:
            err('impl', f, n, f'impl {tb} for {gb}: methods {sorted(have)} != required {sorted(want)}')
    # impls メソッドがあるのに対応する impl 行が無い (逆方向)
    declared = {(tb, owner) for _, _, tb, _, owner in impl_lines}
    declared |= {(tb, f'{tb}.{gb}') for _, _, tb, gb, _ in impl_lines}
    for key, ms in impl_pairs.items():
        if key not in declared:
            f, n = impl_where[key]
            err('impl', f, n, f'methods {sorted(ms)} impl {key[0]} on {key[1]} but no impl line exists')

    # 効果 (4.3 / 5.1, ksn check の E4021 の参照実装): impl のメソッドの uses はトレイト宣言の uses に覆われ、
    # トレイトが panics never なら impl も never。トレイトの効果変数は impl 行の実引数で置き換える。
    # 束縛できない効果変数が残るとき、宣言の無いメソッドでは黙る。
    for (tb, owner), ms in impl_fn_effs.items():
        if tb not in traits or (tb, owner) not in impl_args:
            continue
        args = impl_args[(tb, owner)]
        bind_u, bind_p = {}, {}
        for (gname, gkind), a in zip(trait_gen.get(tb, []), args):
            if gkind == 'uses' and a.startswith('[') and a.endswith(']'):
                bind_u[gname] = split_list(a[1:-1])
            elif gkind == 'panics' and a in ('never', 'may'):
                bind_p[gname] = a
        for meth, (iu, ip, f, n) in ms.items():
            decl = trait_effs.get(tb, {}).get(meth)
            if decl is None:
                continue
            us, pv = decl
            tu, unknown = [], False
            for e in us:
                if not e[:1].isupper():
                    tu.append(e)
                elif e in bind_u and all(not x[:1].isupper() for x in bind_u[e]):
                    tu.extend(bind_u[e])
                else:
                    unknown = True
            tp = pv if pv in ('never', 'may') else bind_p.get(pv)
            if not unknown:
                for e in iu:
                    if not covered(tu, e):
                        err('effect', f, n, f'{owner}.{meth} uses effect `{e}` not declared by trait method {tb}.{meth}')
                        break
            if tp == 'never' and ip == 'may':
                err('effect', f, n, f'{owner}.{meth} may panic but trait method {tb}.{meth} declares `panics never`')

    # dyn 安全性 (5.1): ジェネリクスなし、レシーバ以外の引数と戻り値に Self なし、Self の値渡しレシーバなし。
    # スーパートレイトも同じ条件を満たす必要がある。
    def dyn_problems(tr, seen=None):
        seen = seen if seen is not None else set()
        if tr in seen:
            return []
        seen.add(tr)
        bad = []
        for meth, sig in trait_methods.get(tr, {}).items():
            m = re.match(r'fn \w+(\[)?', sig)
            if m.group(1):
                bad.append(f'{tr}.{meth} is generic')
                continue
            ps = sig.index('(')
            depth, j = 0, ps
            while j < len(sig):
                depth += sig[j] == '('
                depth -= sig[j] == ')'
                if depth == 0:
                    break
                j += 1
            inner, rest = sig[ps + 1:j], sig[j + 1:]
            params = Lint.split_top(None, inner)
            if params and params[0].replace(' ', '') == 'self:Self':
                bad.append(f'{tr}.{meth} takes self by value')
            for p in params[1:]:
                if re.search(r'\bSelf\b', p):
                    bad.append(f'{tr}.{meth} uses Self in an argument')
            ret = rest.split('uses')[0]
            if re.search(r'\bSelf\b', ret):
                bad.append(f'{tr}.{meth} uses Self in the return type')
        for item in trait_requires_raw.get(tr, []):
            if re.search(r'\bSelf\b', item):
                bad.append(f'{tr} requires {item} (Self in a supertrait argument)')
        for sup in trait_requires.get(tr, []):
            bad += dyn_problems(sup, seen)
        return bad

    for f, its in all_items:
        for it in its:
            for m in re.finditer(r'dyn ((?:core|alloc|std)[A-Za-z0-9_.]+)', it['text']):
                for b in dyn_problems(m.group(1)):
                    err('dyn', f, it['line'] + 1, f'dyn {m.group(1)} is not dyn-safe: {b}')

    # 依存方向 (8.2) と層規則 (3.5)
    rank = {'core': 0, 'alloc': 1, 'std': 2}
    for f, its in all_items:
        for it in its:
            if it['kw'] in ('import', 'impl', 'end') or not it.get('name'):
                continue
            own = it['name'].split('.')[0]
            block = it['text'] + '\n' + '\n'.join(it['body'])
            for m in TYPEREF.finditer(block):
                ref = m.group(1)
                if ref not in known:
                    ref = ref.rsplit('.', 1)[0]
                    if ref not in known:
                        continue
                rr = ref.split('.')[0]
                if own in rank and rank[rr] > rank[own]:
                    err('dep', f, it['line'] + 1, f'{own} must not depend on {rr} ({ref})')
    status = 0
    for code, f, n, msg in sorted(errs, key=lambda e: (e[1], e[2], e[0])):
        print(f'[{code}] {f}:{n}: {msg}')
        status = 1
    print('xref: ' + ('FAIL' if status else 'ok') +
          f' ({len(types)} types, {len(traits)} traits, {len(impl_lines)} impls, {sum(len(v) for v in owner_methods.values())} fns)')
    sys.exit(status)


if __name__ == '__main__':
    main()
