[日本語](../tutorial.md) | English

# Kasane Tutorial

In this tutorial you will build three basic Kasane programs.

1. A Hello program that prints text to standard output
2. A program that reads a file and counts how many times each word occurs, using a `HashMap`
3. A small HTTP server that uses `std.http` and handles a single round trip

The source of each program is placed as a real package in `docs/tutorial/1`, `docs/tutorial/2` and `docs/tutorial/3`, next to this document. Run the `ksn` commands below in the directory that contains the target's `kasane.toml`.

## 1. Hello

### Package

```text
docs/tutorial/1/
├── kasane.lock
├── kasane.toml
└── src/
    └── hello.ksnl
```

The `entry` in `kasane.toml` is `hello.main`, so the entry point is `hello.main`.

### Source

The contents of `src/hello.ksnl` are as follows.

```kasane
fn hello.main() -> Result<(), std.io.Error> uses [io.console] panics never
    std.io.print_line("こんにちは、Kasane!")?
    Ok(())
end hello.main
```

The function header expresses the return value, the effects, and the panic contract. This example uses only console output, so it declares `uses [io.console]`, and it does not panic, so it declares `panics never`. The `?` in the body is an operator that returns `Err` if the `Result` of the standard output fails.

### Run

```bash
cd docs/tutorial/1
ksn fmt
ksn check
ksn run
```

`ksn fmt` formats the source into the canonical form. `ksn check` confirms there are 0 diagnostics and prints nothing. The output of `ksn run` is as follows.

```text
こんにちは、Kasane!
```

## 2. Read a file and count the occurrences of each word

### Package

```text
docs/tutorial/2/
├── kasane.lock
├── kasane.toml
├── sample.txt
└── src/
    └── wordcount.ksnl
```

The input file `sample.txt` is as follows.

```text
Kasane is a small systems language.
Kasane checks effects and borrows.
Language design matters.
```

### Source

The contents of `src/wordcount.ksnl` are as follows.

```kasane
enum wordcount.Error derive [Debug, Clone, Copy, PartialEq, Eq, From] pub
    Alloc(alloc.alloc.AllocError)
    Io(std.io.Error)
    Utf8(core.str.Utf8Error)
end wordcount.Error

fn wordcount.add_word(counts: &mut std.collections.HashMap<std.string.String, usize>, word: &str) -> Result<(), wordcount.Error> uses [alloc] panics never
    match counts.get_mut_str(word) {
        Some(count) => {
            *count = count.wrapping_add(1usize)
            Ok(())
        }
        None => {
            let key = std.string.String.from_str(word)?
            match counts.insert(key, 1usize) {
                Ok(_) => Ok(())
                Err(error) => Err(wordcount.Error.Alloc(error))
            }
        }
    }
end wordcount.add_word

fn wordcount.count_text(text: &str) -> Result<std.collections.HashMap<std.string.String, usize>, wordcount.Error> uses [alloc] panics never
    let mut counts = std.collections.HashMap.new()
    let mut word = std.string.String.new()
    for c in text.chars() {
        if c.is_ascii_whitespace() {
            if !word.is_empty() {
                wordcount.add_word(&mut counts, word.as_str())?
                word.clear()
            }
        } else {
            word.push(c)?
        }
    }
    if !word.is_empty() {
        wordcount.add_word(&mut counts, word.as_str())?
    }
    Ok(counts)
end wordcount.count_text

fn wordcount.main() -> Result<(), wordcount.Error> uses [alloc, io.console, io.fs] panics never
    let text = wordcount.read_file("sample.txt")?
    let counts = wordcount.count_text(text.as_str())?
    wordcount.print_count(&counts, "Kasane")?
    wordcount.print_count(&counts, "checks")?
    wordcount.print_count(&counts, "Language")?
    let mut line = std.string.String.new()
    line.push_str("unique words: ")?
    line.push_u64(u64.from_usize(counts.len()))?
    std.io.print_line(line.as_str())?
    Ok(())
end wordcount.main

fn wordcount.print_count(counts: &std.collections.HashMap<std.string.String, usize>, word: &str) -> Result<(), wordcount.Error> uses [alloc, io.console] panics never
    let count = match counts.get_str(word) {
        Some(value) => *value
        None => 0usize
    }
    let mut line = std.string.String.new()
    line.push_str(word)?
    line.push_str(": ")?
    line.push_u64(u64.from_usize(count))?
    std.io.print_line(line.as_str())?
    Ok(())
end wordcount.print_count

fn wordcount.read_file(path: &str) -> Result<std.string.String, wordcount.Error> uses [alloc, io.fs] panics never
    let mut data = std.vec.Vec.new()
    with mut file = std.fs.open(path)? close core.mem.drop(file.close()) {
        core.mem.drop(std.fs.read_to_end(&mut file, &mut data)?)
    }
    let mut text = std.string.String.new()
    text.push_str(core.str.from_utf8(data.as_slice())?)?
    Ok(text)
end wordcount.read_file
```

