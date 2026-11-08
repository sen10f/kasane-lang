#!/usr/bin/env python3
"""Kasane lint: ebnfcheck.py が検査しない「意味検査(ksn check/fmt 相当)」の参照実装。

使い方: python -I ksnlint.py <root> ok|fail <file>...
  <root>: ソースルート。ファイルの <root> からの相対パスがモジュール名になる (3.6)。
  ok  : 全ファイルが違反なしであること。
  fail: 各ファイルが、ファイル名の先頭語(例 indent_x.ksnh -> indent)に対応する違反を
        少なくとも1つ報告すること。

検査項目 (コード):
  fqn     FQN がモジュール名で始まる / end が header と同名 (3.1, 3.6)
  name    命名規約 (3.7, 4.6)
  order   定義順・同一群内のFQN順・derive/uses/requires/capture/境界の並び (3.1, 4.3)
  blank   トップレベルの空行、import 連続、コメント直後 (3.1)
  indent  インデント深さ (3.1, 4.6)
  unused  未使用の変数・引数・import、_ 始まりの使用 (5.4, 4.6)
  pub     pub な定義のヘッダが非公開の型を参照 (3.1)
  attr    uses の冗長 (io と io.fs) など (5.1)
"""
import re
import sys

GROUP = {'import': 0, 'const': 1, 'static': 1, 'type': 2, 'enum': 2, 'trait': 2,
         'extern': 3, 'impl_only': 4, 'fn': 5, 'test': 6}
BODY_KINDS = ('fn', 'test')
FIELD_KINDS = ('type', 'enum', 'trait')
DERIVE_ORDER = ['Debug', 'Clone', 'Copy', 'PartialEq', 'Eq', 'PartialOrd', 'Ord', 'Hash']
PRIMS = {'i8', 'i16', 'i32', 'i64', 'i128', 'isize', 'u8', 'u16', 'u32', 'u64', 'u128', 'usize',
         'f32', 'f64', 'bool', 'char', 'str'}
# 3.7: 語は英字で始まり、数字は直前の語に付ける (foo2 は可、foo_2 は不可)
SNAKE = re.compile(r'_?[a-z][a-z0-9]*(?:_[a-z][a-z0-9]*)*$')
UPPER_CAMEL = re.compile(r'[A-Z][a-z0-9]*(?:[A-Z][a-z0-9]*)*$')
SCREAMING = re.compile(r'[A-Z][A-Z0-9]*(?:_[A-Z][A-Z0-9]*)*$')


