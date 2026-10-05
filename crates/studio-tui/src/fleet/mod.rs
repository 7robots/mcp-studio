//! The Fleet module: one row per server, every probe source as a column, and
//! the selected server's facts and checks beside it.
//!
//! The data is a [`FleetReport`] from [`studio_fleet::fleet_status`], which can
//! take half a minute when it really probes (the Okta helper), so it runs in a
//! task while the screen counts the seconds. `r` reloads through the cache,
//! `R` re-probes everything. Tests swap the probe for a [`Loader`] of their own
//! with [`FleetModule::with_loader`].

pub mod keys;
mod ui;

use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::KeyEvent;
use futures::future::BoxFuture;
use ratatui::Frame;
use ratatui::layout::Rect;
use studio_core::Instance;
use studio_core::check::Status;
use studio_fleet::{FleetReport, Providers, ServerStatus, Source, StatusOptions};

use crate::framework::component::{Component, Handled, ModuleId};
use crate::framework::cx::{Cx, Payload};
use crate::framework::keymap::{Hint, Keymap};
use crate::framework::slot::Slot;

use keys::{Context, Key};

pub const ID: ModuleId = "fleet";

/// Produces a fleet report; the flag is `refresh` (ignore the cache).
pub type Loader =
    Arc<dyn Fn(bool) -> BoxFuture<'static, Result<FleetReport, String>> + Send + Sync>;

/// Shows a URL to the user (the system browser in production).
pub type Opener = Arc<dyn Fn(&str) -> anyhow::Result<()> + Send + Sync>;

/// Names a browser command (split on whitespace) to use instead of the
/// system opener.
pub const BROWSER_ENV: &str = "MCP_STUDIO_BROWSER";

/// How far PgUp/PgDn scroll the detail pane.
const PAGE: u16 = 10;

/// Which pane has the keyboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    Matrix,
    Detail,
}

#[derive(Debug)]
struct Loaded {
    generation: u64,
    result: Result<FleetReport, String>,
}

pub struct FleetModule {
    loader: Loader,
    opener: Opener,
    /// The instance's display name, for the screen before the first report.
    instance: Option<String>,
    pub report: Slot<FleetReport>,
    /// Index into [`FleetModule::visible`].
    selected: usize,
    problems_only: bool,
    source: Option<Source>,
    focus: Focus,
    scroll: u16,
    /// The largest useful scroll, as of the last draw.
    max_scroll: u16,
    loading_since: Option<Instant>,
    /// The load in flight was asked for by a key (so its result is announced).
    announce: bool,
    /// How many loads asked for `refresh` (the cache ignored).
    full_refreshes: usize,
}

impl FleetModule {
    /// The real module: `fleet_status` over `instance` with `providers`, and
    /// the system browser (or `$MCP_STUDIO_BROWSER`) for `o`.
    pub fn new(instance: Arc<Instance>, providers: Providers) -> FleetModule {
        let name = instance.display_name().to_string();
        let loader: Loader = Arc::new(move |refresh| {
            let instance = instance.clone();
            let providers = providers.clone();
            Box::pin(async move {
                // A probe that panics is an error on screen, not a dead task.
                tokio::spawn(async move {
                    let opts = StatusOptions {
                        refresh,
                        ..StatusOptions::default()
                    };
                    studio_fleet::fleet_status(&instance, &opts, &providers).await
                })
                .await
                .map_err(|e| format!("the fleet probe stopped: {e}"))
            })
        });
        let mut module = FleetModule::with_loader(loader);
        module.instance = Some(name);
        module
    }

    /// The module over any source of reports (tests, demos).
    pub fn with_loader(loader: Loader) -> FleetModule {
        FleetModule {
            loader,
            opener: system_opener(),
            instance: None,
            report: Slot::default(),
            selected: 0,
            problems_only: false,
            source: None,
            focus: Focus::Matrix,
            scroll: 0,
            max_scroll: 0,
            loading_since: None,
            announce: false,
            full_refreshes: 0,
        }
    }

    /// Uses `opener` for `o`/`O` instead of the browser.
    pub fn with_opener(mut self, opener: Opener) -> FleetModule {
        self.opener = opener;
        self
    }

