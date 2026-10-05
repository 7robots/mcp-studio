//! The Pattern module: the instance's pattern pack and the fleet's
//! conformance to it, in four views switched with Tab / Shift-Tab:
//!
//! - **Overview**: the pack's version, paths, variables with the instance's
//!   values (anything that looks like a credential is hidden), pins, dev
//!   pins, overrides, major floors, forbidden dependencies, security files,
//!   required files and scripts, and the wrangler rules.
//! - **Changelog**: the pack's `CHANGELOG.md`, rendered.
//! - **Skill docs**: `SKILL.md` and its references, rendered with the
//!   instance's values substituted.
//! - **Conformance**: one row per fleet repo (the list `mcp-studio pattern`
//!   uses) with its version, hash gate, drift and lint counts; the selected
//!   repo's checks; and a security file's current diff against the blessed
//!   one. `b` blesses the repo into the instance's store only, `B` also
//!   rewrites the repo's `conformance.json` (typed confirmation).
//!
//! Loading the pack, running the checks and blessing all block on the file
//! system, so they run in `spawn_blocking`, each into a generation-guarded
//! [`Slot`].

pub mod data;
pub mod keys;
pub mod markdown;
mod ui;

use std::sync::Arc;

use crossterm::event::KeyEvent;
use ratatui::Frame;
use ratatui::layout::Rect;
use studio_core::Instance;
use studio_gateway::util::{ago, now_unix};
use studio_pattern::BlessOutcome;

use crate::ModuleId;
use crate::framework::component::{Component, Handled};
use crate::framework::cx::{Cx, Payload};
use crate::framework::keymap::Hint;
use crate::framework::overlay::{Confirm, Overlay, Step};
use crate::framework::slot::Slot;

use data::{PackData, RepoReport};
pub use keys::Focus;
use keys::Key;

pub const ID: ModuleId = "pattern";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Overview,
    Changelog,
    Skill,
    Conformance,
}

impl View {
    pub const ALL: [View; 4] = [
        View::Overview,
        View::Changelog,
        View::Skill,
        View::Conformance,
    ];

    pub fn title(self) -> &'static str {
        match self {
            View::Overview => "Overview",
            View::Changelog => "Changelog",
            View::Skill => "Skill docs",
            View::Conformance => "Conformance",
        }
    }

    fn step(self, by: isize) -> View {
        let i = View::ALL.iter().position(|v| *v == self).unwrap_or(0) as isize;
        View::ALL[(i + by).rem_euclid(View::ALL.len() as isize) as usize]
    }
}

/// The scrollable texts, each with its own offset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pane {
    Overview,
    Changelog,
    Doc,
    Checks,
    Diff,
}

const PANES: usize = 5;

#[derive(Default, Clone, Copy, Debug)]
pub(crate) struct Scroll {
    pub offset: usize,
    /// Rows the text took at the last draw, and rows visible.
    pub content: usize,
    pub viewport: usize,
}

#[derive(Debug)]
enum PatternMsg {
    Pack {
        generation: u64,
        result: Result<Arc<PackData>, String>,
    },
    Checks {
        generation: u64,
        result: Result<Vec<RepoReport>, String>,
    },
    Blessed {
        repo: String,
        store_only: bool,
        result: Result<BlessOutcome, String>,
    },
}

/// What a confirmation is for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Purpose {
    pub repo: String,
    pub store_only: bool,
}

pub struct PatternModule {
    instance: Arc<Instance>,
    pub view: View,
    pub pack: Slot<Arc<PackData>>,
    pub reports: Slot<Vec<RepoReport>>,
    /// Index into the fleet's repos.
    pub repo_selected: usize,
    /// Index into the selected repo's security files.
    pub file_selected: usize,
    pub doc_selected: usize,
    pub focus: Focus,
    /// A bless is running (for this repo).
    pub busy: Option<String>,
    pub(crate) overlay: Option<(Overlay, Purpose)>,
    pub(crate) scroll: [Scroll; PANES],
    /// Toast when the next check run finishes (it was asked for).
    announce: bool,
}

