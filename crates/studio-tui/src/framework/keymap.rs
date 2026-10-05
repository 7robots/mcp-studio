//! The keymap registry: one table per context that both dispatches keys and
//! describes them, so the footer hints and the help overlay can never drift
//! from what the keys actually do.
//!
//! ```
//! use studio_tui::Keymap;
//! #[derive(Clone, Copy, Debug, PartialEq)]
//! enum Act { Down, Up, Refresh }
//! let km = Keymap::new("Lists")
//!     .bind(&["j", "down"], "move down", Act::Down)
//!     .footer("j/k", "select")
//!     .bind(&["k", "up"], "move up", Act::Up)
//!     .bind(&["r"], "refresh", Act::Refresh)
//!     .footer("r", "refresh");
//! let key = crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Char('j'));
//! assert_eq!(km.resolve(&key), Some(Act::Down));
//! assert_eq!(studio_tui::framework::keymap::footer_text(&km.hints()), "j/k select  r refresh");
//! ```

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// One key: a code plus whether Ctrl is held. Shift is implied by the
/// character (`G` is Shift+g), so it is not matched separately.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeySpec {
    pub code: KeyCode,
    pub ctrl: bool,
}

impl KeySpec {
    /// Parses `"j"`, `"G"`, `"?"`, `"enter"`, `"esc"`, `"tab"`, `"shift+tab"`,
    /// `"up"`, `"down"`, `"left"`, `"right"`, `"home"`, `"end"`, `"pgup"`,
    /// `"pgdn"`, `"backspace"`, `"space"`, `"f1"`..`"f12"`, `"ctrl+c"`.
    ///
    /// # Panics
    /// On a name it does not know: keymaps are written in code, so that is a
    /// programming error worth failing loudly on.
    pub fn parse(spec: &str) -> KeySpec {
        let (ctrl, name) = match spec.strip_prefix("ctrl+") {
            Some(rest) => (true, rest),
            None => (false, spec),
        };
        let code = match name {
            "enter" => KeyCode::Enter,
            "esc" => KeyCode::Esc,
            "tab" => KeyCode::Tab,
            "shift+tab" | "backtab" => KeyCode::BackTab,
            "up" => KeyCode::Up,
            "down" => KeyCode::Down,
            "left" => KeyCode::Left,
            "right" => KeyCode::Right,
            "home" => KeyCode::Home,
            "end" => KeyCode::End,
            "pgup" => KeyCode::PageUp,
            "pgdn" => KeyCode::PageDown,
            "backspace" => KeyCode::Backspace,
            "space" => KeyCode::Char(' '),
            f if f.len() > 1 && f.starts_with('f') && f[1..].parse::<u8>().is_ok() => {
                KeyCode::F(f[1..].parse().expect("checked"))
            }
            c if c.chars().count() == 1 => KeyCode::Char(c.chars().next().expect("one char")),
            other => panic!("unknown key name {other:?}"),
        };
        KeySpec { code, ctrl }
    }

    pub fn matches(&self, key: &KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl != self.ctrl {
            return false;
        }
        match (self.code, key.code) {
            (KeyCode::BackTab, KeyCode::Tab) => key.modifiers.contains(KeyModifiers::SHIFT),
            (KeyCode::Tab, KeyCode::Tab) => !key.modifiers.contains(KeyModifiers::SHIFT),
            (a, b) => a == b,
        }
    }

    /// How the key is written in hints.
    pub fn label(&self) -> String {
        let base = match self.code {
            KeyCode::Enter => "Enter".into(),
            KeyCode::Esc => "Esc".into(),
            KeyCode::Tab => "Tab".into(),
            KeyCode::BackTab => "S-Tab".into(),
            KeyCode::Up => "↑".into(),
            KeyCode::Down => "↓".into(),
            KeyCode::Left => "←".into(),
            KeyCode::Right => "→".into(),
            KeyCode::Home => "Home".into(),
            KeyCode::End => "End".into(),
            KeyCode::PageUp => "PgUp".into(),
            KeyCode::PageDown => "PgDn".into(),
            KeyCode::Backspace => "Bksp".into(),
            KeyCode::Char(' ') => "Space".into(),
            KeyCode::Char(c) => c.to_string(),
            KeyCode::F(n) => format!("F{n}"),
            other => format!("{other:?}"),
        };
        if self.ctrl {
            format!("Ctrl-{base}")
        } else {
            base
        }
    }
}

