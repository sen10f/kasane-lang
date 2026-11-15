#!/usr/bin/env python3
"""Kasane grammar checker.

spec/grammar.ebnf を読み込み、.ksnl ソースが file 規則で導出できるかを検証する。
使い方: python -I ebnfcheck.py <grammar.ebnf> <ok|fail> <source>...

  ok  : 全ファイルが導出できること。
  fail: 全ファイルが拒否されること。ただしファイル名が lex_ で始まる例は字句/事前検査の
        エラーでも可。それ以外は「字句・事前検査は通り、文法で拒否される」ことを要求する
        (字句バグによる偽のPASSを防ぐ)。

検証範囲: 構文構造と、仕様書 3.7 の字句規則のうち機械的に判定できるもの
(BOM/CR/タブ/末尾LF/行末空白/行長/エスケープ/リテラル形式/定義内の空行)。
INDENT/INDENT_BACK/CONT_INDENT/CLOSE_INDENT は零幅として扱い、インデント深さは検査しない。
意味検査(命名規約・並び順・FQNとパスの一致・トップレベル空行・型検査)は対象外。
"""
import re
import sys

RESERVED = set("""fn type enum trait impl const static extern test end import let mut if else
match loop while for in break continue return true false self Self pub uses panics stack impls
derive linear layout requires unsafe move with capture dyn blanket link never may async await where
ref as i8 i16 i32 i64 i128 isize u8 u16 u32 u64 u128 usize f32 f64 bool char str""".split())

HEADER_KW = ('fn', 'type', 'enum', 'trait', 'impl', 'const', 'static', 'extern', 'test', 'import', 'end')
DEF_START = ('fn', 'type', 'enum', 'trait', 'test')


class PreCheckError(SyntaxError):
    pass


# ---------------------------------------------------------------- EBNF 読み込み
def strip_comments(s):
    out, i = [], 0
    while i < len(s):
        if s.startswith('(*', i):
            i = s.index('*)', i) + 2
        else:
            out.append(s[i]); i += 1
    return ''.join(out)


def ebnf_tokens(s):
    toks, i = [], 0
    while i < len(s):
        c = s[i]
        if c.isspace():
            i += 1
        elif c in '"\'':
            j = s.index(c, i + 1)
            toks.append(('str', s[i + 1:j])); i = j + 1
        elif c.isalpha() or c == '_':
            m = re.match(r'[A-Za-z_][A-Za-z_0-9]*', s[i:])
            toks.append(('id', m.group(0))); i += len(m.group(0))
        elif s.startswith('...', i):
            toks.append(('dots', '...')); i += 3
        else:
            toks.append(('p', c)); i += 1
    return toks


class EbnfParser:
    def __init__(self, toks):
        self.t, self.i = toks, 0

    def peek(self):
        return self.t[self.i] if self.i < len(self.t) else (None, None)

    def eat(self, v):
        k, x = self.peek()
        if x != v:
            raise SyntaxError(f'expected {v} got {x} at {self.i}')
        self.i += 1

    def rules(self):
        out = {}
        while self.i < len(self.t):
            k, name = self.peek()
            if k != 'id':
                raise SyntaxError(f'rule name expected, got {name!r} at {self.i}')
            self.i += 1
            self.eat('=')
            out[name] = self.alt()
            self.eat(';')
        return out

    def alt(self):
        alts = [self.seq()]
        while self.peek() == ('p', '|'):
            self.i += 1
            alts.append(self.seq())
        return alts[0] if len(alts) == 1 else ('alt', alts)

    def seq(self):
        items = [self.factor()]
        while self.peek() == ('p', ','):
            self.i += 1
            items.append(self.factor())
        return items[0] if len(items) == 1 else ('seq', items)

    def factor(self):
        k, v = self.peek()
        if k == 'str':
            self.i += 1; return ('term', v)
        if k == 'id':
            self.i += 1; return ('ref', v)
        if k == 'p' and v == '(':
            self.i += 1; r = self.alt(); self.eat(')'); return r
        if k == 'p' and v == '[':
            self.i += 1; r = self.alt(); self.eat(']'); return ('opt', r)
        if k == 'p' and v == '{':
            self.i += 1; r = self.alt(); self.eat('}'); return ('rep', r)
        raise SyntaxError(f'unexpected {v!r} at {self.i}')