### Run

```bash
cd docs/tutorial/2
ksn fmt
ksn check
ksn run
```

The output is as follows.

```text
Kasane: 2
checks: 1
Language: 1
unique words: 13
```

### How it works

`read_file` opens the file with `std.fs.open` and reads the bytes into a `Vec` with `std.fs.read_to_end`. It validates them as UTF-8 with `core.str.from_utf8` and then moves them into a `String`.

`count_text` looks at the characters from the beginning and splits words at ASCII whitespace. It appends one character at a time to a `String` with `push`, and `clear`s it at each word boundary. `str.split_whitespace` is not provided, so this example uses `char.is_ascii_whitespace`.

The type of the table is `HashMap<String, usize>`. Keys are stored as owned `String`s. Use `get_str`, which does not create a new owned value, for lookups only, and `get_mut_str` for updates. Using `get` or `get_mut` directly on a dictionary with `String` keys is not allowed.

`core.mem.drop` is an operator that explicitly consumes and discards a non-linear value. In `read_file`, `file.close()` consumes the linear file by value and closes it, and its `Result` is discarded with `core.mem.drop`. The number of bytes read returned by `std.fs.read_to_end` is also discarded with `core.mem.drop`. With `with ... close`, the specified cleanup is run when the body is left through normal control flow (it is not run on a panic).

## 3. A small HTTP server

### Package

```text
docs/tutorial/3/
├── kasane.lock
├── kasane.toml
└── src/
    └── httpserver.ksnl
```

### Source

The contents of `src/httpserver.ksnl` are as follows.

```kasane
enum httpserver.Error derive [Debug, Clone, Copy, PartialEq, Eq, From] pub
    Alloc(alloc.alloc.AllocError)
    Http(std.http.Error)
    Io(std.io.Error)
    Net(std.net.Error)
end httpserver.Error

fn httpserver.address(port: u16) -> Result<std.string.String, httpserver.Error> uses [alloc] panics never
    let mut address = std.string.String.new()
    address.push_str("127.0.0.1:")?
    address.push_u64(u64.from_u16(port))?
    Ok(address)
end httpserver.address

fn httpserver.exchange(listener: &std.net.TcpListener) -> Result<(), httpserver.Error> uses [alloc, io.console, io.net] panics never
    let port = listener.local_port()?
    let address = httpserver.address(port)?
    with mut client = std.net.connect(address.as_str())? close core.mem.drop(client.close()) {
        with mut server = listener.accept()? close core.mem.drop(server.close()) {
            let request_bytes = "GET /hello HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
            std.http.write_all(&mut client, request_bytes.as_bytes())?
            let mut decoder = std.http.Decoder.new(8192usize, 65_536usize)
            match std.http.read_request(&mut server, &mut decoder)? {
                Some(request) => {
                    std.io.print_line(request.method.as_str())?
                    std.io.print_line(request.path.as_str())?
                    httpserver.send_response(&mut server, &request)?
                }
                None => return Err(httpserver.Error.Http(std.http.Error.Truncated))
            }
        }
    }
    Ok(())
end httpserver.exchange

fn httpserver.main() -> Result<(), httpserver.Error> uses [alloc, io.console, io.net] panics never
    with listener = std.net.listen("127.0.0.1:0")? close core.mem.drop(listener.close()) {
        httpserver.exchange(&listener)?
    }
    std.io.print_line("200 OK")?
    Ok(())
end httpserver.main

fn httpserver.send_response(stream: &mut std.net.TcpStream, request: &std.http.Request) -> Result<(), httpserver.Error> uses [alloc, io.net] panics never
    let body = "Hello, Kasane!\n"
    let head = "Content-Type: text/plain; charset=utf-8\r\n"
    let bytes = std.http.render(200u16, head, body.as_bytes(), request.keep_alive())?
    std.http.write_all(stream, bytes.as_slice())?
    Ok(())
end httpserver.send_response
```