    pub fn focus(&self) -> Focus {
        self.focus
    }

    pub fn problems_only(&self) -> bool {
        self.problems_only
    }

    /// The source filter (`None` = every source).
    pub fn source_filter(&self) -> Option<Source> {
        self.source
    }

    pub fn scroll(&self) -> u16 {
        self.scroll
    }

    /// How many loads ignored the cache.
    pub fn full_refreshes(&self) -> usize {
        self.full_refreshes
    }

    /// Indices into the report's servers that the filters let through.
    pub fn visible(&self) -> Vec<usize> {
        let Some(report) = &self.report.data else {
            return Vec::new();
        };
        report
            .servers
            .iter()
            .enumerate()
            .filter(|(_, s)| !self.problems_only || self.is_problem(s))
            .map(|(i, _)| i)
            .collect()
    }

    fn is_problem(&self, s: &ServerStatus) -> bool {
        let status = match self.source {
            Some(src) => s.column(src).map(|c| c.status),
            None => Some(s.rollup()),
        };
        matches!(status, Some(Status::Warn | Status::Fail))
    }

    /// The server under the cursor.
    pub fn selected_server(&self) -> Option<&ServerStatus> {
        let i = *self.visible().get(self.selected)?;
        self.report.data.as_ref()?.servers.get(i)
    }

    fn keys(&self) -> Keymap<Key> {
        keys::keymap(Context {
            detail: self.focus == Focus::Detail,
        })
    }

    fn load(&mut self, refresh: bool, cx: &Cx<'_>) {
        if refresh {
            self.full_refreshes += 1;
        }
        let fut = (self.loader)(refresh);
        self.report
            .load(cx, fut, |generation, result| Loaded { generation, result });
        self.loading_since = Some(Instant::now());
    }

    fn select(&mut self, index: usize) {
        let count = self.visible().len();
        let index = index.min(count.saturating_sub(1));
        if index != self.selected {
            self.scroll = 0;
        }
        self.selected = index;
    }

    /// Re-applies the filters, keeping the selected server if it is still shown.
    fn refilter(&mut self, keep: Option<String>) {
        let position = keep.and_then(|name| {
            let servers = &self.report.data.as_ref()?.servers;
            self.visible().iter().position(|&i| servers[i].name == name)
        });
        match position {
            Some(p) => self.selected = p,
            None => {
                self.selected = 0;
                self.scroll = 0;
            }
        }
        self.select(self.selected);
    }

    fn selected_name(&self) -> Option<String> {
        self.selected_server().map(|s| s.name.clone())
    }

    fn cycle_source(&mut self) -> Option<String> {
        let sources = self.report.data.as_ref()?.sources.clone();
        if sources.is_empty() {
            return None;
        }
        self.source = match self.source {
            None => Some(sources[0]),
            Some(s) => sources
                .iter()
                .position(|x| *x == s)
                .and_then(|i| sources.get(i + 1).copied()),
        };
        Some(match self.source {
            Some(s) => format!("Showing the {s} source"),
            None => "Showing every source".into(),
        })
    }

    fn open(&self, repo: bool, cx: &mut Cx<'_>) {
        let Some(server) = self.selected_server() else {
            return;
        };
        let target = if repo {
            format!("https://github.com/{}", server.repo)
        } else {
            match &server.url {
                Some(url) => url.clone(),
                None => {
                    cx.error(format!("{} has no URL (O opens its repo)", server.name));
                    return;
                }
            }
        };
        match (self.opener)(&target) {
            Ok(()) => cx.toast(format!("Opened {target}")),
            Err(e) => cx.error(format!("Could not open {target}: {e:#}")),
        }
    }

    fn scroll_by(&mut self, delta: i32) {
        let next = (i32::from(self.scroll) + delta).clamp(0, i32::from(self.max_scroll));
        self.scroll = next as u16;
    }
}

impl Component for FleetModule {
    fn id(&self) -> ModuleId {
        ID
    }

    fn title(&self) -> String {
        "Fleet".into()
    }

    fn start(&mut self, cx: &mut Cx<'_>) {
        self.load(false, cx);
    }

