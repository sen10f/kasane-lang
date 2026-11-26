# Kasane 所有・移動・借用・線形・効果の決まり

この文書は Kasane（v0.10）の「所有権・移動・借用・線形型・効果（`uses`/`panics`）」、「参照は二級の値」、「`with`・`unsafe`・`suspend`」などの言語規則を、番号付きルール K1–K16 としてまとめたものである。

各ルールは以下の構成である。

1. **文言** — 規則を一文で述べる。
2. **なぜ** — なぜその規則が必要か。
3. **正しいプログラム** — `ksn check` を通る例（ファイル `docs/borrow_rules/K#_ok.ksnl`）。
4. **却下されるプログラム** — `ksn check` が拒否する例と、それが正確に出力する診断コードとメッセージ（ファイル `docs/borrow_rules/K#_ng.ksnl`）。
5. **修正** — 却下例を直す方針。

例はすべて `/home/kasane/kasane-verify/bin/current/ksn`（stage0 をビルドしない）で `ksn check` して確かめた。各例と期待診断は `docs/borrow_rules/check.py` で自動検証され、`python3 -I check.py` が全件 PASS する。`.ksnl` はコメントを書けないため、文中に注記を置く。

注意：`alloc.arena.Arena<T, A>` / `alloc.arena.Handle<T>`（アリーナ＋世代つきハンドル、`設計_共有の取っ手.md` D1）は stdlib に実装済み（`spec/stdlib/alloc/arena.ksnl` + `.sig`）。この文書の検証済みサンプルでは、参照を欄に置けない代替として「インデックス（位置の添字）を持ち、元データを関数の引数で渡す」形を採っている。アリーナ＋ハンドルは同じ書き方（`Handle<T>` は `Copy` で欄・`Vec` に入る）で扱える。

## K1 `Copy` / 移動 / 線形の区別

**文言。** `linear` と宣言された型（または線形な値を含む型）は移動（move）され、所有は一度だけ移る。`Copy` を `derive` した型だけを複数回使える。非線形・非 `Copy` の値も、move 後は使用できない。

**なぜ。** GC なしで資源を管理するため、誰がいつ値を破棄するかを静的に一意に決める必要がある。`Copy` は値を複製して何度でも使えるが、非 `Copy` は所有が移る。

**正しいプログラム** (`docs/borrow_rules/K1_ok.ksnl`):

```
fn t.main() -> () uses [] panics never
    core.mem.drop(t.pair(7u32))
end t.main

fn t.pair[T: core.ops.Copy](x: T) -> (T, T) uses [] panics never
    (x, x)
end t.pair
```

`7u32` は `Copy` なので `x` を二回使える。

**却下されるプログラム** (`docs/borrow_rules/K1_ng.ksnl`):

```ksnl fragment
fn t.main() -> () uses [] panics never
    let s = alloc.string.String.new(std.alloc.SYSTEM)
    core.mem.drop(s)
    core.mem.drop(s)
end t.main
```

`String` は非 `Copy`（非線形）なので、`core.mem.drop(s)` で所有が移った後の二度目の `core.mem.drop(s)` は `ksn check` で：

```ksnl fragment
E3002: use of moved value `s` (moved at line 3)
```

**修正。** `core.mem.drop(s)` は一度だけ書く。複数回使いたい値は `Copy` の型にするか、ヒープを持つ型なら `alloc.alloc.TryClone::try_clone(&s)?` で複製する。

---

## K2 線形値は全経路で 1 回消費する（E3003）

**文言。** `linear` 型の値は、通常の終端・`return`・`?`・各分岐のすべての経路でちょうど 1 回消費（`core.mem.drop`、`self` を値で受ける関数への渡し、分解など）しなければならず、消費されずにスコープを抜けると E3003 である。

**なぜ。** 線型はファイル・ソケット・トランザクションなど「後始末を漏らせない」資源を表すため、どの経路でも正確に 1 回後始末されることを保証する。

**正しいプログラム** (`docs/borrow_rules/K2_ok.ksnl`):

```
type t.L linear pub
    v: u32
end t.L

fn t.eat(l: t.L) -> () uses [] panics never
    let t.L {
        v: v
    } = l
    core.mem.drop(v)
end t.eat

fn t.main() -> () uses [] panics never
    let l = t.L {
        v: 1u32
    }
    core.mem.drop(t.peek(&l))
    t.eat(l)
end t.main

fn t.peek(l: &t.L) -> u32 uses [] panics never
    l.v
end t.peek
```

