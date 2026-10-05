//! The Marketplaces module: every `[[marketplace]]` with its entries,
//! validation and reconciliation; the entry lifecycle (add, draft from a
//! fleet repo, edit, deprecate, reinstate, remove, regenerate) as previewed
//! change sets committed to the local clone; and publishing those commits
//! (push or PR, per the marketplace's `publish` mode).
//!
//! Every change is planned off the UI thread into a
//! [`studio_marketplace::ChangeSet`], shown in a preview (per-file diffs and
//! the commit message), and only applied (`ops::apply_checked`) and
//! committed (`ops::commit`) once confirmed. A missing or dirty clone is
//! refused before anything is planned or written.

pub mod data;
pub mod forms;
pub mod keys;
pub mod pager;
mod ui;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context as _, anyhow, bail};
use crossterm::event::KeyEvent;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Stylize};
use ratatui::text::Line;
use studio_core::check::Status;
use studio_core::config::PublishMode;
use studio_core::{Instance, Secret};
use studio_gateway::util::{ago, now_unix};
use studio_marketplace::ops::{self, Verb};
use studio_marketplace::publish::{self, Gh, GithubApi, Route};
use studio_marketplace::reconcile::DiffKind;
use studio_marketplace::{ChangeSet, Configured, Entry, Location, Marketplace, git};

use crate::framework::component::{Component, Handled, ModuleId};
use crate::framework::cx::{Cx, Payload};
use crate::framework::keymap::Hint;
use crate::framework::overlay::{Answer, Confirm, Form, Overlay, PickItem, Picker, Step};
use crate::framework::slot::Slot;

use data::{FleetRow, Market};
use keys::Key;
use pager::Pager;

/// The module's id.
pub const ID: ModuleId = "marketplaces";

/// Which list `j`/`k` move in the catalog view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    Markets,
    Entries,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    /// Marketplaces, their entries, the selected entry.
    Catalog,
    /// Fleet servers and the marketplaces that list them.
    Fleet,
}

/// Everything loaded from disk in one go.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub markets: Vec<Market>,
    pub fleet: Vec<FleetRow>,
}

/// A publish, decided and waiting for confirmation.
#[derive(Debug, Clone)]
pub struct PublishPlan {
    pub market: String,
    configured: Configured,
    pub route: Route,
    /// `origin/<branch>` before the publish.
    base: String,
    head: String,
    /// Subjects of the unpushed commits, oldest first.
    pub subjects: Vec<String>,
    token: Option<Secret>,
}

type Job<T> = Box<dyn FnOnce() -> anyhow::Result<T> + Send>;

#[derive(Debug)]
enum MkMsg {
    Loaded {
        generation: u64,
        result: Result<Snapshot, String>,
    },
    Planned {
        market: String,
        result: Result<ChangeSet, String>,
    },
    Applied {
        market: String,
        result: Result<(String, ChangeSet), String>,
    },
    Report {
        title: String,
        result: Result<Vec<Line<'static>>, String>,
    },
    Drafted {
        market: String,
        result: Result<Box<Entry>, String>,
    },
    PublishPlanned(Result<Box<PublishPlan>, String>),
    Published {
        market: String,
        result: Result<String, String>,
    },
}

/// What an open framework overlay is for.
#[derive(Clone, Debug)]
enum Purpose {
    Add {
        market: String,
    },
    Edit {
        market: String,
        slug: String,
        entry: Box<Entry>,
    },
    Deprecate {
        market: String,
        slug: String,
    },
    Remove {
        market: String,
        slug: String,
    },
    PickDraft {
        market: String,
        /// (repo dir, slug) per picker row.
        members: Vec<(std::path::PathBuf, Option<String>)>,
    },
    Publish(Box<PublishPlan>),
}

/// A preview waiting for confirmation.
#[derive(Debug, Clone)]
struct Pending {
    market: String,
    cs: ChangeSet,
}

pub struct MarketplaceModule {
    instance: Arc<Instance>,
    pub snapshot: Slot<Snapshot>,
    pub view: View,
    pub focus: Focus,
    pub market: usize,
    pub entry: usize,
    pub fleet_row: usize,
    overlay: Option<(Overlay, Purpose)>,
    pager: Option<(Pager, Option<Pending>)>,
    /// The form a plan came from, reopened with the error if planning fails.
    stashed: Option<(Form, Purpose)>,
    /// What is running (one change at a time).
    pub busy: Option<String>,
    /// Change sets this session committed, by commit, for PR titles.
    committed: HashMap<String, ChangeSet>,
    /// Select this (market, slug) after the next load.
    want: Option<(String, Option<String>)>,
    started: bool,
}

