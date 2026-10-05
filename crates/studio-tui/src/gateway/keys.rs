//! The Gateway module's keys, per context. The same table dispatches the keys
//! and describes them to the footer and the help overlay.

use crate::framework::keymap::Keymap;

use super::pane::Screen;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Down,
    Up,
    First,
    Last,
    NextScreen,
    PrevScreen,
    Reload,
    Days,
    Filter,
    SignIn,
    CancelSignIn,
    PickGateway,
    Register,
    RefreshOne,
    RefreshAll,
    Approve,
    Status,
    Access,
    Classify,
    Timeout,
    Url,
    Delete,
    EditDetails,
    CallTool,
    RevokeUser,
    KindFilter,
    // The result viewer.
    ScrollDown,
    ScrollUp,
    PageDown,
    PageUp,
    Top,
    Bottom,
    CloseViewer,
}

/// What the keymap depends on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Context {
    pub screen: Screen,
    /// `Some(true|false)` once `whoami` has answered.
    pub admin: Option<bool>,
    pub signed_out: bool,
    pub signing_in: bool,
    /// More than one `[[gateway]]`: the picker is offered.
    pub gateways: usize,
    /// The result viewer is open; it takes every key.
    pub viewing: bool,
}

/// The result viewer's keys.
fn viewer() -> Keymap<Key> {
    Keymap::new("Result")
        .bind(&["j", "down"], "scroll down", Key::ScrollDown)
        .footer("j/k", "scroll")
        .bind(&["k", "up"], "scroll up", Key::ScrollUp)
        .bind(&["space", "pgdn"], "page down", Key::PageDown)
        .footer("Space", "page")
        .bind(&["pgup"], "page up", Key::PageUp)
        .bind(&["g", "home"], "top", Key::Top)
        .bind(&["G", "end"], "bottom", Key::Bottom)
        .bind(&["esc", "q", "enter"], "close the result", Key::CloseViewer)
        .footer("Esc", "close")
}

