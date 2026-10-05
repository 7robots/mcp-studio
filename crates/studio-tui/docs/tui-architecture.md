# The Studio TUI

`studio-tui` is a small component framework plus the modules built on it.
The shell (`framework::app::App`) owns the terminal layout and routing; each
module (Fleet, Pattern, Gateway, Marketplaces) is a `Component` that draws into
the area it is given and talks to its own async tasks through typed messages.
Nothing here touches the terminal except `framework::run::run`, so every module
is testable headlessly with `framework::harness::Harness`.

```
┌ MCP Studio  <instance display name>      1 Fleet  2 Pattern  3 Gateway  4 Marketplaces ┐  ← shell header
│                                                                                         │
│   the focused module's draw(area)                                                       │
│                                                                                         │
└ footer: module hints + shell hints (generated) | toast (whole line) | module status() ──┘
```

## Pieces

| Module | What it is |
|---|---|
| `framework::component` | `Component`, the trait a module implements; `Handled`; `ModuleId`. |
| `framework::cx` | `Cx`, handed to every event handler: `spawn`, `send`, `sender`, `toast`, `error`, `quit`, `focused`. `Msg` is the shell's envelope (`to: ModuleId`, boxed payload). |
| `framework::keymap` | `Keymap<A>`: one table that dispatches keys (`resolve`) **and** describes them (`hints`), so the footer and the `?` help can't drift from behaviour. |
| `framework::overlay` | Reusable modals: `Form`, `Choice`, `Picker`, `Confirm` (and `Confirm::typed` for type-the-id guards), wrapped in `Overlay` with `Step`/`Answer` results. |
| `framework::slot` | `Slot<T>`: an async-loaded value with a generation; stale results are dropped, last good data survives an error. |
| `framework::toast` | The status-line message (`TOAST_TTL`, longer `ERROR_TTL` for errors). |
| `framework::widgets` | `centered`, `message`, `slot_placeholder`, `stale_banner`. |
| `framework::app` | The shell: tab strip, number keys, footer, help, message routing, ticks. |
| `framework::harness` | Headless driver: `press`, `type_text`, `erase`, `until`, `until_text`, `settle`, `lines`, `text`, `row_with`. |
| `framework::run` | The crossterm loop (raw mode, alternate screen, events, channel, timers). |
| `gateway` | The Gateway module (one `Pane` per `[[gateway]]`) and its acceptance `gate`. |
| `modules::Placeholder` | "not wired yet" stand-in for a module slot. |
| `demo` | The Gateway module against an in-process `studio-fake` gateway. |

## Writing a module

```rust
use std::any::Any;
use crossterm::event::KeyEvent;
use ratatui::{Frame, layout::Rect, text::Line};
use studio_tui::{Component, Cx, Handled, Hint, Keymap, ModuleId};
use studio_tui::framework::{cx::Payload, slot::Slot};

pub struct Fleet { rows: Slot<Vec<String>> }

#[derive(Debug)]
enum FleetMsg { Loaded { generation: u64, result: Result<Vec<String>, String> } }

#[derive(Clone, Copy, Debug, PartialEq)]
enum K { Reload, Down }

impl Fleet {
    // One table per context: dispatch AND footer/help come from it.
    fn keys(&self) -> Keymap<K> {
        Keymap::new("Fleet")
            .bind(&["r"], "re-probe every server", K::Reload).footer("r", "refresh")
            .bind(&["j", "down"], "move down", K::Down).footer("j/k", "select")
    }

    fn load(&mut self, cx: &Cx<'_>) {
        let generation = self.rows.begin();
        // The future's output arrives in handle_msg.
        cx.spawn(async move {
            FleetMsg::Loaded { generation, result: probe_everything().await }
        });
    }
}

impl Component for Fleet {
    fn id(&self) -> ModuleId { "fleet" }               // messages are routed by this
    fn title(&self) -> String { "Fleet".into() }        // the tab strip label
    fn start(&mut self, cx: &mut Cx<'_>) { self.load(cx) } // first focus only

    fn handle_key(&mut self, key: KeyEvent, cx: &mut Cx<'_>) -> Handled {
        match self.keys().resolve(&key) {
            Some(K::Reload) => { self.load(cx); cx.toast("Refreshing"); }
            Some(K::Down) => { /* move selection */ }
            None => return Handled::No,                  // falls through to q/Esc = quit
        }
        Handled::Yes
    }

    fn handle_msg(&mut self, msg: Payload, cx: &mut Cx<'_>) {
        if let Ok(msg) = msg.downcast::<FleetMsg>() {
            let FleetMsg::Loaded { generation, result } = *msg;
            if let Some(err) = self.rows.finish(generation, result) {
                cx.error(format!("Fleet probe failed: {err}"));
            }
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) { /* ratatui widgets into area */ }
    fn keymap(&self) -> Vec<Hint> { self.keys().hints() }
}
```

