//! A small Markdown renderer for the terminal: enough for a changelog and a
//! skill's docs. Headings, paragraphs (word-wrapped to the width), bullet and
//! numbered lists with hanging indents, block quotes, code fences, rules,
//! front matter, and tables (aligned when they fit, one row per line when
//! they do not). Inline: `code`, **bold**, *emphasis* and `[text](url)`.
//!
//! Wrapping happens here rather than in the `Paragraph`, so the caller knows
//! exactly how many rows the document takes and can clamp its scroll.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// A run of text in one style.
pub type Piece = (String, Style);

fn code_style() -> Style {
    Style::new().fg(Color::Yellow)
}

/// Renders `text` into lines at most `width` columns wide.
pub fn render(text: &str, width: u16) -> Vec<Line<'static>> {
    let width = (width as usize).max(8);
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut para: Option<Block> = None;
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;

    // Front matter: shown dimmed, as is.
    if lines.first().map(|l| l.trim_end()) == Some("---")
        && let Some(end) = lines.iter().skip(1).position(|l| l.trim_end() == "---")
    {
        for l in &lines[..end + 2] {
            push_hard(&mut out, l, Style::new().add_modifier(Modifier::DIM), width);
        }
        i = end + 2;
    }

    while i < lines.len() {
        let raw = lines[i];
        let line = raw.trim_end();
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();

        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            flush(&mut para, &mut out, width);
            let fence = &trimmed[..3];
            i += 1;
            while i < lines.len() && !lines[i].trim_start().starts_with(fence) {
                let code = lines[i].trim_end();
                push_hard(&mut out, &format!("  {code}"), code_style(), width);
                i += 1;
            }
            i += 1; // the closing fence
            continue;
        }
        if trimmed.is_empty() {
            flush(&mut para, &mut out, width);
            if out.last().is_some_and(|l| l.width() > 0) {
                out.push(Line::from(""));
            }
            i += 1;
            continue;
        }
        if let Some((level, title)) = heading(trimmed) {
            flush(&mut para, &mut out, width);
            let style = match level {
                1 => Style::new()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
                2 => Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                _ => Style::new().add_modifier(Modifier::BOLD),
            };
            let pieces: Vec<Piece> = inline(title)
                .into_iter()
                .map(|(t, s)| (t, s.patch(style)))
                .collect();
            out.extend(wrap(&pieces, width, &[], &[]));
            i += 1;
            continue;
        }
        if is_rule(trimmed) {
            flush(&mut para, &mut out, width);
            out.push(Line::styled(
                "─".repeat(width.min(60)),
                Style::new().add_modifier(Modifier::DIM),
            ));
            i += 1;
            continue;
        }
        if trimmed.starts_with('|') {
            flush(&mut para, &mut out, width);
            let start = i;
            while i < lines.len() && lines[i].trim_start().starts_with('|') {
                i += 1;
            }
            out.extend(table(&lines[start..i], width));
            continue;
        }
        if let Some((marker, rest)) = list_item(trimmed) {
            flush(&mut para, &mut out, width);
            let pad = " ".repeat(indent.min(8));
            let first = format!("{pad}{marker} ");
            let cont = " ".repeat(first.width());
            para = Some(Block {
                text: rest.to_string(),
                first: vec![(first, Style::new().fg(Color::Cyan))],
                cont: vec![(cont, Style::new())],
            });
            i += 1;
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix('>') {
            let rest = rest.strip_prefix(' ').unwrap_or(rest);
            let bar = ("│ ".to_string(), Style::new().add_modifier(Modifier::DIM));
            match &mut para {
                Some(b) if b.first.first().map(|p| p.0.as_str()) == Some("│ ") => {
                    b.text.push(' ');
                    b.text.push_str(rest);
                }
                _ => {
                    flush(&mut para, &mut out, width);
                    para = Some(Block {
                        text: rest.to_string(),
                        first: vec![bar.clone()],
                        cont: vec![bar],
                    });
                }
            }
            i += 1;
            continue;
        }
        // Paragraph text, or a list item's continuation.
        match &mut para {
            Some(b) => {
                b.text.push(' ');
                b.text.push_str(trimmed);
            }
            None => {
                para = Some(Block {
                    text: trimmed.to_string(),
                    first: Vec::new(),
                    cont: Vec::new(),
                })
            }
        }
        i += 1;
    }
    flush(&mut para, &mut out, width);
    while out.last().is_some_and(|l| l.width() == 0) {
        out.pop();
    }
    out
}