impl PatternModule {
    pub fn new(instance: Arc<Instance>) -> Self {
        PatternModule {
            instance,
            view: View::Overview,
            pack: Slot::default(),
            reports: Slot::default(),
            repo_selected: 0,
            file_selected: 0,
            doc_selected: 0,
            focus: Focus::Repos,
            busy: None,
            overlay: None,
            scroll: [Scroll::default(); PANES],
            announce: false,
        }
    }

    /// The open overlay, if any.
    pub fn overlay(&self) -> Option<&Overlay> {
        self.overlay.as_ref().map(|(o, _)| o)
    }

    pub fn pack_data(&self) -> Option<&Arc<PackData>> {
        self.pack.data.as_ref()
    }

    pub fn selected_report(&self) -> Option<&RepoReport> {
        self.reports.data.as_ref()?.get(self.repo_selected)
    }

    fn keys(&self) -> crate::framework::keymap::Keymap<Key> {
        keys::keymap(self.view, self.focus)
    }

    fn load_pack(&mut self, cx: &Cx<'_>) {
        let instance = self.instance.clone();
        let generation = self.pack.begin();
        cx.spawn(async move {
            let result = blocking(move || data::load_pack(&instance).map(Arc::new)).await;
            PatternMsg::Pack { generation, result }
        });
    }

    fn run_checks(&mut self, cx: &Cx<'_>) {
        let Some(pack) = self.pack.data.clone() else {
            return;
        };
        let generation = self.reports.begin();
        cx.spawn(async move {
            let result = blocking(move || data::run_checks(&pack)).await;
            PatternMsg::Checks { generation, result }
        });
    }

    fn scroll_pane(&self) -> Pane {
        match (self.view, self.focus) {
            (View::Overview, _) => Pane::Overview,
            (View::Changelog, _) => Pane::Changelog,
            (View::Skill, _) => Pane::Doc,
            (View::Conformance, Focus::Repos) => Pane::Checks,
            (View::Conformance, Focus::Files) => Pane::Diff,
        }
    }

    fn scroll_by(&mut self, pane: Pane, delta: isize) {
        let s = &mut self.scroll[pane as usize];
        let max = s.content.saturating_sub(s.viewport);
        s.offset = s.offset.saturating_add_signed(delta).min(max);
    }

    fn page(&self, pane: Pane) -> isize {
        self.scroll[pane as usize].viewport.saturating_sub(1).max(1) as isize
    }

    /// Moves a selection, resetting what it shows.
    fn select(&mut self, delta: isize) {
        let (index, len, reset): (&mut usize, usize, &[Pane]) = match (self.view, self.focus) {
            (View::Skill, _) => (
                &mut self.doc_selected,
                self.pack.data.as_ref().map_or(0, |p| p.docs.len()),
                &[Pane::Doc],
            ),
            (View::Conformance, Focus::Repos) => (
                &mut self.repo_selected,
                self.reports.data.as_ref().map_or(0, Vec::len),
                &[Pane::Checks, Pane::Diff],
            ),
            (View::Conformance, Focus::Files) => {
                let len = self
                    .reports
                    .data
                    .as_ref()
                    .and_then(|r| r.get(self.repo_selected))
                    .map_or(0, |r| r.files.len());
                (&mut self.file_selected, len, &[Pane::Diff])
            }
            _ => return,
        };
        let before = *index;
        *index = index
            .saturating_add_signed(delta)
            .min(len.saturating_sub(1));
        if *index != before {
            for p in reset {
                self.scroll[*p as usize].offset = 0;
            }
            if self.view == View::Conformance && self.focus == Focus::Repos {
                self.file_selected = 0;
            }
        }
    }

