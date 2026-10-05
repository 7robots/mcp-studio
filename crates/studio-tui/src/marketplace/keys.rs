//! The Marketplaces module's keys. The same table dispatches the keys and
//! describes them to the footer and the help overlay.

use crate::framework::keymap::Keymap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Down,
    Up,
    First,
    Last,
    Focus,
    View,
    Reload,
    Validate,
    Reconcile,
    Add,
    Draft,
    Edit,
    Deprecate,
    Reinstate,
    Remove,
    Regenerate,
    Publish,
}

/// What the keymap depends on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Context {
    /// The fleet view is showing.
    pub fleet: bool,
    /// The selected marketplace has an entry selected.
    pub entry: bool,
}

pub fn keymap(cx: Context) -> Keymap<Key> {
    let catalog = !cx.fleet;
    let entry = catalog && cx.entry;
    Keymap::new("Marketplaces")
        .bind(&["j", "down"], "move the selection down", Key::Down)
        .footer("j/k", "select")
        .bind(&["k", "up"], "move the selection up", Key::Up)
        .bind(&["home"], "first row", Key::First)
        .bind(&["end"], "last row", Key::Last)
        .bind_if(
            catalog,
            &["tab", "shift+tab"],
            "switch between the marketplace list and its entries",
            Key::Focus,
        )
        .footer_if(catalog, "Tab", "list/entries")
        .bind(&["v"], "toggle the fleet view", Key::View)
        .footer("v", if cx.fleet { "catalogs" } else { "fleet" })
        .bind(&["r"], "reload from disk", Key::Reload)
        .section("Marketplace")
        .bind_if(
            catalog,
            &["V"],
            "validate (schemas + invariants)",
            Key::Validate,
        )
        .bind_if(
            catalog,
            &["R"],
            "reconcile + verify: differing files and diffs",
            Key::Reconcile,
        )
        .footer_if(catalog, "V/R", "validate/reconcile")
        .bind_if(catalog, &["a"], "add an entry (form)", Key::Add)
        .footer_if(catalog, "a", "add")
        .bind_if(
            catalog,
            &["A"],
            "add an entry drafted from a fleet server repo",
            Key::Draft,
        )
        .bind_if(
            catalog,
            &["P"],
            "publish unpushed commits (push or PR)",
            Key::Publish,
        )
        .footer_if(catalog, "P", "publish")
        .section("Entry")
        .bind_if(entry, &["e"], "edit the entry's fields", Key::Edit)
        .footer_if(entry, "e", "edit")
        .bind_if(entry, &["d"], "deprecate (with a reason)", Key::Deprecate)
        .bind_if(
            entry,
            &["u"],
            "reinstate a deprecated entry",
            Key::Reinstate,
        )
        .bind_if(entry, &["D"], "remove (type the slug)", Key::Remove)
        .bind_if(
            entry,
            &["g"],
            "regenerate the entry's files from server.yaml (fix drift)",
            Key::Regenerate,
        )
        .footer_if(entry, "d/u/D/g", "deprecate/reinstate/remove/regen")
}
