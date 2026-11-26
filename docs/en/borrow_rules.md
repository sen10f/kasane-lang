[日本語](../borrow_rules.md) | English

# Kasane Rules of Ownership, Move, Borrowing, Linearity and Effects

This document collects the language rules of Kasane (v0.10) on "ownership, move, borrowing, linear types and effects (`uses`/`panics`)", "references are second-class values", and "`with`, `unsafe` and `suspend`" as numbered rules K1–K16.

Each rule has the following structure.

1. **Statement** — states the rule in one sentence.
2. **Why** — why the rule is needed.
3. **Accepted program** — an example that passes `ksn check` (file `docs/borrow_rules/K#_ok.ksnl`).
4. **Rejected program** — an example that `ksn check` rejects, with the exact diagnostic code and message it prints (file `docs/borrow_rules/K#_ng.ksnl`).
5. **Fix** — how to repair the rejected example.

Every example was verified with `ksn check` using the `ksn` of that time (`stage0/target/release/ksn`). Each example and its expected diagnostic is verified automatically by `docs/borrow_rules/check.py`, and `python3 -I check.py` passes in full. Comments cannot be written in `.ksnl`, so notes are placed in the text.

Note: `alloc.arena.Arena<T, A>` / `alloc.arena.Handle<T>` (an arena plus generation-tagged handles, D1 of `docs/design/設計_共有の取っ手.md`) are implemented in the stdlib (`spec/stdlib/alloc/arena.ksnl` + `.sig`). In the verified samples of this document, the form "hold an index (a position) and pass the source data as a function argument" is used as the alternative to putting references in fields. An arena plus handles can be used with the same style (`Handle<T>` is `Copy` and can go in fields and in a `Vec`).

## K1 Distinguishing `Copy`, move and linear

**Statement.** A type declared `linear` (or a type that contains a linear value) is moved, and ownership is transferred exactly once. Only a type with `derive` of `Copy` can be used several times. A non-linear, non-`Copy` value also cannot be used after it is moved.

**Why.** To manage resources without a GC, it must be statically determined uniquely who destroys a value and when. A `Copy` value is duplicated and can be used any number of times, while a non-`Copy` value has its ownership transferred.

**Accepted program** (`docs/borrow_rules/K1_ok.ksnl`):

```
fn t.main() -> () uses [] panics never
    core.mem.drop(t.pair(7u32))
end t.main

fn t.pair[T: core.ops.Copy](x: T) -> (T, T) uses [] panics never
    (x, x)
end t.pair
```

`7u32` is `Copy`, so `x` can be used twice.

**Rejected program** (`docs/borrow_rules/K1_ng.ksnl`):

```ksnl fragment
fn t.main() -> () uses [] panics never
    let s = alloc.string.String.new(std.alloc.SYSTEM)
    core.mem.drop(s)
    core.mem.drop(s)
end t.main
```

`String` is non-`Copy` (non-linear), so the second `core.mem.drop(s)`, after ownership was transferred by `core.mem.drop(s)`, gives the following in `ksn check`:

```ksnl fragment
E3002: use of moved value `s` (moved at line 3)
```

**Fix.** Write `core.mem.drop(s)` only once. Make a value you want to use several times a `Copy` type, or, for a type that owns heap memory, duplicate it with `alloc.alloc.TryClone::try_clone(&s)?`.

---

## K2 A linear value is consumed once on every path (E3003)

**Statement.** A value of a `linear` type must be consumed exactly once (by `core.mem.drop`, by passing it to a function that takes `self` by value, by destructuring, and so on) on every path: normal end, `return`, `?` and each branch. If it leaves the scope without being consumed, E3003.

**Why.** Linearity represents resources whose cleanup must not be missed, such as files, sockets and transactions, so it guarantees that cleanup happens exactly once on every path.

**Accepted program** (`docs/borrow_rules/K2_ok.ksnl`):

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

It borrows with `peek` and then consumes with `eat`, so it is handled exactly once on every path.

**Rejected program** (`docs/borrow_rules/K2_ng.ksnl`):

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

`l` is only borrowed and leaves the scope without being consumed, so in `ksn check`:

```ksnl fragment
E3003: linear value `l` is never consumed when its scope ends
```

**Fix.** Consume `l` once on every path, as in `t.eat(l)`.

---

## K3 Use after move and double drop are forbidden (E3002)

**Statement.** A value cannot be used after it has been moved (or consumed by `core.mem.drop`), and this gives E3002 "use of moved value".

**Why.** Accessing a moved value would refer to freed memory and break memory safety.

