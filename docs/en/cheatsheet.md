[日本語](../../チートシート.md) | English

# Kasane Cheat Sheet

## What always trips up people coming from Rust

Read this before writing. "Wrong" is a form that is stopped by the diagnostic shown; "Right" is the form that passes in the same place (verified with `ksn check`).

- **Integer `+ - *`** (E4004): a `panics never` function cannot use operators that can overflow.
  - Wrong: `a + b`　Right: `a.checked_add(b).ok_or(app.Error.Overflow)?` (use `a.wrapping_add(b)` only when you intend to wrap around, and `saturating_add` to clamp. For `/` and `%`, use `checked_div` / `checked_rem` or `panics may`)
- **`,` after a `match` arm** (E0049): arms are separated by lines. Do not write a trailing `,`.
  - Wrong: `Some(v) => v,`　Right: `Some(v) => v`
- **`as` casts** (E0042): there are no casts. Use `from_*` to widen and `try_from_*` (returns `Result`) to narrow.
  - Wrong: `let y = x as u64`　Right: `let y = core.num.u64.from_u32(x)` / `let y = core.num.u8.try_from_u32(x)`
- **One-line blocks** (E0056): always break the line right after `{`.
  - Wrong: `if flag { return 1u32 }`　Right: put `return 1u32` on the line after `if flag {`, and `}` on its own line
- **How to call a method** (E2029): a method with a receiver is called only as `x.m(..)`. `T.m(&x)`, `u32.wrapping_add(a, b)` and `(*r).m()` are errors. Only associated functions without `self` are called as `T.m(..)`.
  - Wrong: `std.vec.Vec.len(&v)`, `u32.wrapping_add(a, 1u32)`, `(*r).len()`　Right: `v.len()`, `a.wrapping_add(1u32)`, `r.len()`
- **Integer literal suffixes** (E2028): always write an integer literal like `1u32`. Only the array length in `[T; N]` and the number in `stack N` have no suffix. Write numbers as `16u32`, not `0016`; five or more digits as `65_536u32`; four or fewer digits as `9999u32` (no separators); floats as `1.5f64` / `1.0e-5f64`.
  - Wrong: `core.check.eq(x, 1)`, `v[0]`, `let n: u32 = 5`　Right: `core.check.eq(x, 1u32)`, `v[0usize]`, `let n: u32 = 5u32`
- **Trailing `;`** (E0040): a statement ends at the end of the line.
  - Wrong: `let x = 1u32;`　Right: `let x = 1u32`
- **`mut` in a pattern** (E0002): a pattern cannot contain `mut`.
  - Wrong: `Some(mut v) => {`　Right: `Some(v) => {` followed on the next line by `let mut w = v`
- **Assignment in an arm** (E0058): the body of an arm is an expression. Wrap an assignment in `{` and write it on the next line.
  - Wrong: `Some(r) => result = r`　Right: `Some(r) => {` / next line `result = r` / `}` (the closing brace at the same position as the arm)
- **Path separator** (E0041): use `.`, not `::`.
  - Wrong: `core::mem::drop(x)`　Right: `core.mem.drop(x)`
- **Levels of reference** (E2023, E2020): arguments are neither implicitly referenced nor dereferenced.
  - Wrong: `h(&x)` (`&u8` for a `u8` parameter), `Some(&x)` (return type `Option<u32>`)　Right: `h(*c)` (`c: &u8`), `Some(x)`
  - Operands of operators are the same (E2024). Wrong: `c == b' '`, `c >= b'0'`, `total + x` (`c: &u8`, `x: &u32`)　Right: `*c == b' '`, `*c >= b'0'`, `total + *x`
- **Discarding a value** (E2041, E2042): there is no `let _ = e`. A `Result` cannot be discarded as a statement.
  - Wrong: `let _ = f(x)`, the statement `v.push(1u8)`　Right: `core.mem.drop(f(x))`, `v.push(1u8)?`
- **`_` in an `enum` match** (E2021): a `match` on an `enum` or `bool` lists every variant.
  - Wrong: `_ => 0u32`　Right: write them all, as in `app.Light.Green => 0u32`
- **Operations on different integer types** (E2024): `usize` and `u32` are not converted silently (the same goes for a mismatch in the level of references, E2023). `total + x` (`total: usize`, `x: u32`) is an error.
  - Wrong: `total + x`　Right: `total + usize.from_u32(x)` (to narrow, `u32.try_from_usize(x)`, which returns a `Result`)
- **Indexing `a[i]`** (E4003): in `panics never`, an index that can panic when out of range cannot be used. Use `.get(i)` and handle `Some/None` with `match`.
  - Wrong: `let c = buf[i]`　Right: `match buf.get(i) { Some(c) => c, None => return Err(...) }`
- **Redundant type annotations** (E2053): if the right-hand side determines the type, no annotation is needed (`ksn fix` can remove it).
  - Wrong: `let n: u32 = 1u32`　Right: `let n = 1u32`
- **Recursive `panics never` functions** (E4010): the header needs `stack <bytes>` (a declaration of the size of one frame).
  - Wrong: `fn app.fact(n: u64) -> Option<u64> uses [] panics never`　Right: `fn app.fact(n: u64) -> Option<u64> uses [] panics never stack 4096`

## Small examples that run as they are

### An integer loop

```
fn app.count_odd(n: u32) -> u32 uses [] panics never
    let mut odd = 0u32
    for i in core.iter.range(0u32, n) {
        if i & 1u32 == 1u32 {
            odd = odd.wrapping_add(1u32)
        }
    }
    odd
end app.count_odd

fn app.main() -> () uses [] panics never
    core.mem.drop(app.count_odd(5u32))
end app.main

test app.count_odd.test_basic
    core.check.eq(app.count_odd(5u32), 2u32)
end app.count_odd.test_basic
```

### enum and match

```
enum app.Light derive [Debug, Clone, Copy, PartialEq] pub
    Red
    Blink(u32)
    Green
end app.Light

fn app.main() -> () uses [] panics never
    core.mem.drop(app.wait_secs(app.Light.Blink(2u32)))
end app.main

fn app.wait_secs(l: app.Light) -> u32 uses [] panics never pub
    match l {
        app.Light.Red => 30u32
        app.Light.Blink(n) => n.wrapping_add(1u32)
        app.Light.Green => 0u32
    }
end app.wait_secs

test app.wait_secs.test_blink
    core.check.eq(app.wait_secs(app.Light.Blink(2u32)), 3u32)
end app.wait_secs.test_blink
```

This is not a summary of the specification, but a list of the forms you use when writing. Every example has been verified with `ksn check`.
The extension is `.ksnl`, and sources are `src/<module>.ksnl` (module `a.b` is `src/a/b.ksnl`).

## Shape of a definition (start with the FQN, close with `end FQN`)

```
type app.Acc derive [Debug, Clone] pub
    total: i64
    hits: u32
end app.Acc

enum app.Shape derive [Debug, Clone, PartialEq] pub
    Circle(i32)
    Rect(i32, i32)
    Unit
end app.Shape

fn app.add(a: i32, b: i32) -> i32 uses [] panics may pub
    a + b
end app.add

test app.add.test_basic
    core.check.eq(app.add(1i32, 2i32), 3i32)
end app.add.test_basic
```

