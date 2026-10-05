//! A large scrollable overlay for diffs and reports. A change preview (the
//! change set's per-file diffs and commit message) is confirmed from here;
//! a report (validation, reconcile + verify) is only read.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use studio_marketplace::ChangeSet;

use crate::framework::keymap::Keymap;
use crate::framework::overlay::Step;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PagerKey {
    Down,
    Up,
    PageDown,
    PageUp,
    Top,
    Bottom,
    Apply,
    Close,
}

#[derive(Debug, Clone)]
pub struct Pager {
    pub title: String,
    pub lines: Vec<Line<'static>>,
    pub scroll: u16,
    /// A preview: `Enter`/`y` confirms.
    pub confirm: bool,
    /// Rows of text last drawn, for paging.
    page: u16,
}

impl Pager {
    pub fn report(title: impl Into<String>, lines: Vec<Line<'static>>) -> Pager {
        Pager {
            title: title.into(),
            lines,
            scroll: 0,
            confirm: false,
            page: 10,
        }
    }

    /// The preview of a change set: commit message, files, notes, diffs.
    pub fn preview(market: &str, cs: &ChangeSet) -> Pager {
        let mut lines: Vec<Line<'static>> = vec![Line::from("Commit message").bold()];
        for l in cs.commit_message().lines() {
            lines.push(Line::from(format!("  {l}")).fg(Color::Yellow));
        }
        lines.push(Line::from(""));
        lines.push(Line::from(format!("Files ({})", cs.changes.len())).bold());
        for c in &cs.changes {
            let color = match c.action() {
                "create" => Color::Green,
                "delete" => Color::Red,
                _ => Color::Cyan,
            };
            lines.push(Line::from(vec![
                Span::from(format!("  {:<6}  ", c.action())).fg(color),
                Span::from(c.path.clone()),
            ]));
        }
        lines.push(Line::from(""));
        lines.extend(diff_lines(&cs.diff()));
        Pager {
            title: format!("Preview {market}: {}", cs.title()),
            lines,
            scroll: 0,
            confirm: true,
            page: 10,
        }
    }

    pub fn keymap(&self) -> Keymap<PagerKey> {
        Keymap::new("Preview")
            .bind_if(
                self.confirm,
                &["enter", "y"],
                "apply and commit",
                PagerKey::Apply,
            )
            .footer_if(self.confirm, "Enter", "apply + commit")
            .bind(&["j", "down"], "scroll down", PagerKey::Down)
            .footer("j/k", "scroll")
            .bind(&["k", "up"], "scroll up", PagerKey::Up)
            .bind(&["pgdn", "space"], "page down", PagerKey::PageDown)
            .bind(&["pgup"], "page up", PagerKey::PageUp)
            .bind(&["home"], "top", PagerKey::Top)
            .bind(&["end"], "bottom", PagerKey::Bottom)
            .bind(&["esc", "q", "n"], "close", PagerKey::Close)
            .footer("Esc", if self.confirm { "cancel" } else { "close" })
    }

    fn max_scroll(&self) -> u16 {
        (self.lines.len() as u16).saturating_sub(1)
    }

    /// `Done(())` = confirmed (previews only).
    pub fn handle_key(&mut self, key: KeyEvent) -> Step<()> {
        // Enter on a report closes it.
        if !self.confirm && key.code == KeyCode::Enter {
            return Step::Cancel;
        }
        let Some(k) = self.keymap().resolve(&key) else {
            return Step::Open;
        };
        let max = self.max_scroll();
        match k {
            PagerKey::Down => self.scroll = (self.scroll + 1).min(max),
            PagerKey::Up => self.scroll = self.scroll.saturating_sub(1),
            PagerKey::PageDown => self.scroll = (self.scroll + self.page).min(max),
            PagerKey::PageUp => self.scroll = self.scroll.saturating_sub(self.page),
            PagerKey::Top => self.scroll = 0,
            PagerKey::Bottom => self.scroll = max,
            PagerKey::Apply => return Step::Done(()),
            PagerKey::Close => return Step::Cancel,
        }
        Step::Open
    }

    pub fn draw(&mut self, frame: &mut Frame) {
        let full = frame.area();
        let width = full.width.saturating_sub(4).max(20).min(full.width);
        let height = full.height.saturating_sub(2).max(5).min(full.height);
        let area = Rect {
            x: full.x + (full.width - width) / 2,
            y: full.y + (full.height - height) / 2,
            width,
            height,
        };
        frame.render_widget(Clear, area);
        let total = self.lines.len();
        let block = Block::bordered()
            .title(format!(" {} ", self.title))
            .title_bottom(format!(
                " {}/{} ",
                (self.scroll as usize + 1).min(total.max(1)),
                total
            ))
            .border_style(Style::new().fg(Color::Cyan));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let [body, hint] =
            Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);
        self.page = body.height.max(1);
        frame.render_widget(
            Paragraph::new(self.lines.clone()).scroll((self.scroll, 0)),
            body,
        );
        let text = if self.confirm {
            "Enter/y apply + commit to the local clone  j/k PgUp/PgDn scroll  Esc cancel"
        } else {
            "j/k PgUp/PgDn scroll  Esc close"
        };
        frame.render_widget(Line::from(text).dim(), hint);
    }
}

/// A unified diff, coloured line by line.
pub fn diff_lines(diff: &str) -> Vec<Line<'static>> {
    diff.lines()
        .map(|l| {
            let line = Line::from(l.to_string());
            if l.starts_with("+++") || l.starts_with("---") {
                line.bold()
            } else if l.starts_with('+') {
                line.fg(Color::Green)
            } else if l.starts_with('-') {
                line.fg(Color::Red)
            } else if l.starts_with("@@") {
                line.fg(Color::Cyan)
            } else {
                line
            }
        })
        .collect()
}
