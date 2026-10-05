//! One gateway's state in the Gateway module: its session, the three screens'
//! data, the selection, an open overlay, a running action or sign-in.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::KeyEvent;
use ratatui::style::Color;
use studio_gateway::actions::{self, Action, Outcome, ScopeDrift};
use studio_gateway::model::{
    Identity, POLICY_RETENTION_DAYS, PolicyEvents, Server, ServerList, UsageStats,
};
use studio_gateway::{GatewayError, GatewayResult, Session, Url};
use tokio::task::AbortHandle;

use crate::framework::component::Handled;
use crate::framework::cx::Cx;
use crate::framework::overlay::{Answer, Choice, Confirm, Form, Overlay, PickItem, Picker, Step};
use crate::framework::slot::Slot;

use super::keys::{self, Key};
use super::{GwMsg, Opener};

/// Runs listed on the Usage screen.
pub const RECENT_RUNS: u64 = 20;
/// Events fetched for the Policy screen (the gateway's maximum).
pub const POLICY_LIMIT: u64 = 100;
const USAGE_DAYS: [u64; 3] = [1, 7, 30];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    Servers,
    Usage,
    Policy,
}

impl Screen {
    pub const ALL: [Screen; 3] = [Screen::Servers, Screen::Usage, Screen::Policy];

    pub fn title(self) -> &'static str {
        match self {
            Screen::Servers => "Servers",
            Screen::Usage => "Usage",
            Screen::Policy => "Policy",
        }
    }

    /// Screens whose data comes from admin-only tools.
    pub fn admin_only(self) -> bool {
        self != Screen::Servers
    }
}

/// What a pane's tasks report back.
#[derive(Debug)]
pub(crate) enum PaneMsg {
    Identity {
        generation: u64,
        result: GatewayResult<Identity>,
    },
    Servers {
        generation: u64,
        result: GatewayResult<ServerList>,
    },
    Usage {
        generation: u64,
        result: GatewayResult<UsageStats>,
    },
    Policy {
        generation: u64,
        result: GatewayResult<PolicyEvents>,
    },
    Action {
        result: GatewayResult<Outcome>,
    },
    /// The sign-in page the browser was sent to.
    LoginUrl(Url),
    /// The sign-in finished: the granted scopes, or why not.
    Login(Result<String, String>),
}

/// What an open overlay is for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Purpose {
    Register,
    Timeout(String),
    ChangeUrl(Box<Server>),
    Status(String),
    Class {
        server: String,
        tool: String,
    },
    PickTool {
        server: String,
        tools: Vec<(String, String)>,
    },
    /// A confirmation that runs this action.
    Run(Action),
}

/// A browser sign-in in progress.
#[derive(Debug)]
pub struct Login {
    /// The authorization URL, once the browser was sent there.
    pub url: Option<Url>,
    abort: AbortHandle,
}

pub struct Pane {
    pub(crate) index: usize,
    pub session: Arc<Session>,
    pub screen: Screen,
    pub identity: Slot<Identity>,
    pub servers: Slot<ServerList>,
    pub usage: Slot<UsageStats>,
    pub policy: Slot<PolicyEvents>,
    /// Index into the servers table.
    pub selected: usize,
    /// Index into the policy event table.
    pub event_selected: usize,
    pub usage_days: u64,
    pub policy_days: u64,
    /// `None`, `deny` or `flag`.
    pub policy_filter: Option<&'static str>,
    /// Set when the stored sign-in is missing or expired; the screens say so
    /// instead of showing stale data as if it were live.
    pub signed_out: Option<String>,
    pub(crate) overlay: Option<(Overlay, Purpose)>,
    /// An admin action is running; another waits for it.
    pub busy: bool,
    /// Scope drift the latest refresh of each server reported.
    pub drift: BTreeMap<String, ScopeDrift>,
    pub login: Option<Login>,
    next_poll: Option<Instant>,
    pub(crate) started: bool,
}

impl Pane {
    pub(crate) fn new(index: usize, session: Arc<Session>) -> Pane {
        Pane {
            index,
            session,
            screen: Screen::Servers,
            identity: Slot::default(),
            servers: Slot::default(),
            usage: Slot::default(),
            policy: Slot::default(),
            selected: 0,
            event_selected: 0,
            usage_days: 7,
            policy_days: 1,
            policy_filter: None,
            signed_out: None,
            overlay: None,
            busy: false,
            drift: BTreeMap::new(),
            login: None,
            next_poll: None,
            started: false,
        }
    }