- Write the whole header: parameter types, return type, `uses [...]`, and `panics never|may`. Add `pub` at the end when it is public.
- There is one canonical form: a file that differs by even one byte from the output of `ksn fmt` is an error in `ksn check` (E0020). Token spacing (`core . check.eq (a,b)` and the like), the order of definitions, blank lines, the order of `derive` / `uses` / `capture`, a trailing `return`, and `while true` are fixed by `ksn fmt`. Run `ksn fmt` after you write.
- `import` is one module per line (`import app.net`), gathered at the top of the file. Putting a `//` line, or a `///` line with no blank line after it, right before it gives E0002 (a module description is `///` followed by one blank line). Per-item imports such as `import app.net.f`, and end-of-line comments, cannot be written.
- How names are written (when you `import app.net`): in the canonical form, types and traits in a header are the FQN (`app.net.Header`), definitions of `net` in a body use the short form (`net.parse_header(..)`), and definitions of the same module are always the FQN. Whichever form you write, `ksn fmt` turns it into the canonical form (`ksn check` reports a non-canonical form as E0020) (`net.Header` in a header becomes `app.net.Header`, and `app.net.parse_header(` in a body becomes `net.parse_header(`; an `import` that only a header used is removed). When in doubt, write the FQN and run `ksn fmt`.
- `String` has `==` / `!=` / `<` / `cmp` / `Hash` / `Debug` / `Display` (between `String`s. It can also be passed to `core.check.eq`). What lowers to native (LLVM) is `==` / `!=` (between local variables), `<` / `<=` / `>` / `>=` / `cmp` / `partial_cmp` (`String` and `&str`; byte-wise lexicographic order), and appending with `Debug` / `Display`. `Hash` (including `Fnv64`) does not become native.
- Type suffixes: `1u8` `2i32` `3usize` `1.5f64`. Write them on integer and float literals whose type is not determined by context (a standalone `let x = 1.5` gives E2027).
- When the checker gives `E2044` (cannot determine the type), write a type on that binding (`let v: std.vec.Vec<u8> = ...`). A type it cannot determine does not pass.
- There is no automatic referencing or dereferencing of arguments: to pass a `&u8` where a `u8` is expected, write `*c`; to pass a value where `&T` is expected, write `&x`. A method on a type parameter is only what its bound declares.
- Type equality includes the direction and variance of references: passing `&T` where `&mut T` is expected (an argument, a `let` annotation, inside `Some(..)`, and so on) gives E2023 / E2020 (`&mut` → `&` is fine). Under `&mut` and in type arguments the type is invariant, so `Option<&T>` and `Option<&mut T>` do not match either.
- Type equality also compares lengths, element counts and kinds: an array length (passing `[u64; 2]` to `&[u64; 3]`), a tuple's element count, and differences of kind such as an integer versus an array or a closure versus a function are E2023 (assignment is E2024). A type parameter is rigid and matches only itself (in `fn t.cast[T, U]`, assigning `z = x` to `let mut z = y` is E2024. Do not leave borrows to the type parameter; declare the parameter as `&T`).
- A mismatch of numeric types is never converted silently (E2023 for both functions and `extern`). Widen or narrow explicitly with `i64.from_i32(x)` or `i32.try_from_u64(x)`.

## Effects

| Form | Meaning |
| --- | --- |
| `uses []` | no side effects |
| `uses [io.console]` | console output |
| `uses [alloc]` | heap allocation (`Vec.push` and so on) |
| `panics never` | does not panic (indexing `a[i]`, integer `+ - * / %`, and calls to `panics may` functions are not allowed) |
| `panics may` | may panic |

The effects of a callee must be included in the caller's declaration (E4001 / E4002).

`uses` and `panics` are required on every function (omitting them is E4020), and must match the effects of the body exactly (E4025; `ksn fix` writes them). A function in `impls` must match the effects promised on the `impl` line.
For unimplemented places, use `core.panic.panic("todo")` (`core.panic.todo` was removed on 2026-11-21. E2057, fixed by `ksn fix`).

## Integers

In a `panics never` function, `+` `-` `*` `/` `%` and unary `-` panic on overflow or division by zero (E4004), so basically do not use them. Combine `checked_*` such as `checked_add` / `checked_sub` / `checked_mul` with `ok_or(..)?` (or handle `Some/None` with `match`); use `wrapping_*` only when you intend to wrap around, and `saturating_*` only when you intend to clamp at the upper or lower bound.

```ksnl fragment
let total = a.checked_add(b).ok_or(app.Error.Overflow)?
let diff  = a.checked_sub(b).ok_or(app.Error.Underflow)?
let prod  = a.checked_mul(b).ok_or(app.Error.Overflow)?
let wrapped = a.wrapping_add(b)   // 巻き戻しを意図
let clamped = a.saturating_add(b) // 上端・下端で打ち止め
```

- There is no `as` cast. Use `i32.try_from_i64(x)`, `x.to_f64()`, `c.to_u32()`.
- `wrapping_*` / `checked_*` are `add sub mul neg shl shr` (`checked_` also has `div rem abs`). `wrapping_*` returns a value, so it can be used in `panics never`, but if you do not intend to wrap, use `checked_*`.
- Comparisons and `& | ^ !` can be written as operators. There are no shift operators `<<` `>>`: use `x.wrapping_shl(n)` / `x.wrapping_shr(n)` (wrapping) or `x.checked_shl(n)` (`Option`). The shift amount `n` is `u32`.

## Control flow and values

```ksnl fragment
let mut total = 0u32
for x in core.iter.range(0u32, 5u32) {
    total = total + x
}
let label = match shape {
    app.Shape.Circle(r) => r
    app.Shape.Rect(w, h) => w * h
    app.Shape.Unit => 0i32
}
if total > 3u32 {
    return Err(app.Error.TooBig)
}
```

- A `match` lists every variant. `_` cannot be used for `enum` and `bool`. To look at only one form, use `if let t.Kind.Num(n) = k { .. } else { .. }` (`else` can be omitted; the binding is only inside `then`) and `k is t.Kind.Num(_)`, which returns `bool` (it cannot bind names; use `_`).
- The body of an arm starts on the same line as `=>`. Do not break the line after `=>`. Write a block as `Some(v) => {` (do not put `{` on the next line). You cannot write an assignment `x = e` directly as an arm body: put `result = r` on the line after `Some(r) => {` and close with `}` (the one-line block `=> { result = r }` is not allowed either).
- Shadowing is forbidden. Name an unused binding `_x`. `let _ = e` is forbidden (use `core.mem.drop(e)`).
- `mut` cannot be written inside a pattern (`Ok(mut v) =>` is E0002). To mutate, rebind inside the arm: put `let mut w = v` on the line after `Ok(v) => {`.
- An expression statement that discards a value is forbidden: make it explicit with `core.mem.drop(x)` or `return`. If the `then` branch of an `if` without `else` in statement position ends with a value, it is E2042 (`if flag { true }` is an error. Rewrite it with `return` / `core.mem.drop(..)`). The same goes for the value of a block, `unsafe` or `with` in the middle of statements (and for `if` with `else`, and `match`, as statements). The same goes for a value at the end of the body of `while` / `loop` / `for`. For a call that returns `Result`, prefer `v.push(x)?` over `core.mem.drop(v.push(x))`, and if you truly discard it, use `core.mem.drop(..)`.
- If you destructure a linear value by value (`let` / `match` / `if let`), it is consumed even when you take only `_` or only Copy fields (using it afterwards is E3002). To merely peek, use `match &r { .. }`.
- A linear value is consumed exactly once on every path (normal end, `return`, `?`, each branch). Creating and discarding it in a branch of an `if` / `match` in statement position, or consuming it in only one branch, is E3003. Consuming it only in the right-hand side of `&&` / `||` is not allowed either (`c && r.close() == 1u32` is E3003. Split it, as in `if c { r.close() }`, and consume it on each path).

## Option / Result

```ksnl fragment
let r = v.push(10u8)?     // 確保失敗は app.Error に変換される (下の From)
match src.get(i) {
    Some(c) => c
    None => return Err(app.Error.Eof)
}
```

- `?` works only when the error types match exactly, or when your own error type is an `enum` with `derive [From]` that has a variant whose only payload is the original error type (it is wrapped as `E2.V(err)`). Otherwise E2020. Two or more variants that carry the same type is E2049. You can also write `.map_err(f)?`. `?` on `Option` has no conversion.
- `derive [From]` is only for `enum` (a `type`, or an `enum` with no variant that has a single payload, is E2050). It is one level only. For conversions such as merging several errors into one variant or keeping only part of the contents, write `map_err(function)`. `ksn get --derived app.Error` shows the list of wrapping variants.

```
enum app.Error derive [Debug, Clone, Copy, PartialEq, From] pub
    Alloc(alloc.alloc.AllocError)
    Io(std.io.Error)
end app.Error

fn app.fill(v: &mut std.vec.Vec<u8>) -> Result<(), app.Error> uses [alloc, io.console] panics never
    v.push(1u8)?
    std.io.print_line("filled")?
    Ok(())
end app.fill
```
- Instead of indexing, use `a.get(i)` (`Option`). For ranges, use `a.sub(i, j)`.
- `a.get(i)` is `Option<&T>`. To compare, write `a.get(i) == Some(&x)` (a reference inside a type argument cannot be dereferenced. If you need the value, use `match` and `Some(*x)`).
- Sorting and searching (`core.slice`, `panics never`, no allocation): `s.sort()` (`T: Ord`, stable), `s.sort_by(fn(a: &T, b: &T) -> core.cmp.Ordering capture [] { a.cmp(b) })` (stable), `s.sort_unstable()` / `s.sort_unstable_by(..)` (heap sort, unstable), `s.binary_search(&x)` → `Result<usize, usize>` (`Ok(position)` / `Err(insertion position)`; sorted input only), `s.reverse()`, `s.contains(&x)`. From a `Vec`, use `v.as_mut_slice().sort()` (`v.sort()` also works).

## Collections and strings (pass an allocator)