    fn confirm_bless(&mut self, store_only: bool, cx: &mut Cx<'_>) {
        if let Some(repo) = &self.busy {
            cx.toast(format!("Still blessing {repo}"));
            return;
        }
        let (Some(pack), Some(report)) = (self.pack.data.clone(), self.selected_report()) else {
            cx.toast("Nothing to bless yet");
            return;
        };
        if let Some(why) = &report.missing {
            cx.error(format!("{}: {why}", report.name));
            return;
        }
        let repo = report.name.clone();
        let store = pack.store_dir.join(&repo);
        let overlay = if store_only {
            Overlay::Confirm(Confirm::new(
                format!("Bless {repo} (store only)"),
                vec![
                    format!("Regenerates {repo}'s blessed diffs in the instance's store:"),
                    format!("  {}", store.display()),
                    "The repo's working tree is not touched: its conformance.json stays".into(),
                    "as it is, so the repo's own hash gate is unchanged.".into(),
                ],
            ))
        } else {
            let manifest = report
                .dir
                .join(&pack.pack.manifest.security.conformance_file);
            Overlay::Confirm(Confirm::typed(
                format!("Full bless {repo}"),
                vec![
                    format!("Regenerates {repo}'s blessed diffs in the instance's store:"),
                    format!("  {}", store.display()),
                    "AND rewrites the repo's manifest in its working tree:".into(),
                    format!("  {}", manifest.display()),
                    format!(
                        "with pack version {} and the current security-file hashes.",
                        pack.pack.version()
                    ),
                    "This changes the repo's checkout; review and commit it there.".into(),
                    format!("Type {repo} to confirm."),
                ],
                repo.clone(),
            ))
        };
        self.overlay = Some((overlay, Purpose { repo, store_only }));
    }

    fn bless(&mut self, purpose: Purpose, cx: &mut Cx<'_>) {
        let Some(pack) = self.pack.data.clone() else {
            return;
        };
        self.busy = Some(purpose.repo.clone());
        cx.toast(format!("Blessing {}...", purpose.repo));
        let Purpose { repo, store_only } = purpose;
        cx.spawn(async move {
            let name = repo.clone();
            let result = blocking(move || data::bless(&pack, &name, store_only)).await;
            PatternMsg::Blessed {
                repo,
                store_only,
                result,
            }
        });
    }

    fn blessed(
        &mut self,
        repo: String,
        store_only: bool,
        result: Result<BlessOutcome, String>,
        cx: &mut Cx<'_>,
    ) {
        self.busy = None;
        match result {
            Ok(o) => {
                let file = self
                    .pack
                    .data
                    .as_ref()
                    .map(|p| p.pack.manifest.security.conformance_file.clone())
                    .unwrap_or_default();
                let manifest = if o.manifest_written {
                    format!("{file} rewritten")
                } else if o.manifest_stale {
                    format!("{file} is stale (kept: store only)")
                } else {
                    format!("{file} unchanged")
                };
                let mode = if store_only { "store only" } else { "full" };
                cx.toast(format!(
                    "{repo}: blessed ({mode}; {} stored diff(s) changed; {manifest})",
                    o.diffs_changed.len()
                ));
                self.run_checks(cx);
            }
            Err(e) => cx.error(format!("Bless {repo} failed: {e}")),
        }
    }
}

/// Runs `f` on the blocking pool.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| format!("the task failed: {e}"))?
}

impl Component for PatternModule {
    fn id(&self) -> ModuleId {
        ID
    }

    fn title(&self) -> String {
        "Pattern".into()
    }

    fn start(&mut self, cx: &mut Cx<'_>) {
        self.load_pack(cx);
    }