impl MarketplaceModule {
    pub fn new(instance: Arc<Instance>) -> Self {
        MarketplaceModule {
            instance,
            snapshot: Slot::default(),
            view: View::Catalog,
            focus: Focus::Markets,
            market: 0,
            entry: 0,
            fleet_row: 0,
            overlay: None,
            pager: None,
            stashed: None,
            busy: None,
            committed: HashMap::new(),
            want: None,
            started: false,
        }
    }

    pub fn markets(&self) -> &[Market] {
        self.snapshot
            .data
            .as_ref()
            .map(|s| s.markets.as_slice())
            .unwrap_or(&[])
    }

    pub fn fleet(&self) -> &[FleetRow] {
        self.snapshot
            .data
            .as_ref()
            .map(|s| s.fleet.as_slice())
            .unwrap_or(&[])
    }

    pub fn selected_market(&self) -> Option<&Market> {
        self.markets().get(self.market)
    }

    pub fn selected_entry(&self) -> Option<&data::EntryView> {
        self.selected_market()?.entries.get(self.entry)
    }

    /// The open framework overlay, if any.
    pub fn overlay(&self) -> Option<&Overlay> {
        self.overlay.as_ref().map(|(o, _)| o)
    }

    /// The open preview or report, if any.
    pub fn pager(&self) -> Option<&Pager> {
        self.pager.as_ref().map(|(p, _)| p)
    }

    /// Nothing loading or running.
    pub fn idle(&self) -> bool {
        self.busy.is_none() && !self.snapshot.loading
    }

    fn keys(&self) -> crate::framework::keymap::Keymap<Key> {
        keys::keymap(keys::Context {
            fleet: self.view == View::Fleet,
            entry: self.selected_entry().is_some(),
        })
    }

    // -- loading -------------------------------------------------------------

    fn load(&mut self, cx: &Cx<'_>) {
        if let Some(m) = self.selected_market() {
            let slug = self.selected_entry().map(|e| e.slug().to_string());
            self.want.get_or_insert((m.id().to_string(), slug));
        }
        let instance = self.instance.clone();
        self.snapshot.load(
            cx,
            async move {
                tokio::task::spawn_blocking(move || {
                    let markets = data::load_markets(&instance);
                    let fleet = data::fleet_rows(&instance, &markets);
                    Snapshot { markets, fleet }
                })
                .await
                .map_err(|e| e.to_string())
            },
            |generation, result| MkMsg::Loaded { generation, result },
        );
    }

    fn restore_selection(&mut self) {
        let Some((market, slug)) = self.want.take() else {
            self.clamp();
            return;
        };
        if let Some(i) = self.markets().iter().position(|m| m.id() == market) {
            self.market = i;
            self.entry = slug
                .and_then(|s| self.markets()[i].entries.iter().position(|e| e.slug() == s))
                .unwrap_or(self.entry);
        }
        self.clamp();
    }

    fn clamp(&mut self) {
        let markets = self.markets().len();
        self.market = self.market.min(markets.saturating_sub(1));
        let entries = self.selected_market().map_or(0, |m| m.entries.len());
        self.entry = self.entry.min(entries.saturating_sub(1));
        let fleet = self.fleet().len();
        self.fleet_row = self.fleet_row.min(fleet.saturating_sub(1));
    }

    // -- running work --------------------------------------------------------

