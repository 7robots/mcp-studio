//! The shell: module tab strip and number keys, placeholders, the generated
//! footer and help, quitting, and the Component API end to end with a tiny
//! module that spawns work and receives its result.

mod common;

use std::any::Any;

use crossterm::event::KeyEvent;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Line;
use studio_tui::framework::cx::Payload;
use studio_tui::{App, Component, Cx, Handled, Harness, Hint, Keymap, ModuleId};

use common::{Env, TITLE, gw, load};

const WIDE: (u16, u16) = (150, 40);

#[tokio::test]
async fn the_tab_strip_names_the_instance_and_every_module() {
    let env = Env::new().await;
    let mut h = env.harness(WIDE).await;
    load(&mut h).await;
    let top = h.lines()[0].clone();
    assert!(top.contains("MCP Studio") && top.contains(TITLE), "{top}");
    for tab in ["1 Fleet", "2 Pattern", "3 Gateway", "4 Marketplaces"] {
        assert!(top.contains(tab), "{tab}: {top}");
    }
    assert_eq!(h.app.active_id(), Some("gateway"));
}

#[tokio::test]
async fn number_keys_switch_modules_and_placeholders_say_so() {
    let env = Env::new().await;
    let mut h = env.harness(WIDE).await;
    load(&mut h).await;
    h.press("1");
    assert_eq!(h.app.active_id(), Some("fleet"));
    assert!(h.text().contains("Fleet: not wired yet"), "{}", h.text());
    h.press("2");
    assert!(h.text().contains("Pattern: not wired yet"));
    h.press("4");
    assert!(h.text().contains("Marketplaces: not wired yet"));
    // Gateway keys do nothing elsewhere; q still quits from a placeholder later.
    h.press("j");
    h.press("3");
    assert!(h.text().contains("Servers (4)"));
    // Coming back did not reload.
    assert_eq!(env.called("list_servers"), 1);
}

#[tokio::test]
async fn footer_and_help_are_generated_from_the_keymaps() {
    let env = Env::new().await;
    let mut h = env.harness(WIDE).await;
    load(&mut h).await;
    let footer = h.lines().last().unwrap().clone();
    assert!(
        footer.contains("j/k select") && footer.contains("a add"),
        "{footer}"
    );
    assert!(
        footer.contains("1-4 modules") && footer.contains("? help"),
        "{footer}"
    );
    h.press("?");
    let text = h.text();
    for line in [
        "Studio",
        "Gateway module",
        "Servers (admin)",
        "register a server",
        "delete (type the id to confirm)",
        "sign in through the browser",
    ] {
        assert!(text.contains(line), "{line}:\n{text}");
    }
    // Keys go to the help while it is open.
    h.press("1");
    assert_eq!(h.app.active_id(), Some("gateway"));
    h.press("?");
    assert!(!h.app.help_open());
    // Each screen has its own keys.
    h.press("tab");
    h.press("tab");
    h.until_text("Events").await;
    let footer = h.lines().last().unwrap().clone();
    assert!(
        footer.contains("f filter") && !footer.contains("a add"),
        "{footer}"
    );
}

#[tokio::test]
async fn ctrl_c_quits_even_from_a_form() {
    let env = Env::new().await;
    let mut h = env.harness(WIDE).await;
    load(&mut h).await;
    h.press("a");
    assert!(gw(&h.app).overlay().is_some());
    h.press("q");
    assert!(h.app.running(), "q is typed into the form");
    h.press("ctrl+c");
    assert!(!h.app.running());
}

/// A minimal module, written the way the docs say to.
#[derive(Default)]
struct Counter {
    count: u64,
    started: bool,
}

#[derive(Debug)]
enum CounterMsg {
    Add(u64),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum CounterKey {
    Bump,
    Twice,
}

impl Counter {
    fn keys(&self) -> Keymap<CounterKey> {
        Keymap::new("Counter")
            .bind(&["i"], "add one, in a task", CounterKey::Bump)
            .footer("i", "add")
            .bind(&["I"], "add two, through a sender", CounterKey::Twice)
    }
}

impl Component for Counter {
    fn id(&self) -> ModuleId {
        "counter"
    }
    fn title(&self) -> String {
        "Counter".into()
    }
    fn start(&mut self, _cx: &mut Cx<'_>) {
        self.started = true;
    }
    fn handle_key(&mut self, key: KeyEvent, cx: &mut Cx<'_>) -> Handled {
        match self.keys().resolve(&key) {
            Some(CounterKey::Bump) => {
                cx.spawn(async { CounterMsg::Add(1) });
            }
            Some(CounterKey::Twice) => {
                let tx = cx.sender::<CounterMsg>();
                tokio::spawn(async move {
                    tx.send(CounterMsg::Add(1));
                    tx.send(CounterMsg::Add(1));
                });
            }
            None => return Handled::No,
        }
        Handled::Yes
    }
    fn handle_msg(&mut self, msg: Payload, cx: &mut Cx<'_>) {
        if let Ok(msg) = (msg as Box<dyn Any + Send>).downcast::<CounterMsg>() {
            let CounterMsg::Add(n) = *msg;
            self.count += n;
            cx.toast(format!("now {}", self.count));
        }
    }
    fn draw(&mut self, frame: &mut Frame, area: Rect) {
        frame.render_widget(Line::from(format!("count {}", self.count)), area);
    }
    fn keymap(&self) -> Vec<Hint> {
        self.keys().hints()
    }
    fn status(&self) -> Option<String> {
        Some(format!("{} so far", self.count))
    }
}

#[tokio::test]
async fn a_custom_module_spawns_work_and_gets_its_messages() {
    let modules: Vec<Box<dyn Component>> = vec![
        Box::new(Counter::default()),
        Box::new(studio_tui::modules::Placeholder::new("other", "Other")),
    ];
    let (app, rx) = App::new("Test", modules);
    let mut h = Harness::new(app, rx, (80, 10));
    assert!(h.app.module::<Counter>().unwrap().started);
    assert!(h.text().contains("count 0"));
    assert!(h.lines().last().unwrap().contains("i add"));
    assert!(h.lines().last().unwrap().contains("0 so far"));
    h.press("i");
    h.until_text("count 1").await;
    assert!(h.text().contains("now 1"), "the toast");
    h.press("I");
    h.until_text("count 3").await;
    // Unbound keys fall through to the shell.
    h.press("2");
    assert_eq!(h.app.active_id(), Some("other"));
    h.press("esc");
    assert!(!h.app.running());
}