### Run

```bash
cd docs/tutorial/3
ksn fmt
ksn check
ksn run
```

The output is as follows.

```text
GET
/hello
200 OK
```

### How it works

`std.net.listen` creates a listening TCP listener. If you specify `0` as the port number of the address, a free port is chosen. You get that number with `local_port`, and a client in the same program connects to it.

On the server side, prepare a `std.http.Decoder` and parse the request with `std.http.read_request`. Print the parsed `request.method` and `request.path`, build the response with `std.http.render`, and send it with `std.http.write_all`.

This example uses no external HTTP client and no infinite wait: the listener and the client make one round trip inside the same program. So `ksn run` finishes without waiting for external input.

`httpserver.Error` keeps `std.http.Error` and `std.net.Error` separately, and `derive [From]` performs the error conversion for `?`. Even when there are several kinds of errors, you state the convertible types explicitly with `From`.

## When writing Kasane

### Definitions end with the FQN. Leave the order to `ksn fmt`

Every definition is written with a fully qualified name (FQN) such as `fn name.function` or `type name.type`, and is closed with `end` followed by the same FQN. The order of definitions has no meaning. `ksn fmt` sorts definitions in FQN order and also fixes the common form of bodies, blank lines and so on.

### Do not use `.0` on tuple elements

Kasane has no positional field access like `t.0`. Destructure a tuple and take the elements out as named values. Instead of `t.0`, write `let (first, second) = t`.

### References are second-class values

`&T` and `&mut T` are second-class values that borrow an existing value only for a short time. You can pass them to functions and hold them temporarily in local variables, but you cannot store them in type fields, containers or channels. For values you want to keep for a long time, use owned values such as `String`. This rule is also why the `HashMap` above uses `String` as its key.

### The declaration of effects and panics must match the body exactly

`uses [...]` expresses effects such as I/O and allocation, and `panics ...` expresses whether the function may panic. A declaration must match the body exactly, and declarations are themselves checked. Omissions and excessive declarations are also errors. When it is determined uniquely from the body, `ksn fix` can complete or rewrite the `uses` and `panics` declarations.

### Give integer literals a suffix that states the type

State the type of an integer literal with a suffix, as in `1u32`, `1u64`, `1usize` and `200u16`. If you omit the suffix on an integer literal whose type could be inferred, `ksn check` gives E2028. When it can be inferred, `ksn fix` can add the suffix. `ksn fmt` separates decimal numbers of five or more digits with `_` every three digits.

### Linear values must always be consumed

A linear value (a type declared `linear`, or a type that contains a linear value) is an error if it leaves the scope without being consumed. To consume it, pass it to a function that takes `self` by value, destructure it and take the values, or pass it to the owner's `with` as cleanup. `core.mem.drop` is only for non-linear values, so it is not used to consume linear values.

### Use `ksn fmt` and `ksn check` together

`ksn fmt` fixes variations in how things are written, and `ksn check` checks for semantic errors. `ksn fmt --check` gives E0020 if the file is not in the canonical form. For newly written code, check in this order: first `ksn fmt`, then `ksn check`, and finally `ksn run`.

## 4. Calling C functions (`extern` + `link`, `layout c` structs, callbacks)

To call a C shared library from Kasane, write `link "<path>:<symbol>"` in an `extern` definition and call it inside an `unsafe` block. A struct declares the same memory layout as C with `layout c`, and a Kasane function can be passed as a callback to a C function that takes a function pointer.

