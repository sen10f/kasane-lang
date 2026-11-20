# ui.event — Kasane event handling package

Package `ui_event` provides Z8 event infrastructure for Kasane UI: an event enum,
hit testing over layout rects, focus management with tab order, pointer capture,
event routing, and scripted replay.

## Module: `ui.event`

### Types

**`ui.event.Modifiers`** — keyboard modifier state. Fields are public so callers
can construct them directly.

- `shift: bool` — Shift key held
- `ctrl: bool` — Control key held
- `alt: bool` — Alt key held

**`ui.event.Event`** — input/window events.

| Variant | Meaning |
|---------|---------|
| `PointerMove(f32, f32)` | pointer moved to (x, y) |
| `PointerDown(f32, f32, u8)` | pointer button pressed at (x, y), button id |
| `PointerUp(f32, f32, u8)` | pointer button released at (x, y), button id |
| `Wheel(f32, f32, f32, f32)` | wheel scroll (x, y, dx, dy) |
| `KeyDown(u32, ui.event.Modifiers)` | key pressed, key code + modifiers |
| `KeyUp(u32, ui.event.Modifiers)` | key released |
| `TextInput(Vec<u8>)` | UTF-8 text input |
| `IMEStart` | IME composition started |
| `IMEUpdate(Vec<u8>)` | IME composition updated |
| `IMECommit(Vec<u8>)` | IME committed text |
| `FocusGained` | focus gained |
| `FocusLost` | focus lost |
| `Resize(f32, f32)` | window resized to (w, h) |
| `DPIChange(f32)` | DPI scale changed |
| `Quit` | application quit requested |

**`ui.event.Action`** — the return value of `View::on_event`. An enum, not a type.

| Variant | Meaning |
|---------|---------|
| `NoOp` | nothing to do |
| `Redraw` | redraw requested |
| `Focus(usize)` | move focus to node |
| `Capture(Option<usize>)` | set/release pointer capture |
| `Quit` | quit the application |

**`ui.event.Ctx`** — event context holding focus, capture, tab order, and text input
buffer.

### Functions

- **`ui.event.hit_test(layout, root, x, y) -> Option<usize>`** — find the topmost
  node at (x, y). Children are iterated in reverse (topmost first), so the last
  child drawn wins when rects overlap. Returns `None` if the point is outside the
  root rect.

- **`ui.event.route(ctx, layout, tree, root, event, x, y) -> Option<usize>`** —
  route an event to a target node. Pointer events go to the capture target (if
  set) or hit test result. Keyboard/IME events go to the focused node.
  `FocusGained` hit-tests when nothing is focused. `FocusLost`, `Resize`,
  `DPIChange`, `Quit` route nowhere (`None`).

- **`ui.event.replay(ctx, events, layout, tree, root, x, y) -> ()`** — replay a
  recorded event stream, updating capture on press/release and focus on Tab.
  Deterministic: the same stream yields the same final context.

- **`ui.event.rect_contains(rect, x, y) -> bool`** — point-in-rect test.

### `ui.event.Ctx` methods

| Method | Effect |
|--------|--------|
| `new() -> Ctx` | empty context |
| `set_focus(node)` | set focused node |
| `blur()` | clear focus |
| `focus_next()` | advance focus to next tab-order node (wraps) |
| `focus_previous()` | move focus to previous tab-order node (wraps) |
| `set_capture(node)` | grab pointer capture |
| `release_capture()` | release pointer capture |
| `get_focus() -> Option<usize>` | current focus |
| `get_capture() -> Option<usize>` | current capture target |
| `tab_push(node)` | append a node to tab order |

### `ui.event.View` trait

```kasane
trait ui.event.View pub
    fn on_event(self: &Self, event: &ui.event.Event, ctx: &mut ui.event.Ctx) -> ui.event.Action uses [] panics never
end ui.event.View
```

Implement `on_event` to handle events per view and return an `Action`.

## Building

This package depends on `ui_tree` (path `../tree`), which transitively depends
on `ui_layout`. `kasane.lock` is pinned to `version = 1`; do not run `ksn update`
on this package (it would regenerate the lock to version 2).

Tests are in `src/ui/event_test.ksnh` and cover hit testing (leaf, parent/child,
nested, topmost, outside), capture during drag, focus tab order and wrap, IME
commit routing to focus, key events with modifiers, replay determinism, and quit
routing.

Run tests:

```sh
ksn test --both
```

## License

Same as the Kasane project.