```ksnl fragment
let mut v = std.vec.Vec.new()
v.push(1u8)?
let n = v.len()
v.extend_from_slice(&src)?        // T: TryClone。確保は先に reserve するので、失敗しても追加は途中で止まらない
v.append(&mut other)?             // other の要素を順に移して空にする
v.dedup()                         // 隣り合う等しい要素を 1 つに (T: PartialEq)
let has = v.contains(&x)
let mut s = std.string.String.new()
s.push_str("abc")?
let text = s.as_str()            // &String を &str の引数に渡すときも必ず .as_str() (暗黙の変換は無い。E2023)
std.io.print_line(s.as_str())?    // print_line(&s) / print_line(r) (r: &String) は E2023
let mut m = std.collections.HashMap.new()
core.mem.drop(m.insert(key, value)?)
```

- `BtreeMap` keeps the order of keys: `m.iter()` (`(&K, &V)` in key order), `m.range(&lo, &hi)` (a half-open interval of at least `lo` and less than `hi`, in key order), and `m.first()` / `m.last()` (`Option<(&K, &V)>`). Iterate in the form `for (k, v) in m.range(&2u32, &5u32) { .. }`.
- `std.collections.HashSet<T>` is created with `HashSet.new()`, with `insert(v)?` (`Result<bool, AllocError>`, `true` if newly inserted), `contains(&v)` / `remove(&v)` (`bool`), `len()` / `is_empty()`, and `iter()` (iterates `&T`). For `String` elements, only the forms `contains_str(s.as_str())` / `remove_str(..)` (`contains(&k)` / `remove(&k)` are E2054).
- For a `HashMap` with `String` keys, **only lookup by `&str` is correct** (one canonical form): `m.get_str(k.as_str())` (`Option<&V>`), `m.get_mut_str(..)` (`Option<&mut V>`), `m.contains_key_str(..)`, `m.remove_str(..)` (`Option<V>`). `m.get(&k)` / `get_mut` / `contains_key` / `remove` are E2054 (`ksn fix` rewrites them to `get_str(k.as_str())`). You do not need a `String` for the lookup (a `&str` can be passed as it is: `m.get_str("one")`). `insert`, which takes the key, is unchanged. A map whose key type is not determined is E2044. It works on both the tree walker and native (LLVM).
- Clone a `String` with `s.try_clone()` (`Result<String, alloc.alloc.AllocError>`, `uses [alloc]`). The `Debug` of `Vec<T>` is `[a, b]`, and a `Vec` can be a field under `derive [Debug]` (native only when the elements are integers, `bool` or `char`; otherwise it falls back to the bundle).
- Shared handles (there is no `Rc` / `Arc`. Cycles do not leak, because an arena discards all its slots): `let mut a = alloc.arena.Arena.new(std.alloc.SYSTEM)` is an `Arena<T, A>`, and `a.insert(v)?` returns a `Handle<T>` (a `Copy` value with only `index` and `generation`. The fields are private, and only the `Arena` side can create one). `a.get(h)` / `a.get_mut(h)` return `Option<&T>` / `Option<&mut T>`, `a.remove(h)` returns `Option<T>`, and there are `a.contains(h)`, `a.len()`, `a.is_empty()`. A handle that has been `remove`d no longer matches the generation and gives `None` (like a weak reference. An old handle to a reused slot does not match either). `Handle<T>` can also be an element of a `Vec` (a `T: linear` or a type that carries references cannot be put in).
- Methods of `str`: `len` `is_empty` `as_bytes` `chars` `split_at`. Bytes → string is `core.str.from_utf8`.
- Searching (no allocation): `find` `rfind` (`Option<usize>`), `contains` `starts_with` `ends_with` (`bool`), `eq_ignore_ascii_case` (ignores only ASCII case).
- Slicing (no allocation; the result is a `&str` pointing into the original string): `trim` `trim_start` `trim_end` (ASCII whitespace), `strip_prefix` `strip_suffix` (`Option<&str>`), `split_once` (`Option<(&str, &str)>`).
- Splitting and iterating: `for p in line.split(",")` (the pieces are `&str`. Delimiters at the ends or in a row give empty pieces. If the delimiter is empty, the whole string is one piece).
- Digits → integer: `i32.parse("-42")` is `Result<i32, core.num.ParseIntError>` (decimal. For other radixes, `from_str_radix`).
- Strict decimal (IDs and the like): `u64.parse_decimal(s)` (same for the other types) rejects `+`, leading zeros (except `"0"`), `-0`, whitespace, and non-ASCII digits (signed types accept only a leading `-`). Use this to parse IDs.
- Comparing secrets (tokens and the like): `core.slice.ct_eq(a, b)` (`&[u8]` with `&[u8]`, `bool`) compares all bytes regardless of the contents (branching depends only on the length). If the lengths differ it returns `false` immediately (the length is not treated as secret). Do not compare with `==`.
- Integer → string is appended to a `String`: `s.push_i64(x)` `s.push_u64(x)`. They are only for `i64` / `u64`, so widen other types with `i64.from_i32(x)` or `u64.from_usize(n)`.