`peek` で借用してから `eat` で消費し、全経路で 1 回ずつ処理される。

**却下されるプログラム** (`docs/borrow_rules/K2_ng.ksnl`):

```ksnl fragment
type t.L linear pub
    v: u32
end t.L

fn t.peek(l: &t.L) -> u32 uses [] panics never
    l.v
end t.peek

fn t.main() -> () uses [] panics never
    let l = t.L { v: 1u32 }
    core.mem.drop(t.peek(&l))
end t.main
```

`l` は借用のみで消費されずにスコープを抜けるため、`ksn check` で：

```ksnl fragment
E3003: linear value `l` is never consumed when its scope ends
```

**修正.** `t.eat(l)` のように、すべての経路で `l` を 1 回消費する。

---

## K3 移動後の使用・二重 drop は禁止（E3002）

**文言。** 移動（または `core.mem.drop` による消費）後の値を使用することはできず、E3002 "use of moved value" となる。

**なぜ.** 移動済みの値にアクセスすると解放済みメモリを参照することになり、メモリ安全性が破られる。

**正しいプログラム** (`docs/borrow_rules/K3_ok.ksnl`):

```
fn t.main() -> () uses [] panics never
    let s = std.string.String.new()
    let n = s.len()
    core.mem.drop(n)
    core.mem.drop(s)
end t.main
```

`s.len()` で使用した後、`s` を一度だけ `drop` する。

**却下されるプログラム** (`docs/borrow_rules/K3_ng.ksnl`):

```ksnl fragment
fn t.main() -> () uses [] panics never
    let s = alloc.string.String.new(std.alloc.SYSTEM)
    core.mem.drop(s)
    let n = s.len()
    core.mem.drop(n)
end t.main
```

`core.mem.drop(s)` の後の `s.len()` は移動済みの `s` に触れるため、`ksn check` で：

```ksnl fragment
E3002: use of moved value `s` (moved at line 3)
```

**修正.** 値を使い終わったら一度 `drop` し、それ以降は触らない。使用は `drop` の前におく。

---

## K4 参照は二級の値—型の欄に置けない（E3060）

**文言.** 参照（`&T` / `&mut T`、および `Option<&T>` など参照を運ぶ型）は、引数・戻り値・局所の束縛にだけ置ける。`type`/`enum` の欄・バリアントペイロード・ライフタイム引数付きの型宣言、`Vec`/`Box`/`HashMap` などの型引数に置くと E3060 となる。`&'static T`（領域を持たない参照）だけは例外で欄・型引数に置ける。

**なぜ.** 参照には有効範囲（領域）があり、それをデータ構造に長期保存するとダングリング参照となる。参照を二級の値にすることで、このクラスのバグを型検査で防ぐ。

**正しいプログラム** (`docs/borrow_rules/K4_ok.ksnl`):

```
type t.Parser
    pos: usize
end t.Parser

fn t.main() -> () uses [] panics never
    let src = [72u8, 105u8]
    let mut p = t.Parser {
        pos: 0usize
    }
    let b = t.next(&mut p, &src)
    core.mem.drop(b)
end t.main

fn t.next(p: &mut t.Parser, src: &[u8]) -> Option<u8> uses [] panics never
    match src.get(p.pos) {
        Some(b) => {
            p.pos = p.pos.wrapping_add(1usize)
            Some(*b)
        }
        None => None
    }
end t.next
```

参照を欄に持たず、`pos`（添字）だけを持ち、元データ `src` を毎回引数で渡す。`alloc.arena.Handle<T>` も同じ方針（インデックスベース）である。

**却下されるプログラム** (`docs/borrow_rules/K4_ng.ksnl`):

```ksnl fragment
type t.H['a]
    r: &'a alloc.string.String
end t.H

fn t.main() -> () uses [] panics never
end t.main
```

`type` にライフタイム引付きで、欄が `&'a String` なので、`ksn check` で（2 件とも E3060）：

```ksnl fragment
E3060: t.H: a type cannot declare a lifetime parameter (references are second-class values); hold a position index and pass the source data as an argument instead
E3060: t.H: a field or variant payload cannot hold a reference (references are second-class values); hold an index or an owned value, and pass the source data as an argument
```