    fn handle_key(&mut self, key: KeyEvent, cx: &mut Cx<'_>) -> Handled {
        let Some(action) = self.keys().resolve(&key) else {
            return Handled::No;
        };
        let detail = self.focus == Focus::Detail;
        match action {
            Key::Down => self.select(self.selected + 1),
            Key::Up => self.select(self.selected.saturating_sub(1)),
            Key::First if detail => self.scroll = 0,
            Key::Last if detail => self.scroll = self.max_scroll,
            Key::First => self.select(0),
            Key::Last => self.select(usize::MAX),
            Key::Focus => {
                if self.selected_server().is_some() {
                    self.focus = Focus::Detail;
                }
            }
            Key::Unfocus => self.focus = Focus::Matrix,
            Key::ScrollDown => self.scroll_by(1),
            Key::ScrollUp => self.scroll_by(-1),
            Key::PageDown => self.scroll_by(i32::from(PAGE)),
            Key::PageUp => self.scroll_by(-i32::from(PAGE)),
            Key::Refresh => {
                self.announce = true;
                self.load(false, cx);
                cx.toast("Refreshing fleet status");
            }
            Key::FullRefresh => {
                self.announce = true;
                self.load(true, cx);
                cx.toast("Re-probing every server; this can take a while");
            }
            Key::ProblemsOnly => {
                let keep = self.selected_name();
                self.problems_only = !self.problems_only;
                self.refilter(keep);
                cx.toast(if self.problems_only {
                    "Showing servers with problems only"
                } else {
                    "Showing every server"
                });
            }
            Key::SourceFilter => {
                let keep = self.selected_name();
                if let Some(text) = self.cycle_source() {
                    self.scroll = 0;
                    self.refilter(keep);
                    cx.toast(text);
                }
            }
            Key::OpenUrl => self.open(false, cx),
            Key::OpenRepo => self.open(true, cx),
        }
        Handled::Yes
    }

    fn handle_msg(&mut self, msg: Payload, cx: &mut Cx<'_>) {
        let Ok(msg) = msg.downcast::<Loaded>() else {
            return;
        };
        let Loaded { generation, result } = *msg;
        if !self.report.is_current(generation) {
            return;
        }
        let keep = self.selected_name();
        self.loading_since = None;
        if let Some(err) = self.report.finish(generation, result) {
            cx.error(format!("Fleet status failed: {err}"));
            self.announce = false;
            return;
        }
        if let Some(src) = self.source
            && !self
                .report
                .data
                .as_ref()
                .is_some_and(|r| r.sources.contains(&src))
        {
            self.source = None;
        }
        self.refilter(keep);
        if std::mem::take(&mut self.announce)
            && let Some(report) = &self.report.data
        {
            let n = report.servers.len();
            cx.toast(format!(
                "Fleet status updated: {n} server{}{}",
                if n == 1 { "" } else { "s" },
                if report.from_cache {
                    " (cached; R re-probes)"
                } else {
                    ""
                }
            ));
        }
    }

    fn next_deadline(&self) -> Option<Instant> {
        // Redraw once a second while loading, for the elapsed time.
        let since = self.loading_since?;
        let elapsed = since.elapsed().as_secs();
        Some(since + Duration::from_secs(elapsed + 1))
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) {
        ui::draw(frame, self, area);
    }

    fn keymap(&self) -> Vec<Hint> {
        self.keys().hints()
    }

    fn status(&self) -> Option<String> {
        if let Some(since) = self.loading_since {
            return Some(format!("probing {}s", since.elapsed().as_secs()));
        }
        let at = self.report.loaded_at?;
        let now = studio_fleet::time::now_unix();
        Some(format!(
            "updated {} ago",
            studio_fleet::time::age(now - at as i64)
        ))
    }
}

/// Opens a URL with `$MCP_STUDIO_BROWSER` or the system opener.
pub fn system_opener() -> Opener {
    Arc::new(|url: &str| {
        let default = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        let command = std::env::var(BROWSER_ENV)
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| default.into());
        let mut parts = command.split_whitespace();
        let program = parts.next().unwrap_or(default);
        std::process::Command::new(program)
            .args(parts)
            .arg(url)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| anyhow::anyhow!("starting {program}: {e}"))?;
        Ok(())
    })
}