- Formatting (no macros): `alloc.fmt` appends to a `String`. `alloc.fmt.write(&mut s, &x)` (`Display`) / `alloc.fmt.debug(&mut s, &x)` (`Debug`; types with `derive [Debug]` are also accepted) return `Result<(), alloc.alloc.AllocError>`. Width, fill and radix are passed as a value `alloc.fmt.Spec`: `let hex = alloc.fmt.Spec.new().with_radix(16u32).with_upper().with_width(6usize).with_fill('0')` → `alloc.fmt.write_u64(&mut s, 48879u64, &hex)?` (`"00BEEF"`). There are also `write_i64` / `write_str`, and `with_left()` for left alignment. In native, what you can pass to `write` / `debug` is integers, `bool`, `&str`, `String` (`Display`), and types with `derive [Debug]` (the `Display` of `char` and a user's `Display` fall back to the bundle).

```ksnl fragment
let mut line = std.string.String.new()
line.push_str("n=")?
line.push_i64(i64.from_i32(n))?
line.push_bytes(buf)?        // buf: &[u8]。エラーは PushBytesError（UTF-8 でなければ InvalidUtf8）。AllocError と混ぜるなら map_err(f)?
let a = std.string.String.replace(std.alloc.SYSTEM, "a-b", "-", "+")?   // "a+b"
let parts = ["x", "y", "z"]
let j = std.string.String.join(std.alloc.SYSTEM, &parts, ", ")?        // "x, y, z"
```

- `replace(allocator, text, from, to)` and `join(allocator, parts, separator)` return a new `String` (if `from` is empty, nothing is replaced). The methods of `str` can be used directly on `String` as well.
- Holding a `split` in a variable and advancing it with `.next()` works only in the interpreter (native backs off with `skipped`). Passing it directly to `for` also works in native.

## Closures and higher-order functions

```ksnl fragment
let total = core.iter.fold(
    core.iter.range(0u32, 5u32),
    0u32,
    fn(acc: u32, x: u32) -> u32 capture [] {
        acc + x
    },
)
```

- List the outer bindings that the body uses in `capture [&a, move b]`. A name in `&a` is a `&T` in the body, so use `*a` for the value (`move b` stays `b`).
- Writing a reference captured with `capture [&a]` (or a reference derived from it) into the place that a closure parameter points to (including `*sl = Some(a.as_str())`, `mem.replace`, nested closures, and through a `&mut` that was `move`d) is an error (E3030). The borrow of a capture can flow only into the closure's return value and the body's locals. To write it into a parameter, use an owned value (`a.clone()`).
- A higher-order function can be written as `fn app.ap[F: core.ops.Fn<(usize,), u8>](f: F) -> u8 uses [] panics never`. If you omit the effect arguments `E, P` of an `Fn*` bound, the `uses` / `panics` of the closure you pass propagate to the caller as they are (the same as `uses [E] panics P`). Only when you want to restrict the effects, write it explicitly as `Fn<(usize,), u8, [], never>`. Only one bound per function can be omitted.

## Trait objects (`dyn`) and `Box<dyn>`

```
type app.Bus pub
    slots: std.vec.Vec<app.Slot>
end app.Bus

trait app.Handler[E: uses, P: panics] pub
    fn handle(self: &mut Self, event: &str) -> u32 uses [E] panics P
end app.Handler

type app.Slot
    handler: std.boxed.Box<dyn app.Handler<[io.console], never> + 'static>
end app.Slot

fn app.Bus.register[H: app.Handler<[io.console], never>](self: &mut app.Bus, h: H) -> Result<(), alloc.alloc.AllocError> uses [alloc] panics never pub
    let b = std.boxed.Box.new(h)?
    self.slots.push(app.Slot {
        handler: alloc.boxed.Box.into_dyn(b)
    })?
    Ok(())
end app.Bus.register
```

- `dyn Tr<E, P> + 'static` can be written only behind `&` / `&mut` or as the first type argument of `Box` (`Vec<dyn Tr>` is E2047; `Vec<Box<dyn ..>>` is fine). A `dyn` put in a `Box` must state `+ 'static` explicitly.
- `&T` → `&dyn Tr` converts implicitly. `Box<T, A>` → `Box<dyn Tr, A>` is not implicit; write it explicitly with `alloc.boxed.Box.into_dyn(b)` (the target is determined by the expected type of a `let` annotation, an argument, a field, and so on. If it is not determined, E2044).
- `x.m()` calls the methods of `Tr` and its supertraits (via the vtable). A call through `dyn Tr<E, P>` has `uses [E] panics P`, and if the caller does not cover it, E4001 / E4002. A trait that is not `dyn`-safe is E2033, and a linear type cannot be put in a `Box` (E3006).
- Dropping destroys the fields of the concrete type (such as `String`) first and then frees through the `Box`'s allocator. Native supports only `Box<struct or enum>` with `std.alloc.System` (a `Box` of a scalar, and a `dyn` of a trait with type parameters, are `skipped`).

## Common constraints

- Comments (`//`, `///`) cannot be written in `.ksnl` (E0059). Humans write `.ksnh` (a `.ksnl` with comment lines added) and make the `.ksnl` with `ksn strip` (a mismatch is E9050). The LLM writes only `.ksnl`. A dependency's `.ksnh` is neither read nor checked (what is published is only `.ksnl`). When an LLM changes a `.ksnl`, reattach the comments of the human's `.ksnh` with `ksn sync` (lost comments with `--drop-lost` / `--keep-lost`, conflicts with `--take ksnh|ksnl`).
- Strings, characters and comments have one canonical form: write printable characters as they are (`"\x41"`, `"\u{41}"`, `"\u{3042}"` are E0005. `ksn fix` repairs them). The only escapes are `\\` `\"` (`\'` in a `char`) `\n` `\t` `\r` `\0`, and `\u{202e}` (lowercase, no leading zeros) for characters that cannot be written as they are (direction-changing characters, invisible characters, control characters, BOM, non-ASCII whitespace). A character that cannot be written as it is, even in a string or a comment, is E0004. Strings and comments must be NFC (E0006). Invalid UTF-8 is E0007. `\xHH` is only for non-printable bytes in `b".."`.
- Struct literals and struct patterns always have one field per line (separated by line breaks, no `,`. The `}` is indented like the line of `{`). `let p = t.P {` / `    x: 1u32` / `    y: 2u32` / `}`. `ksn fmt` fixes the one-line form.
- A recursive type needs an explicit `Drop` implementation (E4013). Represent a tree with a list of nodes and indices (an arena), or, if the recursion is only `Box<Self>` / `Option<Box<Self>>`, use `derive [Drop]` (`enum t.L derive [Drop] pub` + `Cons(i64, std.boxed.Box<t.L>)`. It drops iteratively regardless of depth. `Vec<Self>`, mutual recursion and `linear` are not allowed, E2051). A non-recursive type does not need `derive [Drop]` (E2051).
- A type that implements `Drop` (by hand or via `derive`) cannot be destructured by a pattern that binds its fields by owned value (E3002. Match through a reference, as in `match &p { t.Q { v: v, kid: _k } => *v }`, or pass it whole to `core.mem.drop(p)`).
- The upper limit of call depth is 100000 by default (beyond it, `stack overflow`, exit code 102). The limit of the main thread is raised only in the entry function's header with `depth M` (after `stack N`. For example `fn app.main() -> Result<(), std.io.Error> uses [io.console] panics may depth 1000000`). Writing it on anything other than the entry is E4014, and an out-of-range value (anything other than 1 to 10000000) is E4015. To raise the limit of a `spawn`ed thread, use `std.thread.spawn_deep(std.alloc.SYSTEM, f, 500000usize)`.
- A recursive `panics never` function gets `stack <bytes>` right after `panics never` in its header:

```
fn app.fact(n: u64) -> Option<u64> uses [] panics never stack 4096
    if n == 0u64 {
        return Some(1u64)
    }
    match app.fact(n.wrapping_sub(1u64)) {
        Some(r) => r.checked_mul(n)
        None => None
    }
end app.fact
```

- Clean up resources with `with name = initializer close cleanup-expression { body }`. In the body, `?` / `return` / `break` / `continue` can be used, and on every exit the `close` expression runs exactly once. The `close` expression consumes the binding and returns `()`. Do not move the binding in the body.
  ```
  fn app.head(path: &str, buf: &mut [u8]) -> Result<usize, std.io.Error> uses [io.fs] panics never
      with mut file = std.fs.open(path)? close core.mem.drop(file.close()) {
          file.read(buf)?
      }
  end app.head
  ```
- For a file you write, end the body with `f.sync()?` (or `f.flush()?` if durability is not needed), and return a write failure with `?`. A failure of `close` may be discarded (the same for files opened for reading and for sockets after completion).
  ```
  fn app.save(path: &str, data: &[u8]) -> Result<(), std.io.Error> uses [io.fs] panics never
      with mut file = std.fs.create(path)? close core.mem.drop(file.close()) {
          core.mem.drop(file.write(data)?)
          file.sync()?
      }
      Ok(())
  end app.save
  ```
- Open files that you serve rooted at a directory (anything that escapes with `..`, absolute paths, and any symlink are rejected): open the root with `std.fs.Dir.open(root)`, and open files with `root.open_file_beneath(rel)`. Reading uses `read_to_end_limited(max)`, which returns `Err` when the limit is exceeded (it also stops a growing file). Do not join paths as in `std.fs.open(root.join(rel))`. `metadata` and `read_to_end_limited` are currently supported only on the tree walker (`ksn run`) (not yet on native).
  ```
  fn app.serve(root: &std.fs.Dir, rel: &str) -> Result<std.vec.Vec<u8>, std.io.Error> uses [alloc, io.fs] panics never
      with mut file = root.open_file_beneath(rel)? close core.mem.drop(file.close()) {
          file.read_to_end_limited(1048576usize)
      }
  end app.serve
  ```
- An input reference with an elided lifetime (`x: &u32`) cannot be returned or written as a named (`&'a u32`) or `'static` output (E3010. The region of an elided reference is fresh for each call). The same applies to the inside of `Option<&u32>` and `& &u32`, and to values taken out by `*x`, `match` / `for` / destructuring, or through a closure. If you return or store it, declare the input as named too (`x: &'a u32`).
- `unsafe` is allowed only in the modules listed in `[unsafe] modules` of `kasane.toml`.
- Types that own heap memory (`std.vec.Vec` and so on) implement `alloc.alloc.TryClone` (`try_clone(&self) -> Result<Self, alloc.alloc.AllocError> uses [alloc]`) rather than `Clone`. It is generated, in the order `derive [Debug, Clone, Copy, TryClone]`, when every field is `TryClone`, and otherwise E2036. It can be used in generic functions with `T: alloc.alloc.TryClone`, and a `Clone` type also has `TryClone`.
- JSON (`std.json`): `derive [ToJson, FromJson]` (the order within `derive` is `..., From, ToJson, FromJson`). `std.json.to_string(&x)` is `Result<String, AllocError>`; parse with `std.json.parse(text)` → `Doc`, and `T.from_json(&doc, doc.root())` is `Result<T, std.json.Error>` (`FromJson` is an associated function and cannot be called from a type parameter). `Doc.value_at(doc, at)` gets a `Value` at any position (`None` if out of range), and `Value.index()` gets the current position (the `doc`/`at` fields of `Value` are private). A struct is `{"x":1}`, and an enum is `"V"` / `{"V":value}` / `{"V":[value, ..]}`. Fields can be only integers, `bool`, `std.string.String`, `Vec<T, System>`, `Option<T>`, and types with ToJson/FromJson (anything else is E2036). Only types consisting of integers and `bool` can be lowered to native.
- FFI (`examples/ffi`): `extern app.f(x: i32) -> i32 link "f"` (calls only inside `unsafe "name" "reason" { .. }`). A mismatch of numeric types in a call to an `extern` is also E2023 (no silent conversion. Go through an explicit conversion such as `u64.try_from_i32(x)`). `link "libm.so.6:cbrt"` does `dlopen` / `dlsym` at run time (failure is exit code 101). A C struct is `type app.P layout c derive [Clone, Copy]` (passed by value, returned by value, and `*mut app.P`). For a C callback, write `fn(*const i32, *const i32) -> i32 uses [] panics never` as the parameter, and pass `app.compare` or a closure with no captures (pass `void *` user data as `*mut app.Acc`). For an array or slice, use `core.ptr.from_mut(&mut v)` to get a pointer to the first element.
- Reserved words (cannot be used as variable names): `fn type enum trait impl const static extern test end import let mut if else match loop while for in break continue return true false self Self pub uses panics unsafe move with dyn blanket never may async await where ref as` and the names of primitive types. `stack depth derive linear layout requires capture link impls close is` can also be used as identifiers (`close` is special only in the position of `with`).
- Functions of `extern` and `core.ptr` cannot be used as values (E5003): they cannot be put in a variable, a field, a closure, a return value, or an `if let` binding (the same inside `unsafe`). Calls to both `extern` and `core.ptr` are only inside `unsafe "name" "reason" { .. }` (just `unsafe {` is E0002). Callbacks passed to C (a function-type parameter) are excluded.
- Shared state (such as a server's store): `static app.STORE: std.sync.Mutex<app.Store> = std.sync.Mutex.new(app.mk())`. `app.mk` is a `const fn` with no arguments (built with `Vec.new(std.alloc.SYSTEM)` and the like). `let mut g = app.STORE.lock()` → `let s = g.get_mut()`, then `s.n = ...`, `s.items.push(..)`. For mostly-read data use `std.sync.RwLock` (the same form: `read()` → `get()`, `write()` → `get()` / `get_mut()`). A guard is held until the end of the binding (or `core.mem.drop(g)`), and the reference from `get()` cannot escape the guard (E3030).

## Borrows are second-class values (E3060)

- References (`&T` / `&mut T`, and types that carry references such as `Option<&T>`, `Result<&T, E>`, tuples and `core.slice.Iter`) are placed **only in parameters, return values and local bindings**. They cannot be placed in a field of a `type` / `enum` or in the contents of a variant, in the type arguments of `Vec` `HashMap` `Box` `Mutex` channels and the like, or in the storage of a closure with `capture [&x]` (E3060). A type declaration with lifetime parameters (`type t.View['a]`) cannot be written either.
- References cannot be written in the trait's type arguments in an `impl` heading either (E3060): `impl core.iter.Iterator<&u64, ..> for X` is an error (only `&'static` and the standard library's types with lifetimes are allowed).
- How to fix it (when an LLM writes `type t.Parser['a] { src: &'a str, pos: usize }`): **hold an index for the position, and pass the original data as a function argument**. Rewrite it as `type t.Parser { pos: usize }` and pass `src` each time, as in `fn t.Parser.next(self: &mut t.Parser, src: &str) -> Option<u8>`. For a value you want to keep for a long time, own it (`String`, `Vec<T>`). Make a `Vec<&T>` a `Vec<usize>` of indices.
- Allowed: taking a reference as an argument; returning a reference (`fn t.get(self: &t.S, i: usize) -> Option<&T>`; tie it to an argument by elision or `['a]`); holding `let r = v.get(0)` as a local; putting a reference in the type argument of a generic function (`t.id(&x)`). `&'static T` has no region, so it can also be placed in fields and containers.
- A container whose type is determined by inference (`let mut v = std.vec.Vec.new(..)` with references `push`ed) passes only when it can be shown that only string literals go in. Otherwise annotate it as `let v: std.vec.Vec<&'static str> = ..`, or put in owned `String`s.
- For formatting, use `core.fmt.Formatter.new(limit)` (an owned fixed-length buffer; the upper limit is 256 bytes). For JSON, use `T.from_json(&doc, doc.root())` and `std.json.decode_*(&doc, v)` (`Value` is just an index, and you pass the `Doc` as an argument).

## Suspension and cooperative execution (`suspend`, `std.task`)

- Suspension is the effect `uses [suspend]`. A suspension point is a call to a function that has `suspend`, and `yield_now` and `Task.join_task` are such calls. The effect propagates by the usual rules (E4001).
- The executor is `std.task.block_on(f)`: it runs `f` as the root task, waits for all the remaining tasks, and then returns. The caller also declares `uses [suspend]`. Nesting is not allowed.
- `std.task.spawn_local(f) -> std.task.Task<T>` (`f` is `'static`): creates a task. It runs inside `block_on`, when the others yield or wait. Receive the result with `t.join_task()`. The fields of `Task` are private (E1122. Pass the handle as it is to `join_task` / `cancel`).
- The failure of `std.thread.spawn` / `spawn_deep` is `Err(std.thread.SpawnError<F>)` (`code: i32` and the returned closure `closure: F`). If the closure captures a linear value, `SpawnError<F>` is also linear, and the `Err` arm must consume it, for example by taking out `closure` and running it (`task()`) (discarding with `Err(_)` is E3003). Only the tree walker returns the closure; in native and the VM, if a thread cannot be created, the program stops with a `spawn failed: ...` panic (101).
- The `'static` bound is checked in one place (E2037): a closure for `spawn_local` cannot capture a value that contains a local borrow (`capture [&x]` is E2037. The same for an argument of type `&'static T` and for a type parameter `F: 'static`. A local borrow such as `as_str()` of a `String` does not pass either). String literals, `const`s, and the results of functions that return `&'static` pass.
- A borrow used across a suspension point is E3050 (using `r` after `yield_now()` following `let r = &x`). A borrow passed as an argument is excluded.
- A `MutexGuard` or an RwLock guard cannot be held across a suspension point either (E3050). Do `core.mem.drop(g)` and then `yield_now()`. A task that holds a linear value or an owned value cannot be `cancel`led (E4022).
- Multiple threads: `std.task.block_on_threads(std.alloc.SYSTEM, n, f)` + `std.task.spawn(f)` (`f` and its result are `Send` + `'static`. The caller needs `uses [alloc, suspend, thread]`). Locals that live across a suspension point must also be `Send` (E2074. For a type parameter, `T: core.marker.Send`). For work that holds a non-`Send` value, use `block_on` + `spawn_local`. `n` is ignored by the tree walker (it is deterministic).
- Scheduling is deterministic (round-robin in id order). Native (LLVM) and wasm32 give the same result as the tree walker (a function with `uses [suspend]` is converted into a state machine at each suspension point). A suspending function that returns `Result`, and `?`, are lowered as well (a form that puts `Option<Result<..>>` in the frame is E9040). Cancellation (`Task.cancel`, stage 4 of 5.6) and suspension that waits for a JS Promise on wasm32 (`std.task.await_js`, stage 5) are implemented. `await_js` can be used only in wasm32 builds (the tree walker panics because there is no JS counterpart, and native is E9040).

```
fn t.main() -> () uses [suspend] panics may
    std.task.block_on(fn() -> () capture [] {
        let a = std.task.spawn_local(
            fn() -> u32 capture [] {
                t.worker()
            },
        )
        let r = a.join_task()
        core.mem.drop(r)
    })
end t.main

fn t.worker() -> u32 uses [suspend] panics never
    std.task.yield_now()
    7u32
end t.worker
```

### Handling many connections on one thread (`accept_async` / `read_async` / `write_async`)

- `std.net.TcpListener.accept_async(&self)`, `TcpStream.read_async(&mut self, &mut buf)` and `write_async(&mut self, &buf)` are suspension points with `uses [io.net, suspend]`, in the same form as the blocking versions (`accept` / `read` / `write`). Use them inside `std.task.block_on`. File descriptor numbers, epoll and EAGAIN do not appear. Borrows are arguments, so you can call them while holding `&mut buf`.
- Use one task per connection with `std.task.spawn_local(fn() -> usize capture [move conn] { ... })` (`TcpStream` is linear, but the closures of `block_on` / `spawn_local` can capture linear values). `block_on` does not return until all tasks have finished.
- Timeouts work as they are with `set_read_timeout(ms)` / `set_read_deadline(ms)`, and give `Err(e)` with `e.kind == std.io.ErrorKind.TimedOut`. `write_async` writes up to 16384 bytes at a time (the return value is the number written; resend the rest with `while`).
- Outside the executor they do not wait, and behave the same as the blocking versions. Native on Windows is E9040, and wasm32 is unsupported. Example: `stage0/tests/data/net_async.ksnl` (an echo of 256 connections).

## Time, calendar, logging

```ksnl fragment
let now = std.time.SystemTime.now()                  // uses [time]。UNIX 時刻 (ナノ秒) の値
let secs = now.unix_seconds()                         // i64（UNIX 秒、床）
let d = std.datetime.from_unix_seconds(secs)          // UTC の DateTime { year, month, day, hour, minute, second, nanos }
let text = d.to_rfc3339()?                            // Result<String, AllocError>: "2026-11-14T10:20:30Z"
let p = std.datetime.parse_rfc3339("2000-02-29T00:00:00+09:00")  // Result<DateTime, std.datetime.Error>（UTC に直す）
std.log.info("started", &[("port", "8080")])?        // uses [alloc, io.console, time]。戻り値は Result<(), std.io.Error>
```

- `DateTime.now()` is the current UTC. All fields of `DateTime` are `i64`. Parsing (`parse_rfc3339`) checks the range of the fields.
- The parse errors are `std.datetime.Error.Eof(position)` `Unexpected(position)` `OutOfRange(start of field)` `Trailing(position)` (the position is a byte index).
- `std.log.debug` / `warn` / `error` have the same form. The output is one line like `{"ts":"…","level":"info","msg":"started","port":"8080"}`, written to standard error. `std.io.write_stderr(&[u8])` is a single write to standard error (`uses [io.console]`).
- Floor division of integers (rounds negative numbers toward the smaller value) is `std.time.div_floor(a, b)` / `std.time.rem_floor(a, b)`. In a `panics never` function, use these instead of `/` and `%`.

## Commands

| Command | Purpose |
| --- | --- |
| `ksn check [--json]` | Check. `--json` includes suggested fixes (`fix`) |
| `ksn check --signatures-only` | Checks a `fn` with no body (`end` right after the header) as a declaration without a body (interface first. It looks at signatures, types, bounds and imports, and skips the checks that depend on the body). `build` / `run` / `test` do not accept it (E0067) |

| `ksn check --native` | In addition to the normal check, points out functions that cannot be lowered to native (KIR) with a non-fatal warning `W9040`. Run it before `ksn build --release` |
| `ksn fix` | Apply suggested fixes and format |
| `ksn fmt [--check]` | Format into the canonical form |
| `ksn build [--release] [--target <T>] [--emit-llvm <file>]` | Native generation (LLVM). `--target` is the triple in the "Targets and wasm32" section. What cannot be lowered gives the reason and E9040 (`--release`; debug falls back to the bundle with the interpreter). `--emit-llvm <file>` writes LLVM IR (`.ll`) instead of an executable |
| `ksn run` / `ksn test` | Run / test (the default is the VM. A form that cannot be lowered falls back to the tree walker. `--engine=tree` or `KSN_ENGINE=tree` selects the tree walker, and `--engine=vm` shows the reason for the fallback) |
| `ksn debug [--json] [-- args]` / `ksn test --debug <test FQN>` | Debugger (on the interpreter). One command per line on standard input. `--json` gives one event per line |
| `ksn dap` | Debug Adapter Protocol server (from VS Code and others) |
| `ksn context <FQN>` | Shows only the definition and the signatures it refers to (passes a small context). An LLM finds the FQN with `ksn index` and reads this output or `ksn get <FQN>` |
| `ksn get <FQN>` / `ksn sig <FQN>` | The whole definition / the one-line header |
| `ksn index [--std]` | An index for LLMs. One line of JSON per definition (`fqn` `kind` `file` `start` `end` `header` `doc` `uses` `panics` `tests` `pub`; in FQN order). A module description is the line whose `kind` is `module` |
| `ksn unsafe [--json]` | A list of `unsafe` (permitted modules, manual `Send`/`Sync`, `unsafe` blocks, `extern`, types holding raw pointers; dependencies too). Read only |
| `ksn doc [--std] [--private]` | Static HTML API documentation (`target/doc/`). Links and code examples in `///` are also included |
| `ksn test --doc` | Runs only the code examples in `///` (```` ```ksn ````). `ksn test` also runs the examples |
| `ksn test --both --native-required` | With `--both`, makes tests that could not be lowered to native and fell back to the bundle fail (with a reason). Without the flag, they fall back and pass as before |
| `ksn explain <Exxxx>` | Explanation of an error code |

To see the E9040 of `ksn build --release` beforehand, run `ksn check --native` before `ksn build --release` (a normal `ksn check` does not emit W9040).
Before finishing a module, run `ksn check --native` and `ksn test --both --native-required` (the latter makes tests that could not be lowered to native and fell back to the bundle fail. `--both` is required).

## Documentation (`///`)

- A `///` right before a definition is its description. A `///` at the top of a file followed by one blank line is the module description.
- The body has paragraphs, `` `code` ``, `**emphasis**`, `- ` bullet lists, and ```` ```ksn ```` fences (examples).
- Links are `` [`FQN`]`` (a unique short name also works. Fields, variants, tests and `impl`s cannot be pointed to). If a link does not resolve, `ksn check` / `ksn doc` stop with E1125.
- `ksn test` runs the fenced examples (written the same way as a test body; no `fn main` is needed. A failure is a panic. Only fences whose info is `ksn` are examples; other fences are display only).

## HTTP and TLS

For an upstream response (the reading side of a proxy or load balancer), use `std.http.ResponseDecoder` with limits. The head and body stop as soon as they exceed the limit while being read, and the body is read while taking it out with `take_body`. The framing is Content-Length / chunked / until close (`finish`).

```ksnl fragment
// 上流の応答を上限つきで読む: 頭 8 KiB、本体 1 MiB (応答 1 件ごと)。1xx は読み捨て、CL と TE の両方や二重の長さは BadRequest
let mut up = std.http.ResponseDecoder.new(8192usize, 1048576usize)
up.feed(chunk)?                                   // 来たバイトだけ渡す (何度でも)
let body = up.take_body()?                        // 溜まった本体を取り出す (保持は取り出すまで)
if up.is_done() {                                 // 応答が終わった
    let status = up.response().status             // 最終の応答 (1xx は含まない)
    up.reset_next()?                              // 残りのバイトから次の応答へ (keep-alive)
}
up.finish()?                                      // 接続が閉じた (閉じるまでの応答はここで完了、途中なら Truncated)
```

```ksnl fragment
enum app.Error derive [Debug, Clone, Copy, PartialEq, From] pub
    Http(std.http.Error)
    Net(std.net.Error)
    Tls(std.tls.Error)
end app.Error

fn app.serve() -> Result<(), app.Error> uses [alloc, io.fs, io.net] panics never
    with acceptor = std.tls.TlsAcceptor.load("cert.pem", "key.pem")? close acceptor.close() {
        with mut listener = std.net.listen("127.0.0.1:8443")? close core.mem.drop(listener.close()) {
            let conn = listener.accept()?
            with mut tls = acceptor.accept(conn)? close core.mem.drop(tls.close()) {
                let mut decoder = std.http.Decoder.new(8192usize, 65536usize)
                match std.http.read_request(&mut tls, &mut decoder)? {
                    Some(req) => {
                        let keep = req.keep_alive()
                        let head = "Content-Type: text/plain\r\n"
                        let bytes = std.http.render(200u16, head, b"ok", keep)?
                        std.http.write_all(&mut tls, bytes.as_slice())?
                    }
                    None => ()
                }
            }
        }
    }
    Ok(())
end app.serve

fn app.client() -> Result<(), app.Error> uses [alloc, io.fs, io.net] panics never
    let conn = std.tls.connect_with("127.0.0.1:8443", "localhost", "cert.pem")?
    core.mem.drop(conn.close())
    Ok(())
end app.client
```

- `Decoder.next()` takes out one request at a time. Leftover bytes become the next request, so you can read several requests in order on one keep-alive connection.
- A request that exceeds a limit is `HeadTooLarge` / `BodyTooLarge` (returned without reading). `Transfer-Encoding` supports only `chunked` (anything else is `Unsupported`).
- `TlsStream` and `TlsAcceptor` are linear: always `close` them (`with ... close` is convenient. `acceptor.close()` returns `()`). `read` / `write` are implementations of `std.io.Read` / `Write`. When the handshake fails, `TlsAcceptor.accept` closes `tcp` and returns `Err`.
- `TcpStream` is also linear, and its descriptor field is private (to read it, use `tcp.fd()`. You cannot forge an fd to create one). `std.net` also has timeouts (`set_read_timeout` / `set_write_timeout`, `std.io.ErrorKind.TimedOut`); deadlines (`set_read_deadline(ms)` / `set_write_deadline(ms)` make "ms from now" the deadline for all subsequent reads or writes, giving `TimedOut` once it passes; `clear_deadlines()` removes them; it is wall-clock time, so it also works against a sender that delivers one byte at a time); `set_nodelay(bool)`; `TcpListener.accept_timeout(ms)` and `std.net.connect_timeout(addr, ms)` (a timeout is a `std.net.Error` whose code is ETIMEDOUT, 110 on Linux); `shutdown`; `peer_addr()` / `local_addr()` (`Result<std.string.String, std.io.Error>`; the raw-buffer versions are `peer_addr_raw` / `local_addr_raw`); `resolve`; and `UdpSocket`.
- Defending against slow peers (slowloris): call `tcp.set_read_deadline(5000u64)?` once before you start reading a request. The deadline is not extended on each read, so it is the deadline for the whole request.
- A self-signed certificate for tests: `openssl req -x509 -newkey rsa:2048 -nodes -keyout key.pem -out cert.pem -days 2 -subj /CN=localhost -addext subjectAltName=DNS:localhost,IP:127.0.0.1`. `curl --cacert cert.pem https://127.0.0.1:8443/stats`.
- The sample HTTPS server is `apps/shortener_https` (`ksn run 127.0.0.1:8443 0 links.db cert.pem key.pem`. The arguments are `address connections store certificate key`, and a connection count of 0 means never stop). The log is one JSON object per line.

## HTTP server template (static files, deadlines, workers)

- The template below passes `ksn check` and `ksn build --release`, and has been run and verified (`/` gives 200, `/../x` gives 404, and the next request is still answered after a client that does not read). The entry is `app.main` (`entry` in `kasane.toml`). The workers are a fixed two plus the main thread (inside `app.main`).
- There are two deadlines per connection: `set_read_deadline` is the deadline for the whole of one request (it is not extended on each read), and `set_write_deadline` is the deadline for the whole response. When either passes, `TimedOut`.
- Static files are opened only with `std.fs.Dir.open_file_beneath` (it never goes outside the root). Reading is limited with `read_to_end_limited(limit)` (`Err` if exceeded).

```
enum app.Error derive [Debug, Clone, Copy, PartialEq, From] pub
    Alloc(alloc.alloc.AllocError)
    Http(std.http.Error)
    Io(std.io.Error)
    Net(std.net.Error)
end app.Error

fn app.handle(stream: &mut std.net.TcpStream, root: &'static str) -> Result<(), app.Error> uses [alloc, io.fs, io.net] panics never
    stream.set_read_deadline(5000u64)?
    stream.set_write_deadline(5000u64)?
    let mut decoder = std.http.Decoder.new(8192usize, 65_536usize)
    let req = match std.http.read_request(stream, &mut decoder)? {
        Some(r) => r
        None => return Ok(())
    }
    let raw = req.path.as_str().strip_prefix("/").unwrap_or("")
    let rel = if raw.is_empty() {
        "index.html"
    } else {
        raw
    }
    let mut status = 404u16
    let mut body = std.vec.Vec.new()
    with dir = std.fs.Dir.open(root)? close core.mem.drop(dir.close()) {
        match dir.open_file_beneath(rel) {
            Ok(file0) => {
                let mut file = file0
                if let Ok(bytes) = file.read_to_end_limited(1_048_576usize) {
                    status = 200u16
                    body = bytes
                }
                core.mem.drop(file.close())
            }
            Err(_e) => {
            }
        }
    }
    let bytes = std.http.render(status, "Content-Type: text/plain\r\n", body.as_slice(), false)?
    std.http.write_all(stream, bytes.as_slice())?
    Ok(())
end app.handle

fn app.main() -> Result<(), app.Error> uses [alloc, io.fs, io.net, thread] panics may
    with mut listener = std.net.listen("127.0.0.1:8080")? close core.mem.drop(listener.close()) {
        let (jobs, queue) = std.sync.channel(std.alloc.SYSTEM, 64usize)?
        let q1 = queue.clone()
        let spawned = std.thread.spawn(std.alloc.SYSTEM, fn() -> () capture [move q1] {
            app.worker(q1, "public")
        })
        let h1 = match spawned {
            Ok(h) => h
            Err(failed) => {
                let std.thread.SpawnError {
                    code: _code
                    closure: task
                } = failed
                core.mem.drop(task())
                core.panic.panic("spawn failed")
            }
        }
        let mut accepted = Ok(())
        loop {
            let stream = match listener.accept() {
                Ok(s) => s
                Err(e) => {
                    accepted = Err(app.Error.Net(e))
                    break
                }
            }
            match jobs.send(stream) {
                Ok(_u) => {
                }
                Err(back) => core.mem.drop(back.value.close())
            }
        }
        core.mem.drop(jobs)
        app.worker(queue, "public")
        core.mem.drop(h1.join())
        accepted
    }
end app.main

fn app.worker(first: std.sync.Receiver<std.net.TcpStream>, root: &'static str) -> () uses [alloc, io.fs, io.net, thread] panics never
    let mut queue = first
    loop {
        match queue.recv() {
            Ok(stream) => with mut s = stream close core.mem.drop(s.close()) {
                core.mem.drop(app.handle(&mut s, root))
            }
            Err(_e) => match queue.close() {
                Ok(_) => break
                Err(back) => {
                    queue = back
                }
            }
        }
    }
end app.worker
```

### Rules when writing the template

- One component is up to about 200 lines. If it grows past that, split it (split into modules and use `import`).
- Hold trees and nested structures by indices into an arena (`alloc.arena.Arena` and `Handle`). A recursive `enum` needs an explicit `Drop` implementation (E4013), and a recursive function needs `stack N` (E4010).
- Make fields a flat `type` (record).

### Shared state (between workers)

- Hold shared state in a `static` `std.sync.Mutex`. It can be a number or a struct (one that has `Vec` / `String` / `HashMap`). Build the initial value of a struct with a `const fn` that takes no arguments (see "Shared state (such as a server's store)" above). If reads dominate, use `std.sync.RwLock`.
- The `N` of `channel(alloc, N)` is the capacity: a `send` on a full channel waits until a `recv` makes room. If you send more than `N` where there is no receiver, it deadlocks (101).
- Hand over work (for example, passing connections from the acceptor to workers) through a channel (`std.sync.channel`) (`examples/shortener/src/shortener/pool.ksnl`). The form that passes shared state around through a channel is no longer used.

```
static app.HITS: std.sync.Mutex<u64> = std.sync.Mutex.new(0u64) pub

fn app.hit() -> u64 uses [thread] panics never
    let mut guard = app.HITS.lock()
    let cur = *guard.get()
    let next = cur.wrapping_add(1u64)
    *guard.get_mut() = next
    next
end app.hit
```

## External commands (`std.process`) and random numbers (`std.rand`)

```ksnl fragment
let mut cmd = std.process.Command.new("ls")       // 効果は process。Command と Child は線形
cmd.arg("-l")
cmd.stdout_piped()
cmd.stderr_piped()
let mut out = std.vec.Vec.new()
let mut err = std.vec.Vec.new()
let st = cmd.output(&mut out, &mut err)?          // 終了まで待つ。ExitStatus { code, signal }
let mut tok = std.rand.Rng.from_entropy()         // uses [rand]
let x = tok.next_u64()
let mut key = [0u8; 32]
std.rand.fill_secure(&mut key)?                   // OS の乱数を直接読む (CSPRNG)
```

- `Command` / `Child` are linear: `spawn` / `wait` / `output` consume them. `output` collects standard output and standard error into a `Vec` and returns an `ExitStatus`. `write_stdin` / `read_stdout` / `read_stderr` can be used only when a pipe was opened, as with `stdout_piped`. To leave a process running, use `spawn()` → `Child` (`id` `kill` `close_stdin` `wait`).
- If you just want to stop, use `std.process.exit(code)` / `std.process.abort()` (they return `!`).
- `Rng.from_seed(s)` is deterministic (the same sequence on the tree walker and LLVM), and `Rng.from_entropy()` takes one seed from the OS's randomness (64 bits at a time with `next_u64()`). Neither is a CSPRNG, so do not use them for keys, tokens or nonces. For cryptographic uses, use `std.rand.fill_secure(&mut buf)?` (`Err` if the OS randomness source cannot be read).

## Targets and wasm32

```ksnl fragment
extern app.alert(text_ptr: *const u8, text_len: usize) -> () link "env:alert"
```

- `ksn build --target` accepts `x86_64-linux-gnu` (omitted / host), `aarch64-linux-gnu` (the C development files are in `/usr/aarch64-linux-gnu`, overridable with `KSN_SYSROOT`), `wasm32-unknown-unknown` (plain wasm), and `wasm32-wasi` (WASI preview 1). The output goes to `target/<triple>/<debug|release>/`. An unsupported triple gives E9040 with a reason.
- The width of `usize` / `isize` depends on the target: 64 bits on x86_64 and aarch64, 32 bits on wasm32. The range check of literals (`ksn check --target` / `build`) is also done with the width of the target. On wasm32, conversions that can lose a value (`usize.from_u64`, `isize.from_i64`, `isize.from_u32`) do not exist and become `try_from_*` such as `usize.try_from_u64` (E1101. Write code that passes on every target with `try_from_*`).
- Usable on wasm32-wasi: `std.fs` (relative paths under pre-opened directories), arguments and environment, wall-clock time (`std.time`), and random numbers (`std.rand`). `std.task` (suspension) can be used on both wasm32 triples. Not usable (E9040): threads, sockets (`std.net`), external commands (`std.process`), printing floating-point numbers, and C FFI (on both wasm32 triples). `std.fs`, arguments and environment, wall-clock time and random numbers cannot be used on `wasm32-unknown-unknown` (plain wasm) (E9040).
- Connecting wasm32 and JS: `extern ... link "env:name"` is an import of JS `env.name` (it passes `ksn check` and does not resolve in native), and a function whose last FQN component starts with `ksn_` is exported under that name. `ksn bindgen <definition file>` generates glue code for the DOM, localStorage and fetch. Run with `node tools/wasm/run.mjs` (plain wasm) / `node tools/wasm/run_wasi.mjs` (WASI).

## Debugger (`ksn debug`)

- Commands (one per line. Lines starting with `#` and blank lines are ignored): `break <file>:<line> [if <expr>]` / `break <FQN> [if <expr>]` / `break <line>` (the current file while stopped), `delete <id>|all`, `breakpoints`, `run` (`step` stops at the first statement), `continue`, `step` (into calls), `next` (the next statement at the same depth), `finish` (to the caller, with the return value), `bt [thread id]`, `frame <n>`, `locals [n]`, `print <expr>`, `threads`, `quit`.
- Where it stops: breakpoints, steps, and panics (it shows the message and the panicking frame, and resuming gives exit code 101). It does not stop inside the standard library.
- The expressions of `print` and conditions must be side-effect free: variables, fields, operators, and calls to functions/methods that are `uses [] panics never` and do not take `&mut`. Assignments, blocks, loops, closures, `?` and calls with effects are rejected (an `error` event). Overflow of `+` and out-of-range indexing are evaluation errors, and the program does not panic.
- Events of `--json` (one object per line, distinguished by `event`): `ready` `breakpoint` `breakpoints` `deleted` `stopped` (`reason` = breakpoint / function breakpoint / step / entry / panic, `message`, `frame` = `{index, function, file, line, column, source}`) `stack` `locals` `value` `output` (`stream` = stdout / stderr) `error` `exited` (`code`) `terminated`.
- Example: `printf 'break demo.fib if n == 3u32\nrun\nbt\nlocals\nquit\n' | ksn debug --json`. If it stops inside a spawned thread, that thread's call chain is shown (`bt 1` is the side that is joining). If the program reads standard input, it competes with the commands for input.

## Dependency locking (`kasane.lock` version 2)

- For a `git` dependency, in addition to `hash`, the full text of the headers of public definitions (including `uses` / `panics`) is recorded. If you raise the dependency's version and its public effects or signatures change, it stops: effects widened is E9060, a signature changed or removed is E9061, narrowed or added is E9062 (all fatal. They show the old `-` and the new `+` headers).
- `ksn lock --diff` (show the difference without writing) → review → `ksn lock --accept <dependency>` (`--accept --all`). `ksn update` does not rewrite recorded signatures. For version 1, use `ksn lock --upgrade`. `path` dependencies are not recorded.

## Addendum: briefly verified forms

### Shortening lines longer than 100 characters (E0055)
Except for definition header lines and lines containing strings, a line may be at most 100 characters. Shorten a long expression by binding subexpressions with `let` (add them by hand if `ksn fmt` cannot split it). Beyond 100 characters, `ksn check` gives `E0055`.

```
fn t.long() -> u32 uses [] panics never
    let a = 1u32
    let b = 2u32
    let ab = a.wrapping_add(b)
    ab.wrapping_mul(3u32).wrapping_add(4u32)
end t.long
```

### Converting the error type with `?` (`.map_err(function)?`)
`?` works only when the error types match exactly, or when your own error enum has `derive [From]` and a variant whose only payload is the original type. Otherwise convert with a named function. `?` on `Option` has no conversion.

```
enum t.Error derive [Debug, Clone, Copy, PartialEq, From] pub
    Alloc(alloc.alloc.AllocError)
end t.Error

fn t.conv(e: alloc.alloc.AllocError) -> t.Error uses [] panics never
    t.Error.Alloc(e)
end t.conv

fn t.main() -> Result<(), t.Error> uses [alloc] panics never
    let s = std.string.String.from_str("hello").map_err(t.conv)?
    let r = s.as_str()
    core.mem.drop(r)
    core.mem.drop(s)
    Ok(())
end t.main
```
`t.conv` wraps `alloc.alloc.AllocError` as `t.Error.Alloc(e)`, and `.map_err(t.conv)?` connects it to `?`.

### Linear values (E3003 / E3006)
A type declared `linear` is consumed exactly once on every path out of its scope.

```ksnl fragment
type t.R linear
    n: u32
end t.R

fn t.main() -> () uses [] panics never
    let r = t.R { n: 1u32 }
end t.main
```
The above is `E3003` (`r` is not consumed). Ways to make it pass: pass it to a function that takes it by value, `return` it, destructure it, a `move` capture, or `close`.

```
type t.R linear
    n: u32
end t.R

fn t.main() -> () uses [] panics never
    let r = t.R {
        n: 1u32
    }
    core.mem.drop(t.take(r))
end t.main

fn t.take(r: t.R) -> u32 uses [] panics never
    match r {
        t.R {
            n: n
        } => n
    }
end t.take
```

`E3006` is when a linear value is passed to a type parameter not declared `linear` (such as `core.mem.drop(r)`). With `fn t.id[T: linear](x: T) -> T ...` and `[T: linear]`, it can be passed.

### Module locations and `import`
Sources are `src/<module>.ksnl` (`t.util` is `src/t/util.ksnl`). Gather `import t.util` at the top of the file and call with the short name `util.double(..)` in the body (the target must be `pub`).

```ksnl fragment
import t.util

fn t.main() -> u32 uses [] panics never
    util.double(21u32)
end t.main
```
`src/t/util.ksnl` is `fn t.util.double(n: u32) -> u32 uses [] panics never pub ... end t.util.double`.

### Closing a test (`end <FQN>`)
Start with `test <FQN>` and close with `end <FQN>`. It is checked and run together by `ksn check` / `ksn test`.

```ksnl fragment
test t.main.test_true
    core.check.eq(1u32, 1u32)
end t.main.test_true
```

### Reserved words (cannot be used as variable names)
`fn type enum trait impl const static extern test end import let mut if else match loop while for in break continue return true false self Self pub uses panics unsafe move with dyn blanket never may async await where ref as` and the names of primitive types. `stack depth derive linear layout requires capture link impls close is` can be used as identifiers (`close` is special only in the position of `with`).

### Passing a `'static` string
A string literal is `'static`, so it can be passed as it is to a function that takes `&'static str`. A local borrow is `E2037`.

```
fn t.echo(s: &'static str) -> &'static str uses [] panics never
    s
end t.echo

fn t.main() -> () uses [] panics never
    let s = t.echo("hi")
    core.mem.drop(s)
end t.main
```

### What Kasane does not have
- The `as` cast (`E0042`: use `u64.from_u8(x)` / `u8.try_from_u32(x)` / `c.to_u32()`)
- The `<<` / `>>` operators (`E0044`: `x.wrapping_shl(n)` / `x.wrapping_shr(n)`)
- `core.cmp.max` (`E1101`: there are per-type methods such as `1u32.max(2u32)`)
- Type aliases (`type t.Alias = u32` is a grammar error: `type` is only for declaring structs and enums)
- Tuple index access `p.0` (`E0002`: destructure with `let (a, b) = p`)

### What does not yet lower to native (`--release`)
Things where `ksn build --release` stops with `E9040` (as of 2026-11-23): `std.thread.sleep` / `std.time.Instant` / `std.time.Duration` and `core.sync.Atomic*` in a `static` (`new` only with a literal initial value) can now be lowered. On wasm32, the monotonic clock and `sleep` are unsupported (they fail with a reason). Other things that do not lower: `stdout().flush()` on a connected stdout, `core.ptr.add_const`, `derive [ToJson]` for anything other than int/bool, `dyn` with type arguments, and a user's `Display`. A debug build falls back, with a reason, to the bundle with the interpreter.

- A manual `impl core.marker.Send` / `Sync` can be written only in the module that defines the type (and in `[unsafe] modules`). In another module, or for a standard type, it is E5010, and for a type where it holds automatically, E5011.
- The bound `T: core.marker.Send` of `Send` / `Sync` is checked on the caller's side (E2030). A manual impl writes the bound, as in `impl[T: core.marker.Send] core.marker.Send for Type<T>` (E5008; inside a module with unsafe permission, and only for your own type).

## Glossary of Kasane-specific terms

| Japanese | English used in this document |
| --- | --- |
| 木走査 | tree walker (the interpreter that walks the syntax tree directly) |
| 効果 | effects (`uses` / `panics`) |
| 線形型 | linear types |
| 書き方は 1 通り | one canonical form |
| 下ろす | lower (translate to KIR / native code) |
| 検査器 | checker |
| 取っ手 | handle |
| アリーナ | arena |
| 二級の参照 / 二級の値 | second-class references / values |
| 中断点 | suspension point |
| 実行器 | executor |
| 退避 | fallback (e.g. to the bundle) |
| バンドル | bundle (a portable package that includes the interpreter) |