**修正.** 欄から参照を外し、`pos: usize` などの位置を持ち、ソースデータを引数で渡す（K4_ok）。`Vec<&T>` の代わりに `Vec<usize>`（添字の配列）を使う。

---

## K5 共有 vs `&mut` の衝突（E3010）

**文言.** 同じ場所に対する `&`（共有借用）と `&mut`（可変借用）、または `&mut` と `&mut` が同時に生きていると E3010 となる。

**なぜ.** 可変借用はその場所への排他的アクセスを意味し、共有借用との共存や二重の可変借用はデータ競合・破壊的変更の見落としを招く。

**正しいプログラム** (`docs/borrow_rules/K5_ok.ksnl`):

```
fn t.a_two(a: &mut u32, b: &mut u32) -> () uses [] panics never
    *a = 1u32
    *b = 2u32
end t.a_two

fn t.good() -> u32 uses [] panics never
    let mut x = 1u32
    let mut y = 2u32
    t.a_two(&mut x, &mut y)
    x.wrapping_add(y)
end t.good

fn t.main() -> () uses [] panics never
    core.mem.drop(t.good())
end t.main
```

**却下されるプログラム** (`docs/borrow_rules/K5_ng.ksnl`):

```ksnl fragment
fn t.a_two(a: &mut u32, b: &mut u32) -> () uses [] panics never
    *a = 1u32
    *b = 2u32
end t.a_two

fn t.bad() -> u32 uses [] panics never
    let mut x = 1u32
    t.a_two(&mut x, &mut x)
    x
end t.bad

fn t.main() -> Result<(), std.io.Error> uses [] panics never
    Ok(())
end t.main
```

同じ `x` に二つの `&mut` を渡すため、`ksn check` で：

```ksnl fragment
E3010: cannot use `x` while it is mutably borrowed by an earlier operand of the same expression
```

**修正.** `&mut` 借用は最後の使用までで終わる（NLL）。別々の値を渡すか、借用が終わってから再度借用する。

---

## K6 借用は最後の使用まで（NLL）— 生きている間の移動は E3010

**文言.** 借用はその参照の最後の使用までで終了し、それ以降の移動（move）は許される。借用がまだ生きている間に持ち主を move すると E3010 となる。

**なぜ.** Kasane の借用検査は NLL（非発生ライフタイム）に近い扱いをし、不要に長く借用を延ばさない。これにより「借用直後の move」など安全なコードを通しつつ、借用中の move は拒否する。

**正しいプログラム** (`docs/borrow_rules/K6_ok.ksnl`):

```
type t.S
    name: std.string.String
    other: std.string.String
end t.S

fn t.consume(s: std.string.String) -> () uses [io.console] panics never
    core.mem.drop(std.io.print_line(s.as_str()))
    core.mem.drop(s)
end t.consume

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics never
    let x = t.mks()
    let r = &x.name
    core.mem.drop(t.peek(r))
    t.consume(x.other)
    Ok(())
end t.main

fn t.mk() -> std.string.String uses [alloc] panics never
    let mut s = std.string.String.new()
    core.mem.drop(s.push_str("hello, this is a long heap string"))
    s
end t.mk

fn t.mks() -> t.S uses [alloc] panics never
    t.S {
        name: t.mk()
        other: t.mk()
    }
end t.mks

fn t.peek(s: &std.string.String) -> usize uses [io.console] panics never
    core.mem.drop(std.io.print_line(s.as_str()))
    s.len()
end t.peek
```

`r = &x.name` の使用が `peek` で終わってから、`x.other`（別の欄）を move しているので通る。

**却下されるプログラム** (`docs/borrow_rules/K6_ng.ksnl`):

```ksnl fragment
fn t.consume(s: alloc.string.String) -> () uses [io.console] panics never
    core.mem.drop(std.io.print_line(s.as_str()))
    core.mem.drop(s)
end t.consume

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics never
    let s = t.mk()
    let r = &s
    t.consume(s)
    let n = t.peek(r)
    core.check.eq(n, 33usize)
    core.mem.drop(std.io.print_line(r.as_str()))
    Ok(())
end t.main

fn t.mk() -> alloc.string.String uses [] panics never
    alloc.string.String.new(std.alloc.SYSTEM)
end t.mk

fn t.peek(s: &alloc.string.String) -> usize uses [] panics never
    s.len()
end t.peek
```