    pub fn id(&self) -> &str {
        &self.session.profile().id
    }

    pub fn host(&self) -> String {
        self.session
            .gateway()
            .host_str()
            .unwrap_or_default()
            .to_string()
    }

    pub(crate) fn admin_scope(&self) -> &str {
        &self.session.profile().admin_scope
    }

    /// `Some(true|false)` once `whoami` has answered.
    pub fn admin(&self) -> Option<bool> {
        self.identity.data.as_ref().map(|i| i.admin)
    }

    /// No action running and no server list loading.
    pub fn idle(&self) -> bool {
        !self.busy && !self.servers.loading
    }

    /// The open overlay, if any.
    pub fn overlay(&self) -> Option<&Overlay> {
        self.overlay.as_ref().map(|(o, _)| o)
    }

    pub fn selected_server(&self) -> Option<&Server> {
        self.servers.data.as_ref()?.servers.get(self.selected)
    }

    pub(crate) fn keymap(&self, gateways: usize) -> crate::framework::keymap::Keymap<Key> {
        keys::keymap(keys::Context {
            screen: self.screen,
            admin: self.admin(),
            signed_out: self.signed_out.is_some(),
            signing_in: self.login.is_some(),
            gateways,
        })
    }

    pub(crate) fn start(&mut self, cx: &mut Cx<'_>) {
        if self.started {
            return;
        }
        self.started = true;
        self.load_identity(cx);
        self.load_servers(cx);
        self.schedule_poll(Instant::now());
    }

    fn spawn(
        &self,
        cx: &Cx<'_>,
        fut: impl Future<Output = PaneMsg> + Send + 'static,
    ) -> AbortHandle {
        let pane = self.index;
        cx.spawn(async move {
            GwMsg {
                pane,
                body: fut.await,
            }
        })
    }

    // -- loads -------------------------------------------------------------

    fn load_identity(&mut self, cx: &Cx<'_>) {
        let generation = self.identity.begin();
        let session = self.session.clone();
        self.spawn(cx, async move {
            PaneMsg::Identity {
                generation,
                result: session.identity().await,
            }
        });
    }

    fn load_servers(&mut self, cx: &Cx<'_>) {
        let generation = self.servers.begin();
        let session = self.session.clone();
        self.spawn(cx, async move {
            PaneMsg::Servers {
                generation,
                result: session.list_servers(true, true).await,
            }
        });
    }

    fn load_usage(&mut self, cx: &Cx<'_>) {
        let generation = self.usage.begin();
        let (session, days) = (self.session.clone(), self.usage_days);
        self.spawn(cx, async move {
            PaneMsg::Usage {
                generation,
                result: session.usage_stats(days, RECENT_RUNS).await,
            }
        });
    }

    fn load_policy(&mut self, cx: &Cx<'_>) {
        let generation = self.policy.begin();
        let (session, days, filter) = (self.session.clone(), self.policy_days, self.policy_filter);
        self.spawn(cx, async move {
            PaneMsg::Policy {
                generation,
                result: session.policy_events(days, POLICY_LIMIT, filter).await,
            }
        });
    }

    /// Loads the current screen's data, if the caller may read it. An admin-only
    /// screen waits for `whoami`, and is never requested for a view-only token.
    fn load_screen(&mut self, cx: &Cx<'_>) {
        match self.screen {
            Screen::Servers => self.load_servers(cx),
            _ if self.admin() != Some(true) => {}
            Screen::Usage => self.load_usage(cx),
            Screen::Policy => self.load_policy(cx),
        }
    }

    pub(crate) fn reload(&mut self, cx: &Cx<'_>) {
        self.signed_out = None;
        self.load_identity(cx);
        self.load_screen(cx);
        if self.screen != Screen::Servers {
            self.load_servers(cx);
        }
    }