**Accepted program** (`docs/borrow_rules/K3_ok.ksnl`):

```
fn t.main() -> () uses [] panics never
    let s = std.string.String.new()
    let n = s.len()
    core.mem.drop(n)
    core.mem.drop(s)
end t.main
```

After using it with `s.len()`, `s` is `drop`ped exactly once.

**Rejected program** (`docs/borrow_rules/K3_ng.ksnl`):

```ksnl fragment
fn t.main() -> () uses [] panics never
    let s = alloc.string.String.new(std.alloc.SYSTEM)
    core.mem.drop(s)
    let n = s.len()
    core.mem.drop(n)
end t.main
```

`s.len()` after `core.mem.drop(s)` touches the moved `s`, so in `ksn check`:

```ksnl fragment
E3002: use of moved value `s` (moved at line 3)
```

**Fix.** `drop` a value once when you are done with it, and do not touch it afterwards. Put uses before the `drop`.

---

## K4 References are second-class values: they cannot be placed in type fields (E3060)

**Statement.** References (`&T` / `&mut T`, and types that carry references such as `Option<&T>`) can be placed only in parameters, return values and local bindings. Placing them in a `type`/`enum` field, in a variant payload, in a type declaration with lifetime parameters, or in the type arguments of `Vec`/`Box`/`HashMap` and the like gives E3060. Only `&'static T` (a reference with no region) is an exception and can be placed in fields and type arguments.

**Why.** A reference has a valid range (a region), and storing it in a data structure for a long time creates a dangling reference. By making references second-class values, this class of bugs is prevented by type checking.

**Accepted program** (`docs/borrow_rules/K4_ok.ksnl`):

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

It holds no reference in a field, only `pos` (an index), and passes the source data `src` as an argument each time. `alloc.arena.Handle<T>` follows the same policy (index-based).

**Rejected program** (`docs/borrow_rules/K4_ng.ksnl`):

```ksnl fragment
type t.H['a]
    r: &'a alloc.string.String
end t.H

fn t.main() -> () uses [] panics never
end t.main
```

The `type` has a lifetime parameter and the field is `&'a String`, so in `ksn check` (both are E3060):

```ksnl fragment
E3060: t.H: a type cannot declare a lifetime parameter (references are second-class values); hold a position index and pass the source data as an argument instead
E3060: t.H: a field or variant payload cannot hold a reference (references are second-class values); hold an index or an owned value, and pass the source data as an argument
```

**Fix.** Remove the reference from the field, hold a position such as `pos: usize`, and pass the source data as an argument (K4_ok). Use `Vec<usize>` (an array of indices) instead of `Vec<&T>`.

---

## K5 Conflict between shared and `&mut` (E3010)

**Statement.** If a `&` (shared borrow) and a `&mut` (mutable borrow) of the same place, or a `&mut` and another `&mut`, are alive at the same time, E3010.

**Why.** A mutable borrow means exclusive access to the place, and coexisting with a shared borrow, or two mutable borrows, leads to data races and overlooked destructive changes.

**Accepted program** (`docs/borrow_rules/K5_ok.ksnl`):

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

**Rejected program** (`docs/borrow_rules/K5_ng.ksnl`):

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

Two `&mut` are passed for the same `x`, so in `ksn check`:

```ksnl fragment
E3010: cannot use `x` while it is mutably borrowed by an earlier operand of the same expression
```

**Fix.** A `&mut` borrow lasts until its last use (NLL). Pass separate values, or borrow again after the borrow has ended.

---

## K6 A borrow lasts until its last use (NLL): a move while it is alive is E3010

**Statement.** A borrow ends at the last use of that reference, and a move after that is allowed. If you move the owner while the borrow is still alive, E3010.

**Why.** Kasane's borrow checking is close to NLL (non-lexical lifetimes) and does not extend borrows longer than necessary. This lets safe code such as "a move right after a borrow" pass, while rejecting a move during a borrow.

**Accepted program** (`docs/borrow_rules/K6_ok.ksnl`):

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

The use of `r = &x.name` ends at `peek`, and only then is `x.other` (a different field) moved, so it passes.

**Rejected program** (`docs/borrow_rules/K6_ng.ksnl`):

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

After `r = &s`, `s` is moved by `t.consume(s)`, and `r` is used afterwards, so in `ksn check`:

```ksnl fragment
E3010: cannot move `s` while it is borrowed by `r`
```

**Fix.** End the use of the borrow `r` before `t.consume(s)`, or use `r` up before `t.consume`.

---