/// A paragraph being collected: its text and the prefixes of its first and
/// following rows (a bullet and its hanging indent, a quote bar).
struct Block {
    text: String,
    first: Vec<Piece>,
    cont: Vec<Piece>,
}

fn flush(para: &mut Option<Block>, out: &mut Vec<Line<'static>>, width: usize) {
    if let Some(b) = para.take() {
        out.extend(wrap(&inline(&b.text), width, &b.first, &b.cont));
    }
}

fn heading(line: &str) -> Option<(usize, &str)> {
    let level = line.chars().take_while(|c| *c == '#').count();
    if (1..=6).contains(&level) {
        let rest = &line[level..];
        if let Some(title) = rest.strip_prefix(' ') {
            return Some((level, title.trim().trim_end_matches('#').trim_end()));
        }
    }
    None
}

fn is_rule(line: &str) -> bool {
    let t: String = line.chars().filter(|c| !c.is_whitespace()).collect();
    t.len() >= 3
        && (t.chars().all(|c| c == '-')
            || t.chars().all(|c| c == '*')
            || t.chars().all(|c| c == '_'))
}

/// `- x`, `* x`, `+ x`, `1. x`, `1) x` → (marker as shown, rest).
fn list_item(line: &str) -> Option<(String, &str)> {
    for bullet in ["- ", "* ", "+ "] {
        if let Some(rest) = line.strip_prefix(bullet) {
            return Some(("•".into(), rest.trim_start()));
        }
    }
    let digits = line.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits > 0 && digits <= 3 {
        let rest = &line[digits..];
        for end in [". ", ") "] {
            if let Some(after) = rest.strip_prefix(end) {
                return Some((format!("{}.", &line[..digits]), after.trim_start()));
            }
        }
    }
    None
}

/// Inline markup → styled pieces.
pub fn inline(text: &str) -> Vec<Piece> {
    let mut out: Vec<Piece> = Vec::new();
    let mut plain = String::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    let flush_plain = |plain: &mut String, out: &mut Vec<Piece>| {
        if !plain.is_empty() {
            out.push((std::mem::take(plain), Style::new()));
        }
    };
    let find = |from: usize, pat: &[char]| -> Option<usize> {
        (from..chars.len().saturating_sub(pat.len() - 1)).find(|&j| chars[j..].starts_with(pat))
    };
    while i < chars.len() {
        let c = chars[i];
        if c == '`'
            && let Some(end) = find(i + 1, &['`'])
        {
            flush_plain(&mut plain, &mut out);
            out.push((chars[i + 1..end].iter().collect(), code_style()));
            i = end + 1;
            continue;
        }
        if c == '*'
            && chars.get(i + 1) == Some(&'*')
            && let Some(end) = find(i + 2, &['*', '*'])
            && end > i + 2
        {
            flush_plain(&mut plain, &mut out);
            let inner: String = chars[i + 2..end].iter().collect();
            for (t, s) in inline(&inner) {
                out.push((t, s.add_modifier(Modifier::BOLD)));
            }
            i = end + 2;
            continue;
        }
        if c == '*'
            && chars
                .get(i + 1)
                .is_some_and(|n| !n.is_whitespace() && *n != '*')
            && let Some(end) = find(i + 1, &['*'])
        {
            flush_plain(&mut plain, &mut out);
            let inner: String = chars[i + 1..end].iter().collect();
            for (t, s) in inline(&inner) {
                out.push((t, s.add_modifier(Modifier::ITALIC)));
            }
            i = end + 1;
            continue;
        }
        if c == '['
            && let Some(close) = find(i + 1, &[']'])
            && chars.get(close + 1) == Some(&'(')
            && let Some(paren) = find(close + 2, &[')'])
        {
            flush_plain(&mut plain, &mut out);
            let label: String = chars[i + 1..close].iter().collect();
            for (t, s) in inline(&label) {
                out.push((t, s.add_modifier(Modifier::UNDERLINED)));
            }
            i = paren + 1;
            continue;
        }
        plain.push(c);
        i += 1;
    }
    flush_plain(&mut plain, &mut out);
    out
}