    pub(crate) fn handle_msg(&mut self, msg: PaneMsg, cx: &mut Cx<'_>) {
        let error = match msg {
            PaneMsg::Identity { generation, result } => {
                let error = self.identity.finish(generation, result);
                if error.is_none() && self.screen.admin_only() && self.admin() == Some(true) {
                    self.load_screen(cx);
                }
                error
            }
            PaneMsg::Servers { generation, result } => {
                let error = self.servers.finish(generation, result);
                let count = self.servers.data.as_ref().map_or(0, |l| l.servers.len());
                self.selected = self.selected.min(count.saturating_sub(1));
                error
            }
            PaneMsg::Usage { generation, result } => self.usage.finish(generation, result),
            PaneMsg::Policy { generation, result } => {
                let error = self.policy.finish(generation, result);
                let count = self.policy.data.as_ref().map_or(0, |p| p.events.len());
                self.event_selected = self.event_selected.min(count.saturating_sub(1));
                error
            }
            PaneMsg::Action { result } => {
                self.busy = false;
                self.load_servers(cx);
                match result {
                    Ok(outcome) => {
                        for (server, drift) in outcome.drift {
                            match drift {
                                Some(drift) => self.drift.insert(server, drift),
                                None => self.drift.remove(&server),
                            };
                        }
                        cx.toast(outcome.message);
                        None
                    }
                    Err(err) => {
                        self.fail(&err, cx);
                        Some(err)
                    }
                }
            }
            PaneMsg::LoginUrl(url) => {
                if let Some(login) = &mut self.login {
                    login.url = Some(url);
                }
                None
            }
            PaneMsg::Login(result) => {
                self.login = None;
                match result {
                    Ok(scope) => {
                        cx.toast(format!("Signed in to {} with scopes: {scope}", self.host()));
                        self.reload(cx);
                    }
                    Err(err) => cx.error(format!("Sign-in failed: {err}")),
                }
                None
            }
        };
        if let Some(err @ (GatewayError::NotLoggedIn(_) | GatewayError::SessionExpired)) = error {
            self.signed_out = Some(err.to_string());
        }
    }

    // -- timers ------------------------------------------------------------