The real package is in `examples/tutorial_ffi/`.

### Package

```text
examples/tutorial_ffi/
├── kasane.lock        (version = 1)
├── kasane.toml        ([unsafe] modules = ["tutorial_ffi"])
├── build.sh           (cc -shared -fPIC -o libffi.so c/ffi.c)
├── c/
│   └── ffi.c          (rect_area, rect_from, rect_grow, rect_each_vertex)
└── src/
    ├── tutorial_ffi.ksnh  (コメント付き。正のソース)
    └── tutorial_ffi.ksnl  (`ksn strip` で生成。コメント無しの正規形)
```

(The parenthesized notes in the tree above are in Japanese: `ksn strip` generates the comment-free canonical `.ksnl` from the commented `.ksnh`, which is the source of truth; `kasane.toml` has `[unsafe] modules = ["tutorial_ffi"]`.)

List the module name in `[unsafe] modules` of `kasane.toml` (a module that uses `extern` / `unsafe` / `core.ptr` must be permitted here).

### Source (main parts)

The C side (`c/ffi.c`) passes and returns the `Rect` / `Point` structs by value, and calls a callback.

```c
typedef struct { double x; double y; } Point;
typedef struct { Point min; Point max; } Rect;
double rect_area(Rect r) { return (r.max.x - r.min.x) * (r.max.y - r.min.y); }
Rect rect_from(Point a, Point b) { Rect r = { a, b }; return r; }
void rect_grow(Rect *r, double d) { ... }
typedef void (*vertex_fn)(double x, double y, void *user);
void rect_each_vertex(Rect r, vertex_fn f, void *user) { ... }
```

Build `libffi.so` with `build.sh` (`ksn run` / `ksn test` run with this directory as the working directory, so `link "./libffi.so:..."` can be resolved by a relative path).

On the Kasane side, define a `layout c` struct and link with `extern`.

```ksnl fragment
type tutorial_ffi.Rect layout c derive [Clone, Copy]
    min: tutorial_ffi.Point
    max: tutorial_ffi.Point
end tutorial_ffi.Rect

extern tutorial_ffi.rect_area(r: tutorial_ffi.Rect) -> f64 link "./libffi.so:rect_area"
extern tutorial_ffi.rect_grow(r: *mut tutorial_ffi.Rect, d: f64) -> () link "./libffi.so:rect_grow"
extern tutorial_ffi.rect_each_vertex(r: tutorial_ffi.Rect, f: fn(f64, f64, *mut tutorial_ffi.Acc) -> () uses [] panics never, user: *mut tutorial_ffi.Acc) -> () link "./libffi.so:rect_each_vertex"
```

Take a raw pointer to a local struct with `core.ptr.from_mut`, and read and write the callback's user data with `core.ptr.read` / `core.ptr.write`. Keep all of this inside `unsafe "ffi" "..."` blocks.

### Run

```bash
cd examples/tutorial_ffi
sh build.sh
ksn strip
ksn fmt
ksn fmt --check
ksn check
ksn test --both
```

The output (`ksn test --both`) is as follows.

```text
test tutorial_ffi.area.test_area ... ok (both: llvm)
test tutorial_ffi.vertex_stats.test_vertices ... ok (both: llvm)

2 passed; 0 failed
```

`ksn run` also prints `tutorial_ffi ok` and exits with code 0 (this assumes `libffi.so` is in the current directory).

### Notes

- Comments cannot be written in `.ksnl` (E0059). Write comments in `.ksnh` and generate the `.ksnl` with `ksn strip` (commit both).
- With a `./` prefix, as in `link "./libffi.so:..."`, the interpreter can `dlopen` a shared library in the current directory. A bare file name such as `link "libffi.so:..."` may not be resolved.

## 5. Generate bindings from a C header with `ksn bindgen --c`

`ksn bindgen --c <header.h>` parses a C header with libclang (opened only at run time with `dlopen`; it is not linked into the toolchain) and generates Kasane `extern` declarations and `pub fn` wrappers. The output is exactly in the canonical form of `ksn fmt`, and both `.ksnh` and `.ksnl` are written.

The real package is in `examples/tutorial_bindgen/`.