    /// Runs blocking `job` on the blocking pool; `wrap` builds the message.
    fn run<T: Send + 'static>(
        &self,
        cx: &Cx<'_>,
        job: Job<T>,
        wrap: impl FnOnce(Result<T, String>) -> MkMsg + Send + 'static,
    ) {
        cx.spawn(async move {
            let result = match tokio::task::spawn_blocking(job).await {
                Ok(r) => r.map_err(|e| format!("{e:#}")),
                Err(e) => Err(e.to_string()),
            };
            wrap(result)
        });
    }

    fn market_by_id(&self, id: &str) -> Option<&Market> {
        self.markets().iter().find(|m| m.id() == id)
    }

    /// Refuses a change to `m` when its clone is missing or (as last seen)
    /// dirty; the job checks again before writing.
    fn writable(&self, m: &Market, cx: &mut Cx<'_>) -> bool {
        if let Some(what) = &self.busy {
            cx.toast(format!("Busy: {what}"));
            return false;
        }
        if let Err(e) = refuse(m) {
            cx.error(e);
            return false;
        }
        true
    }

    /// Plans a change off the UI thread; the result opens the preview.
    fn plan(
        &mut self,
        cx: &mut Cx<'_>,
        market: &str,
        what: String,
        plan: impl FnOnce(&Path, &Marketplace) -> anyhow::Result<ChangeSet> + Send + 'static,
    ) {
        let Some(m) = self.market_by_id(market) else {
            return;
        };
        let configured = m.configured.clone();
        let market = market.to_string();
        self.busy = Some(what);
        self.run(
            cx,
            Box::new(move || {
                let ws = open(&configured)?;
                plan(&ws.dir, &configured.market)
            }),
            move |result| MkMsg::Planned { market, result },
        );
    }

    fn apply(&mut self, cx: &mut Cx<'_>, pending: Pending) {
        let Some(m) = self.market_by_id(&pending.market) else {
            return;
        };
        let configured = m.configured.clone();
        let Pending { market, cs } = pending;
        self.busy = Some(format!("committing {}", cs.title()));
        self.run(
            cx,
            Box::new(move || {
                let ws = open(&configured)?;
                unchanged_since_plan(&ws.dir, &cs)?;
                ops::apply_checked(&ws.dir, &configured.market, &cs)?;
                let commit = ops::commit(&ws.dir, &cs)?;
                Ok((commit.head, cs))
            }),
            move |result| MkMsg::Applied { market, result },
        );
    }

    fn report(&mut self, cx: &mut Cx<'_>, title: String, job: Job<Vec<Line<'static>>>) {
        self.busy = Some(title.clone());
        self.run(cx, job, move |result| MkMsg::Report { title, result });
    }

    // -- actions -------------------------------------------------------------

    fn action(&mut self, key: Key, cx: &mut Cx<'_>) {
        let Some(m) = self.selected_market().cloned() else {
            cx.toast("No marketplace is configured");
            return;
        };
        let id = m.id().to_string();
        let entry = self.selected_entry().cloned();
        match key {
            Key::Validate => {
                if let Some(dir) = cloned_dir(&m, cx) {
                    let market = m.market().clone();
                    self.report(
                        cx,
                        format!("Validate {id}"),
                        Box::new(move || Ok(validation_lines(&dir, &market))),
                    );
                }
            }
            Key::Reconcile => {
                if let Some(dir) = cloned_dir(&m, cx) {
                    let market = m.market().clone();
                    self.report(
                        cx,
                        format!("Reconcile {id}"),
                        Box::new(move || reconcile_lines(&dir, &market)),
                    );
                }
            }
            Key::Add => {
                if self.writable(&m, cx) {
                    let form = forms::add_form(&format!("Add an entry to {id}"), None);
                    self.overlay = Some((Overlay::Form(form), Purpose::Add { market: id }));
                }
            }
            Key::Draft => {
                if !self.writable(&m, cx) {
                    return;
                }
                let members = studio_fleet::discover::configured_members(&self.instance);
                if members.is_empty() {
                    cx.toast("No fleet servers configured (fleet.include / [[fleet.server]])");
                    return;
                }
                let items = self.fleet_items(&members);
                let rows = members
                    .iter()
                    .map(|(repo, _)| {
                        let slug = self
                            .instance
                            .config
                            .fleet_server(repo)
                            .and_then(|s| s.marketplace_slug.clone());
                        (self.instance.repo_dir(repo), slug)
                    })
                    .collect();
                let picker = Picker::new(
                    format!("Draft an entry for {id} from a fleet server"),
                    items,
                    "draft",
                );
                self.overlay = Some((
                    Overlay::Picker(picker),
                    Purpose::PickDraft {
                        market: id,
                        members: rows,
                    },
                ));
            }
            Key::Publish => self.prepare_publish(&m, cx),
            Key::Edit | Key::Deprecate | Key::Reinstate | Key::Remove | Key::Regenerate => {
                let Some(e) = entry else {
                    cx.toast("No entry selected");
                    return;
                };
                if !self.writable(&m, cx) {
                    return;
                }
                let slug = e.slug().to_string();
                match key {
                    Key::Edit => {
                        let form = forms::edit_form(
                            &format!("Edit {slug} in {id}"),
                            &slug,
                            &e.plugin.entry,
                        );
                        self.overlay = Some((
                            Overlay::Form(form),
                            Purpose::Edit {
                                market: id,
                                slug,
                                entry: Box::new(e.plugin.entry.clone()),
                            },
                        ));
                    }
                    Key::Deprecate => {
                        let form = Form::new(format!("Deprecate {slug}")).field(
                            "reason",
                            "why, and what to use instead",
                            "",
                        );
                        self.overlay =
                            Some((Overlay::Form(form), Purpose::Deprecate { market: id, slug }));
                    }
                    Key::Reinstate => {
                        let s = slug.clone();
                        self.plan(
                            cx,
                            &id,
                            format!("planning reinstate {slug}"),
                            move |d, m| ops::plan_reinstate(d, m, &s),
                        );
                    }
                    Key::Remove => {
                        let confirm = Confirm::typed(
                            format!("Remove {slug}"),
                            vec![
                                format!(
                                    "Remove {slug} from {id}: its directory and both catalog entries."
                                ),
                                "This breaks existing installs. Deprecating is usually better (d)."
                                    .into(),
                                "A preview of the change follows before anything is written."
                                    .into(),
                            ],
                            slug.clone(),
                        );
                        self.overlay = Some((
                            Overlay::Confirm(confirm),
                            Purpose::Remove { market: id, slug },
                        ));
                    }
                    _ => {
                        let s = slug.clone();
                        self.plan(
                            cx,
                            &id,
                            format!("planning regenerate {slug}"),
                            move |d, m| ops::plan_regenerate(d, m, &s),
                        );
                    }
                }
            }
            _ => {}
        }
    }

    fn fleet_items(&self, members: &[(String, Vec<String>)]) -> Vec<PickItem> {
        members
            .iter()
            .map(|(repo, origins)| {
                let dir = self.instance.repo_dir(repo);
                let (tag, color) = if dir.is_dir() {
                    (origins.join("+"), Color::DarkGray)
                } else {
                    ("not cloned".to_string(), Color::Red)
                };
                PickItem::tagged(repo.clone(), tag, color)
            })
            .collect()
    }

    fn prepare_publish(&mut self, m: &Market, cx: &mut Cx<'_>) {
        if let Some(what) = &self.busy {
            cx.toast(format!("Busy: {what}"));
            return;
        }
        if let Err(e) = refuse(m) {
            cx.error(e);
            return;
        }
        let configured = m.configured.clone();
        self.busy = Some(format!("deciding how to publish {}", m.id()));
        cx.spawn(async move {
            let result = decide_publish(configured)
                .await
                .map(Box::new)
                .map_err(|e| format!("{e:#}"));
            MkMsg::PublishPlanned(result)
        });
    }

    fn publish(&mut self, plan: PublishPlan, cx: &mut Cx<'_>) {
        let cs = self.publish_changeset(&plan);
        let market = plan.market.clone();
        self.busy = Some(format!("publishing {market}"));
        self.run(
            cx,
            Box::new(move || {
                let m = &plan.configured.market;
                let dir = plan.configured.location.path();
                let commit = ops::Commit {
                    base: Some(plan.base.clone()),
                    head: plan.head.clone(),
                };
                let out = publish::publish(
                    dir,
                    &m.branch,
                    &m.repo,
                    &cs,
                    &commit,
                    &plan.route,
                    &Gh::new(plan.token.clone()),
                )?;
                Ok(match out {
                    publish::Published::Pushed { branch } => {
                        format!("Pushed to {}:{branch}", m.repo)
                    }
                    publish::Published::PullRequest { branch, url } => {
                        format!("Opened a pull request from {branch}: {url}")
                    }
                })
            }),
            move |result| MkMsg::Published { market, result },
        );
    }

    /// The change set a PR is titled and branched after: the one this
    /// session committed when there is exactly one commit, else a summary.
    fn publish_changeset(&self, plan: &PublishPlan) -> ChangeSet {
        if plan.subjects.len() == 1
            && let Some(cs) = self.committed.get(&plan.head)
        {
            return cs.clone();
        }
        ChangeSet {
            verb: Verb::Update,
            slug: format!("studio-{}", short(&plan.head)),
            name: format!("{} marketplace changes", plan.subjects.len()),
            changes: Vec::new(),
            notes: plan.subjects.clone(),
        }
    }

    // -- keys ----------------------------------------------------------------

    fn pager_key(&mut self, key: KeyEvent, cx: &mut Cx<'_>) {
        let Some((mut pager, pending)) = self.pager.take() else {
            return;
        };
        match pager.handle_key(key) {
            Step::Open => self.pager = Some((pager, pending)),
            Step::Cancel => {
                if pending.is_some() {
                    cx.toast("Cancelled: nothing was written");
                }
            }
            Step::Done(()) => {
                if let Some(p) = pending {
                    self.apply(cx, p);
                }
            }
        }
    }

    fn overlay_key(&mut self, key: KeyEvent, cx: &mut Cx<'_>) {
        let Some((mut overlay, purpose)) = self.overlay.take() else {
            return;
        };
        let step = overlay.handle_key(key);
        let answer = match step {
            Step::Open => {
                self.overlay = Some((overlay, purpose));
                return;
            }
            Step::Cancel => return,
            Step::Done(answer) => answer,
        };
        match (purpose, answer) {
            (Purpose::Add { market }, Answer::Values(values)) => match forms::parse_add(&values) {
                Err(e) => self.reopen(overlay, Purpose::Add { market }, e),
                Ok((entry, slug)) => {
                    self.stash(
                        overlay,
                        Purpose::Add {
                            market: market.clone(),
                        },
                    );
                    self.want = Some((market.clone(), Some(slug.clone())));
                    self.plan(cx, &market, format!("planning add {slug}"), move |d, m| {
                        ops::plan_add(d, m, entry, Some(&slug))
                    });
                }
            },
            (
                Purpose::Edit {
                    market,
                    slug,
                    entry,
                },
                Answer::Values(values),
            ) => match forms::parse_edit(&slug, &entry, &values) {
                Err(e) => self.reopen(
                    overlay,
                    Purpose::Edit {
                        market,
                        slug,
                        entry,
                    },
                    e,
                ),
                Ok(patch) if patch.is_empty() => cx.toast("Nothing changed"),
                Ok(patch) => {
                    self.stash(
                        overlay,
                        Purpose::Edit {
                            market: market.clone(),
                            slug: slug.clone(),
                            entry,
                        },
                    );
                    let s = slug.clone();
                    self.plan(
                        cx,
                        &market,
                        format!("planning update {slug}"),
                        move |d, m| ops::plan_update(d, m, &s, &patch),
                    );
                }
            },
            (Purpose::Deprecate { market, slug }, Answer::Values(values)) => {
                let reason = values.first().cloned().unwrap_or_default();
                if reason.trim().is_empty() {
                    self.reopen(
                        overlay,
                        Purpose::Deprecate { market, slug },
                        "a deprecation needs a reason".into(),
                    );
                    return;
                }
                self.stash(
                    overlay,
                    Purpose::Deprecate {
                        market: market.clone(),
                        slug: slug.clone(),
                    },
                );
                let s = slug.clone();
                self.plan(
                    cx,
                    &market,
                    format!("planning deprecate {slug}"),
                    move |d, m| ops::plan_deprecate(d, m, &s, &reason),
                );
            }
            (Purpose::Remove { market, slug }, Answer::Confirmed) => {
                let s = slug.clone();
                self.plan(
                    cx,
                    &market,
                    format!("planning remove {slug}"),
                    move |d, m| ops::plan_remove(d, m, &s),
                );
            }
            (Purpose::PickDraft { market, members }, Answer::Picked(i)) => {
                let Some((dir, slug)) = members.get(i).cloned() else {
                    return;
                };
                self.busy = Some(format!("drafting from {}", dir.display()));
                self.run(
                    cx,
                    Box::new(move || {
                        if !dir.is_dir() {
                            bail!("{} is not cloned", dir.display());
                        }
                        let draft =
                            studio_marketplace::draft::entry_from_server(&dir, slug.as_deref())?;
                        Ok(Box::new(draft.entry))
                    }),
                    move |result| MkMsg::Drafted { market, result },
                );
            }
            (Purpose::Publish(plan), Answer::Confirmed) => self.publish(*plan, cx),
            _ => {}
        }
    }

    fn reopen(&mut self, mut overlay: Overlay, purpose: Purpose, error: String) {
        if let Overlay::Form(f) = &mut overlay {
            f.set_error(error);
        }
        self.overlay = Some((overlay, purpose));
    }

    fn stash(&mut self, overlay: Overlay, purpose: Purpose) {
        if let Overlay::Form(mut f) = overlay {
            f.error = None;
            self.stashed = Some((f, purpose));
        }
    }

    fn move_selection(&mut self, by: isize) {
        let (current, len) = match (self.view, self.focus) {
            (View::Fleet, _) => (self.fleet_row, self.fleet().len()),
            (View::Catalog, Focus::Markets) => (self.market, self.markets().len()),
            (View::Catalog, Focus::Entries) => (
                self.entry,
                self.selected_market().map_or(0, |m| m.entries.len()),
            ),
        };
        let last = len.saturating_sub(1);
        let next = match by {
            isize::MIN => 0,
            isize::MAX => last,
            n => current.saturating_add_signed(n).min(last),
        };
        match (self.view, self.focus) {
            (View::Fleet, _) => self.fleet_row = next,
            (View::Catalog, Focus::Markets) => {
                if next != current {
                    self.entry = 0;
                }
                self.market = next;
            }
            (View::Catalog, Focus::Entries) => self.entry = next,
        }
    }
}