class Lint:
    def __init__(self, path, root):
        self.path, self.errs = path, []
        rel = path.replace('\\', '/')
        r = root.replace('\\', '/').rstrip('/') + '/'
        if root in ('', '.'):
            r = ''
        assert rel.startswith(r), (rel, r)
        rel = rel[len(r):]
        self.sig = rel.endswith('.sig')
        self.module = re.sub(r'\.(ksnh|ksnl|sig)$', '', rel).replace('/', '.')
        self.lines = open(path, encoding='utf8', newline='').read().split('\n')
        if self.lines and self.lines[-1] == '':
            self.lines.pop()

    def err(self, code, n, msg):
        self.errs.append((code, n, msg))

    # ------------------------------------------------------------ 構造の読み取り
    def items(self):
        L, items, i = self.lines, [], 0
        lead_start = None
        while i < len(L):
            l = L[i]
            if l == '':
                lead_start = None
                i += 1; continue
            if l.startswith('//'):
                if lead_start is None:
                    lead_start = i
                i += 1; continue
            m = re.match(r'(import|const|static|extern|type|enum|trait|impl|fn|test|end)\b', l)
            if not m or l.startswith(' '):
                self.err('indent', i + 1, f'top-level line not at column 0 or unknown: {l[:30]!r}')
                i += 1; lead_start = None; continue
            kw = m.group(1)
            start = lead_start if lead_start is not None else i
            it = {'kw': kw, 'line': i, 'start': start, 'text': l, 'lead_end': i}
            if kw == 'import':
                it['name'] = l.split()[1]
            elif kw == 'impl':
                it['name'] = l
            elif kw in ('const', 'static', 'extern'):
                it['name'] = re.match(r'\w+\s+([A-Za-z0-9_.]+)', l).group(1)
            else:
                it['name'] = re.match(r'\w+\s+([A-Za-z0-9_.]+)', l).group(1)
            if kw in FIELD_KINDS or (kw in BODY_KINDS and not (self.sig and kw == 'fn')):
                j = i + 1
                while j < len(L) and not L[j].startswith('end '):
                    j += 1
                if j >= len(L):
                    self.err('fqn', i + 1, 'missing end')
                    j = len(L) - 1
                else:
                    endname = L[j].split()[1]
                    if endname != it['name']:
                        self.err('fqn', j + 1, f'end {endname} != header {it["name"]}')
                it['end'] = j
                it['body'] = L[i + 1:j]
                it['body_start'] = i + 1
                i = j + 1
            else:
                it['end'] = i
                it['body'] = []
                i += 1
            items.append(it)
            lead_start = None
        return items

    # ------------------------------------------------------------ 各検査
    def check_fqn_and_names(self, items):
        pref = self.module + '.'
        for it in items:
            kw, name, n = it['kw'], it.get('name'), it['line'] + 1
            if kw in ('import', 'impl'):
                continue
            if not name.startswith(pref):
                self.err('fqn', n, f'{name} does not start with module {self.module}.')
                continue
            rest = name[len(pref):].split('.')
            if kw in ('const', 'static'):
                if not SCREAMING.match(rest[-1]) or len(rest) != 1 and False:
                    self.err('name', n, f'{rest[-1]} not SCREAMING_SNAKE_CASE')
            elif kw in ('type', 'enum', 'trait'):
                if len(rest) != 1 or not UPPER_CAMEL.match(rest[0]) or re.search(r'[A-Z]{2}', rest[0]):
                    self.err('name', n, f'{rest} not UpperCamelCase (no consecutive capitals)')
            elif kw == 'test':
                if not rest[-1].startswith('test_'):
                    self.err('name', n, 'test name must start with test_')
            else:  # fn / extern
                for c in rest:
                    if c in PRIMS or c == 'blanket':
                        continue
                    if c[0].isupper():
                        if not UPPER_CAMEL.match(c) or re.search(r'[A-Z]{2}', c):
                            self.err('name', n, f'{c} not UpperCamelCase')
                    elif not SNAKE.match(c):
                        self.err('name', n, f'{c} not snake_case')

    def check_order(self, items):
        prev_g, prev_key = -1, None
        entries = []
        for idx, it in enumerate(items):
            kw = it['kw']
            if kw == 'end':
                continue
            if kw == 'impl':
                nxt = items[idx + 1] if idx + 1 < len(items) else None
                attached = nxt is not None and nxt['kw'] == 'fn' and ' impls ' in nxt['text']
                it['attached'] = attached
                if attached:
                    continue
                g, key = GROUP['impl_only'], it['text']
            elif kw == 'fn' and idx > 0 and items[idx - 1]['kw'] == 'impl' and items[idx - 1].get('attached'):
                g, key = GROUP['fn'], it['name']
            else:
                g, key = GROUP[kw], it['name']
            entries.append((g, key, it['line'] + 1))
        # impl 行の位置 (3.1): 各 impl の最初の impls メソッドの直前に impl 行がある
        first_seen = set()
        for idx, it in enumerate(items):
            if it['kw'] != 'fn':
                continue
            mi = re.search(r' impls (.+?)(?: pub)?$', it['text'])
            if not mi:
                continue
            owner = it['name'].rsplit('.', 1)[0]
            key = (mi.group(1), owner)
            if key in first_seen:
                continue
            first_seen.add(key)
            prev = items[idx - 1] if idx > 0 else None
            ok = False
            if prev is not None and prev['kw'] == 'impl':
                body = re.sub(r'^impl(\[(?:[^\[\]]|\[[^\]]*\])*\])?\s+', '', prev['text'])
                trait, target = body.split(' for ', 1)
                tg = re.split(r'[<\[ ]', target.strip(), 1)[0]
                ok = trait.strip() == mi.group(1) and (owner == tg or owner.endswith('.' + tg.split('.')[-1]))
            if not ok:
                self.err('order', it['line'] + 1,
                         f'first method of impl {mi.group(1)} for {owner} must be directly preceded by its impl line')
        for g, key, n in entries:
            if g < prev_g:
                self.err('order', n, f'group order violated at {key}')
            elif g == prev_g and prev_key is not None and key < prev_key:
                self.err('order', n, f'{key} should come before {prev_key} (UTF-8 byte order)')
            if g > prev_g:
                prev_key = None
            prev_g, prev_key = g, key
        # import 辞書順は上の key 比較で検査済み

    def check_blank(self, items):
        if self.sig:
            return
        L = self.lines
        for k in range(1, len(items)):
            a, b = items[k - 1], items[k]
            gap = b['start'] - a['end'] - 1
            both_imports = a['kw'] == 'import' and b['kw'] == 'import'
            impl_attach = a['kw'] == 'impl' and b['kw'] == 'fn' and ' impls ' in b['text']
            if both_imports or impl_attach:
                if gap != 0:
                    self.err('blank', b['line'] + 1, 'no blank line allowed here')
            else:
                if gap != 1:
                    self.err('blank', b['line'] + 1, f'exactly one blank line required (found {gap})')
        # コメント直後の空行
        for n in range(len(L) - 1):
            if L[n].startswith('//') and L[n + 1] == '':
                self.err('blank', n + 2, 'blank line after comment')

    def split_top(self, s, sep=','):
        out, d, cur = [], 0, ''
        for ch in s:
            if ch in '[(<{':
                d += 1
            elif ch in '])>}':
                d -= 1
            if ch == sep and d == 0:
                out.append(cur.strip()); cur = ''
            else:
                cur += ch
        if cur.strip():
            out.append(cur.strip())
        return out

    def bracket(self, s, start):
        """s[start] が '[' のとき対応する ']' までの内側を返す。"""
        d = 0
        for i in range(start, len(s)):
            if s[i] == '[':
                d += 1
            elif s[i] == ']':
                d -= 1
                if d == 0:
                    return s[start + 1:i]
        return ''

    def generics_of(self, text):
        """ヘッダの型パラメータ列 `[...]` の内側を返す (無ければ None)。"""
        m = re.match(r'(?:impl|fn|type|enum|trait)(?:\[)?', text)
        if text.startswith('impl['):
            return self.bracket(text, 4)
        m = re.match(r'(?:fn|type|enum|trait)\s+[A-Za-z0-9_.]+\[', text)
        if m:
            return self.bracket(text, m.end() - 1)
        return None

    def strip_args(self, s):
        """`A<B + C>` のように <> や () の内側の '+' を除いた文字列 (トップレベルの '+' 判定用)。"""
        out, d = '', 0
        for ch in s:
            if ch in '<([':
                d += 1
            elif ch in '>)]':
                d -= 1
            elif d == 0:
                out += ch
        return out

    def check_attrs(self, items):
        for it in items:
            text, n = it['text'], it['line'] + 1
            for m in re.finditer(r'\buses \[', text):
                inner = self.bracket(text, m.end() - 1)
                els = self.split_top(inner)
                conc = [e for e in els if e and e[0].islower()]
                var = [e for e in els if e and e[0].isupper()]
                if els != sorted(conc) + var:
                    self.err('order', n, f'uses list order: {els}')
                for a in conc:
                    for b in conc:
                        if a != b and b.startswith(a + '.'):
                            self.err('attr', n, f'redundant uses: {a} includes {b}')
                if len(set(els)) != len(els):
                    self.err('attr', n, 'duplicate uses element')
            m = re.search(r'\bderive \[([^\]]*)\]', text)
            if m:
                ds = [d.strip() for d in m.group(1).split(',')]
                idx = [DERIVE_ORDER.index(d) for d in ds if d in DERIVE_ORDER]
                if idx != sorted(idx) or len(idx) != len(ds):
                    self.err('order', n, f'derive order: {ds}')
            m = re.search(r'\brequires (.*?)(?: pub)?$', text)
            if m and it['kw'] == 'trait':
                rs = self.split_top(m.group(1))
                if rs != sorted(rs):
                    self.err('order', n, f'requires order: {rs}')
            # 型パラメータの境界 A + B の並び (linear は先頭、ライフタイムは末尾、他はFQN順)
            gen = self.generics_of(text)
            if gen is not None:
                for ent in self.split_top(gen):
                    if ':' not in ent:
                        continue
                    bound = ent.split(':', 1)[1].strip()
                    if bound in ('uses', 'panics') or '+' not in self.strip_args(bound):
                        continue
                    parts = [p.strip() for p in self.split_top(bound, '+')]

                    def rank(p):
                        return (0, '') if p == 'linear' else ((2, p) if p.startswith("'") else (1, p))
                    if parts != sorted(parts, key=rank):
                        self.err('order', n, f'bound order: {parts}')
        # capture の並び
        for k, l in enumerate(self.lines):
            m = re.search(r'\bcapture \[([^\]]*)\]', l)
            if m and m.group(1).strip():
                names = [re.sub(r'^(?:&mut |&|move )', '', c.strip()) for c in m.group(1).split(',')]
                if names != sorted(names):
                    self.err('order', k + 1, f'capture order: {names}')

    def check_indent(self, items):
        for it in items:
            if it['kw'] in FIELD_KINDS:
                for k, l in enumerate(it['body']):
                    if l and not (l.startswith('    ') and not l.startswith('     ')):
                        self.err('indent', it['body_start'] + k + 1, 'declaration line must be indented by exactly 4')
                continue
            if it['kw'] not in BODY_KINDS or (self.sig and it['kw'] == 'fn'):
                continue
            stack, stmt_base = [], 0
            for k, l in enumerate(it['body']):
                if l == '':
                    continue
                n = it['body_start'] + k + 1
                ind = len(l) - len(l.lstrip(' '))
                s = l.strip()

                def expect(e):
                    if ind != e:
                        self.err('indent', n, f'indent {ind}, expected {e}')
                if ind % 4:
                    self.err('indent', n, f'indent {ind} not multiple of 4')
                if s.startswith('//'):
                    expect(stack[-1][1] + 4 if stack else 4)
                    continue
                if s[0] == '}':
                    if not stack or stack[-1][0] != 'brace':
                        self.err('indent', n, 'unbalanced }')
                    else:
                        expect(stack[-1][1]); stack.pop()
                    if s.endswith('{'):
                        stack.append(('brace', ind))
                    continue
                if s[0] in ')]':
                    if not stack or stack[-1][0] != 'wrap':
                        self.err('indent', n, 'unbalanced closing bracket')
                    else:
                        expect(stack[-1][1]); stack.pop()
                    continue
                if s.startswith('.'):
                    expect(stmt_base + 4)
                    if s.endswith(('(', '[')):
                        stack.append(('wrap', ind))
                    continue
                in_wrap = bool(stack) and stack[-1][0] == 'wrap'
                expect(stack[-1][1] + 4 if stack else 4)
                if not in_wrap:
                    stmt_base = ind
                if s.endswith('{'):
                    stack.append(('brace', ind))
                elif s.endswith(('(', '[')):
                    stack.append(('wrap', ind))
            if stack:
                self.err('indent', it['end'] + 1, 'unclosed block/bracket in body')

    def idents_declared(self, it):
        names = []
        head = it['text']
        if it['kw'] in ('fn', 'test'):
            hm = re.match(r'(?:fn|test)\s+[A-Za-z0-9_.]+', head)
            pos = hm.end() if hm else 0
            if pos < len(head) and head[pos] == '[':
                d = 0
                while pos < len(head):
                    if head[pos] == '[':
                        d += 1
                    elif head[pos] == ']':
                        d -= 1
                        if d == 0:
                            pos += 1
                            break
                    pos += 1
            m = re.compile(r'\(').search(head, pos)
            if m:
                depth, i, start = 0, m.start(), m.start()
                j = i
                while j < len(head):
                    if head[j] == '(':
                        depth += 1
                    elif head[j] == ')':
                        depth -= 1
                        if depth == 0:
                            break
                    j += 1
                inner = head[i + 1:j]
                for p in self.split_top(inner):
                    nm = p.split(':')[0].strip()
                    if nm and nm != 'self':
                        names.append(('param', nm))
        body = '\n'.join(it['body'])
        for m in re.finditer(r'\blet (?:mut )?([a-z_][A-Za-z0-9_]*)\b(?!\s*\{)', body):
            names.append(('let', m.group(1)))
        for m in re.finditer(r'\blet \(([^)]*)\)', body):
            for nm in m.group(1).split(','):
                nm = nm.strip()
                if nm:
                    names.append(('let', nm))
        for m in re.finditer(r'\blet [A-Za-z0-9_.]+ \{([^}]*)\}', body):
            for f in m.group(1).split(','):
                if ':' in f:
                    names.append(('let', f.split(':')[1].strip()))
        for m in re.finditer(r'\bfn\(([^)]*)\)', body):
            for p in self.split_top(m.group(1)):
                nm = p.split(':')[0].strip()
                if nm:
                    names.append(('param', nm))
        for m in re.finditer(r'\bfor ([a-z_][a-z0-9_]*) in\b', body):
            names.append(('let', m.group(1)))
        return names, body

    def check_names_and_unused(self, items):
        for it in items:
            n = it['line'] + 1
            if it['kw'] == 'type':
                for k, l in enumerate(it['body']):
                    nm = l.strip().split(':')[0]
                    if not SNAKE.match(nm):
                        self.err('name', it['body_start'] + k + 1, f'field {nm} not snake_case')
            if it['kw'] not in BODY_KINDS or (self.sig and it['kw'] == 'fn'):
                continue
            names, body = self.idents_declared(it)
            head = it['text']
            for kind, nm in names:
                if nm == '_':
                    continue
                if not SNAKE.match(nm):
                    self.err('name', n, f'{kind} {nm} not snake_case')
                    continue
                cnt = len(re.findall(r'(?<![A-Za-z0-9_.])' + re.escape(nm) + r'(?![A-Za-z0-9_])', body))
                if kind == 'param':
                    cnt_head = len(re.findall(r'(?<![A-Za-z0-9_.])' + re.escape(nm) + r'(?=:)', head))
                    if nm.startswith('_'):
                        if cnt > 0:
                            self.err('unused', n, f'{nm} is marked unused but used')
                    elif cnt == 0:
                        self.err('unused', n, f'unused parameter {nm}')
                else:
                    if nm.startswith('_'):
                        if cnt > 1:
                            self.err('unused', n, f'{nm} is marked unused but used')
                    elif cnt < 2:
                        self.err('unused', n, f'unused variable {nm}')
            # match 腕のパターン束縛 (Name(a, b) =>)
            for m in re.finditer(r'^\s*[A-Za-z0-9_.]+\(([^)]*)\)\s*=>(.*)$', body, flags=re.M):
                for nm in [x.strip() for x in m.group(1).split(',')]:
                    if nm and nm != '_' and SNAKE.match(nm) and not nm.startswith('_'):
                        if not re.search(r'(?<![A-Za-z0-9_.])' + re.escape(nm) + r'(?![A-Za-z0-9_])', m.group(2)):
                            self.err('unused', n, f'unused pattern binding {nm}')

    def check_imports(self, items):
        text = '\n'.join(l for l in self.lines if not l.startswith('import '))
        for it in items:
            if it['kw'] != 'import':
                continue
            tail = it['name'].split('.')[-1]
            if not re.search(r'(?<![A-Za-z0-9_.])' + re.escape(tail) + r'\.', text):
                self.err('unused', it['line'] + 1, f'unused import {it["name"]}')

    def check_pub(self, items):
        private = [it['name'] for it in items
                   if it['kw'] in FIELD_KINDS and not it['text'].rstrip().endswith(' pub')]
        for it in items:
            if it['kw'] in ('fn', 'type', 'enum', 'trait') and it['text'].rstrip().endswith(' pub'):
                for p in private:
                    if re.search(r'(?<![A-Za-z0-9_.])' + re.escape(p) + r'(?![A-Za-z0-9_])', it['text']):
                        self.err('pub', it['line'] + 1, f'pub definition mentions private {p}')

    def run(self):
        items = self.items()
        self.check_fqn_and_names(items)
        self.check_order(items)
        self.check_blank(items)
        self.check_attrs(items)
        self.check_indent(items)
        self.check_names_and_unused(items)
        self.check_imports(items)
        self.check_pub(items)
        return sorted(self.errs, key=lambda e: (e[1], e[0]))


def main():
    sys.stdout.reconfigure(encoding='utf-8')
    root, expect, files = sys.argv[1], sys.argv[2], sys.argv[3:]
    bad = 0
    for f in files:
        errs = Lint(f, root).run()
        if expect == 'ok':
            good = not errs
        else:
            key = f.replace('\\', '/').rsplit('/', 1)[-1].split('_')[0]
            good = any(c == key for c, _, _ in errs)
        if not good:
            bad += 1
        status = 'PASS' if good else 'FAIL'
        detail = '' if (expect == 'fail' and good) else ''.join(f'\n    [{c}] line {n}: {m}' for c, n, m in errs)
        print(f'{status} {f}{detail}')
    sys.exit(1 if bad else 0)


if __name__ == '__main__':
    main()