### Package

```text
examples/tutorial_bindgen/
├── kasane.lock   (version = 1)
├── kasane.toml   ([unsafe] modules = ["tutorial_bindgen"])
├── build.sh      (cc -shared -fPIC -o libdemo.so c/demo.c)
├── c/
│   ├── demo.h    (Color 列挙, Point 構造体, MAX_POINTS 定数, area/make_point/for_each)
│   └── demo.c
└── src/
    └── tutorial_bindgen.ksnl  (bindgen の生成物 + main)
```

### Generate

```bash
cd examples/tutorial_bindgen
sh build.sh
ksn bindgen --c c/demo.h --prefix tutorial_bindgen --link libdemo.so -o src/tutorial_bindgen.ksnh
```

An example of the generated output (part of `src/tutorial_bindgen.ksnh`):

```ksnl fragment
const tutorial_bindgen.RED: u32 = 0u32 pub
const tutorial_bindgen.MAX_POINTS: i32 = 100i32 pub

type tutorial_bindgen.Point layout c derive [Clone, Copy] pub
    x: f64
    y: f64
end tutorial_bindgen.Point

extern tutorial_bindgen.raw_area(p: tutorial_bindgen.Point) -> f64 link "./libdemo.so:area"
extern tutorial_bindgen.raw_make_point(x: f64, y: f64) -> tutorial_bindgen.Point link "./libdemo.so:make_point"

fn tutorial_bindgen.area(p: tutorial_bindgen.Point) -> f64 uses [] panics may pub
    unsafe "bindgen" "C の関数 area を呼ぶ" {
        tutorial_bindgen.raw_area(p)
    }
end tutorial_bindgen.area
```

An `extern` is visible only from inside its module, so it gets a `raw_` prefix, and a `pub fn` wrapper (wrapped in `unsafe "bindgen" "..."`) makes it callable from outside. Integer constants from `#define` become `const`s of the smallest integer type, enums become a `const` for each element, structs become `layout c`, and function pointers become `fn(..) -> ..`. Bit-fields, `union`, variadic arguments, `static`/`inline` functions and the like are skipped, and the reason is left in a comment at the top.

After generation, add `main` to the same module and put the package name (= the prefix) in `[unsafe] modules`.

### Run

```bash
cd examples/tutorial_bindgen
ksn fmt --check
ksn check
ksn test --both
ksn run
```

Output:

```text
tutorial_bindgen ok
```

### Notes

- Put the output path at `src/<module>.ksnh`, which corresponds to the module name (a notice is shown if it differs).
- The generated output is in the form of `ksn fmt`, so `ksn fmt --check` passes as it is. Specify the library name to link with `--link`, and prepare that shared library with `build.sh` (place it in the current directory, because `ksn run` / `ksn test` `dlopen` it).
- `ksn bindgen` does not support C++ headers, `long double` / `_Complex`, or nested anonymous struct/union (see "remaining" in section 2.10dj of 実装計画.md).

## 6. Cross builds (`x86_64-windows-gnu` / `aarch64-linux-gnu` / `wasm32-wasi`)

With `ksn build --target <triple>` you can build a native executable for a target different from the host. The output goes to `target/<triple>/<debug|release>/`, and no bundle is created.

The real package is in `examples/tutorial_cross/` (a simple program that prints "tutorial_cross ok").

### Supported targets

`x86_64-linux-gnu`, `aarch64-linux-gnu`, `x86_64-windows-gnu`, `wasm32-unknown-unknown`, `wasm32-wasi` (omitted means the current machine).

### Running and results

```bash
cd examples/tutorial_cross
ksn fmt --check
ksn check
ksn build --target x86_64-windows-gnu
ksn build --target aarch64-linux-gnu
ksn build --target wasm32-wasi
```

Example output for each target:

```text
native executable: target/x86_64-windows-gnu/debug/tutorial_cross.exe
built target/x86_64-windows-gnu/debug (debug, target x86_64-windows-gnu; cross-compiled, not run here)
native executable: target/aarch64-linux-gnu/debug/tutorial_cross
built target/aarch64-linux-gnu/debug (debug, target aarch64-linux-gnu; cross-compiled, not run here)
native executable: target/wasm32-wasi/debug/tutorial_cross.wasm
built target/wasm32-wasi/debug (debug, target wasm32-wasi; cross-compiled, not run here)
```