/// Refuses changes to a marketplace whose clone is missing or dirty (as
/// last loaded).
fn refuse(m: &Market) -> Result<(), String> {
    match &m.configured.location {
        Location::NotCloned(dir) => Err(format!(
            "{}: not cloned at {}; clone {} there to change it",
            m.id(),
            dir.display(),
            m.market().repo
        )),
        Location::Cloned(dir) if m.clone.dirty => Err(format!(
            "{}: {} has uncommitted changes; commit or stash them first",
            m.id(),
            dir.display()
        )),
        Location::Cloned(_) => Ok(()),
    }
}

fn cloned_dir(m: &Market, cx: &mut Cx<'_>) -> Option<std::path::PathBuf> {
    match &m.configured.location {
        Location::Cloned(d) => Some(d.clone()),
        Location::NotCloned(d) => {
            cx.error(format!("{}: not cloned at {}", m.id(), d.display()));
            None
        }
    }
}

/// The local clone, refused when missing, dirty or on another branch.
fn open(c: &Configured) -> anyhow::Result<studio_marketplace::Workspace> {
    let m = &c.market;
    if let Location::NotCloned(dir) = &c.location {
        bail!(
            "{}: not cloned at {}; clone {} there to change it",
            m.id,
            dir.display(),
            m.repo
        );
    }
    git::open_workspace(
        c.location.path(),
        &git::github_clone_url(&m.repo),
        &m.branch,
        false,
    )
}