### Registering it

`studio_tui::studio_app(title, gateway_module)` builds the standard shell: it
puts the modules in tab order (`1 Fleet, 2 Pattern, 3 Gateway, 4 Marketplaces`)
with placeholders for the unbuilt ones. To wire a module, replace its
`modules::Placeholder::new(...)` entry there with `Box::new(YourModule::new(..))`
(add a constructor argument to `studio_app` for whatever it needs, e.g. the
`Instance`). For a custom app, call `App::new(title, vec![Box::new(a), Box::new(b)])`
directly; a module's position is its number key. `App::focus_id(id)` picks the
first module shown.

### Messages and async work

- `cx.spawn(fut)` runs `fut` on the tokio runtime and delivers its output (any
  `Send + 'static` type) to **this** module's `handle_msg`, boxed. Downcast to
  your own message enum. It returns an `AbortHandle` (the Gateway module uses
  it to cancel a browser sign-in on `Esc`).
- `cx.sender::<M>()` gives a cloneable `Sender<M>` for tasks that report more
  than once (the Gateway sign-in sends the authorization URL, then the result).
- Messages are namespaced by `ModuleId`: a module only ever sees its own.
- Use a `Slot<T>` per loaded value. `begin()` before spawning, `finish(gen, result)`
  on arrival: a result whose generation is stale is dropped, never applied, so
  there is no need to cancel superseded loads. `Slot::load(cx, fut, wrap)` does
  both in one call.
- `tick(now, cx)` is called every loop turn for every started module, with
  `cx.focused()` saying whether it is on screen; return the next time you need
  one from `next_deadline()` (polling). The Gateway module polls only while
  focused.

### Keys

The shell resolves, in order: `Ctrl-c` (quit, always); the help overlay's own
keys while it is open; then, unless the module `captures_input()`, the number
keys `1`..`9` (switch module) and `?` (help); then the module's `handle_key`;
and finally `q`/`Esc` quit **only if** the module returned `Handled::No`.
While `captures_input()` is true (a form or modal is open) every key except
`Ctrl-c` goes to the module, so typing `1` or `q` into a form works.

Tab / Shift-Tab are free for modules (the Gateway module uses them for its
Servers / Usage / Policy screens).

### Overlays

Keep `Option<(Overlay, Purpose)>` with your own `Purpose` enum; return
`captures_input() == true` while it is `Some`; send it keys; act on the result:

```rust
match overlay.handle_key(key) {
    Step::Open => self.overlay = Some((overlay, purpose)),
    Step::Cancel => {}
    Step::Done(Answer::Values(v)) => /* validate; on error form.set_error(..) and keep it open */,
    Step::Done(Answer::Choice(value)) | Step::Done(Answer::Picked(i)) | Step::Done(Answer::Confirmed) => ..,
}
```

Draw it last in your `draw` with `overlay.draw(frame)` (it centres itself on the
whole frame).

### Toasts and status

`cx.toast(..)` / `cx.error(..)` set the single status-line message, which takes
the whole footer line until it expires. `status()` is the footer's right side
when there is no toast (e.g. `updated 12s ago`).

## Testing

```rust
let (app, rx) = studio_tui::App::new("Test", vec![Box::new(Fleet::new(..))]);
let mut h = studio_tui::Harness::new(app, rx, (120, 40)); // starts the app, draws
h.press("r");
h.until_text("updated").await;              // pumps messages and timers, 5 s limit
assert!(h.row_with("weather").unwrap().contains("ok"));
let fleet = h.app.module::<Fleet>().unwrap();   // typed access to module state
```

The Gateway module's tests (`tests/ui.rs`, `actions.rs`, `login.rs`, `multi.rs`)
run against `studio_fake::FakeGateway`; `tests/shell.rs` has a complete minimal
module (`Counter`) exercising `spawn`, `sender`, `handle_msg`, toasts, the
generated footer and fall-through keys.

## The Gateway module

- One `Pane` per `[[gateway]]` (`GatewayModule::new(sessions, opener)`), each
  with its own `Session`, slots, selection, overlay and sign-in; `p` opens a
  picker when there is more than one, and only the pane on screen loads or polls.
- Screens: Servers (registry + detail), Usage (`usage_stats`), Policy
  (`policy_events`); the latter two only for an admin sign-in and never
  requested for a view-only one.
- Admin actions on Servers: register `a`, refresh `x`/`X`, approve drift `A`,
  status `s`, access `o`, classify `c`, timeout `t`, change URL `u` (typed id),
  delete `D` (typed id).
- `L` signs in from inside the TUI: `Session::login` runs in a spawned task with
  the module's `Opener` (the system browser in production, an auto-consenting
  GET in the demo and tests); the authorization URL is shown while it waits and
  `Esc` cancels. The not-signed-in and expired screens offer it.