pub fn keymap(cx: Context) -> Keymap<Key> {
    if cx.viewing {
        return viewer();
    }
    let admin = cx.admin == Some(true);
    let lists = matches!(
        cx.screen,
        Screen::Servers | Screen::Policy | Screen::Scopes | Screen::Connections
    );
    let servers = cx.screen == Screen::Servers;
    let mut km = Keymap::new("Gateway")
        .bind_if(
            cx.signing_in,
            &["esc"],
            "cancel the browser sign-in",
            Key::CancelSignIn,
        )
        .footer_if(cx.signing_in, "Esc", "cancel sign-in")
        .bind(
            &["L"],
            "sign in through the browser (again, for a new scope)",
            Key::SignIn,
        )
        .footer_if(cx.signed_out && !cx.signing_in, "L", "sign in")
        .bind(&["j", "down"], "move the selection down", Key::Down)
        .footer_if(lists, "j/k", "select")
        .bind(&["k", "up"], "move the selection up", Key::Up)
        .bind(&["g", "home"], "first row", Key::First)
        .bind(&["G", "end"], "last row", Key::Last)
        .bind(
            &["tab"],
            "next screen: Servers, Usage, Policy, Scopes, Connections",
            Key::NextScreen,
        )
        .footer("Tab", "screens")
        .bind(&["shift+tab"], "previous screen", Key::PrevScreen)
        .bind(&["r"], "refresh from the gateway", Key::Reload)
        .footer_if(!(servers && admin), "r", "refresh")
        .bind_if(
            matches!(cx.screen, Screen::Usage | Screen::Policy),
            &["d"],
            "days: Usage 1/7/30, Policy 1/7",
            Key::Days,
        )
        .footer_if(
            matches!(cx.screen, Screen::Usage | Screen::Policy),
            "d",
            "days",
        )
        .bind_if(
            cx.screen == Screen::Policy,
            &["f"],
            "filter: all, deny, flag",
            Key::Filter,
        )
        .footer_if(cx.screen == Screen::Policy, "f", "filter")
        .bind_if(
            cx.screen == Screen::Connections,
            &["f"],
            "filter by kind",
            Key::KindFilter,
        )
        .footer_if(cx.screen == Screen::Connections, "f", "kind")
        .bind_if(
            cx.gateways > 1,
            &["p"],
            "pick another gateway",
            Key::PickGateway,
        )
        .footer_if(cx.gateways > 1, "p", "gateway");
    if servers {
        km = km
            .bind(
                &["C"],
                "call a tool through the gateway (write tools ask first)",
                Key::CallTool,
            )
            // An admin's footer is full; the help lists it.
            .footer_if(cx.admin == Some(false), "C", "call");
    }
    if cx.screen == Screen::Connections {
        km = km
            .section("Connections (admin)")
            .bind(
                &["R"],
                "revoke everything the gateway holds for a user (type the sub to confirm)",
                Key::RevokeUser,
            )
            .footer_if(admin, "R", "revoke user");
    }
    if servers {
        km = km
            .section("Servers (admin)")
            .bind(&["a"], "register a server", Key::Register)
            .footer_if(admin, "a", "add")
            .bind(&["x"], "refresh the selected server", Key::RefreshOne)
            .footer_if(admin, "x", "refresh")
            .bind(&["X"], "refresh every server", Key::RefreshAll)
            .bind(
                &["A"],
                "approve the scope drift a refresh found",
                Key::Approve,
            )
            .bind(&["s"], "status: active, disabled, quarantined", Key::Status)
            .footer_if(admin, "s", "status")
            .bind(&["o"], "toggle read-only / read-write", Key::Access)
            .footer_if(admin, "o", "access")
            .bind(&["c"], "classify one of the server's tools", Key::Classify)
            .footer_if(admin, "c", "classify")
            .bind(&["t"], "edit the call timeout", Key::Timeout)
            .footer_if(admin, "t", "timeout")
            .bind(
                &["e"],
                "edit the display name and description",
                Key::EditDetails,
            )
            .footer_if(admin, "e", "edit")
            .bind(&["u"], "change the URL (type the id to confirm)", Key::Url)
            .footer_if(admin, "u", "url")
            .bind(&["D"], "delete (type the id to confirm)", Key::Delete)
            .footer_if(admin, "D", "delete");
    }
    km
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::framework::keymap::footer_text;
    use crossterm::event::{KeyCode, KeyEvent};

    fn cx(screen: Screen, admin: Option<bool>) -> Context {
        Context {
            screen,
            admin,
            signed_out: false,
            signing_in: false,
            gateways: 1,
            viewing: false,
        }
    }

    #[test]
    fn new_screens_and_the_viewer_have_their_own_keys() {
        let conns = keymap(cx(Screen::Connections, Some(true)));
        let r = KeyEvent::new(KeyCode::Char('R'), crossterm::event::KeyModifiers::SHIFT);
        assert_eq!(conns.resolve(&r), Some(Key::RevokeUser));
        assert!(footer_text(&conns.hints()).contains("R revoke user"));
        assert_eq!(
            conns.resolve(&KeyEvent::from(KeyCode::Char('f'))),
            Some(Key::KindFilter)
        );
        assert_eq!(keymap(cx(Screen::Servers, Some(true))).resolve(&r), None);
        let servers = footer_text(&keymap(cx(Screen::Servers, Some(true))).hints());
        assert!(servers.contains("e edit"), "{servers}");
        let viewer = footer_text(&keymap(cx(Screen::Servers, Some(false))).hints());
        assert!(viewer.contains("C call"), "{viewer}");
        let mut c = cx(Screen::Servers, Some(true));
        c.viewing = true;
        let km = keymap(c);
        assert_eq!(
            km.resolve(&KeyEvent::from(KeyCode::Char('q'))),
            Some(Key::CloseViewer)
        );
        assert_eq!(km.resolve(&KeyEvent::from(KeyCode::Char('a'))), None);
    }

    #[test]
    fn admin_keys_live_on_the_servers_screen_and_show_only_for_admins() {
        let admin = footer_text(&keymap(cx(Screen::Servers, Some(true))).hints());
        assert!(
            admin.contains("a add") && admin.contains("D delete"),
            "{admin}"
        );
        let viewer = keymap(cx(Screen::Servers, Some(false)));
        let text = footer_text(&viewer.hints());
        assert!(
            !text.contains("a add") && text.contains("r refresh"),
            "{text}"
        );
        // Still bound, so pressing one can say why it is refused.
        let a = KeyEvent::from(KeyCode::Char('a'));
        assert_eq!(viewer.resolve(&a), Some(Key::Register));
        assert_eq!(keymap(cx(Screen::Usage, Some(true))).resolve(&a), None);
    }

    #[test]
    fn screen_specific_keys() {
        let policy = keymap(cx(Screen::Policy, Some(true)));
        assert_eq!(
            policy.resolve(&KeyEvent::from(KeyCode::Char('f'))),
            Some(Key::Filter)
        );
        let usage = keymap(cx(Screen::Usage, Some(true)));
        assert_eq!(usage.resolve(&KeyEvent::from(KeyCode::Char('f'))), None);
        assert!(footer_text(&usage.hints()).contains("d days"));
    }

    #[test]
    fn sign_in_and_picker_hints_follow_the_state() {
        let mut c = cx(Screen::Servers, None);
        c.signed_out = true;
        c.gateways = 2;
        let text = footer_text(&keymap(c).hints());
        assert!(
            text.contains("L sign in") && text.contains("p gateway"),
            "{text}"
        );
        c.signing_in = true;
        let km = keymap(c);
        assert!(footer_text(&km.hints()).contains("Esc cancel sign-in"));
        assert_eq!(
            km.resolve(&KeyEvent::from(KeyCode::Esc)),
            Some(Key::CancelSignIn)
        );
    }
}