/// A binding as hints and help see it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hint {
    /// Help-overlay group, e.g. `"Gateway"`, `"Servers (admin)"`.
    pub section: String,
    /// The keys as the help overlay writes them (`"j ↓"`).
    pub keys: String,
    /// One line for the help overlay.
    pub help: String,
    /// `(keys, label)` when the binding also shows in the footer.
    pub footer: Option<(String, String)>,
}

#[derive(Clone, Debug)]
struct Binding<A> {
    section: String,
    keys: Vec<KeySpec>,
    display: String,
    help: String,
    footer: Option<(String, String)>,
    action: A,
}

/// A table of bindings for one context. Build it with chained calls; resolve
/// keys with [`Keymap::resolve`]; describe it with [`Keymap::hints`].
#[derive(Clone, Debug)]
pub struct Keymap<A> {
    section: String,
    bindings: Vec<Binding<A>>,
}

impl<A: Clone> Keymap<A> {
    /// An empty table whose bindings go in `section` until [`Keymap::section`].
    pub fn new(section: impl Into<String>) -> Keymap<A> {
        Keymap {
            section: section.into(),
            bindings: Vec::new(),
        }
    }

    /// Bindings added after this go in `section`.
    pub fn section(mut self, section: impl Into<String>) -> Self {
        self.section = section.into();
        self
    }

    /// Binds `keys` (see [`KeySpec::parse`]) to `action`. The first match wins
    /// in [`Keymap::resolve`], so bind specific keys before general ones.
    pub fn bind(mut self, keys: &[&str], help: impl Into<String>, action: A) -> Self {
        let specs: Vec<KeySpec> = keys.iter().map(|k| KeySpec::parse(k)).collect();
        let display = specs
            .iter()
            .map(KeySpec::label)
            .collect::<Vec<_>>()
            .join(" ");
        self.bindings.push(Binding {
            section: self.section.clone(),
            keys: specs,
            display,
            help: help.into(),
            footer: None,
            action,
        });
        self
    }

    /// [`Keymap::bind`] only when `cond` holds (context-dependent keys).
    pub fn bind_if(self, cond: bool, keys: &[&str], help: impl Into<String>, action: A) -> Self {
        if cond {
            self.bind(keys, help, action)
        } else {
            self
        }
    }

    /// Shows the last binding in the footer as `keys label`.
    pub fn footer(self, keys: &str, label: &str) -> Self {
        self.footer_if(true, keys, label)
    }

    /// [`Keymap::footer`] only when `cond` holds.
    pub fn footer_if(mut self, cond: bool, keys: &str, label: &str) -> Self {
        if cond && let Some(last) = self.bindings.last_mut() {
            last.footer = Some((keys.into(), label.into()));
        }
        self
    }

    /// Overrides how the last binding's keys are written in the help overlay.
    pub fn display(mut self, keys: &str) -> Self {
        if let Some(last) = self.bindings.last_mut() {
            last.display = keys.into();
        }
        self
    }

    /// Appends `other`'s bindings (keeping their sections).
    pub fn extend(mut self, other: Keymap<A>) -> Self {
        self.bindings.extend(other.bindings);
        self
    }

    /// The action bound to `key`, if any.
    pub fn resolve(&self, key: &KeyEvent) -> Option<A> {
        self.bindings
            .iter()
            .find(|b| b.keys.iter().any(|k| k.matches(key)))
            .map(|b| b.action.clone())
    }

    pub fn hints(&self) -> Vec<Hint> {
        self.bindings
            .iter()
            .map(|b| Hint {
                section: b.section.clone(),
                keys: b.display.clone(),
                help: b.help.clone(),
                footer: b.footer.clone(),
            })
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }
}

/// The footer line for `hints`: each footer binding as `keys label`.
pub fn footer_text(hints: &[Hint]) -> String {
    hints
        .iter()
        .filter_map(|h| h.footer.as_ref())
        .map(|(keys, label)| format!("{keys} {label}"))
        .collect::<Vec<_>>()
        .join("  ")
}