### What is needed to run (as far as verified on this machine)

- `aarch64-linux-gnu`: it can run if you have the sysroot at `/usr/aarch64-linux-gnu` and `qemu-aarch64`.

  ```bash
  qemu-aarch64 -L /usr/aarch64-linux-gnu target/aarch64-linux-gnu/debug/tutorial_cross
  # 出力: tutorial_cross ok（終了コード 0）
  ```

- `x86_64-windows-gnu`: you can build an `.exe` (PE32+) with the built-in lld (MinGW) and the sysroot at `/usr/x86_64-w64-mingw32`. Running it needs `wine64`, but `wine64` is not installed on this machine, so only the build has been verified (running is unverified).

- `wasm32-wasi`: you can build a `.wasm`. No WASI runtime (such as `wasmtime`) is installed on this machine, so only the build has been verified (running is unverified).

### Notes

- `aarch64-linux-gnu` uses the C development files (`/usr/aarch64-linux-gnu`), which can be overridden with `KSN_SYSROOT`.
- `x86_64-windows-gnu` needs `gcc-mingw-w64-x86-64-posix` and `mingw-w64-x86-64-dev` (already installed here).
- `wasm32` and `--backend c` cannot have C libraries, so specifying `[native]` gives E9040.
- The `ffi` example (which looks up `libc.so.6`) does not work on Windows (see "unsupported" in section 2.10dg of 実装計画.md).

## 7. A first step with the UI packages (layout + tree + event, no GPU)

There are UI packages written in Kasane under `ui/`. This section touches only three of them, `ui.layout` (layout), `ui.tree` (element tree and diff updates) and `ui.event` (event delivery and hit testing), and does not cover GPU rendering. For details, see `ui/kasane/<name>/README.md` of each package.

The real package is in `examples/tutorial_ui/` (it uses the three packages in `ui/` as `path` dependencies).

### Package

```text
examples/tutorial_ui/
├── kasane.lock   (version = 1、path 依存を [[package]] で記録)
├── kasane.toml   ([dependencies] に ui_layout / ui_tree / ui_event)
└── src/
    └── tutorial_ui.ksnl
```

`kasane.toml` specifies the dependencies by relative paths to `ui/kasane/`:

```toml
[dependencies]
ui_layout = { path = "../../ui/kasane/layout" }
ui_tree   = { path = "../../ui/kasane/tree" }
ui_event  = { path = "../../ui/kasane/event" }
```

`ksn update` writes `[[package]]` entries to `kasane.lock` (still `version = 1`, in the `source = "path+..."` form).

### Source (main parts)

```ksnl fragment
fn tutorial_ui.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let mut tree = ui.layout.Tree.new()
    let root_style = ui.layout.Style.default()
    let root = tree.push_node(root_style)
    let child_style = ui.layout.Style.default()
    let c = tree.push_node(child_style)
    tree.push_child(root, c)
    tree.layout(root, 300.0f32, 200.0f32)
    let rect = tree.rect(c)
    core.mem.drop(rect)

    let mut utree = ui.tree.Tree.new()
    let root2 = utree.push_node(ui.tree.ViewKind.Block, ui.layout.Style.default(), None).unwrap()
    let child2 = utree.push_node(ui.tree.ViewKind.Block, ui.layout.Style.default(), None).unwrap()
    core.mem.drop(utree.push_child(root2, child2))
    core.check.eq(utree.child_count(root2), 1usize)

    let target = ui.event.hit_test(&tree, root, 150.0f32, 150.0f32)
    core.mem.drop(target)

    std.io.print_line("tutorial_ui ok")?
    Ok(())
end tutorial_ui.main
```

The fields of `ui.layout.Style` / `Size` are private to the module, so from outside you use defaults such as `Style.default()`, build the tree with `push_node` / `push_child` / `layout` / `rect`, and read the rectangles. `ui.tree.Tree` builds an element tree with `push_node` / `push_child` / `child_count`, and `ui.event.hit_test` performs hit testing on the layout rectangles.