`r = &s` の後に `t.consume(s)` で `s` を move してしまい、その後 `r` を使うため、`ksn check` で：

```ksnl fragment
E3010: cannot move `s` while it is borrowed by `r`
```

**修正.** 借用 `r` の使用を `t.consume(s)` より前に終わらせるか、`t.consume` の前に `r` を使い切る。

---

## K7 参照の返却—エリュージョンの 2 規則、E3030、一時値は文末で破棄（2.10aa）

**文言.** 戻り値の参照のライフタイムは、(a) 引数の参照がちょうど 1 つ、または (b) 第 1 引数が `self: &...` / `&mut ...` で戻り値が `self` に由来、のときだけ省略（エリュージョン）でき、それ以外はヘッダで明示必須（明示すると E3021）。一時値への参照を返すと E3030。一時値は文の終端で破棄され、`let` による寿命延長は行われない（実装計画.md 2.10aa）。

**なぜ.** 省略した参照の領域は「呼び出しごとに新しく、互いに無関係」であり、安易に返すと呼び出し終了後に無効な参照を返すことになる。

**正しいプログラム** (`docs/borrow_rules/K7_ok.ksnl`):

```
type t.S
    v: u32
end t.S

fn t.S.get(self: &t.S) -> &u32 uses [] panics never
    &self.v
end t.S.get

fn t.main() -> Result<(), std.io.Error> uses [] panics never
    let s = t.S {
        v: 1u32
    }
    core.mem.drop(*s.get())
    Ok(())
end t.main
```

`self: &t.S` から `&u32` を返すので、エリュージョン規則 (b) により省略が正規形である。

**却下されるプログラム** (`docs/borrow_rules/K7_ng.ksnl`):

```ksnl fragment
fn t.bad(x: &u32) -> &u32 uses [] panics never
    &((*x).wrapping_add(1u32))
end t.bad

fn t.main() -> Result<(), std.io.Error> uses [] panics never
    let a = 1u32
    core.mem.drop(*t.bad(&a))
    Ok(())
end t.main
```

`(*x).wrapping_add(1u32)` は式の評価で生じる一時値であり、その参照を返すと一時値が文末で破棄された後に dangling となるため、`ksn check` で：

```ksnl fragment
E3030: returns a reference to a temporary value, which does not live long enough
```

**修正.** 一時値に名前を付けてから借りる（`let tmp = ...; &tmp`）、または値そのものを返す。呼び出し側で `let tmp = t.make(); let r = &tmp.v` のように書く。

---

## K8 参照越しの分解と `match`（束縛モード）

**文言.** `match &r { .. }` / `match &mut r { .. }` / `let (a, b) = &pair` のように参照越しで分解すると、束縛される名前は `&` の欄の型（`&T` / `&mut T`）になる。値を取り出すには `*` が必要。`match &r` は借りて分解するだけで消費しない。値のまま分解（`let` / `match` / `if let`）した線形値は `_` だけ・`Copy` の欄だけの分解でも常に消費とみなされ、その後の使用は E3002 となる。

**なぜ.** 参照越しの分解は所有を移さず、元の借用を維持する。線形値を「覗くだけ」に使いつつ、所有は後で通常通り消費できるようにするため。

**正しいプログラム** (`docs/borrow_rules/K8_ok.ksnl`):

```
type t.Pt
    x: u32
    y: u32
end t.Pt

fn t.bump(v: &mut u32) -> () uses [] panics never
    *v = v.wrapping_add(1u32)
end t.bump

fn t.main() -> Result<(), std.io.Error> uses [] panics may
    let mut o = Some(1u32)
    match &o {
        Some(p) => core.mem.drop(t.rd(p).wrapping_add(*p))
        None => core.mem.drop(0u32)
    }
    match &mut o {
        Some(p) => {
            t.bump(p)
            *p = p.wrapping_add(1u32)
        }
        None => core.mem.drop(())
    }
    let pair = (1u32, 2u32)
    let (a, b) = &pair
    core.mem.drop(a.wrapping_add(*b))
    let pt = t.Pt {
        x: 1u32
        y: 2u32
    }
    let t.Pt {
        x: px
        y: py
    } = &pt
    core.mem.drop(px.wrapping_add(*py))
    let nested = Some((1u32, 2u32))
    if let Some((c, d)) = &nested {
        core.mem.drop(c.wrapping_add(*d))
    }
    core.check.eq(pt.x, 1u32)
    Ok(())
end t.main

fn t.rd(v: &u32) -> u32 uses [] panics never
    *v
end t.rd
```