    fn handle_key(&mut self, key: KeyEvent, cx: &mut Cx<'_>) -> Handled {
        if let Some((mut overlay, purpose)) = self.overlay.take() {
            match overlay.handle_key(key) {
                Step::Open => self.overlay = Some((overlay, purpose)),
                Step::Cancel => cx.toast("Bless cancelled"),
                Step::Done(_) => self.bless(purpose, cx),
            }
            return Handled::Yes;
        }
        let Some(k) = self.keys().resolve(&key) else {
            return Handled::No;
        };
        let pane = self.scroll_pane();
        match k {
            Key::NextView => self.view = self.view.step(1),
            Key::PrevView => self.view = self.view.step(-1),
            Key::Refresh => {
                self.announce = true;
                self.load_pack(cx);
                cx.toast("Reloading the pack and re-running the checks");
            }
            Key::ScrollDown => self.scroll_by(pane, 1),
            Key::ScrollUp => self.scroll_by(pane, -1),
            Key::PageDown => self.scroll_by(pane, self.page(pane)),
            Key::PageUp => self.scroll_by(pane, -self.page(pane)),
            Key::Top => self.scroll[pane as usize].offset = 0,
            Key::Bottom => self.scroll_by(pane, isize::MAX / 2),
            Key::Down => self.select(1),
            Key::Up => self.select(-1),
            Key::Files => {
                if self.selected_report().is_some_and(|r| !r.files.is_empty()) {
                    self.focus = Focus::Files;
                } else {
                    cx.toast("No security files to show for this repo");
                }
            }
            Key::Repos => self.focus = Focus::Repos,
            Key::BlessStore => self.confirm_bless(true, cx),
            Key::BlessFull => self.confirm_bless(false, cx),
        }
        Handled::Yes
    }

    fn handle_msg(&mut self, msg: Payload, cx: &mut Cx<'_>) {
        let Ok(msg) = msg.downcast::<PatternMsg>() else {
            return;
        };
        match *msg {
            PatternMsg::Pack { generation, result } => {
                let ok = result.is_ok();
                if let Some(err) = self.pack.finish(generation, result) {
                    cx.error(format!("Pattern pack: {err}"));
                    self.announce = false;
                } else if ok && self.pack.is_current(generation) {
                    let docs = self.pack.data.as_ref().map_or(0, |p| p.docs.len());
                    self.doc_selected = self.doc_selected.min(docs.saturating_sub(1));
                    self.run_checks(cx);
                }
            }
            PatternMsg::Checks { generation, result } => {
                let ok = result.is_ok();
                if let Some(err) = self.reports.finish(generation, result) {
                    cx.error(format!("Conformance checks failed: {err}"));
                } else if ok && self.reports.is_current(generation) {
                    let n = self.reports.data.as_ref().map_or(0, Vec::len);
                    self.repo_selected = self.repo_selected.min(n.saturating_sub(1));
                    let files = self.selected_report().map_or(0, |r| r.files.len());
                    self.file_selected = self.file_selected.min(files.saturating_sub(1));
                    if files == 0 {
                        self.focus = Focus::Repos;
                    }
                    if std::mem::take(&mut self.announce) {
                        cx.toast(format!("Checked {n} repo(s)"));
                    }
                }
            }
            PatternMsg::Blessed {
                repo,
                store_only,
                result,
            } => self.blessed(repo, store_only, result, cx),
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) {
        ui::draw(frame, self, area);
        if let Some((overlay, _)) = &self.overlay {
            overlay.draw(frame);
        }
    }

    fn keymap(&self) -> Vec<Hint> {
        self.keys().hints()
    }

    fn captures_input(&self) -> bool {
        self.overlay.is_some()
    }

    fn status(&self) -> Option<String> {
        let pack = self.pack.data.as_ref()?;
        let checks = if let Some(repo) = &self.busy {
            format!("blessing {repo}...")
        } else if self.reports.loading {
            "checking...".into()
        } else {
            match self.reports.loaded_at {
                Some(at) => format!("checked {}", ago(Some(at), now_unix())),
                None => "not checked".into(),
            }
        };
        Some(format!(
            "{} {} · {checks}",
            pack.pack.name(),
            pack.pack.version()
        ))
    }
}