    fn schedule_poll(&mut self, now: Instant) {
        let every = self.session.profile().refresh_seconds;
        self.next_poll = (every > 0).then(|| now + Duration::from_secs(every));
    }

    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.next_poll
    }

    pub(crate) fn tick(&mut self, now: Instant, cx: &mut Cx<'_>) {
        if self.next_poll.is_some_and(|at| at <= now) {
            // A signed-out session would only repeat the same refusal.
            if self.signed_out.is_none() && cx.focused() {
                self.load_screen(cx);
            }
            self.schedule_poll(now);
        }
    }

    /// An action's failure, worded for what the user can do about it.
    fn fail(&self, err: &GatewayError, cx: &mut Cx<'_>) {
        let text = match err {
            GatewayError::InsufficientScope { .. } => {
                format!(
                    "Refused: {err}. Sign in again with {} (L).",
                    self.admin_scope()
                )
            }
            GatewayError::Tool(message) => format!("Refused: {message}"),
            other => format!("Failed: {other}"),
        };
        cx.error(text);
    }

    // -- sign-in -----------------------------------------------------------

    pub(crate) fn sign_in(&mut self, cx: &mut Cx<'_>, opener: &Opener, timeout: Duration) {
        if self.login.is_some() {
            cx.toast("Already waiting for the browser sign-in (Esc cancels)");
            return;
        }
        let session = self.session.clone();
        let opener = opener.clone();
        let urls = cx.sender::<GwMsg>();
        let pane = self.index;
        let abort = self.spawn(cx, async move {
            let open = move |url: &Url| {
                urls.send(GwMsg {
                    pane,
                    body: PaneMsg::LoginUrl(url.clone()),
                });
                opener(url)
            };
            let result = session.login(timeout, open).await;
            PaneMsg::Login(result.map(|t| t.scope).map_err(|e| format!("{e:#}")))
        });
        self.login = Some(Login { url: None, abort });
        cx.toast("Opening the gateway sign-in page in the browser...");
    }

    fn cancel_sign_in(&mut self, cx: &mut Cx<'_>) {
        if let Some(login) = self.login.take() {
            login.abort.abort();
            cx.toast("Sign-in cancelled");
        }
    }

    // -- keys --------------------------------------------------------------

    pub(crate) fn handle_key(
        &mut self,
        key: KeyEvent,
        cx: &mut Cx<'_>,
        gateways: usize,
        opener: &Opener,
        login_timeout: Duration,
    ) -> Handled {
        if self.overlay.is_some() {
            self.overlay_key(key, cx);
            return Handled::Yes;
        }
        let Some(action) = self.keymap(gateways).resolve(&key) else {
            return Handled::No;
        };
        match action {
            Key::PickGateway => return Handled::No,
            Key::SignIn => self.sign_in(cx, opener, login_timeout),
            Key::CancelSignIn => self.cancel_sign_in(cx),
            Key::NextScreen => self.switch(self.cycle(1), cx),
            Key::PrevScreen => self.switch(self.cycle(Screen::ALL.len() - 1), cx),
            Key::Reload => {
                self.reload(cx);
                cx.toast("Refreshing");
            }
            Key::Down => self.move_selection(1),
            Key::Up => self.move_selection(-1),
            Key::First => self.move_selection(isize::MIN),
            Key::Last => self.move_selection(isize::MAX),
            Key::Days => self.cycle_days(cx),
            Key::Filter => {
                self.policy_filter = match self.policy_filter {
                    None => Some("deny"),
                    Some("deny") => Some("flag"),
                    _ => None,
                };
                self.event_selected = 0;
                self.load_screen(cx);
            }
            admin => self.admin_key(admin, cx),
        }
        Handled::Yes
    }

    fn cycle(&self, by: usize) -> Screen {
        let at = Screen::ALL
            .iter()
            .position(|s| *s == self.screen)
            .unwrap_or(0);
        Screen::ALL[(at + by) % Screen::ALL.len()]
    }

    fn switch(&mut self, screen: Screen, cx: &Cx<'_>) {
        if self.screen == screen {
            return;
        }
        self.screen = screen;
        let loaded = match screen {
            Screen::Servers => self.servers.data.is_some(),
            Screen::Usage => self.usage.data.is_some(),
            Screen::Policy => self.policy.data.is_some(),
        };
        if !loaded {
            self.load_screen(cx);
        }
    }

    fn cycle_days(&mut self, cx: &Cx<'_>) {
        match self.screen {
            Screen::Usage => {
                let at = USAGE_DAYS
                    .iter()
                    .position(|d| *d == self.usage_days)
                    .unwrap_or(0);
                self.usage_days = USAGE_DAYS[(at + 1) % USAGE_DAYS.len()];
            }
            Screen::Policy => {
                self.policy_days = if self.policy_days >= POLICY_RETENTION_DAYS {
                    1
                } else {
                    POLICY_RETENTION_DAYS
                };
            }
            Screen::Servers => return,
        }
        self.load_screen(cx);
    }

    fn move_selection(&mut self, by: isize) {
        let (index, count) = match self.screen {
            Screen::Servers => (
                &mut self.selected,
                self.servers.data.as_ref().map_or(0, |l| l.servers.len()),
            ),
            Screen::Policy => (
                &mut self.event_selected,
                self.policy.data.as_ref().map_or(0, |p| p.events.len()),
            ),
            Screen::Usage => return,
        };
        if count == 0 {
            return;
        }
        *index = match by {
            isize::MIN => 0,
            isize::MAX => count - 1,
            by => (*index as isize + by).clamp(0, count as isize - 1) as usize,
        };
    }

    // -- admin actions -----------------------------------------------------

    fn open(&mut self, overlay: Overlay, purpose: Purpose) {
        self.overlay = Some((overlay, purpose));
    }

    fn admin_key(&mut self, key: Key, cx: &mut Cx<'_>) {
        if self.admin() != Some(true) {
            cx.toast(format!(
                "That needs {}; this sign-in is view-only",
                self.admin_scope()
            ));
            return;
        }
        if self.busy {
            cx.toast("Still working on the last change");
            return;
        }
        match key {
            Key::Register => {
                let hint = self
                    .session
                    .profile()
                    .server_url_hint
                    .clone()
                    .unwrap_or_else(|| "https://".into());
                let form = Form::new("Register a server")
                    .field("URL", &hint, "")
                    .field("id", "optional; the first label of the host", "")
                    .field("description", "optional; the server's own replaces it", "")
                    .field("timeout ms", "optional; 1000-120000, default 10000", "");
                return self.open(Overlay::Form(form), Purpose::Register);
            }
            Key::RefreshAll => return self.run(Action::Refresh { server: None }, cx),
            _ => {}
        }
        let Some(server) = self.selected_server().cloned() else {
            return;
        };
        let id = server.id.clone();
        match key {
            Key::Timeout => {
                let form = Form::new(format!("Timeout for {id}")).field(
                    "timeout ms",
                    "1000-120000",
                    &server.timeout_ms.to_string(),
                );
                self.open(Overlay::Form(form), Purpose::Timeout(id));
            }
            Key::Url => {
                let form =
                    Form::new(format!("New URL for {id}")).field("URL", "https://", &server.url);
                self.open(Overlay::Form(form), Purpose::ChangeUrl(Box::new(server)));
            }
            Key::RefreshOne => self.run(Action::Refresh { server: Some(id) }, cx),
            Key::Approve => match self.drift.get(&id) {
                Some(drift) => {
                    let lines = vec![
                        format!("approved    {}", drift.approved.join(" ")),
                        format!("advertised  {}", drift.advertised.join(" ")),
                        String::new(),
                        "Adopt the advertised set? The gateway will mint these for every call."
                            .into(),
                    ];
                    self.open(
                        Overlay::Confirm(Confirm::new(format!("Approve {id}'s scopes"), lines)),
                        Purpose::Run(Action::ApproveScopes { server: id }),
                    );
                }
                None => cx.toast(format!(
                    "No scope drift recorded for {id}; refresh it first (x)"
                )),
            },
            Key::Status => {
                let choice = Choice::new(
                    format!("Status of {id}"),
                    vec![format!("{id} is {}.", server.status)],
                )
                .option('a', "active (clears errors, lifts quarantine)", "active")
                .option('d', "disabled", "disabled")
                .option('q', "quarantined", "quarantined");
                self.open(Overlay::Choice(choice), Purpose::Status(id));
            }
            Key::Access => {
                let (access, lines) = if server.read_only() {
                    (
                        "read_write",
                        vec![format!("Every tool on {id} becomes callable again.")],
                    )
                } else {
                    let total = server.tool_classes.len();
                    let read = server.read_tools();
                    let mut lines = vec![format!(
                        "{read} of {total} tools remain callable: those classified read."
                    )];
                    if read == 0 {
                        lines.push("None is, so every call to this server will be refused.".into());
                    }
                    lines.push("Enforced in observe mode too.".into());
                    ("read_only", lines)
                };
                self.open(
                    Overlay::Confirm(Confirm::new(
                        format!("Make {id} {}", access.replace('_', "-")),
                        lines,
                    )),
                    Purpose::Run(Action::Access { server: id, access }),
                );
            }
            Key::Classify => {
                let tools: Vec<(String, String)> = server
                    .tool_classes
                    .iter()
                    .map(|t| (t.name.clone(), t.classification.clone()))
                    .collect();
                if tools.is_empty() {
                    cx.toast(format!("{id} has no tools in the catalogue"));
                } else {
                    let items = tools
                        .iter()
                        .map(|(name, class)| PickItem::tagged(name, class, class_color(class)))
                        .collect();
                    let picker = Picker::new(format!("Classify a tool on {id}"), items, "classify");
                    self.open(
                        Overlay::Picker(picker),
                        Purpose::PickTool { server: id, tools },
                    );
                }
            }
            Key::Delete => {
                let confirm = Confirm::typed(
                    format!("Delete {id}"),
                    vec![
                        format!(
                            "Unregisters {id}: its tools, classifications and scope claims go with it."
                        ),
                        "The server itself is untouched. Type its id to confirm.".into(),
                    ],
                    id.clone(),
                );
                self.open(
                    Overlay::Confirm(confirm),
                    Purpose::Run(Action::Delete { server: id }),
                );
            }
            _ => {}
        }
    }

    fn run(&mut self, action: Action, cx: &mut Cx<'_>) {
        self.busy = true;
        cx.toast(format!("{}...", action.describe()));
        let session = self.session.clone();
        self.spawn(cx, async move {
            PaneMsg::Action {
                result: actions::run(&session, action).await,
            }
        });
    }

    fn overlay_key(&mut self, key: KeyEvent, cx: &mut Cx<'_>) {
        let Some((mut overlay, purpose)) = self.overlay.take() else {
            return;
        };
        match overlay.handle_key(key) {
            Step::Open => self.overlay = Some((overlay, purpose)),
            Step::Cancel => {}
            Step::Done(answer) => self.answer(overlay, purpose, answer, cx),
        }
    }

    fn answer(&mut self, mut overlay: Overlay, purpose: Purpose, answer: Answer, cx: &mut Cx<'_>) {
        match (purpose, answer) {
            (purpose, Answer::Values(values)) => match form_action(&purpose, &values) {
                Ok(Some(action)) => self.run(action, cx),
                Ok(None) => {
                    if let Purpose::ChangeUrl(server) = purpose {
                        let url = values[0].trim().to_string();
                        let id = server.id.clone();
                        let confirm = Confirm::typed(
                            format!("Move {id}"),
                            vec![
                                format!("from  {}", server.url),
                                format!("to    {url}"),
                                String::new(),
                                "Unregisters and re-registers under the same id, then restores its"
                                    .into(),
                                "status, access and classifications. Type its id to confirm."
                                    .into(),
                            ],
                            id,
                        );
                        self.open(
                            Overlay::Confirm(confirm),
                            Purpose::Run(Action::ChangeUrl { server, url }),
                        );
                    }
                }
                Err(message) => {
                    if let Overlay::Form(form) = &mut overlay {
                        form.set_error(message);
                    }
                    self.overlay = Some((overlay, purpose));
                }
            },
            (Purpose::Status(server), Answer::Choice(value)) => {
                if let Some(status) = known(&value, &["active", "disabled", "quarantined"]) {
                    self.run(Action::Status { server, status }, cx);
                }
            }
            (Purpose::Class { server, tool }, Answer::Choice(value)) => {
                let class = known(&value, &["read", "write", "destructive"]);
                if class.is_some() || value == "clear" {
                    self.run(
                        Action::Classify {
                            server,
                            tool,
                            class,
                        },
                        cx,
                    );
                }
            }
            (Purpose::PickTool { server, tools }, Answer::Picked(i)) => {
                let Some((tool, class)) = tools.get(i).cloned() else {
                    return;
                };
                let choice = Choice::new(
                    format!("Classify {server}.{tool}"),
                    vec![format!("Now {class}.")],
                )
                .option('r', "read", "read")
                .option('w', "write", "write")
                .option('d', "destructive", "destructive")
                .option('n', "none (clear the record)", "clear");
                self.open(Overlay::Choice(choice), Purpose::Class { server, tool });
            }
            (Purpose::Run(action), Answer::Confirmed) => self.run(action, cx),
            _ => {}
        }
    }
}