### Run

```bash
cd examples/tutorial_ui
ksn fmt --check
ksn check
ksn test --both
ksn run
```

Output:

```text
tutorial_ui ok
```

`ksn test --both` also passes the internal tests of the `ui.layout` / `ui.tree` / `ui.event` packages (16 / 13 / 15 tests respectively) with 0 failures.

### Notes

- For all three packages, `ksn test --both` confirms that the interpreter and the native bundle agree (some parts that cannot be lowered to KIR, such as floating-point comparisons and `dyn.call`, fall back to running on the interpreter / bundle).
- GPU rendering and window backends are not included in these three packages. The `native/` directory in `ui/` and anything GPU-related are separate roadmap items.
- For the design and the full API of each package, see `ui/kasane/layout/README.md`, `ui/kasane/tree/README.md` and `ui/kasane/event/README.md`.



## 8. Sharing data without `Rc` / `Arc` (`alloc.arena` and `Handle<T>`)

Kasane has no `Rc` / `Arc`. When you want to share the same value from several places, put it in an `alloc.arena.Arena<T, A>` and pass around a handle `Handle<T>` (a `Copy` value that holds only an index and a generation, and that only the `Arena` side can create). Elements are placed in the slots of one buffer, and indices do not move even on `grow`, so a handle stays valid while the value is borrowed.

Create a handle with `insert`, borrow with `get` / `get_mut`, and discard with `remove`. A handle that has been `remove`d behaves like a "weak reference", and `get` returns `None`. When a slot is reused, its generation goes up by one, so an old handle does not match, and is distinguished from the handle created by the later `insert`.

```kasane
fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let mut a = alloc.arena.Arena.new(std.alloc.SYSTEM)
    core.check.is_true(a.is_empty())
    core.check.eq(a.len(), 0usize)
    let h0 = a.insert(1u32).map_err(t.oom)?
    let h1 = a.insert(2u32).map_err(t.oom)?
    core.check.eq(a.len(), 2usize)
    core.check.is_true(a.contains(h0))
    core.check.eq(a.get(h0), Some(&1u32))
    let copied = h0
    core.check.eq(a.get(copied), Some(&1u32))
    core.check.is_true(h0 == copied)
    match a.get_mut(h1) {
        Some(v) => core.mem.drop(core.mem.replace(v, 20u32))
        None => core.panic.panic("no h1")
    }
    core.check.eq(a.get(h1), Some(&20u32))
    core.check.eq(a.remove(h1), Some(20u32))
    core.check.is_true(a.get(h1).is_none())
    core.check.is_true(!a.contains(h1))
    core.check.eq(a.len(), 1usize)
    let h2 = a.insert(3u32).map_err(t.oom)?
    core.check.is_true(h2 != h1)
    core.check.eq(a.get(h2), Some(&3u32))
    core.mem.drop(std.io.print_line("arena ok"))
    Ok(())
end t.main

fn t.oom(_e: alloc.alloc.AllocError) -> std.io.Error uses [] panics never
    std.io.Error {
        kind: std.io.ErrorKind.OutOfMemory
        code: 0i32
    }
end t.oom
```

After `h1` is `remove`d, `3u32` goes into the same slot and `h2 != h1` (the generations differ). `a.get(h1)` is `None`, and an old handle does not hit a reused slot. A handle is `Copy`, so you can put it in a `Vec` and pass it around, and `T: linear` or a type that carries references cannot be put in an `Arena`. A handle holds only (index, generation), so it would match a handle with the same (index, generation) of a different `Arena` (a handle of a different type is E2023).

## 9. Sets and range iteration (`HashSet` and `BtreeMap.range`)

`HashSet<T>` has `new`, `insert`, `remove`, `contains`, `len`, `is_empty` and `iter`, and when the element is a `String`, you pass a `&str`, as in `contains_str` / `remove_str` (looking up with `&String` is E2054). `BtreeMap<K, V>` iterates in key order with `iter`, takes the half-open interval `[lo, hi)` with `range(&lo, &hi)`, and takes the two ends with `first` / `last`.