**却下されるプログラム** (`docs/borrow_rules/K8_ng.ksnl`):

```ksnl fragment
type t.S
    v: alloc.string.String
end t.S

fn t.mk() -> alloc.string.String uses [] panics never
    alloc.string.String.new(std.alloc.SYSTEM)
end t.mk

fn t.main() -> () uses [] panics never
    let s = t.S { v: t.mk() }
    match &s {
        t.S { v: v } => core.mem.drop(*v)
    }
end t.main
```

`match &s { t.S { v: v } => .. }` では `v: &alloc.string.String` であり、`*v` は共有参照の背後からの移動になるため、`ksn check` で：

```ksnl fragment
E3002: cannot move out of `*v`: it is behind a reference and its type is not Copy; borrow it with `&*v` or call `.clone()`
```

**修正.** `match &s` で覗くだけにする（`*v` の代わりに `&*v` で再借用するか、値が必要なら所有を移す形で `match s` を使う）。

---

## K9 クロージャと `capture`

**文言.** クロージャ式は `fn(x: T) -> U capture [&a, move b] { .. }` で、本体が使う外側の束縛は必ず `capture` に列挙する（漏れは E1101）。`capture [&a]` の `a` は本体で `&T` 型なので値を使うには `*a` と書き、`capture [move b]` の `b` は `T`（値としてそのまま使える）。クロージャの `uses`/`panics` は本体から推論される。

**なぜ.** キャプチャを明示することで、どの外側変数を閉じ込めるかを明確にし、借用の衝突や線形値の誤った移動を防ぐ。`&a` を暗黙に `T` として扱うと段数ミス（E2023）になる。

**正しいプログラム** (`docs/borrow_rules/K9_ok.ksnl`):

```
fn t.main() -> Result<(), std.io.Error> uses [] panics never
    let k = 7u64
    let mut m = 1u64
    let f = fn(x: u64) -> u64 capture [&k] {
        x.wrapping_mul(*k)
    }
    let g = fn() -> () capture [&mut m] {
        *m = m.wrapping_add(1u64)
    }
    g()
    core.mem.drop(f(2u64))
    core.mem.drop(m)
    Ok(())
end t.main
```

`capture [&k]` で `k` は `&u64` になるので `*k` で値を取り出す。

**却下されるプログラム** (`docs/borrow_rules/K9_ng.ksnl`):

```ksnl fragment
type t.S
    n: u32
end t.S

fn t.bad() -> u32 uses [] panics never
    let s = t.S { n: 1u32 }
    let c = fn() -> u32 capture [move s] {
        s.n
    }
    let k = s.n
    c().wrapping_add(k)
end t.bad

fn t.main() -> Result<(), std.io.Error> uses [] panics never
    Ok(())
end t.main
```

`capture [move s]` で `s` の所有がクロージャへ移り、その後の `s.n` は移動済みの `s` に触れるため、`ksn check` で：

```ksnl fragment
E3002: use of moved value `s.n` (moved `s` at line 7)
```

**修正.** `move` で取り込んだ値はクロージャ外で使わない。共有したいなら `Copy` の値を `capture [&k]` で参照として取り込む。

---

## K10 `spawn` / `Send`

**文言.** `std.thread.spawn` の閉包 `F`（と `T`、`channel` の `T`）は `core.marker.Send` に加えて `'static` を満たす必要があり、局所の借用を含む値を `capture [&x]` で取り込むと E2037 となる。線形値は `move` で取り込むとその消費義務がクロージャへ移り、クロージャ本体は全経路で消費しなければならず、`JoinHandle<T, A>` が `T` の義務を運び `join` で呼び出し側へ返す。

**なぜ.** スレッド間で移動した値は生存期間が不定になり、局所の借用をまたぐとスタックフレーム消滅後に dangling 参照となる。`'static` ＋ `Send` でこれを静的に防ぐ。

**正しいプログラム** (`docs/borrow_rules/K10_ok.ksnl`):