## K7 Returning references: the two elision rules, E3030, temporaries are dropped at the end of the statement (2.10aa)

**Statement.** The lifetime of a returned reference can be elided only when (a) there is exactly one reference parameter, or (b) the first parameter is `self: &...` / `&mut ...` and the return value derives from `self`; otherwise it must be stated explicitly in the header (stating it when it could be elided gives E3021). Returning a reference to a temporary value is E3030. A temporary is dropped at the end of the statement, and the lifetime is not extended by `let` (docs/plan/実装計画.md 2.10aa).

**Why.** The region of an elided reference is "fresh for every call and unrelated to the others", so returning one carelessly would return a reference that is invalid after the call ends.

**Accepted program** (`docs/borrow_rules/K7_ok.ksnl`):

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

It returns `&u32` from `self: &t.S`, so by elision rule (b) the elided form is the canonical form.

**Rejected program** (`docs/borrow_rules/K7_ng.ksnl`):

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

`(*x).wrapping_add(1u32)` is a temporary produced by evaluating an expression, and returning a reference to it would dangle after the temporary is dropped at the end of the statement, so in `ksn check`:

```ksnl fragment
E3030: returns a reference to a temporary value, which does not live long enough
```

**Fix.** Give the temporary a name before borrowing it (`let tmp = ...; &tmp`), or return the value itself. On the calling side, write something like `let tmp = t.make(); let r = &tmp.v`.

---

## K8 Destructuring through a reference, and `match` (binding mode)

**Statement.** When you destructure through a reference, as in `match &r { .. }` / `match &mut r { .. }` / `let (a, b) = &pair`, the bound names get the type of the field under the `&` (`&T` / `&mut T`). To take out the value you need `*`. `match &r` only borrows and destructures; it does not consume. A linear value destructured by value (`let` / `match` / `if let`) is always considered consumed, even if you take only `_` or only `Copy` fields, and a later use is E3002.

**Why.** Destructuring through a reference does not transfer ownership and keeps the original borrow. This is so that a linear value can be only "peeked at" while ownership can still be consumed later as usual.

**Accepted program** (`docs/borrow_rules/K8_ok.ksnl`):

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

**Rejected program** (`docs/borrow_rules/K8_ng.ksnl`):

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

In `match &s { t.S { v: v } => .. }`, `v` is `&alloc.string.String`, and `*v` would be a move out from behind a shared reference, so in `ksn check`:

```ksnl fragment
E3002: cannot move out of `*v`: it is behind a reference and its type is not Copy; borrow it with `&*v` or call `.clone()`
```

**Fix.** Only peek with `match &s` (re-borrow with `&*v` instead of `*v`, or, if you need the value, use `match s` in a form that transfers ownership).

---

## K9 Closures and `capture`

**Statement.** A closure expression is `fn(x: T) -> U capture [&a, move b] { .. }`, and the outer bindings used by the body must always be listed in `capture` (an omission is E1101). `a` in `capture [&a]` has type `&T` in the body, so to use the value write `*a`, and `b` in `capture [move b]` is `T` (it can be used as a value as it is). The `uses`/`panics` of a closure are inferred from the body.

**Why.** Stating captures explicitly makes clear which outer variables are enclosed, and prevents borrow conflicts and wrong moves of linear values. Treating `&a` implicitly as `T` would be a mistake in the level of references (E2023).

**Accepted program** (`docs/borrow_rules/K9_ok.ksnl`):

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

With `capture [&k]`, `k` becomes `&u64`, so the value is taken out with `*k`.

**Rejected program** (`docs/borrow_rules/K9_ng.ksnl`):

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

With `capture [move s]`, ownership of `s` moves into the closure, and the later `s.n` touches the moved `s`, so in `ksn check`:

```ksnl fragment
E3002: use of moved value `s.n` (moved `s` at line 7)
```

**Fix.** Do not use a value taken in by `move` outside the closure. If you want to share it, take a `Copy` value in as a reference with `capture [&k]`.

---

## K10 `spawn` / `Send`

**Statement.** The closure `F` of `std.thread.spawn` (and `T`, and the `T` of a `channel`) must satisfy `'static` in addition to `core.marker.Send`, and capturing a value that contains a local borrow with `capture [&x]` gives E2037. If a linear value is captured by `move`, the obligation to consume it moves into the closure, the closure body must consume it on every path, and `JoinHandle<T, A>` carries the obligation of `T` and returns it to the caller on `join`.

**Why.** A value moved between threads has an indeterminate lifetime, and crossing a local borrow would leave a dangling reference after the stack frame disappears. `'static` plus `Send` prevents this statically.