/// Greedy word wrap of styled pieces; words wider than a row are split.
pub(crate) fn wrap(
    pieces: &[Piece],
    width: usize,
    first: &[Piece],
    cont: &[Piece],
) -> Vec<Line<'static>> {
    let prefix_width = |p: &[Piece]| p.iter().map(|(t, _)| t.width()).sum::<usize>();
    // Words as (char, style) runs.
    let mut words: Vec<Vec<(char, Style)>> = Vec::new();
    let mut word: Vec<(char, Style)> = Vec::new();
    for (text, style) in pieces {
        for ch in text.chars() {
            if ch.is_whitespace() {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
            } else {
                word.push((ch, *style));
            }
        }
    }
    if !word.is_empty() {
        words.push(word);
    }

    let mut rows: Vec<Vec<(char, Style)>> = Vec::new();
    let mut row: Vec<(char, Style)> = Vec::new();
    let mut row_width = 0;
    let avail = |rows: &Vec<Vec<(char, Style)>>| {
        let p = if rows.is_empty() {
            prefix_width(first)
        } else {
            prefix_width(cont)
        };
        width.saturating_sub(p).max(4)
    };
    for w in words {
        let ww: usize = w.iter().map(|(c, _)| c.width().unwrap_or(0)).sum();
        if row_width > 0 && row_width + 1 + ww > avail(&rows) {
            rows.push(std::mem::take(&mut row));
            row_width = 0;
        }
        if row_width > 0 {
            row.push((' ', Style::new()));
            row_width += 1;
        }
        for (c, s) in w {
            let cw = c.width().unwrap_or(0);
            if row_width + cw > avail(&rows) && row_width > 0 {
                rows.push(std::mem::take(&mut row));
                row_width = 0;
            }
            row.push((c, s));
            row_width += cw;
        }
    }
    if !row.is_empty() || rows.is_empty() {
        rows.push(row);
    }
    rows.into_iter()
        .enumerate()
        .map(|(n, chars)| {
            let prefix = if n == 0 { first } else { cont };
            let mut spans: Vec<Span<'static>> = prefix
                .iter()
                .map(|(t, s)| Span::styled(t.clone(), *s))
                .collect();
            spans.extend(merge(chars));
            Line::from(spans)
        })
        .collect()
}

fn merge(chars: Vec<(char, Style)>) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut text = String::new();
    let mut style: Option<Style> = None;
    for (c, s) in chars {
        if style != Some(s) {
            if let Some(prev) = style
                && !text.is_empty()
            {
                spans.push(Span::styled(std::mem::take(&mut text), prev));
            }
            style = Some(s);
        }
        text.push(c);
    }
    if let Some(s) = style
        && !text.is_empty()
    {
        spans.push(Span::styled(text, s));
    }
    spans
}

/// A line that is never word-wrapped (code), split hard at the width.
fn push_hard(out: &mut Vec<Line<'static>>, text: &str, style: Style, width: usize) {
    let text = text.replace('\t', "    ");
    if text.is_empty() {
        out.push(Line::from(""));
        return;
    }
    let mut row = String::new();
    let mut w = 0;
    for c in text.chars() {
        let cw = c.width().unwrap_or(0);
        if w + cw > width {
            out.push(Line::styled(std::mem::take(&mut row), style));
            w = 0;
        }
        row.push(c);
        w += cw;
    }
    out.push(Line::styled(row, style));
}

fn cells(line: &str) -> Vec<String> {
    let t = line.trim();
    let t = t.strip_prefix('|').unwrap_or(t);
    let t = t.strip_suffix('|').unwrap_or(t);
    t.split('|').map(|c| c.trim().to_string()).collect()
}

fn is_separator(row: &[String]) -> bool {
    row.iter().all(|c| {
        let c = c.trim_matches(':');
        !c.is_empty() && c.chars().all(|ch| ch == '-')
    })
}

