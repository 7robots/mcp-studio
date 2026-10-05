//! Reusable modal widgets. A module keeps an `Option<(Overlay, Purpose)>`
//! with its own `Purpose` enum, sends it every key while it is open (and
//! returns `true` from `captures_input`), and acts on the [`Step`] it gets
//! back:
//!
//! - [`Form`]: labelled text fields; `Tab`/`↑↓` move, `Enter` submits the raw
//!   values (validate them and call [`Form::set_error`] to keep it open).
//! - [`Choice`]: one key per option.
//! - [`Picker`]: a list; `j`/`k` move, `Enter` picks an index.
//! - [`Confirm`]: `y`/`Enter` confirms, `n`/`Esc` cancels; or, built with
//!   [`Confirm::typed`], only `Enter` after typing the expected id exactly.
//!
//! `Esc` cancels every one of them.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};

use super::widgets::centered;

/// The result of one key in an overlay.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step<T> {
    /// Still open.
    Open,
    /// Closed without a result.
    Cancel,
    /// Closed with a result.
    Done(T),
}

const WIDTH: u16 = 84;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormField {
    pub label: String,
    pub hint: String,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Form {
    pub title: String,
    pub fields: Vec<FormField>,
    pub focus: usize,
    pub error: Option<String>,
}

impl Form {
    pub fn new(title: impl Into<String>) -> Form {
        Form {
            title: title.into(),
            fields: Vec::new(),
            focus: 0,
            error: None,
        }
    }

    /// Adds a field; `hint` shows dimmed while it is empty and unfocused.
    pub fn field(mut self, label: &str, hint: &str, value: &str) -> Form {
        self.fields.push(FormField {
            label: label.into(),
            hint: hint.into(),
            value: value.into(),
        });
        self
    }

    pub fn set_error(&mut self, message: impl Into<String>) {
        self.error = Some(message.into());
    }

    pub fn values(&self) -> Vec<String> {
        self.fields.iter().map(|f| f.value.clone()).collect()
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Step<Vec<String>> {
        let n = self.fields.len().max(1);
        match key.code {
            KeyCode::Esc => return Step::Cancel,
            KeyCode::Enter => return Step::Done(self.values()),
            KeyCode::Tab | KeyCode::Down => self.focus = (self.focus + 1) % n,
            KeyCode::BackTab | KeyCode::Up => self.focus = (self.focus + n - 1) % n,
            KeyCode::Backspace => {
                if let Some(f) = self.fields.get_mut(self.focus) {
                    f.value.pop();
                }
            }
            KeyCode::Char(ch) => {
                if let Some(f) = self.fields.get_mut(self.focus) {
                    f.value.push(ch);
                }
            }
            _ => {}
        }
        Step::Open
    }

    fn lines(&self) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        for (i, f) in self.fields.iter().enumerate() {
            let label = Span::from(format!("{:>12}  ", f.label)).bold();
            let value = if f.value.is_empty() && i != self.focus {
                Span::from(f.hint.clone()).dim()
            } else {
                Span::from(f.value.clone())
            };
            let mut spans = vec![label, value];
            if i == self.focus {
                spans.push(cursor());
            }
            lines.push(Line::from(spans));
        }
        lines.push(Line::from(""));
        if let Some(error) = &self.error {
            lines.push(Line::from(error.clone()).fg(Color::Red));
        }
        lines.push(Line::from("Enter submit  Tab next field  Esc cancel").dim());
        lines
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChoiceOption {
    pub key: char,
    pub label: String,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choice {
    pub title: String,
    pub lines: Vec<String>,
    pub options: Vec<ChoiceOption>,
}

impl Choice {
    pub fn new(title: impl Into<String>, lines: Vec<String>) -> Choice {
        Choice {
            title: title.into(),
            lines,
            options: Vec::new(),
        }
    }

    /// One option: pressing `key` closes the choice with `value`.
    pub fn option(mut self, key: char, label: &str, value: &str) -> Choice {
        self.options.push(ChoiceOption {
            key,
            label: label.into(),
            value: value.into(),
        });
        self
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Step<String> {
        match key.code {
            KeyCode::Esc => Step::Cancel,
            KeyCode::Char(ch) => match self.options.iter().find(|o| o.key == ch) {
                Some(o) => Step::Done(o.value.clone()),
                None => Step::Open,
            },
            _ => Step::Open,
        }
    }

    fn lines(&self) -> Vec<Line<'static>> {
        let mut lines: Vec<Line> = self.lines.iter().map(|l| Line::from(l.clone())).collect();
        lines.push(Line::from(""));
        for o in &self.options {
            lines.push(Line::from(vec![
                Span::from(format!("  {}  ", o.key)).bold(),
                Span::from(o.label.clone()),
            ]));
        }
        lines.push(Line::from(""));
        lines.push(Line::from("Esc cancel").dim());
        lines
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PickItem {
    pub label: String,
    /// A second, coloured column (a tool's classification, a gateway's host).
    pub tag: Option<(String, Color)>,
}

impl PickItem {
    pub fn new(label: impl Into<String>) -> PickItem {
        PickItem {
            label: label.into(),
            tag: None,
        }
    }

    pub fn tagged(label: impl Into<String>, tag: impl Into<String>, color: Color) -> PickItem {
        PickItem {
            label: label.into(),
            tag: Some((tag.into(), color)),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Picker {
    pub title: String,
    pub items: Vec<PickItem>,
    pub selected: usize,
    /// What `Enter` does, for the hint line (`"classify"`, `"switch"`).
    pub verb: String,
}

impl Picker {
    pub fn new(title: impl Into<String>, items: Vec<PickItem>, verb: &str) -> Picker {
        Picker {
            title: title.into(),
            items,
            selected: 0,
            verb: verb.into(),
        }
    }

    pub fn with_selected(mut self, index: usize) -> Picker {
        self.selected = index.min(self.items.len().saturating_sub(1));
        self
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Step<usize> {
        let last = self.items.len().saturating_sub(1);
        match key.code {
            KeyCode::Esc => return Step::Cancel,
            KeyCode::Char('j') | KeyCode::Down => self.selected = (self.selected + 1).min(last),
            KeyCode::Char('k') | KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Enter if !self.items.is_empty() => return Step::Done(self.selected),
            _ => {}
        }
        Step::Open
    }

    fn lines(&self) -> Vec<Line<'static>> {
        let width = self.items.iter().map(|i| i.label.len()).max().unwrap_or(0);
        let mut lines: Vec<Line> = self
            .items
            .iter()
            .enumerate()
            .map(|(i, item)| {
                let mut spans = vec![Span::from(format!("  {:<width$}  ", item.label))];
                if let Some((tag, color)) = &item.tag {
                    spans.push(Span::from(tag.clone()).fg(*color));
                }
                let line = Line::from(spans);
                if i == self.selected {
                    line.add_modifier(Modifier::REVERSED)
                } else {
                    line
                }
            })
            .collect();
        lines.push(Line::from(""));
        lines.push(Line::from(format!("j/k move  Enter {}  Esc cancel", self.verb)).dim());
        lines
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Confirm {
    pub title: String,
    pub lines: Vec<String>,
    /// `(expected, input)`: the text to type before `Enter` confirms.
    pub typed: Option<(String, String)>,
}

impl Confirm {
    /// Confirmed with `y` or `Enter`.
    pub fn new(title: impl Into<String>, lines: Vec<String>) -> Confirm {
        Confirm {
            title: title.into(),
            lines,
            typed: None,
        }
    }

    /// Confirmed only by typing `expected` exactly, then `Enter`.
    pub fn typed(
        title: impl Into<String>,
        lines: Vec<String>,
        expected: impl Into<String>,
    ) -> Confirm {
        Confirm {
            title: title.into(),
            lines,
            typed: Some((expected.into(), String::new())),
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Step<()> {
        if key.code == KeyCode::Esc {
            return Step::Cancel;
        }
        match (&mut self.typed, key.code) {
            (None, KeyCode::Enter | KeyCode::Char('y')) => Step::Done(()),
            (None, KeyCode::Char('n')) => Step::Cancel,
            (Some((expected, input)), KeyCode::Enter) if input == expected => Step::Done(()),
            (Some((_, input)), KeyCode::Char(ch)) => {
                input.push(ch);
                Step::Open
            }
            (Some((_, input)), KeyCode::Backspace) => {
                input.pop();
                Step::Open
            }
            _ => Step::Open,
        }
    }

    fn lines(&self) -> Vec<Line<'static>> {
        let mut lines: Vec<Line> = self.lines.iter().map(|l| Line::from(l.clone())).collect();
        lines.push(Line::from(""));
        match &self.typed {
            Some((expected, input)) => {
                let matched = input == expected;
                lines.push(Line::from(vec![
                    Span::from("  type the id: ").bold(),
                    Span::from(input.clone()).fg(if matched { Color::Green } else { Color::Reset }),
                    cursor(),
                ]));
                lines.push(Line::from(""));
                let hint = if matched {
                    "Enter confirm  Esc cancel"
                } else {
                    "Esc cancel"
                };
                lines.push(Line::from(hint).dim());
            }
            None => lines.push(Line::from("y/Enter confirm  n/Esc cancel").dim()),
        }
        lines
    }
}

fn cursor() -> Span<'static> {
    Span::styled(" ", Style::new().add_modifier(Modifier::REVERSED))
}

/// Any of the widgets, for modules that keep one open overlay at a time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Overlay {
    Form(Form),
    Choice(Choice),
    Picker(Picker),
    Confirm(Confirm),
}

/// What an [`Overlay`] closed with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    Values(Vec<String>),
    Choice(String),
    Picked(usize),
    Confirmed,
}

impl Overlay {
    pub fn title(&self) -> &str {
        match self {
            Overlay::Form(w) => &w.title,
            Overlay::Choice(w) => &w.title,
            Overlay::Picker(w) => &w.title,
            Overlay::Confirm(w) => &w.title,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Step<Answer> {
        fn map<T>(step: Step<T>, f: impl FnOnce(T) -> Answer) -> Step<Answer> {
            match step {
                Step::Open => Step::Open,
                Step::Cancel => Step::Cancel,
                Step::Done(v) => Step::Done(f(v)),
            }
        }
        match self {
            Overlay::Form(w) => map(w.handle_key(key), Answer::Values),
            Overlay::Choice(w) => map(w.handle_key(key), Answer::Choice),
            Overlay::Picker(w) => map(w.handle_key(key), Answer::Picked),
            Overlay::Confirm(w) => map(w.handle_key(key), |()| Answer::Confirmed),
        }
    }

    /// Draws the overlay centred over the whole frame.
    pub fn draw(&self, frame: &mut Frame) {
        let lines = match self {
            Overlay::Form(w) => w.lines(),
            Overlay::Choice(w) => w.lines(),
            Overlay::Picker(w) => w.lines(),
            Overlay::Confirm(w) => w.lines(),
        };
        draw_box(frame, self.title(), lines);
    }
}

/// A bordered, cleared box in the middle of the frame.
pub fn draw_box(frame: &mut Frame, title: &str, lines: Vec<Line<'static>>) {
    let height = (lines.len() as u16 + 2).min(frame.area().height);
    let area = centered(frame.area(), WIDTH.min(frame.area().width), height);
    frame.render_widget(Clear, area);
    let block = Block::bordered()
        .title(format!(" {title} "))
        .border_style(Style::new().fg(Color::Cyan));
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    fn k(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn typed(w: &mut Form, text: &str) {
        for ch in text.chars() {
            w.handle_key(k(KeyCode::Char(ch)));
        }
    }

    #[test]
    fn form_edits_moves_and_submits() {
        let mut f = Form::new("F").field("a", "", "x").field("b", "hint", "");
        typed(&mut f, "yz");
        f.handle_key(k(KeyCode::Backspace));
        f.handle_key(k(KeyCode::Tab));
        typed(&mut f, "q1");
        assert_eq!(
            f.handle_key(k(KeyCode::Enter)),
            Step::Done(vec!["xy".into(), "q1".into()])
        );
        f.handle_key(k(KeyCode::Up));
        assert_eq!(f.focus, 0);
        assert_eq!(f.handle_key(k(KeyCode::Esc)), Step::Cancel);
    }

    #[test]
    fn choice_takes_only_its_keys() {
        let mut c = Choice::new("C", vec![]).option('a', "active", "active");
        assert_eq!(c.handle_key(k(KeyCode::Char('z'))), Step::Open);
        assert_eq!(
            c.handle_key(k(KeyCode::Char('a'))),
            Step::Done("active".into())
        );
    }

    #[test]
    fn picker_clamps_and_picks() {
        let mut p = Picker::new("P", vec![PickItem::new("a"), PickItem::new("b")], "pick");
        p.handle_key(k(KeyCode::Char('k')));
        assert_eq!(p.selected, 0);
        for _ in 0..5 {
            p.handle_key(k(KeyCode::Char('j')));
        }
        assert_eq!(p.handle_key(k(KeyCode::Enter)), Step::Done(1));
        let mut empty = Picker::new("P", vec![], "pick");
        assert_eq!(empty.handle_key(k(KeyCode::Enter)), Step::Open);
    }

    #[test]
    fn typed_confirm_needs_the_exact_text() {
        let mut c = Confirm::typed("D", vec![], "abc");
        assert_eq!(c.handle_key(k(KeyCode::Enter)), Step::Open);
        assert_eq!(
            c.handle_key(k(KeyCode::Char('y'))),
            Step::Open,
            "y is just a letter here"
        );
        c.handle_key(k(KeyCode::Backspace));
        for ch in "abc".chars() {
            c.handle_key(k(KeyCode::Char(ch)));
        }
        assert_eq!(c.handle_key(k(KeyCode::Enter)), Step::Done(()));
        let mut plain = Confirm::new("C", vec![]);
        assert_eq!(plain.handle_key(k(KeyCode::Char('n'))), Step::Cancel);
        assert_eq!(plain.handle_key(k(KeyCode::Char('y'))), Step::Done(()));
    }
}
