//! The Fleet module's keys. The same table dispatches the keys and describes
//! them to the footer and the help overlay.

use crate::framework::keymap::Keymap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Down,
    Up,
    First,
    Last,
    Focus,
    Unfocus,
    ScrollDown,
    ScrollUp,
    PageDown,
    PageUp,
    Refresh,
    FullRefresh,
    ProblemsOnly,
    SourceFilter,
    OpenUrl,
    OpenRepo,
}

/// What the keymap depends on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Context {
    /// The detail pane has the keyboard (j/k scroll it).
    pub detail: bool,
}

pub fn keymap(cx: Context) -> Keymap<Key> {
    let mut km = Keymap::new("Fleet");
    if cx.detail {
        km = km
            .bind(&["enter", "tab", "esc"], "back to the matrix", Key::Unfocus)
            .footer("Esc", "matrix")
            .bind(
                &["j", "down", "J"],
                "scroll the detail down",
                Key::ScrollDown,
            )
            .footer("j/k", "scroll")
            .bind(&["k", "up", "K"], "scroll the detail up", Key::ScrollUp)
            .bind(&["g", "home"], "top of the detail", Key::First)
            .bind(&["G", "end"], "bottom of the detail", Key::Last);
    } else {
        km = km
            .bind(&["j", "down"], "select the next server", Key::Down)
            .footer("j/k", "select")
            .bind(&["k", "up"], "select the previous server", Key::Up)
            .bind(&["g", "home"], "first server", Key::First)
            .bind(&["G", "end"], "last server", Key::Last)
            .bind(
                &["enter", "tab"],
                "focus the detail pane (to scroll it)",
                Key::Focus,
            )
            .footer("Enter", "detail")
            .bind(&["J"], "scroll the detail down", Key::ScrollDown)
            .bind(&["K"], "scroll the detail up", Key::ScrollUp);
    }
    km.bind(&["pgdn"], "scroll the detail a page down", Key::PageDown)
        .bind(&["pgup"], "scroll the detail a page up", Key::PageUp)
        .bind(
            &["r"],
            "refresh (served from the cache while it is fresh)",
            Key::Refresh,
        )
        .footer("r", "refresh")
        .bind(
            &["R"],
            "re-probe every server, ignoring the cache",
            Key::FullRefresh,
        )
        .footer("R", "re-probe")
        .bind(
            &["f"],
            "problems only: hide servers with nothing failing or warning",
            Key::ProblemsOnly,
        )
        .footer("f", "problems")
        .bind(
            &["s"],
            "source filter: all, then each source",
            Key::SourceFilter,
        )
        .footer("s", "source")
        .bind(&["o"], "open the server's URL in the browser", Key::OpenUrl)
        .footer("o", "open")
        .bind(&["O"], "open the server's repo on GitHub", Key::OpenRepo)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::framework::keymap::footer_text;
    use crossterm::event::{KeyCode, KeyEvent};

    #[test]
    fn j_selects_in_the_matrix_and_scrolls_in_the_detail() {
        let j = KeyEvent::from(KeyCode::Char('j'));
        assert_eq!(
            keymap(Context { detail: false }).resolve(&j),
            Some(Key::Down)
        );
        let detail = keymap(Context { detail: true });
        assert_eq!(detail.resolve(&j), Some(Key::ScrollDown));
        assert_eq!(
            detail.resolve(&KeyEvent::from(KeyCode::Esc)),
            Some(Key::Unfocus)
        );
        assert!(footer_text(&detail.hints()).contains("Esc matrix"));
        // Esc in the matrix is not bound: it falls through to quit.
        assert_eq!(
            keymap(Context { detail: false }).resolve(&KeyEvent::from(KeyCode::Esc)),
            None
        );
    }
}