```
fn t.main() -> Result<(), std.io.Error> uses [alloc, thread] panics may
    let spawned = std.thread.spawn(
        std.alloc.SYSTEM,
        fn() -> u32 capture [] {
            t.worker()
        },
    )
    let handle = match spawned {
        Ok(h) => h
        Err(_e) => core.panic.panic("spawn failed")
    }
    let answer = handle.join()
    core.check.eq(answer, 42u32)
    Ok(())
end t.main

fn t.worker() -> u32 uses [] panics never
    40u32.wrapping_add(2u32)
end t.worker
```

`capture []` で外側の局所変数を取り込まず、`'static` な関数 `t.worker` だけを呼ぶ。

**却下されるプログラム** (`docs/borrow_rules/K10_ng.ksnl`):

```ksnl fragment
fn t.go[F: core.marker.Send + core.ops.FnOnce<(), usize, E, P> + 'static, E: uses, P: panics](f: F) -> Result<std.thread.JoinHandle<usize, std.alloc.System>, std.thread.SpawnError<F>> uses [alloc, thread, E] panics P
    std.thread.spawn(std.alloc.SYSTEM, f)
end t.go

fn t.main() -> () uses [alloc, thread] panics never
    core.mem.drop(t.start())
end t.main

fn t.mk() -> std.string.String uses [] panics never
    std.string.String.new()
end t.mk

fn t.start() -> usize uses [alloc, thread] panics never
    let s = t.mk()
    let started = t.go(
        fn() -> usize capture [&s] {
            s.len()
        },
    )
    match started {
        Ok(h) => h.join()
        Err(failed) => {
            let std.thread.SpawnError {
                code: _code
                closure: task
            } = failed
            task()
        }
    }
end t.start
```

`capture [&s]` でローカル `s` の借用を取り込んだが、`s` は `'static` ではないため、`ksn check` で：

```ksnl fragment
E2037: the argument `f` of `t.go` (a `'static` bound) must be 'static, but the value may hold a borrow of a local (5.5)
```

**修正.** `spawn` に渡す閉包は局所の借用を取り込まない（`capture []`、または `&'static T` / 文字列リテラル / `const` など `'static` と分かる値だけを使う）。

---

## K11 `suspend` をまたぐ借用（E3050）

**文言.** 中断点（`uses [suspend]` の関数の呼び出し、`yield_now` や `Task.join_task` など）をまたいで局所の `&` / `&mut` 借用を使用すると E3050 となる。中断点の呼び出しに引数として渡す借用、および中断点の後で使わない借用は対象外。

**なぜ.** 中断点でタスクの状態が保存・復元されるため、中断をまたぐ借用は保存先の枠が無効になった後に dangling になるおそれがある。

**正しいプログラム** (`docs/borrow_rules/K11_ok.ksnl`):

```
fn t.main() -> () uses [suspend] panics never
    let mut x = 1u32
    let r = &mut x
    *r = 2u32
    std.task.yield_now()
    core.mem.drop(x)
end t.main
```

`r` の最後の使用が `*r = 2u32` であり、`yield_now()` の前で借用が終わる。

**却下されるプログラム** (`docs/borrow_rules/K11_ng.ksnl`):

```ksnl fragment
fn t.main() -> () uses [suspend] panics never
    let mut x = 1u32
    let r = &mut x
    std.task.yield_now()
    *r = 2u32
end t.main
```

`yield_now()`（中断点）の後で `r`（`x` の借用）を使用するため、`ksn check` で：

```ksnl fragment
E3050: cannot hold a borrow of `x` across a suspension point: `r` is used after the call that may suspend
```

**修正.** 借用の中断点をまたがないようにし、中断点の前で使い切るか、引数として渡す（呼び出しの間だけ生きる）。

---

## K12 `uses [...]` 効果と `panics never` / `panics may`

**文言.** すべての関数はヘッダで `uses [...]` と `panics never | may` を必ず宣言し、本体の効果と完全に一致させなければならない（広すぎても狭すぎても誤り）。未宣言の副作用は E4001、呼び出し元の `panics never` への `panics may` 関数の呼び出しは E4002、宣言の省略は E4020、不一致は E4025 である。

**なぜ.** 宣言を広くしておいて後から副作用を足すと、依存のロックが記録する公開の署名・効果をすり抜けられるため。

**正しいプログラム** (`docs/borrow_rules/K12_ok.ksnl`):

```
fn t.log() -> Result<(), std.io.Error> uses [io.console] panics never
    std.io.print_line("x")
