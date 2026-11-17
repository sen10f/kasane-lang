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
DERIVE_ORDER = ['Debug', 'Clone', 'Copy', 'TryClone', 'Drop', 'PartialEq', 'Eq', 'PartialOrd', 'Ord', 'Hash', 'From', 'ToJson', 'FromJson']
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
        # ファイル先頭の `///` だけの塊の直後の空行は、モジュールの説明の区切り (3.2)
        head_doc = 0
        while head_doc < len(L) and L[head_doc].startswith('///'):
            head_doc += 1
        for n in range(len(L) - 1):
            if L[n].startswith('//') and L[n + 1] == '':
                if n + 1 == head_doc and head_doc > 0 and all(x.startswith('///') for x in L[:head_doc]):
                    continue
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
                    # `) {` と `)? close g(` は、閉じと同じ行で次の波括弧・括弧を開く
                    if s.endswith('{'):
                        stack.append(('brace', ind))
                    elif s.endswith(('(', '[')):
                        stack.append(('wrap', ind))
                    continue
                if s.startswith('.'):
                    expect(stmt_base + 4)
                    if s.endswith('{'):
                        # `with .. = a` の次の `.b()? close c() {` のように、チェーンの最後の行が波括弧を開く
                        stack.append(('brace', stmt_base))
                    elif s.endswith(('(', '[')):
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

    def matching_close(self, s, op):
        d = 0
        for k in range(op, len(s)):
            if s[k] in '([{':
                d += 1
            elif s[k] in ')]}':
                d -= 1
                if d == 0:
                    return k
        return None

    def pattern_binds(self, pat, allow_upper, out):
        """パターンの文字列から束縛名を集める (タプル・path(args)・path { f: p }・入れ子・mut/ref/&・@・|)。"""
        pat = pat.strip()
        alts = self.split_top(pat, '|')
        if len(alts) > 1:
            self.pattern_binds(alts[0], allow_upper, out)
            return
        p = pat
        while True:
            q = p.lstrip()
            for pre in ('mut ', 'ref ', '&'):
                if q.startswith(pre):
                    q = q[len(pre):]
                    break
            if len(q) == len(p):
                break
            p = q
        p = p.strip()
        if p in ('', '_', '..'):
            return
        if '@' in p:
            name, rest = p.split('@', 1)
            name = name.strip()
            if not rest.startswith('=') and re.fullmatch(r'[A-Za-z_][A-Za-z0-9_]*', name):
                self.pattern_binds(name, allow_upper, out)
                self.pattern_binds(rest, allow_upper, out)
                return
        if p[0] in '([':
            end = self.matching_close(p, 0)
            if end is not None:
                for part in self.split_top(p[1:end]):
                    self.pattern_binds(part, allow_upper, out)
            return
        m = re.match(r'[A-Za-z0-9_.]*', p)
        path, rest = p[:m.end()], p[m.end():].lstrip()
        if rest[:1] in ('(', '{') and rest:
            end = self.matching_close(rest, 0)
            if end is not None:
                inner = rest[1:end]
                for part in self.split_top(inner):
                    if rest[0] == '{' and ':' in part:
                        self.pattern_binds(part.split(':', 1)[1], allow_upper, out)
                    else:
                        self.pattern_binds(part, allow_upper, out)
            return
        if rest == '' and re.fullmatch(r'[A-Za-z_][A-Za-z0-9_]*', path) and path not in ('true', 'false'):
            if path[0].islower() or path[0] == '_' or (allow_upper and path[0].isupper()):
                out.append(path)

    def binding_sites(self, body):
        """(種類, パターン, 大文字の名前も束縛か)。let / if let / while let / for / with / クロージャの引数。"""
        out = []
        for m in re.finditer(r'(?<![A-Za-z0-9_.])(let |for |with |fn\()', body):
            kw, i = m.group(1), m.start()
            if kw == 'fn(':
                op = i + 2
                close = self.matching_close(body, op)
                if close is not None:
                    for part in self.split_top(body[op + 1:close]):
                        if ':' in part:
                            out.append(('cparam', part.split(':', 1)[0], False))
                continue
            start = m.end()
            is_for = kw == 'for '
            d, end = 0, len(body)
            for at in range(start, len(body)):
                ch = body[at]
                if ch in '([{':
                    d += 1
                elif ch in ')]}':
                    d -= 1
                if d < 0 or (d == 0 and ch in ';\n'):
                    end = at
                    break
                if d == 0 and is_for and body.startswith(' in ', at):
                    end = at
                    break
                if d == 0 and not is_for and ch == '=' and body[at + 1:at + 2] not in ('=', '>'):
                    end = at
                    break
                if d == 0 and not is_for and ch == ':':
                    end = at
                    break
            before = body[:i].rstrip()
            refutable = before.endswith('if') or before.endswith('while')
            out.append(('let', body[start:end], not refutable))
        return out

    def count_assign(self, text, nm):
        n = 0
        for m in re.finditer(r'(?<![A-Za-z0-9_.])' + re.escape(nm) + r'(?![A-Za-z0-9_])', text):
            rest = text[m.end():].lstrip(' \t')
            if not rest.startswith('=') or rest.startswith('==') or rest.startswith('=>'):
                continue
            pre = text[:m.start()].rstrip(' \t')
            if pre == '' or pre[-1] in '\n{;' or pre.endswith('=>'):
                n += 1
        return n

    def count_capture_mut(self, text, nm):
        n = 0
        for m in re.finditer(r'\bcapture \[([^\]]*)\]', text):
            for c in m.group(1).split(','):
                c = c.strip()
                if c.startswith('&mut ') and c[5:].strip() == nm:
                    n += 1
        return n

    def arms(self, body):
        """match の腕: (パターン, ガードと本体)。行頭の `パターン =>`。本体は同じ行の残り (空なら次の行)、`{` で終わればブロック全体。"""
        out = []
        for m in re.finditer(r'^[ \t]*([^\n]*?)[ \t]*=>(.*)$', body, re.M):
            pat = m.group(1)
            if not pat or ' = ' in pat or pat.startswith('let ') or pat.startswith('if let '):
                continue
            guard = ''
            idx = pat.find(' if ')
            if idx >= 0:
                guard, pat = pat[idx:], pat[:idx]
            arm_body = guard + ' ' + m.group(2)
            rest_start = m.end()
            if not m.group(2).strip():
                rest = body[rest_start:]
                if rest.startswith('\n'):
                    rest = rest[1:]
                line = rest.split('\n', 1)[0]
                rest_start += (len(body[rest_start:]) - len(rest)) + len(line)
                arm_body += line
            if arm_body.rstrip().endswith('{'):
                rest = body[rest_start:]
                d, cut = 1, len(rest)
                for k, ch in enumerate(rest):
                    if ch == '{':
                        d += 1
                    elif ch == '}':
                        d -= 1
                        if d == 0:
                            cut = k
                            break
                arm_body += '\n' + rest[:cut]
            out.append((pat, arm_body))
        return out

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
        # 束縛は pattern_binds の 1 か所で集める (stage0 の lint.rs と同じ規則)
        for kind, pat, upper in self.binding_sites(body):
            bs = []
            self.pattern_binds(pat, upper, bs)
            for nm in bs:
                names.append((kind, nm))
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
            arm_list = self.arms(body)
            arm_names = []
            for pat, _ in arm_list:
                self.pattern_binds(pat, False, arm_names)
            seen = {}
            for kind, nm in names:
                seen[(kind, nm)] = seen.get((kind, nm), 0) + 1
            for (kind, nm), decls in seen.items():
                if nm in ('_', 'true', 'false'):
                    continue
                if not SNAKE.match(nm):
                    self.err('name', n, f'{kind} {nm} not snake_case')
                    continue
                cnt = len(re.findall(r'(?<![A-Za-z0-9_.])' + re.escape(nm) + r'(?![A-Za-z0-9_])', body))
                if kind == 'param':
                    if nm.startswith('_'):
                        if cnt > 0:
                            self.err('unused', n, f'{nm} is marked unused but used')
                    elif cnt == 0:
                        self.err('unused', n, f'unused parameter {nm}')
                elif nm.startswith('_'):
                    if cnt > decls + arm_names.count(nm):
                        self.err('unused', n, f'{nm} is marked unused but used')
                else:
                    assigns = self.count_assign(body, nm)
                    cap_mut = self.count_capture_mut(body, nm) if assigns > 0 else 0
                    if max(cnt - (decls + assigns + cap_mut), 0) == 0:
                        what = 'parameter' if kind == 'cparam' else 'variable'
                        self.err('unused', n, f'unused {what} {nm}')
            # match の腕のパターン束縛 (腕の範囲だけで数える)
            for pat, rest in arm_list:
                bs = []
                self.pattern_binds(pat, False, bs)
                for nm in dict.fromkeys(bs):
                    if SNAKE.match(nm) and not nm.startswith('_'):
                        if not re.search(r'(?<![A-Za-z0-9_.])' + re.escape(nm) + r'(?![A-Za-z0-9_])', rest):
                            self.err('unused', n, f'unused pattern binding {nm}')

    def check_imports(self, items):
        text = '\n'.join(l for l in self.lines if not l.startswith('import '))
        for it in items:
            if it['kw'] != 'import':
                continue
            tail = it['name'].split('.')[-1]
            # 3.3: 短縮形でも FQN でも、ヘッダでも本体でも、参照があれば使用済み。
            if not any(re.search(r'(?<![A-Za-z0-9_.])' + re.escape(n) + r'\.', text) for n in (tail, it['name'])):
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