/// The tree still has the `before` side the preview showed.
fn unchanged_since_plan(root: &Path, cs: &ChangeSet) -> anyhow::Result<()> {
    for c in &cs.changes {
        let now = std::fs::read_to_string(root.join(&c.path)).ok();
        if now != c.before {
            bail!(
                "{} changed since the preview; plan the change again",
                c.path
            );
        }
    }
    Ok(())
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(7)]
}

/// What publishing `c` would do: the unpushed commits and the route.
async fn decide_publish(c: Configured) -> anyhow::Result<PublishPlan> {
    let mode = c.market.publish;
    let prep = c.clone();
    let (base, head, subjects, token) = tokio::task::spawn_blocking(move || {
        let ws = open(&prep)?;
        let m = &prep.market;
        let tracking = format!("refs/remotes/origin/{}", m.branch);
        let base =
            git::run(&ws.dir, &["rev-parse", "--verify", "-q", &tracking]).map_err(|_| {
                anyhow!(
                    "no origin/{} in {}; fetch it first",
                    m.branch,
                    ws.dir.display()
                )
            })?;
        let head = git::run(&ws.dir, &["rev-parse", "HEAD"])?;
        let log = git::run(
            &ws.dir,
            &["log", "--reverse", "--format=%s", &format!("{base}..HEAD")],
        )?;
        let subjects: Vec<String> = log.lines().map(str::to_string).collect();
        if subjects.is_empty() {
            bail!(
                "{}: nothing to publish; {} is even with origin/{}",
                m.id,
                ws.dir.display(),
                m.branch
            );
        }
        let token = match mode {
            PublishMode::Push => None,
            _ => studio_core::exec::github_token(m.github_account.as_deref()).ok(),
        };
        Ok::<_, anyhow::Error>((base, head, subjects, token))
    })
    .await
    .context("publish preparation panicked")??;
    let api = token.clone().map(GithubApi::new);
    let route = publish::decide(mode, api.as_ref(), &c.market.repo, &c.market.branch).await?;
    Ok(PublishPlan {
        market: c.market.id.clone(),
        configured: c,
        route,
        base,
        head,
        subjects,
        token,
    })
}