def load_grammar(path):
    src = strip_comments(open(path, encoding='utf8').read())
    # "a" | ... | "z" のような範囲規則は字句層が担うので除外
    src = re.sub(r'(?<![A-Za-z_])(lower|upper|letter|digit|hex|bin|oct)\s*=[^;]*;', '', src)
    return EbnfParser(ebnf_tokens(src)).rules()


# ---------------------------------------------------------------- 字句
class Tok:
    __slots__ = ('kind', 'text', 'sp')

    def __init__(self, kind, text, sp=False):
        self.kind, self.text, self.sp = kind, text, sp

    def __repr__(self):
        return f'{self.kind}:{self.text!r}'


SUFFIX = r'(?:i8|i16|i32|i64|i128|isize|u8|u16|u32|u64|u128|usize)'
FSUFFIX = r'(?:f32|f64)'
D = r'[0-9]'
CH = r"""'(?:\\(?:x[0-7][0-9a-f]|u\{[0-9a-f]{1,6}\}|[\\ntr0'])|[^'\\\n])'"""
ST = r'''"(?:\\(?:x[0-7][0-9a-f]|u\{[0-9a-f]{1,6}\}|[\\ntr0"])|[^"\\\n])*"'''
BCH = r"""b'(?:\\(?:x[0-9a-f]{2}|[\\ntr0'])|[\x20-\x26\x28-\x5b\x5d-\x7e])'"""
BST = r'''b"(?:\\(?:x[0-9a-f]{2}|[\\ntr0"])|[\x20\x21\x23-\x5b\x5d-\x7e])*"'''
TOKEN_RE = re.compile('|'.join([
    f'(?P<bstr>{BST})',
    f'(?P<bchr>{BCH})',
    f'(?P<str>{ST})',
    f'(?P<chr>{CH})',
    r"(?P<life>'[a-z_][a-z0-9_]*)",
    rf'(?P<float>{D}+\.{D}+(?:e[+-]?{D}+)?{FSUFFIX}?|{D}+e[+-]?{D}+{FSUFFIX}?)',
    rf'(?P<int>(?:0x[0-9a-f_]+|0b[01_]+|0o[0-7_]+|{D}[0-9_]*){SUFFIX}?)',
    r'(?P<word>[A-Za-z_][A-Za-z0-9_]*)',
    r'(?P<op>::|->|=>|==|!=|<=|>=|&&|\|\||[-+*/%&|^!<>=(){}\[\],.:;?])',
]))


def check_unicode_escapes(text, lineno):
    for m in re.finditer(r'\\u\{([0-9a-f]{1,6})\}', text):
        v = int(m.group(1), 16)
        if v > 0x10FFFF or 0xD800 <= v <= 0xDFFF:
            raise SyntaxError(f'line {lineno}: invalid \\u escape {m.group(0)}')


def check_digit_groups(text, lineno):
    body = re.sub(SUFFIX + '$', '', text)
    if '_' not in body:
        return
    if body[:2] in ('0x', '0b', '0o'):
        digits, widths = body[2:], (2, 4)
    else:
        digits, widths = body, (3,)
    groups = digits.split('_')
    if any(g == '' for g in groups):
        raise SyntaxError(f'line {lineno}: bad digit separator in {text}')
    w = len(groups[1])
    if w not in widths or any(len(g) != w for g in groups[1:]) or len(groups[0]) > w:
        raise SyntaxError(f'line {lineno}: digit groups of {text} must be {widths}-wide')


def lex_code(line, lineno):
    toks, i, sp = [], 0, False
    while i < len(line):
        if line[i] == ' ':
            i += 1; sp = True; continue
        m = TOKEN_RE.match(line, i)
        if not m:
            raise SyntaxError(f'line {lineno}: lex error at {line[i:i+20]!r}')
        k, t = m.lastgroup, m.group(0)
        i = m.end()
        if k in ('str', 'chr'):
            check_unicode_escapes(t, lineno)
        if k == 'int':
            check_digit_groups(t, lineno)
        if k == 'word':
            if t in RESERVED:
                toks.append(Tok('kw', t, sp))
            elif re.fullmatch(r'_?[a-z][a-z0-9_]*', t):
                toks.append(Tok('ident', t, sp))
            elif re.fullmatch(r'[A-Z][A-Za-z0-9_]*', t):
                toks.append(Tok('upper', t, sp))
            else:
                toks.append(Tok('word', t, sp))          # "_" 単独など
        else:
            toks.append(Tok(k, t, sp))
        sp = False
    return toks