/// `value` as one of the `&'static str`s the actions take.
fn known(value: &str, options: &[&'static str]) -> Option<&'static str> {
    options.iter().copied().find(|o| *o == value)
}

/// A submitted form as an action. `Ok(None)` means it needs a typed
/// confirmation first (a URL change); `Err` is a message to show in the form.
fn form_action(purpose: &Purpose, fields: &[String]) -> Result<Option<Action>, String> {
    let value = |i: usize| {
        fields
            .get(i)
            .map(|v| v.trim().to_string())
            .unwrap_or_default()
    };
    let optional = |i: usize| Some(value(i)).filter(|v| !v.is_empty());
    let timeout = |text: &str| -> Result<u64, String> {
        match text.parse::<u64>() {
            Ok(ms) if (1000..=120_000).contains(&ms) => Ok(ms),
            _ => Err("timeout must be a whole number of milliseconds, 1000-120000".into()),
        }
    };
    let https = |url: &str| -> Result<(), String> {
        if url.starts_with("https://") && url.len() > 8 {
            Ok(())
        } else {
            Err("the URL must start with https://".into())
        }
    };
    match purpose {
        Purpose::Register => {
            let url = value(0);
            https(&url)?;
            let timeout_ms = optional(3).map(|t| timeout(&t)).transpose()?;
            Ok(Some(Action::Register {
                url,
                id: optional(1),
                description: optional(2),
                timeout_ms,
            }))
        }
        Purpose::Timeout(server) => Ok(Some(Action::Timeout {
            server: server.clone(),
            timeout_ms: timeout(&value(0))?,
        })),
        Purpose::ChangeUrl(server) => {
            let url = value(0);
            https(&url)?;
            if url == server.url {
                return Err("that is the current URL".into());
            }
            Ok(None)
        }
        _ => Ok(None),
    }
}

pub(crate) fn health_color(health: &str) -> Color {
    match health {
        "ok" => Color::Green,
        "degraded" | "stale" => Color::Yellow,
        "failing" => Color::Red,
        _ => Color::DarkGray,
    }
}

pub(crate) fn class_color(class: &str) -> Color {
    match class {
        "read" => Color::Green,
        "write" => Color::Yellow,
        "destructive" => Color::Red,
        _ => Color::DarkGray,
    }
}