fn status_mark(s: Status) -> (&'static str, Color) {
    match s {
        Status::Pass => ("ok", Color::Green),
        Status::Warn => ("warn", Color::Yellow),
        Status::Fail => ("FAIL", Color::Red),
        Status::Skip => ("skip", Color::DarkGray),
    }
}

fn validation_lines(dir: &Path, m: &Marketplace) -> Vec<Line<'static>> {
    let r = studio_marketplace::validate_dir(dir, m);
    let mut lines = Vec::new();
    let schema: Vec<_> = r.schema_errors().collect();
    let data: Vec<_> = r.data_errors().collect();
    let (word, color) = if r.is_ok() {
        ("valid", Color::Green)
    } else {
        ("INVALID", Color::Red)
    };
    lines.push(
        Line::from(format!(
            "{word}: {} files checked, {} schema problem(s), {} data problem(s)",
            r.files_checked,
            schema.len(),
            data.len()
        ))
        .fg(color)
        .bold(),
    );
    lines.push(Line::from(format!("  {}", dir.display())).dim());
    if !schema.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from("Schema problems (the schemas themselves are broken)").bold());
        for f in &schema {
            lines.push(Line::from(format!("  {}: {}", f.file, f.message)).fg(Color::Red));
        }
    }
    if !data.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from("Data problems").bold());
        for f in &data {
            lines.push(Line::from(format!("  {}: {}", f.file, f.message)).fg(Color::Red));
        }
    }
    for n in &r.notes {
        lines.push(Line::from(format!("note: {n}")).dim());
    }
    lines
}

