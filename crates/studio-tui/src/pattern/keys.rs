//! The Pattern module's keys, per view. The same table dispatches the keys
//! and describes them to the footer and the help overlay.

use crate::framework::keymap::Keymap;

use super::View;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    NextView,
    PrevView,
    Refresh,
    /// Scroll the text on screen (Overview, Changelog, a skill doc, or the
    /// focused conformance pane).
    ScrollDown,
    ScrollUp,
    PageDown,
    PageUp,
    Top,
    Bottom,
    /// Move the selection (skill doc, repo, security file).
    Down,
    Up,
    /// Conformance: move between the repo table and the security files.
    Files,
    Repos,
    BlessStore,
    BlessFull,
}

/// Where the Conformance view's keys go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    Repos,
    Files,
}

pub fn keymap(view: View, focus: Focus) -> Keymap<Key> {
    let km = Keymap::new("Pattern")
        .bind(
            &["tab"],
            "next view: Overview, Changelog, Skill docs, Conformance",
            Key::NextView,
        )
        .footer("Tab", "views")
        .bind(&["shift+tab"], "previous view", Key::PrevView)
        .bind(
            &["r"],
            "reload the pack and re-run the conformance checks",
            Key::Refresh,
        )
        .footer_if(view == View::Conformance, "r", "re-run");
    match view {
        View::Overview | View::Changelog => km
            .section(view.title())
            .bind(&["j", "down"], "scroll down", Key::ScrollDown)
            .footer("j/k", "scroll")
            .bind(&["k", "up"], "scroll up", Key::ScrollUp)
            .bind(&["pgdn", "space"], "page down", Key::PageDown)
            .bind(&["pgup"], "page up", Key::PageUp)
            .bind(&["g", "home"], "top", Key::Top)
            .bind(&["G", "end"], "bottom", Key::Bottom),
        View::Skill => km
            .section("Skill docs")
            .bind(&["j", "down"], "next document", Key::Down)
            .footer("j/k", "document")
            .bind(&["k", "up"], "previous document", Key::Up)
            .bind(&["J"], "scroll the document down", Key::ScrollDown)
            .footer("J/K", "scroll")
            .bind(&["K"], "scroll the document up", Key::ScrollUp)
            .bind(&["pgdn", "space"], "page down", Key::PageDown)
            .bind(&["pgup"], "page up", Key::PageUp)
            .bind(&["g", "home"], "top of the document", Key::Top)
            .bind(&["G", "end"], "bottom of the document", Key::Bottom),
        View::Conformance => {
            let km = km.section("Conformance");
            let km = match focus {
                Focus::Repos => km
                    .bind(&["j", "down"], "next repo", Key::Down)
                    .footer("j/k", "repo")
                    .bind(&["k", "up"], "previous repo", Key::Up)
                    .bind(
                        &["enter", "l"],
                        "select the repo's security files (show their diffs)",
                        Key::Files,
                    )
                    .footer("Enter", "files")
                    .bind(&["J"], "scroll the checks down", Key::ScrollDown)
                    .bind(&["K"], "scroll the checks up", Key::ScrollUp),
                Focus::Files => km
                    .bind(&["esc", "h"], "back to the repo table", Key::Repos)
                    .footer("Esc", "repos")
                    .bind(&["j", "down"], "next security file", Key::Down)
                    .footer("j/k", "file")
                    .bind(&["k", "up"], "previous security file", Key::Up)
                    .bind(&["J"], "scroll the diff down", Key::ScrollDown)
                    .footer("J/K", "scroll")
                    .bind(&["K"], "scroll the diff up", Key::ScrollUp),
            };
            km.bind(&["pgdn", "space"], "page down", Key::PageDown)
                .bind(&["pgup"], "page up", Key::PageUp)
                .section("Conformance (writes)")
                .bind(
                    &["b"],
                    "bless the repo into the instance's store only (the repo is untouched)",
                    Key::BlessStore,
                )
                .footer("b", "bless store")
                .bind(
                    &["B"],
                    "full bless: also rewrite the repo's conformance.json (type the name)",
                    Key::BlessFull,
                )
                .footer("B", "full bless")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::framework::keymap::footer_text;
    use crossterm::event::{KeyCode, KeyEvent};

    #[test]
    fn each_view_has_its_own_keys() {
        let b = KeyEvent::from(KeyCode::Char('b'));
        assert_eq!(keymap(View::Overview, Focus::Repos).resolve(&b), None);
        assert_eq!(
            keymap(View::Conformance, Focus::Repos).resolve(&b),
            Some(Key::BlessStore)
        );
        let footer = footer_text(&keymap(View::Conformance, Focus::Files).hints());
        assert!(
            footer.contains("Esc repos") && footer.contains("B full bless"),
            "{footer}"
        );
        // Esc is only taken while the files have the focus, so it still quits.
        let esc = KeyEvent::from(KeyCode::Esc);
        assert_eq!(keymap(View::Conformance, Focus::Repos).resolve(&esc), None);
        assert_eq!(
            keymap(View::Skill, Focus::Repos).resolve(&KeyEvent::from(KeyCode::Char('J'))),
            Some(Key::ScrollDown)
        );
    }
}