/// The help overlay's lines: `(section, [(keys, help)])`, sections in first-seen order.
pub fn help_sections(hints: &[Hint]) -> Vec<(String, Vec<(String, String)>)> {
    let mut out: Vec<(String, Vec<(String, String)>)> = Vec::new();
    for h in hints {
        let entry = (h.keys.clone(), h.help.clone());
        match out.iter_mut().find(|(s, _)| *s == h.section) {
            Some((_, lines)) => lines.push(entry),
            None => out.push((h.section.clone(), vec![entry])),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum A {
        Down,
        Last,
        Next,
        Prev,
        Quit,
        Kill,
    }

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    fn km() -> Keymap<A> {
        Keymap::new("Move")
            .bind(&["j", "down"], "move down", A::Down)
            .footer("j/k", "select")
            .bind(&["G", "end"], "last", A::Last)
            .bind(&["tab"], "next screen", A::Next)
            .footer("Tab", "screens")
            .bind(&["shift+tab"], "previous screen", A::Prev)
            .section("App")
            .bind(&["q", "esc"], "quit", A::Quit)
            .footer("q", "quit")
            .bind(&["ctrl+c"], "quit now", A::Kill)
    }

    #[test]
    fn resolves_chars_named_keys_and_modifiers() {
        let km = km();
        assert_eq!(
            km.resolve(&key(KeyCode::Char('j'), KeyModifiers::NONE)),
            Some(A::Down)
        );
        assert_eq!(
            km.resolve(&key(KeyCode::Down, KeyModifiers::NONE)),
            Some(A::Down)
        );
        // Shift comes with the uppercase character and is not matched on its own.
        assert_eq!(
            km.resolve(&key(KeyCode::Char('G'), KeyModifiers::SHIFT)),
            Some(A::Last)
        );
        assert_eq!(
            km.resolve(&key(KeyCode::Char('g'), KeyModifiers::NONE)),
            None
        );
        assert_eq!(
            km.resolve(&key(KeyCode::Tab, KeyModifiers::NONE)),
            Some(A::Next)
        );
        assert_eq!(
            km.resolve(&key(KeyCode::BackTab, KeyModifiers::SHIFT)),
            Some(A::Prev)
        );
        assert_eq!(
            km.resolve(&key(KeyCode::Tab, KeyModifiers::SHIFT)),
            Some(A::Prev)
        );
        assert_eq!(
            km.resolve(&key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(A::Kill)
        );
        // Ctrl must match: plain c is not Ctrl-c, and Ctrl-q is not q.
        assert_eq!(
            km.resolve(&key(KeyCode::Char('c'), KeyModifiers::NONE)),
            None
        );
        assert_eq!(
            km.resolve(&key(KeyCode::Char('q'), KeyModifiers::CONTROL)),
            None
        );
    }

    #[test]
    fn footer_and_help_come_from_the_same_bindings() {
        let hints = km().hints();
        assert_eq!(footer_text(&hints), "j/k select  Tab screens  q quit");
        let help = help_sections(&hints);
        assert_eq!(help.len(), 2);
        assert_eq!(help[0].0, "Move");
        assert_eq!(help[0].1[0], ("j ↓".to_string(), "move down".to_string()));
        assert_eq!(help[0].1[2], ("Tab".to_string(), "next screen".to_string()));
        assert_eq!(help[1].1[1], ("Ctrl-c".to_string(), "quit now".to_string()));
        // Every footer entry is also in the help.
        for h in hints.iter().filter(|h| h.footer.is_some()) {
            assert!(
                help.iter()
                    .any(|(_, l)| l.iter().any(|(_, t)| *t == h.help))
            );
        }
    }

    #[test]
    fn conditional_bindings_and_overrides() {
        let km = Keymap::new("S")
            .bind_if(false, &["a"], "admin only", A::Down)
            .bind(&["x"], "refresh", A::Next)
            .footer_if(false, "x", "refresh")
            .display("x X");
        assert_eq!(
            km.resolve(&key(KeyCode::Char('a'), KeyModifiers::NONE)),
            None
        );
        let hints = km.hints();
        assert_eq!(hints.len(), 1);
        assert_eq!(hints[0].keys, "x X");
        assert_eq!(footer_text(&hints), "");
    }

    #[test]
    fn the_first_binding_wins() {
        let km = Keymap::new("S")
            .bind(&["r"], "one", A::Down)
            .bind(&["r"], "two", A::Last);
        assert_eq!(
            km.resolve(&key(KeyCode::Char('r'), KeyModifiers::NONE)),
            Some(A::Down)
        );
    }

    #[test]
    #[should_panic(expected = "unknown key name")]
    fn unknown_names_panic() {
        KeySpec::parse("hyper");
    }

    #[test]
    fn function_keys_parse() {
        assert_eq!(KeySpec::parse("f5").code, KeyCode::F(5));
        assert_eq!(KeySpec::parse("f").code, KeyCode::Char('f'));
    }
}