fn reconcile_lines(dir: &Path, m: &Marketplace) -> anyhow::Result<Vec<Line<'static>>> {
    let checks = studio_marketplace::reconcile(dir, m);
    let report = studio_marketplace::verify(dir, m)?;
    let mut lines = Vec::new();
    let failing: Vec<_> = checks.iter().filter(|c| c.status != Status::Pass).collect();
    let passed = checks.len() - failing.len();
    lines.push(
        Line::from(format!(
            "Reconcile: {} check(s), {passed} passing, {} not",
            checks.len(),
            failing.len()
        ))
        .bold(),
    );
    for c in &failing {
        let (mark, color) = status_mark(c.status);
        lines.push(Line::from(format!("  {mark:<4}  {}  {}", c.id, c.summary)).fg(color));
        if let Some(ev) = &c.evidence {
            for l in ev.lines() {
                lines.push(Line::from(format!("        {l}")).dim());
            }
        }
    }
    lines.push(Line::from(""));
    let (word, color) = if report.is_clean() {
        ("clean", Color::Green)
    } else {
        ("DIFFERS", Color::Red)
    };
    lines.push(
        Line::from(format!(
            "Verify {word}: regenerated {} files, {} byte-identical, {} differ",
            report.generated,
            report.identical,
            report.diffs.len()
        ))
        .fg(color)
        .bold(),
    );
    for d in &report.diffs {
        let k = match d.kind {
            DiffKind::Changed => "changed",
            DiffKind::Missing => "missing",
            DiffKind::Extra => "extra",
        };
        lines.push(Line::from(format!("  {k:<7}  {}", d.path)).fg(Color::Yellow));
    }
    for (f, e) in &report.broken {
        lines.push(Line::from(format!("  broken   {f}: {e}")).fg(Color::Red));
    }
    for d in report.diffs.iter().filter(|d| !d.diff.is_empty()) {
        lines.push(Line::from(""));
        lines.extend(pager::diff_lines(&d.diff));
    }
    if !report.is_clean() {
        lines.push(Line::from(""));
        lines.push(Line::from("g on an entry regenerates its files from server.yaml.").dim());
    }
    Ok(lines)
}

impl Component for MarketplaceModule {
    fn id(&self) -> ModuleId {
        ID
    }

    fn title(&self) -> String {
        "Marketplaces".into()
    }

    fn start(&mut self, cx: &mut Cx<'_>) {
        if !self.started {
            self.started = true;
            self.load(cx);
        }
    }

    fn handle_key(&mut self, key: KeyEvent, cx: &mut Cx<'_>) -> Handled {
        if self.pager.is_some() {
            self.pager_key(key, cx);
            return Handled::Yes;
        }
        if self.overlay.is_some() {
            self.overlay_key(key, cx);
            return Handled::Yes;
        }
        let Some(k) = self.keys().resolve(&key) else {
            return Handled::No;
        };
        match k {
            Key::Down => self.move_selection(1),
            Key::Up => self.move_selection(-1),
            Key::First => self.move_selection(isize::MIN),
            Key::Last => self.move_selection(isize::MAX),
            Key::Focus => {
                self.focus = match self.focus {
                    Focus::Markets => Focus::Entries,
                    Focus::Entries => Focus::Markets,
                }
            }
            Key::View => {
                self.view = match self.view {
                    View::Catalog => View::Fleet,
                    View::Fleet => View::Catalog,
                }
            }
            Key::Reload => {
                self.load(cx);
                cx.toast("Reloading");
            }
            other => self.action(other, cx),
        }
        Handled::Yes
    }