```kasane
fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let mut s = alloc.collections.HashSet.new(std.alloc.SYSTEM)
    core.check.eq(s.is_empty(), true)
    core.check.eq(s.insert(7u32), Ok(true))
    core.check.eq(s.insert(7u32), Ok(false))
    core.check.eq(s.len(), 2usize)
    core.check.eq(s.contains(&7u32), true)
    core.check.eq(s.remove(&7u32), true)
    core.check.eq(s.remove(&7u32), false)
    core.mem.drop(std.io.print_line("hashset ok"))
    Ok(())
end t.main
```

```kasane
fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let mut b = std.collections.BtreeMap.new()
    core.mem.drop(b.insert(1u32, 10u32))
    core.mem.drop(b.insert(2u32, 20u32))
    core.mem.drop(b.insert(3u32, 30u32))
    core.mem.drop(b.insert(5u32, 50u32))
    let mut sum = 0u32
    for (_k, v) in b.range(&2u32, &5u32) {
        sum = sum.wrapping_add(*v)
    }
    core.check.eq(sum, 50u32)
    match b.first() {
        Some((k, v)) => {
            core.check.eq(*k, 1u32)
            core.check.eq(*v, 10u32)
        }
        None => core.panic.panic("no first")
    }
    core.mem.drop(std.io.print_line("btree ok"))
    Ok(())
end t.main
```

## 10. Passing user data to a C callback (`core.ffi.trampoline`)

If a C API passes `void *user` around, as in `void (*cb)(void *user, int x)`, declare the same raw pointer type (such as `*mut t.Acc`) on both sides of the `extern`, and on the Kasane side pass a pointer to the user data with `core.ptr.from_mut(&mut acc)`. To pass a closure that has captures as a C function pointer, use `core.ffi.trampoline(&mut cb)`, and pass the returned `(fn(*mut (), A) -> R, *mut ())` to the C callback and `user` (`spec/stdlib/core/ffi.sig`).

```ksnl fragment
type t.Acc
    total: i32
    count: i32
end t.Acc

extern t.acc_ticks(f: fn(*mut (), i32) -> () uses [] panics never, user: *mut (), lo: i32, hi: i32) -> () link "LIB:acc_ticks"

extern t.acc_twice(f: fn(*mut (), i32) -> () uses [] panics never, user: *mut (), a: i32, b: i32) -> () link "LIB:acc_twice"

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let mut acc = t.Acc { total: 0i32, count: 0i32 }
    let mut cb = fn(x: i32) -> () capture [&mut acc] {
        acc.total = acc.total.wrapping_add(x)
        acc.count = acc.count.wrapping_add(1i32)
    }
    let (f, user) = unsafe "ffi" "trampoline for the closure" {
        core.ffi.trampoline(&mut cb)
    }
    unsafe "ffi" "accumulate through the C callback" {
        t.acc_ticks(f, user, 1i32, 10i32)
    }
    core.check.eq(acc.total, 55i32)
    core.check.eq(acc.count, 10i32)
    unsafe "ffi" "two more calls" {
        t.acc_twice(f, user, 5i32, 7i32)
    }
    core.check.eq(acc.total, 67i32)
    core.check.eq(acc.count, 12i32)
    core.mem.drop(std.io.print_line("userdata ok"))?
    Ok(())
end t.main
```

Linking needs `[unsafe] modules = ["t"]` in `kasane.toml` (8.1) and `link "LIB:acc_ticks"` (`dlopen` at run time), and on the C side, `acc_ticks` / `acc_twice` (the user's data is only passed through `user`) are made into a shared library.

Two limitations:
1. The only things you can pass natively as the actual argument of an `extern` callback are a plain `fn` (a named function, or a closure with `capture []`); you cannot pass a closure value itself as a C function pointer (E5005). To pass a closure that has captures to C, create a pair of a function pointer and `*mut ()` user data with `core.ffi.trampoline`.
2. You must not hold or refer to the `user` pointer that C received after the call to the `extern` returns (its lifetime is only during the call into C. If needed, move it to the heap).