**Accepted program** (`docs/borrow_rules/K10_ok.ksnl`):

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

With `capture []`, no outer local variable is captured, and only the `'static` function `t.worker` is called.

**Rejected program** (`docs/borrow_rules/K10_ng.ksnl`):

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

`capture [&s]` takes in a borrow of the local `s`, but `s` is not `'static`, so in `ksn check`:

```ksnl fragment
E2037: the argument `f` of `t.go` (a `'static` bound) must be 'static, but the value may hold a borrow of a local (5.5)
```

**Fix.** The closure passed to `spawn` must not capture local borrows (use `capture []`, or use only values known to be `'static`, such as `&'static T`, string literals and `const`s).

---

## K11 Borrows across `suspend` (E3050)

**Statement.** Using a local `&` / `&mut` borrow across a suspension point (a call to a function with `uses [suspend]`, such as `yield_now` or `Task.join_task`) is E3050. A borrow passed as an argument to the call at the suspension point, and a borrow not used after the suspension point, are excluded.

**Why.** The state of a task is saved and restored at a suspension point, so a borrow that crosses a suspension may dangle after the frame it was saved in becomes invalid.

**Accepted program** (`docs/borrow_rules/K11_ok.ksnl`):

```
fn t.main() -> () uses [suspend] panics never
    let mut x = 1u32
    let r = &mut x
    *r = 2u32
    std.task.yield_now()
    core.mem.drop(x)
end t.main
```

The last use of `r` is `*r = 2u32`, and the borrow ends before `yield_now()`.

**Rejected program** (`docs/borrow_rules/K11_ng.ksnl`):

```ksnl fragment
fn t.main() -> () uses [suspend] panics never
    let mut x = 1u32
    let r = &mut x
    std.task.yield_now()
    *r = 2u32
end t.main
```

`r` (a borrow of `x`) is used after `yield_now()` (a suspension point), so in `ksn check`:

```ksnl fragment
E3050: cannot hold a borrow of `x` across a suspension point: `r` is used after the call that may suspend
```

**Fix.** Do not let the borrow cross the suspension point: use it up before the suspension point, or pass it as an argument (it lives only during the call).

---

## K12 The effects `uses [...]` and `panics never` / `panics may`

**Statement.** Every function must declare `uses [...]` and `panics never | may` in its header, and they must match the effects of the body exactly (too wide and too narrow are both errors). An undeclared side effect is E4001, a call to a `panics may` function from a `panics never` caller is E4002, omitting the declaration is E4020, and a mismatch is E4025.

**Why.** If you declare widely and add side effects later, you could slip past the public signatures and effects that the dependency lock records.

**Accepted program** (`docs/borrow_rules/K12_ok.ksnl`):

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

**Rejected program** (`docs/borrow_rules/K12_ng.ksnl`):

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

`t.say()` has the effect `io.console`, but `t.main` declares `uses []`, so in `ksn check`:

```ksnl fragment
E4001: f uses effect `io.console` not declared by the caller
```

**Fix.** Add `io.console` to the caller's `uses` (`uses [io.console]`). For `panics` as well, match `never`/`may` to the body (`ksn fix` rewrites the declarations).

---

## K13 The `unsafe` gate (E5001)

**Statement.** `unsafe`, `extern`, raw pointers, `core.ptr`, direct manipulation of allocators and the like can be used only in the modules listed in `[unsafe] modules` of `kasane.toml`. An entry matches exactly or by prefix. Use in a module without permission is E5001.

**Why.** To avoid scattering unsafe operations over the whole language, and to limit what must be audited and reviewed to clearly identified modules.

**Accepted program** (`docs/borrow_rules/K13_ok.ksnl`):

```
fn t.main() -> () uses [] panics never
    core.mem.drop(1u32)
end t.main
```

It uses only safe operations.

**Rejected program** (`docs/borrow_rules/K13_ng.ksnl`):

```ksnl fragment
extern t.getpid() -> i32 link "getpid"

fn t.main() -> Result<(), std.io.Error> uses [] panics never
    Ok(())