    fn handle_msg(&mut self, msg: Payload, cx: &mut Cx<'_>) {
        let Ok(msg) = msg.downcast::<MkMsg>() else {
            return;
        };
        match *msg {
            MkMsg::Loaded { generation, result } => {
                if let Some(err) = self.snapshot.finish(generation, result) {
                    cx.error(format!("Loading marketplaces failed: {err}"));
                }
                self.restore_selection();
            }
            MkMsg::Planned { market, result } => {
                self.busy = None;
                let stashed = self.stashed.take();
                match result {
                    Ok(cs) if cs.is_empty() => {
                        self.want = None;
                        cx.toast(format!("{}: nothing to change", cs.title()));
                    }
                    Ok(cs) => {
                        let pager = Pager::preview(&market, &cs);
                        self.pager = Some((pager, Some(Pending { market, cs })));
                    }
                    Err(e) => {
                        self.want = None;
                        match stashed {
                            Some((mut form, purpose)) => {
                                form.set_error(e);
                                self.overlay = Some((Overlay::Form(form), purpose));
                            }
                            None => cx.error(format!("Refused: {e}")),
                        }
                    }
                }
            }
            MkMsg::Applied { market, result } => {
                self.busy = None;
                match result {
                    Ok((head, cs)) => {
                        cx.toast(format!(
                            "Committed {} {} to the {market} clone (not published; P publishes)",
                            short(&head),
                            cs.title()
                        ));
                        if cs.verb == Verb::Remove {
                            self.want = Some((market, None));
                        }
                        self.committed.insert(head, cs);
                    }
                    Err(e) => {
                        self.want = None;
                        cx.error(format!("Not committed: {e}"));
                    }
                }
                self.load(cx);
            }
            MkMsg::Report { title, result } => {
                self.busy = None;
                match result {
                    Ok(lines) => self.pager = Some((Pager::report(title, lines), None)),
                    Err(e) => cx.error(format!("{title} failed: {e}")),
                }
            }
            MkMsg::Drafted { market, result } => {
                self.busy = None;
                match result {
                    Ok(entry) => {
                        let form = forms::add_form(
                            &format!("Add a drafted entry to {market} (review every field)"),
                            Some(entry.as_ref()),
                        );
                        self.overlay = Some((Overlay::Form(form), Purpose::Add { market }));
                    }
                    Err(e) => cx.error(format!("Draft failed: {e}")),
                }
            }
            MkMsg::PublishPlanned(result) => {
                self.busy = None;
                match result {
                    Ok(plan) => {
                        let confirm = publish_confirm(&plan, &self.publish_changeset(&plan));
                        self.overlay = Some((Overlay::Confirm(confirm), Purpose::Publish(plan)));
                    }
                    Err(e) => cx.error(format!("Not published: {e}")),
                }
            }
            MkMsg::Published { market, result } => {
                self.busy = None;
                match result {
                    Ok(text) => cx.toast(format!("{market}: {text}")),
                    Err(e) => cx.error(format!("Publish failed: {e}")),
                }
                self.load(cx);
            }
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) {
        ui::draw(frame, self, area);
        if let Some((overlay, _)) = &self.overlay {
            overlay.draw(frame);
        }
        if let Some((pager, _)) = &mut self.pager {
            pager.draw(frame);
        }
    }

    fn keymap(&self) -> Vec<Hint> {
        if let Some((pager, _)) = &self.pager {
            return pager.keymap().hints();
        }
        if self.overlay.is_some() {
            return Vec::new();
        }
        self.keys().hints()
    }

    fn captures_input(&self) -> bool {
        self.overlay.is_some() || self.pager.is_some()
    }

    fn status(&self) -> Option<String> {
        if let Some(what) = &self.busy {
            return Some(format!("{what}..."));
        }
        let m = self.selected_market()?;
        let mut parts = Vec::new();
        if let Some(n) = m.clone.unpushed.filter(|n| *n > 0) {
            parts.push(format!("{}: {n} unpushed", m.id()));
        }
        if let Some(at) = self.snapshot.loaded_at {
            parts.push(format!("loaded {}", ago(Some(at), now_unix())));
        }
        (!parts.is_empty()).then(|| parts.join("  "))
    }
}

fn publish_confirm(plan: &PublishPlan, cs: &ChangeSet) -> Confirm {
    let m = &plan.configured.market;
    let mut lines = vec![format!(
        "Publish {} commit(s) from {} to {}:",
        plan.subjects.len(),
        plan.configured.location.path().display(),
        m.repo
    )];
    for s in &plan.subjects {
        lines.push(format!("  {s}"));
    }
    lines.push(String::new());
    lines.push(match &plan.route {
        Route::Push => format!("Route: push to {} (publish = {:?})", m.branch, m.publish),
        Route::PullRequest { reason } => format!(
            "Route: pull request from {} into {} ({reason})",
            cs.branch(),
            m.branch
        ),
    });
    Confirm::new(format!("Publish {}", plan.market), lines)
}