end t.log

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    t.log()?
    core.mem.drop(t.sum(1u8, 2u8))
    Ok(())
end t.main

fn t.sum(a: u8, b: u8) -> u8 uses [] panics may
    a + b
end t.sum
```

**却下されるプログラム** (`docs/borrow_rules/K12_ng.ksnl`):

```ksnl fragment
fn t.main() -> () uses [] panics never
    let f = fn() -> () capture [] {
        t.say()
    }
    f()
end t.main

fn t.say() -> () uses [io.console] panics never
    core.mem.drop(std.io.print_line("hi"))
end t.say
```

`t.say()` は `io.console` 効果を持つが、`t.main` は `uses []` と宣言しているため、`ksn check` で：

```ksnl fragment
E4001: f uses effect `io.console` not declared by the caller
```

**修正.** 呼び出し元の `uses` に `io.console` を加える（`uses [io.console]`）。`panics` も同様に `never`/`may` を本体に合わせる（`ksn fix` が宣言を書き直す）。

---

## K13 `unsafe` の門限（E5001）

**文言.** `unsafe`・`extern`・生ポインタ・`core.ptr`・アロケータ直接操作などは、`kasane.toml` の `[unsafe] modules` に列挙したモジュールだけで使える。列挙は完全一致または接頭辞一致。許可のないモジュールでの使用は E5001。

**なぜ.** 安全でない操作を言語全体に撒き散らさず、監査・レビューの対象を明確なモジュールに限定するため。

**正しいプログラム** (`docs/borrow_rules/K13_ok.ksnl`):

```
fn t.main() -> () uses [] panics never
    core.mem.drop(1u32)
end t.main
```

安全な操作のみを使う。

**却下されるプログラム** (`docs/borrow_rules/K13_ng.ksnl`):

```ksnl fragment
extern t.getpid() -> i32 link "getpid"

fn t.main() -> Result<(), std.io.Error> uses [] panics never
    Ok(())
end t.main
```

`extern` 宣言が `[unsafe] modules` に列挙されていないモジュールにあるため、`ksn check` で：

```ksnl fragment
E5001: extern function `t.getpid` (FFI) is only allowed in modules listed in [unsafe] modules
```

**修正.** `kasane.toml` の `[unsafe] modules` に該当モジュールを追加し、`unsafe` ブロックの中でのみ `extern` を呼ぶ。

---

## K14 `unsafe` の値化禁止（E5003）

**文言.** `extern` の関数と `core.ptr` の関数は値として扱えず、変数・フィールド・クロージャ・戻り値・`if let` の束縛に入れられない（`unsafe` の中でも同様）。呼び出しは `unsafe "名前" "理由" { .. }` の中だけ。違反は E5003。

**なぜ.** 生ポインタ・FFI 関数を値として回すと、呼び出しの安全契約（`unsafe` ブロック内での直接呼び出し）を回避できる。

**正しいプログラム** (`docs/borrow_rules/K14_ok.ksnl`):

```ksnl fragment
extern t.getpid() -> i32 link "getpid"

fn t.main() -> i32 uses [] panics never
    unsafe "getpid" "example" {
        t.getpid()
    }
end t.main
```

`kasane.toml` に `[unsafe] modules = ["t"]` を記載し、`unsafe` ブロックの中で直接呼び出す。

**却下されるプログラム** (`docs/borrow_rules/K14_ng.ksnl`):

```ksnl fragment
extern t.getpid() -> i32 link "getpid"

fn t.main() -> () uses [] panics never
    let f = t.getpid
    core.mem.drop(f)