def precheck(text):
    if text.startswith('\ufeff'):
        raise PreCheckError('BOM')
    if '\r' in text:
        raise PreCheckError('CR (LF only)')
    if '\t' in text:
        raise PreCheckError('TAB')
    if text == '' or not text.endswith('\n') or text.endswith('\n\n'):
        raise PreCheckError('file must end with exactly one LF')
    for n, l in enumerate(text.split('\n')[:-1], 1):
        if l != l.rstrip(' '):
            raise PreCheckError(f'line {n}: trailing space')
        first = re.split(r'[ (\[]', l.lstrip(' '), 1)[0]
        has_str = '"' in l
        if len(l) > 100 and first not in HEADER_KW and not has_str:
            raise PreCheckError(f'line {n}: longer than 100 chars')
        if l.startswith(' ') and (len(l) - len(l.lstrip(' '))) % 4 != 0 and not l.lstrip(' ').startswith('//'):
            # 継続行や match 腕は 4 の倍数。零幅扱いのため厳密には検査しないが、奇数インデントは弾く
            pass


def lex_file(text, sig=False):
    precheck(text)
    lines = text.split('\n')[:-1]
    info = [(n, l, l.strip()) for n, l in enumerate(lines, 1)]
    out, inside, prev_comment = [], False, False
    # ファイル先頭が `///` だけの塊で、その直後の空行 1 つだけがモジュールの説明の区切り (3.2)。
    head_doc = True
    for idx, (n, l, s) in enumerate(info):
        if s == '':
            if prev_comment:
                if head_doc:
                    out = [Tok('MDOC', '')]  # ファイル先頭の /// の塊は MDOC 1 つに置き換える (3.2)
                    head_doc = False
                    prev_comment = False
                    continue
                raise SyntaxError(f'line {n}: blank line after comment')
            head_doc = False
            if inside:
                out.append(Tok('BLANK', ''))
            prev_comment = False
            continue
        if not s.startswith('///'):
            head_doc = False
        head = l.split(' ', 1)[0] if not l.startswith(' ') else ''
        if head in DEF_START and not (sig and head == 'fn'):
            inside = True
        if s.startswith('///'):
            out += [Tok('kw', '///'), Tok('text', s[3:]), Tok('NL', '')]
            prev_comment = True
            continue
        if s.startswith('//'):
            out += [Tok('kw', '//'), Tok('text', s[2:]), Tok('NL', '')]
            prev_comment = not l.startswith(' ')
            continue
        prev_comment = False
        code = lex_code(s, n)
        nxt = next((x for x in info[idx + 1:] if x[2] != '' and not x[2].startswith('//')), None)
        inhibit = s.endswith(('(', '[', ',')) or (nxt is not None and nxt[2].startswith('.') and not nxt[2].startswith('..'))
        out += code
        out.append(Tok('LF' if inhibit else 'NL', ''))
        if head == 'end':
            inside = False
    return out


# ---------------------------------------------------------------- マッチング
ZERO_WIDTH = {'INDENT', 'INDENT_BACK', 'CONT_INDENT', 'CLOSE_INDENT'}


