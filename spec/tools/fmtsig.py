#!/usr/bin/env python3
"""シグネチャ一覧 (.sig) の整形: 定義群の順序と群内のFQN順に並べ替える (仕様書 3.1)。

使い方: python -I fmtsig.py <file.sig>...   (ファイルを上書きする)
- 先頭の // 行(複数可)はファイルの先頭に残す。
- 定義(type/enum/trait ... end、1行の const/static/extern/impl/fn)を群順 → FQN順(UTF-8バイト列順)に並べる。
- メソッドを持つ `impl` 行は、その impl に属する `impls` メソッドのうちFQN最小のものの直前に置く (3.1)。
  メソッドを持たない `impl` 行は fn 群の前に、行の文字列順で置く。
- 群の間と複数行定義の間に空行を1行入れる。
"""
import re
import sys

GROUP = {'const': 1, 'static': 1, 'type': 2, 'enum': 2, 'trait': 2, 'extern': 3, 'impl': 4, 'fn': 5}


def base_of(text):
    return re.split(r'[<\[ ]', text.strip(), 1)[0]


def parse(lines):
    """行列を (kw, name, 本体行リスト, リード行リスト) のブロックに分ける。"""
    out, i, lead = [], 0, []
    while i < len(lines):
        l = lines[i]
        if l == '':
            i += 1; continue
        if l.startswith('//'):
            lead.append(l); i += 1; continue
        kw = l.split(' ', 1)[0].split('[', 1)[0]
        if kw in ('type', 'enum', 'trait'):
            j = i
            while not lines[j].startswith('end '):
                j += 1
            body = lines[i:j + 1]
            i = j + 1
        else:
            body = [l]
            i += 1
        m = re.match(r'\w+(?:\[.*?\])?\s+([A-Za-z0-9_.]+)', body[0])
        name = m.group(1) if m and kw != 'impl' else body[0]
        out.append({'kw': kw, 'name': name, 'body': body, 'lead': lead})
        lead = []
    return out


def fmt(path):
    text = open(path, encoding='utf8', newline='').read()
    lines = text.split('\n')
    if lines and lines[-1] == '':
        lines.pop()
    head = []
    while lines and lines[0].startswith('// '):
        head.append(lines.pop(0))
    blocks = parse(lines)
    fns = [b for b in blocks if b['kw'] == 'fn']
    impls = [b for b in blocks if b['kw'] == 'impl']
    # impl 行 → 属するメソッド (impls <同じトレイト文字列> かつ 所有者が一致)
    attach = {}
    for im in impls:
        body = re.sub(r'^impl(\[(?:[^\[\]]|\[[^\]]*\])*\])?\s+', '', im['body'][0])
        trait, target = body.split(' for ', 1)
        tb, gb = base_of(trait), base_of(target)
        cands = []
        for f in fns:
            mi = re.search(r' impls (.+?)(?: pub)?$', f['body'][0])
            if not mi or base_of(mi.group(1)) != tb:
                continue
            owner = f['name'].rsplit('.', 1)[0]
            if owner == gb or owner.endswith('.' + gb.split('.')[-1]):
                cands.append(f)
        if cands:
            first = min(cands, key=lambda f: f['name'])
            attach.setdefault(id(first), []).append(im)
            im['attached'] = True
    entries = []
    for b in blocks:
        if b['kw'] == 'impl':
            if not b.get('attached'):
                entries.append((GROUP['impl'], b['body'][0], [b]))
            continue
        pre = attach.get(id(b), [])
        entries.append((GROUP[b['kw']], b['name'], sorted(pre, key=lambda x: x['body'][0]) + [b]))
    entries.sort(key=lambda e: (e[0], e[1]))
    out, prev_g, prev_multi = list(head), None, False
    for g, key, group in entries:
        multi = len(group) == 1 and len(group[0]['body']) > 1
        if prev_g is not None and (g != prev_g or multi or prev_multi):
            out.append('')
        for b in group:
            out += b['lead'] + b['body']
        prev_g, prev_multi = g, multi
    open(path, 'w', encoding='utf8', newline='\n').write('\n'.join(out) + '\n')


if __name__ == '__main__':
    for p in sys.argv[1:]:
        fmt(p)