/// A table, aligned when it fits; otherwise one wrapped row per line with
/// the cells separated by ` · `.
fn table(lines: &[&str], width: usize) -> Vec<Line<'static>> {
    let rows: Vec<Vec<Vec<Piece>>> = lines
        .iter()
        .map(|l| cells(l))
        .filter(|r| !is_separator(r))
        .map(|r| r.iter().map(|c| inline(c)).collect())
        .collect();
    let has_header = lines.len() > 1 && is_separator(&cells(lines[1]));
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    let cell_width = |c: &Vec<Piece>| c.iter().map(|(t, _)| t.width()).sum::<usize>();
    let mut widths = vec![0; cols];
    for r in &rows {
        for (j, c) in r.iter().enumerate() {
            widths[j] = widths[j].max(cell_width(c));
        }
    }
    let total = widths.iter().sum::<usize>() + 2 * cols + 2 * cols.saturating_sub(1);
    let mut out = Vec::new();
    for (n, r) in rows.iter().enumerate() {
        let header = has_header && n == 0;
        let bold = |s: Style| {
            if header {
                s.add_modifier(Modifier::BOLD)
            } else {
                s
            }
        };
        if total <= width {
            let mut spans = vec![Span::from("  ")];
            for (j, c) in r.iter().enumerate() {
                if j > 0 {
                    spans.push(Span::from("  "));
                }
                for (t, s) in c {
                    spans.push(Span::styled(t.clone(), bold(*s)));
                }
                let pad = widths[j].saturating_sub(cell_width(c));
                if j + 1 < r.len() && pad > 0 {
                    spans.push(Span::from(" ".repeat(pad)));
                }
            }
            out.push(Line::from(spans));
        } else {
            let mut pieces: Vec<Piece> = Vec::new();
            for (j, c) in r.iter().enumerate() {
                if j > 0 {
                    pieces.push((" · ".into(), Style::new().add_modifier(Modifier::DIM)));
                }
                pieces.extend(c.iter().map(|(t, s)| (t.clone(), bold(*s))));
            }
            out.extend(wrap(
                &pieces,
                width,
                &[("  ".into(), Style::new())],
                &[("    ".into(), Style::new())],
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(lines: &[Line]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn headings_lists_and_wrapping() {
        let md = "# Title\n\nSome words that need to wrap across rows.\n\n- one item that is long enough\n  to continue\n2. second";
        let out = text(&render(md, 20));
        assert_eq!(out[0], "Title");
        assert_eq!(out[1], "");
        assert!(out.iter().all(|l| l.width() <= 20), "{out:#?}");
        assert!(out.contains(&"• one item that is".to_string()), "{out:#?}");
        assert!(out.contains(&"  long enough to".to_string()), "{out:#?}");
        assert!(out.contains(&"2. second".to_string()), "{out:#?}");
    }

    #[test]
    fn inline_markup_is_styled_and_removed() {
        let p = inline("a `code` and **bold** and [link](https://x.example)");
        let joined: String = p.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(joined, "a code and bold and link");
        assert_eq!(p[1].1, code_style());
        assert!(p[3].1.add_modifier.contains(Modifier::BOLD));
        // Lone markers stay as they are.
        let joined: String = inline("2 * 3 and a_b").into_iter().map(|p| p.0).collect();
        assert_eq!(joined, "2 * 3 and a_b");
    }

    #[test]
    fn code_tables_and_front_matter() {
        let md =
            "---\nname: x\n---\n\n```sh\nrun  it\n```\n\n| A | Bee |\n| --- | --- |\n| 1 | 2 |\n";
        let out = text(&render(md, 40));
        assert_eq!(&out[..3], &["---", "name: x", "---"]);
        assert!(out.contains(&"  run  it".to_string()), "{out:#?}");
        assert!(out.contains(&"  A  Bee".to_string()), "{out:#?}");
        assert!(out.contains(&"  1  2".to_string()), "{out:#?}");
        assert!(!out.iter().any(|l| l.contains("---") && l.contains('|')));
        // Too narrow: one row per line.
        let narrow = text(&render(
            "| alpha | beta | gamma |\n|---|---|---|\n| a | b | c |",
            12,
        ));
        assert!(
            narrow.iter().any(|l| l.contains("a · b · c")),
            "{narrow:#?}"
        );
    }
}