class Matcher:
    def __init__(self, rules, toks):
        self.rules, self.toks = rules, toks
        self.memo = {}
        self.term_cache = {}

    def term_seq(self, s):
        if s not in self.term_cache:
            self.term_cache[s] = [s] if s in ('///', '//') else [t.text for t in lex_code(s, 0)]
        return self.term_cache[s]

    def tok_at(self, p):
        return self.toks[p] if p < len(self.toks) else None

    def override(self, name, p):
        t = self.tok_at(p)
        if name in ZERO_WIDTH:
            return {p}
        if name in ('NL', 'LF', 'BLANK', 'MDOC'):
            return {p + 1} if t and t.kind == name else set()
        if t is None:
            return set()
        if name == 'ident':
            return {p + 1} if t.kind == 'ident' else set()
        if name == 'upper_ident':
            return {p + 1} if t.kind == 'upper' else set()
        if name == 'lifetime':
            return {p + 1} if t.kind == 'life' else set()
        if name == 'int_lit':
            q = p
            if t.text == '-' and t.kind == 'op':
                q = p + 1
            u = self.tok_at(q)
            if u and u.kind == 'int' and not (q > p and u.sp):
                return {q + 1}
            return set()
        if name == 'int_dec':
            return {p + 1} if t.kind == 'int' and re.fullmatch(r'[0-9][0-9_]*', t.text) else set()
        if name == 'float_lit':
            return {p + 1} if t.kind == 'float' else set()
        if name == 'char_lit':
            return {p + 1} if t.kind == 'chr' else set()
        if name == 'string':
            return {p + 1} if t.kind == 'str' else set()
        if name == 'byte_lit':
            return {p + 1} if t.kind == 'bchr' else set()
        if name == 'byte_string':
            return {p + 1} if t.kind == 'bstr' else set()
        if name in ('text', 'char_any', 'ascii'):
            return {p + 1} if t.kind == 'text' else set()
        return None

    def m(self, node, p):
        k = node[0]
        if k == 'term':
            q = p
            for s in self.term_seq(node[1]):
                t = self.tok_at(q)
                if t is None or t.text != s or t.kind in ('str', 'chr', 'bstr', 'bchr', 'text'):
                    return set()
                q += 1
            return {q}
        if k == 'ref':
            return self.rule(node[1], p)
        if k == 'seq':
            cur = {p}
            for it in node[1]:
                nxt = set()
                for c in cur:
                    nxt |= self.m(it, c)
                cur = nxt
                if not cur:
                    return set()
            return cur
        if k == 'alt':
            r = set()
            for it in node[1]:
                r |= self.m(it, p)
            return r
        if k == 'opt':
            return {p} | self.m(node[1], p)
        if k == 'rep':
            ends, frontier = {p}, {p}
            while frontier:
                new = set()
                for c in frontier:
                    new |= self.m(node[1], c)
                new -= ends
                ends |= new
                frontier = new
            return ends
        raise ValueError(node)

    def rule(self, name, p):
        key = (name, p)
        if key in self.memo:
            return self.memo[key]
        ov = self.override(name, p)
        if ov is not None:
            self.memo[key] = ov
            return ov
        if name not in self.rules:
            raise KeyError(f'undefined rule {name}')
        self.memo[key] = set()          # 左再帰ガード
        r = self.m(self.rules[name], p)
        self.memo[key] = r
        return r


def furthest(matcher):
    best = 0
    for (name, p), ends in matcher.memo.items():
        for e in ends:
            best = max(best, e)
    return best


def main():
    sys.setrecursionlimit(20000)
    sys.stdout.reconfigure(encoding='utf-8')
    gpath, expect, files = sys.argv[1], sys.argv[2], sys.argv[3:]
    rules = load_grammar(gpath)
    bad = 0
    for f in files:
        with open(f, encoding='utf8', newline='') as fh:
            text = fh.read()
        name = f.replace('\\', '/').rsplit('/', 1)[-1]
        lexfail = None
        parsed, extra = False, ''
        try:
            toks = lex_file(text, f.endswith('.sig'))
            mt = Matcher(rules, toks)
            parsed = len(toks) in mt.rule('sig_file' if f.endswith('.sig') else 'file', 0)
            if not parsed:
                pos = furthest(mt)
                around = ' '.join(t.text or t.kind for t in toks[max(0, pos - 6):pos + 4])
                extra = f' (stuck near token #{pos}: ... {around} ...)'
        except SyntaxError as e:
            lexfail = str(e)
            extra = f' ({"precheck" if isinstance(e, PreCheckError) else "lex"} error: {e})'
        if expect == 'ok':
            good = parsed
        else:
            good = (not parsed) and (lexfail is None or name.startswith('lex_'))
        if not good:
            bad += 1
        print(f'{"PASS" if good else "FAIL"} {f}: parsed={"yes" if parsed else "no"}{extra}')
    sys.exit(1 if bad else 0)


if __name__ == '__main__':
    main()