end t.main
```

`t.getpid` を値として束縛しようとしているため、`ksn check` で：

```ksnl fragment
E5003: the external or raw-pointer function `t.getpid` cannot be used as a value (it can only be called directly, e.g. `t.getpid(..)`)
```

**修正.** `unsafe ".." ".." { t.getpid(..) }` の中で直接呼び出し、変数やフィールドに入れない。

---

## K15 `with` スコープ付き後始末（E3040 / E3041 / E3042 / E3043 / E2045）

**文言.** `with [mut] 名前 = 初期化式 close 後始末式 { 本体 }` は、本体を抜けるすべての経路で出口の値を決めたあとに後始末式を 1 回実行する。本体では `名前` を move・再代入できない（E3040）。後始末式は `名前` を値としてちょうど 1 回消費し、型は `()` でなければならない（E3041 / E2045）。後始末式に `return` / `break` / `continue` / `?` は書けない（E3042）。本体の値が `名前` から借りた参照であってはならない（E3043）。`panic` 時は後始末式を実行しない。

**なぜ.** ファイル・ソケットなど線形資源を、どの出口（`?`/`return`/`break`/`continue` 含む）でも必ず 1 回後始末し、二重破棄や漏れを防ぐため。

**正しいプログラム** (`docs/borrow_rules/K15_ok.ksnl`):

```ksnl fragment
enum t.Error derive [Debug, Clone, Copy, PartialEq, Eq, From] pub
    Io(std.io.Error)
    Alloc(alloc.alloc.AllocError)
    Utf8(core.str.Utf8Error)
end t.Error

fn t.main() -> Result<(), t.Error> uses [alloc, io.console, io.fs] panics may
    with mut f = std.fs.create("/tmp/opencode/ksn_sample.dat")? close core.mem.drop(f.close()) {
        core.mem.drop(f.write("hello".as_bytes())?)
    }
    Ok(())
end t.main
```

**却下されるプログラム** (`docs/borrow_rules/K15_ng.ksnl`):

```ksnl fragment
type t.Res linear
    id: u32
    tag: u32
end t.Res

fn t.Res.shut(self: t.Res) -> () uses [] panics never
    let t.Res { id: id, tag: tag } = self
    core.mem.drop(id)
    core.mem.drop(tag)
end t.Res.shut

fn t.bad() -> () uses [] panics never
    with r = t.Res.new(1u32) close r.shut() {
        r.shut()
    }
end t.bad

fn t.main() -> Result<(), std.io.Error> uses [] panics never
    Ok(())
end t.main
```

`with` 本体で `r.shut()` とし `r` を move しているが、`close` 式 `r.shut()` が `r` を消費するため、`ksn check` で：

```ksnl fragment
E3040: `r` cannot be moved or reassigned in the `with` body; the `close` expression consumes it (4.4)
```

**修正.** `with` 本体では `r` を借用して読み取りだけ行い、move は `close` 式に任せる。`close` 式は `()` を返し `return`/`?` などを含めない。

---

## K16 `drop` の規則—`core.mem.drop` のみ（E3004 / E2032）

**文言.** 破棄は `core.mem.drop(x)` のみで行う。`core.ops.Drop.drop` はメソッド呼び出し・FQN 呼び出しとも直接呼べず、E3004 となる。`linear` 型は `Copy` / `Drop` を `derive` できず、E2032 となる。

**なぜ.** 二重 `drop` を防ぎ、線形資源の破棄を `core.mem.drop` の一箇所で追跡可能にするため。

**正しいプログラム** (`docs/borrow_rules/K16_ok.ksnl`):

```
fn t.main() -> () uses [] panics never
    let s = std.string.String.new()
    core.mem.drop(s)
end t.main
```

**却下されるプログラム** (`docs/borrow_rules/K16_ng.ksnl`):

```ksnl fragment
type t.L derive [Copy] linear pub
    v: u32
end t.L

fn t.main() -> () uses [] panics never
end t.main
```

`linear` 型に `derive [Copy]` を付けているため、`ksn check` で：

```ksnl fragment
E2032: linear type t.L must not derive Copy
```

**修正.** `linear` 型から `derive [Copy]` / `derive [Drop]` を外す。破棄は `core.mem.drop` で行う。

---

## 例の検証方法

`docs/borrow_rules/check.py` は Python 3 標準ライブラリのみを使用し、`python3 -I check.py` で動く。各 `<rule>_{ok,ng}.ksnl` に対して一時パッケージ（`kasane.toml` / `kasane.lock` / `src/t.ksnl`）を組み、`ksn check --json` を実行して、`.expect` の 1 行目（`ok` または診断コード）と一致するかをアサートする。K14（`unsafe` モジュールが必要）の場合は `.expect` の 2 行目に `unsafe` を置き、そのパッケージに `[unsafe] modules = ["t"]` を追加する。

```bash
python3 -I check.py
```

全件 PASS がこの文書の前提である。