end t.main
```

The `extern` declaration is in a module that is not listed in `[unsafe] modules`, so in `ksn check`:

```ksnl fragment
E5001: extern function `t.getpid` (FFI) is only allowed in modules listed in [unsafe] modules
```

**Fix.** Add the module to `[unsafe] modules` in `kasane.toml`, and call an `extern` only inside an `unsafe` block.

---

## K14 `unsafe` functions cannot be used as values (E5003)

**Statement.** Functions of `extern` and `core.ptr` cannot be treated as values, and cannot be put in a variable, a field, a closure, a return value, or an `if let` binding (the same inside `unsafe`). Calls are allowed only inside `unsafe "name" "reason" { .. }`. A violation is E5003.

**Why.** If raw pointers and FFI functions were passed around as values, the safety contract of the call (a direct call inside an `unsafe` block) could be bypassed.

**Accepted program** (`docs/borrow_rules/K14_ok.ksnl`):

```ksnl fragment
extern t.getpid() -> i32 link "getpid"

fn t.main() -> i32 uses [] panics never
    unsafe "getpid" "example" {
        t.getpid()
    }
end t.main
```

Write `[unsafe] modules = ["t"]` in `kasane.toml`, and call it directly inside an `unsafe` block.

**Rejected program** (`docs/borrow_rules/K14_ng.ksnl`):

```ksnl fragment
extern t.getpid() -> i32 link "getpid"

fn t.main() -> () uses [] panics never
    let f = t.getpid
    core.mem.drop(f)
end t.main
```

It tries to bind `t.getpid` as a value, so in `ksn check`:

```ksnl fragment
E5003: the external or raw-pointer function `t.getpid` cannot be used as a value (it can only be called directly, e.g. `t.getpid(..)`)
```

**Fix.** Call it directly inside `unsafe ".." ".." { t.getpid(..) }`, and do not put it in a variable or a field.

---

## K15 `with` scoped cleanup (E3040 / E3041 / E3042 / E3043 / E2045)

**Statement.** `with [mut] name = initializer close cleanup-expression { body }` runs the cleanup expression once, after the exit value has been decided, on every path out of the body. In the body, `name` cannot be moved or reassigned (E3040). The cleanup expression consumes `name` by value exactly once, and its type must be `()` (E3041 / E2045). `return` / `break` / `continue` / `?` cannot be written in the cleanup expression (E3042). The value of the body must not be a reference borrowed from `name` (E3043). On a `panic`, the cleanup expression is not run.

**Why.** To clean up a linear resource such as a file or socket exactly once at every exit (including `?`/`return`/`break`/`continue`), preventing double destruction and leaks.

**Accepted program** (`docs/borrow_rules/K15_ok.ksnl`):

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

**Rejected program** (`docs/borrow_rules/K15_ng.ksnl`):

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

The `with` body does `r.shut()` and moves `r`, but the `close` expression `r.shut()` consumes `r`, so in `ksn check`:

```ksnl fragment
E3040: `r` cannot be moved or reassigned in the `with` body; the `close` expression consumes it (4.4)
```

**Fix.** In the `with` body, only borrow `r` and read from it, and leave the move to the `close` expression. The `close` expression returns `()` and must not contain `return`/`?` and the like.

---

## K16 The rule for `drop`: only `core.mem.drop` (E3004 / E2032)

**Statement.** Destruction is done only with `core.mem.drop(x)`. `core.ops.Drop.drop` cannot be called directly, either as a method call or as an FQN call, and gives E3004. A `linear` type cannot `derive` `Copy` / `Drop`, which gives E2032.

**Why.** To prevent double `drop`s and make the destruction of linear resources traceable in the single place `core.mem.drop`.

**Accepted program** (`docs/borrow_rules/K16_ok.ksnl`):

```
fn t.main() -> () uses [] panics never
    let s = std.string.String.new()
    core.mem.drop(s)
end t.main
```

**Rejected program** (`docs/borrow_rules/K16_ng.ksnl`):

```ksnl fragment
type t.L derive [Copy] linear pub
    v: u32
end t.L

fn t.main() -> () uses [] panics never
end t.main
```

`derive [Copy]` is attached to a `linear` type, so in `ksn check`:

```ksnl fragment
E2032: linear type t.L must not derive Copy
```

**Fix.** Remove `derive [Copy]` / `derive [Drop]` from the `linear` type. Do destruction with `core.mem.drop`.

---

## How the examples are verified

`docs/borrow_rules/check.py` uses only the Python 3 standard library and runs with `python3 -I check.py`. For each `<rule>_{ok,ng}.ksnl`, it assembles a temporary package (`kasane.toml` / `kasane.lock` / `src/t.ksnl`), runs `ksn check --json`, and asserts that the result matches the first line of `.expect` (`ok` or a diagnostic code). For K14 (which needs an `unsafe` module), the second line of `.expect` holds `unsafe`, and `[unsafe] modules = ["t"]` is added to that package.

```bash
python3 -I check.py
```

All cases passing is the premise of this document.
