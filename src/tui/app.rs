//! Dashboard state and input.
//!
//! The screen is a set of panels. On a large terminal they are all visible at once (the "wall");
//! on a medium one some are stacked in a column; on a small one they are grouped into tabs. One
//! panel has the keyboard focus (Tab, or its number); `z` zooms it to the whole screen.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::time::Instant;

use chrono::{DateTime, Local, Utc};
use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;
use ratatui::style::Color;

use super::theme::Theme;
use super::{Analytics, Msg, SystemView};
use crate::claude::registry::LiveSession;
use crate::claude::seats::Seat;
use crate::core::limits::WindowKind;
use crate::core::models::context_window;
use crate::core::period::Period;
use crate::index::Progress;
use crate::index::query::Dim;
use crate::live::{Agent, Snapshot, State, ToolLine};
use crate::ui::fmt;
use crate::vault::Vault;

/// Every panel; the number is the key that focuses it and appears in its title.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Panel {
    Swarm,
    Mana,
    Burn,
    Attention,
    Agent,
    Projects,
    Models,
    History,
    Efficiency,
    System,
}

impl Panel {
    pub const ALL: [Panel; 10] = [
        Panel::Swarm,
        Panel::Mana,
        Panel::Burn,
        Panel::Attention,
        Panel::Agent,
        Panel::Projects,
        Panel::Models,
        Panel::History,
        Panel::Efficiency,
        Panel::System,
    ];

    pub fn number(self) -> u8 {
        match self {
            Panel::Swarm => 1,
            Panel::Mana => 2,
            Panel::Burn => 3,
            Panel::Attention => 4,
            Panel::Agent => 5,
            Panel::Projects => 6,
            Panel::Models => 7,
            Panel::History => 8,
            Panel::Efficiency => 9,
            Panel::System => 0,
        }
    }

    pub fn from_number(n: u8) -> Panel {
        Panel::ALL
            .into_iter()
            .find(|p| p.number() == n)
            .unwrap_or(Panel::Swarm)
    }

    pub fn title(self) -> &'static str {
        match self {
            Panel::Swarm => "SWARM",
            Panel::Mana => "MANA · SEATS",
            Panel::Burn => "BURN",
            Panel::Attention => "ATTENTION",
            Panel::Agent => "AGENT",
            Panel::Projects => "PROJECTS",
            Panel::Models => "MODELS",
            Panel::History => "HISTORY",
            Panel::Efficiency => "EFFICIENCY",
            Panel::System => "SYSTEM",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutMode {
    Auto,
    Wall,
    Column,
    Tabs,
}

/// The arrangement actually drawn, chosen from the terminal size (or forced with `L`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    Wall,
    Column,
    Tabs,
}

impl Shape {
    pub fn pick(mode: LayoutMode, area: Rect) -> Shape {
        match mode {
            LayoutMode::Wall => Shape::Wall,
            LayoutMode::Column => Shape::Column,
            LayoutMode::Tabs => Shape::Tabs,
            LayoutMode::Auto if area.width >= 250 && area.height >= 55 => Shape::Wall,
            LayoutMode::Auto if area.width >= 150 && area.height >= 48 => Shape::Column,
            LayoutMode::Auto => Shape::Tabs,
        }
    }
}

/// Screens of the tabs arrangement (small terminals).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Ops,
    Agent,
    Mana,
    Spend,
    System,
}

pub const TABS: [(Tab, &str, Panel); 5] = [
    (Tab::Ops, "OPS", Panel::Swarm),
    (Tab::Agent, "AGENT", Panel::Agent),
    (Tab::Mana, "MANA", Panel::Mana),
    (Tab::Spend, "SPEND", Panel::Projects),
    (Tab::System, "SYSTEM", Panel::System),
];

#[derive(Debug, Clone)]
pub struct EventLine {
    pub at: DateTime<Utc>,
    pub glyph: &'static str,
    pub text: String,
    pub tone: Tone,
    pub born: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Info,
    Good,
    Warn,
    Bad,
    Accent,
}

pub struct Boot {
    pub started: Instant,
    pub progress: Progress,
    pub ready: Option<(usize, usize, usize)>,
    pub ready_at: Option<Instant>,
    pub skip: bool,
}

/// Something that needs a look, in order of urgency.
#[derive(Debug, Clone)]
pub struct Attn {
    pub severity: u8,
    pub glyph: &'static str,
    pub tone: Tone,
    pub text: String,
    pub since: Option<DateTime<Utc>>,
    pub target: AttnTarget,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AttnTarget {
    Agent(String),
    Mana,
    None,
}

/// One row of the MANA · SEATS panel.
#[derive(Debug, Clone)]
pub enum ManaRow {
    Seat(String),
    Login(String),
}

pub struct App {
    pub theme: Theme,
    pub mask: bool,
    /// SWARM shows activity lanes (true) or the spend sparkline (false); `v` toggles.
    pub lanes: bool,
    pub paused: bool,
    pub eco: bool,
    pub bell: bool,
    pub layout: LayoutMode,
    pub shape: Shape,
    pub tab: Tab,
    pub focus: Panel,
    pub zoom: Option<Panel>,
    pub help: bool,
    /// Help page on small screens: 0 keys, 1 legend (both side by side on wide ones).
    pub help_page: u8,
    pub boot: Option<Boot>,
    pub started: Instant,
    pub snapshot: Snapshot,
    pub live: Vec<LiveSession>,
    pub system: SystemView,
    pub analytics: Analytics,
    pub timeline: (String, Vec<ToolLine>, Option<String>),
    /// The selected agent's requests (key, points).
    pub life: (String, Vec<crate::live::LifePoint>),
    pub events: VecDeque<EventLine>,
    prev_states: HashMap<String, (State, String, f64)>,
    /// The selected agent, by key: rows move, the selection does not.
    pub selected_key: Option<String>,
    /// Per-panel cursor (lists) or scroll offset (text).
    pub cursor: HashMap<Panel, usize>,
    pub period: Period,
    pub group: Dim,
    pub command: Option<String>,
    pub filter: Option<String>,
    pub filter_edit: bool,
    pub toast: Option<(String, Tone, Instant)>,
    pub error: Option<String>,
    pub alerts: HashMap<String, Instant>,
    /// Where each panel was drawn last frame, for the mouse.
    pub rects: Vec<(Panel, Rect)>,
    analytics_request: bool,
    pub(super) seats: Vec<Seat>,
    pub(super) vault_dir: PathBuf,
    pub vault: super::vault_ui::VaultUi,
    pub home: PathBuf,
}

const PERIODS: [&str; 4] = ["today", "7d", "30d", "all"];
const GROUPS: [Dim; 6] = [
    Dim::Project,
    Dim::Model,
    Dim::AgentType,
    Dim::Account,
    Dim::Seat,
    Dim::Session,
];

impl App {
    pub fn new(
        theme: Theme,
        mask: bool,
        no_boot: bool,
        seats: Vec<Seat>,
        vault_dir: PathBuf,
        home: PathBuf,
    ) -> App {
        let skip = no_boot || !theme.animate;
        App {
            theme,
            mask,
            lanes: true,
            paused: false,
            eco: false,
            bell: true,
            layout: LayoutMode::Auto,
            shape: Shape::Wall,
            tab: Tab::Ops,
            focus: Panel::Swarm,
            zoom: None,
            help: false,
            help_page: 0,
            boot: Some(Boot {
                started: Instant::now(),
                progress: Progress::default(),
                ready: None,
                ready_at: None,
                skip,
            }),
            started: Instant::now(),
            snapshot: Snapshot::default(),
            live: Vec::new(),
            system: SystemView::default(),
            analytics: Analytics::default(),
            timeline: (String::new(), Vec::new(), None),
            life: (String::new(), Vec::new()),
            events: VecDeque::new(),
            prev_states: HashMap::new(),
            selected_key: None,
            cursor: HashMap::new(),
            period: Period::Days(7),
            group: Dim::Project,
            command: None,
            filter: None,
            filter_edit: false,
            toast: None,
            error: None,
            alerts: HashMap::new(),
            rects: Vec::new(),
            analytics_request: false,
            seats,
            vault_dir,
            vault: Default::default(),
            home,
        }
    }

    pub fn animating(&self) -> bool {
        !self.paused && self.theme.animate
    }

    pub fn t(&self) -> f64 {
        self.started.elapsed().as_secs_f64()
    }

    pub fn request_analytics(&mut self) {
        self.analytics_request = true;
    }

    pub fn take_analytics_request(&mut self) -> bool {
        std::mem::take(&mut self.analytics_request)
    }

    pub fn cur(&self, p: Panel) -> usize {
        self.cursor.get(&p).copied().unwrap_or(0)
    }

    fn set_cur(&mut self, p: Panel, v: usize) {
        self.cursor.insert(p, v);
    }

    // ───────────── agents ─────────────

    /// Filter chips: `p:project m:model s:seat state:needs`, anything else matches names.
    pub fn visible_agents(&self) -> Vec<&Agent> {
        let f = self.filter.as_deref().unwrap_or("").to_lowercase();
        let terms: Vec<&str> = f.split_whitespace().collect();
        self.snapshot
            .agents
            .iter()
            .filter(|a| {
                terms.iter().all(|t| match t.split_once(':') {
                    Some(("p", v)) => a.project.to_lowercase().contains(v),
                    Some(("m", v)) => a.model.as_deref().unwrap_or("").to_lowercase().contains(v),
                    Some(("s", v)) => a.seat.to_lowercase().contains(v),
                    Some(("state", v)) => format!("{:?}", a.state).to_lowercase().contains(v),
                    _ => a.name.to_lowercase().contains(t) || a.project.to_lowercase().contains(t),
                })
            })
            .collect()
    }

    pub fn selected_index(&self) -> usize {
        let v = self.visible_agents();
        match &self.selected_key {
            Some(k) => v.iter().position(|a| a.key() == *k).unwrap_or(0),
            None => 0,
        }
    }

    pub fn selected_agent(&self) -> Option<&Agent> {
        let v = self.visible_agents();
        v.get(self.selected_index()).copied()
    }

    fn move_agent(&mut self, delta: isize) {
        let keys: Vec<String> = self.visible_agents().iter().map(|a| a.key()).collect();
        if keys.is_empty() {
            return;
        }
        let i = self.selected_index() as isize + delta;
        let i = i.clamp(0, keys.len() as isize - 1) as usize;
        self.selected_key = Some(keys[i].clone());
        self.cursor.insert(Panel::Agent, 0);
    }

    pub fn select_agent(&mut self, key: &str) {
        self.selected_key = Some(key.to_string());
        self.filter = None;
        self.cursor.insert(Panel::Agent, 0);
    }

    // ───────────── accounts ─────────────

    /// The 5-hour and weekly estimate of an account, with the weekly reset.
    pub fn mana_of(
        &self,
        account: Option<&str>,
    ) -> (Option<f64>, Option<f64>, Option<DateTime<Utc>>) {
        let Some(a) = account else {
            return (None, None, None);
        };
        let Some(rep) = self.analytics.limits.iter().find(|r| r.account_uuid == a) else {
            return (None, None, None);
        };
        let five = rep
            .windows
            .iter()
            .find(|w| w.reading.kind == WindowKind::FiveHour)
            .map(|w| w.estimate_now);
        let week = rep
            .windows
            .iter()
            .find(|w| w.reading.kind == WindowKind::SevenDay);
        (
            five,
            week.map(|w| w.estimate_now),
            week.map(|w| w.window.end),
        )
    }

    pub fn account_label(&self, uuid: &str) -> String {
        self.analytics
            .limits
            .iter()
            .find(|r| r.account_uuid == uuid)
            .map(|r| r.name.clone())
            .unwrap_or_else(|| format!("acct-{}", &uuid[..uuid.len().min(8)]))
    }

    /// Rows of the MANA · SEATS panel: every seat, then every saved login that is in no seat.
    pub fn mana_rows(&self) -> Vec<ManaRow> {
        let mut rows: Vec<ManaRow> = self
            .seats
            .iter()
            .map(|s| ManaRow::Seat(s.label.clone()))
            .collect();
        let mut parked: Vec<&crate::vault::Login> = self
            .vault
            .logins
            .iter()
            .filter(|l| l.mounted.is_none())
            .collect();
        parked.sort_by(|a, b| {
            let (fa, wa, _) = self.mana_of(a.account_uuid.as_deref());
            let (fb, wb, _) = self.mana_of(b.account_uuid.as_deref());
            let ka = fa.unwrap_or(0.0).max(wa.unwrap_or(200.0));
            let kb = fb.unwrap_or(0.0).max(wb.unwrap_or(200.0));
            ka.total_cmp(&kb)
        });
        rows.extend(parked.into_iter().map(|l| ManaRow::Login(l.label.clone())));
        rows
    }

    // ───────────── attention ─────────────

    pub fn attention(&self) -> Vec<Attn> {
        let mut out = Vec::new();
        let now = Utc::now();
        for a in &self.snapshot.agents {
            let name = if a.is_main() {
                format!("{} · {}", a.name, a.project)
            } else {
                a.name.clone()
            };
            match &a.state {
                State::NeedsYou(w) => out.push(Attn {
                    severity: 0,
                    glyph: "▲",
                    tone: Tone::Warn,
                    text: format!("{name}: {w}"),
                    since: Some(a.since),
                    target: AttnTarget::Agent(a.key()),
                }),
                State::Stalled => out.push(Attn {
                    severity: 2,
                    glyph: "✖",
                    tone: Tone::Bad,
                    text: format!("{name}: stalled"),
                    since: Some(a.last_activity),
                    target: AttnTarget::Agent(a.key()),
                }),
                State::Tool { name: tool, .. } if (now - a.since).num_minutes() >= 10 => {
                    out.push(Attn {
                        severity: 4,
                        glyph: "◷",
                        tone: Tone::Warn,
                        text: format!("{name}: {tool} running {}", fmt::duration(now - a.since)),
                        since: Some(a.since),
                        target: AttnTarget::Agent(a.key()),
                    })
                }
                _ => {}
            }
            if let Some(m) = &a.model {
                let frac = a.context_tokens as f64 / context_window(m) as f64;
                if frac >= 0.8 && a.state.is_active() {
                    out.push(Attn {
                        severity: 5,
                        glyph: "◔",
                        tone: Tone::Warn,
                        text: format!("{name}: context {:.0}% full", frac * 100.0),
                        since: None,
                        target: AttnTarget::Agent(a.key()),
                    });
                }
            }
        }
        for (kind, n) in &self.snapshot.errors_last_hour {
            out.push(Attn {
                severity: 3,
                glyph: "!",
                tone: Tone::Bad,
                text: format!("API error {kind} ×{n} in the last hour"),
                since: None,
                target: AttnTarget::None,
            });
        }
        let seated: Vec<&str> = self
            .analytics
            .seats
            .iter()
            .filter_map(|s| s.account.as_deref())
            .collect();
        for rep in &self.analytics.limits {
            // A login parked in the vault is not urgent: nothing runs on it.
            let parked = !seated.is_empty() && !seated.contains(&rep.account_uuid.as_str());
            for w in &rep.windows {
                let label = format!("{} {}", rep.name, w.reading.kind.label());
                if w.estimate_now >= 100.0 && !w.reset_passed {
                    out.push(Attn {
                        severity: if parked { 8 } else { 1 },
                        glyph: "⊘",
                        tone: if parked { Tone::Info } else { Tone::Bad },
                        text: format!(
                            "{label}: limit reached · resets in {}{}",
                            fmt::duration(w.window.end - now),
                            if parked {
                                " (saved login, not in a seat)"
                            } else {
                                " · S puts the best login in"
                            }
                        ),
                        since: None,
                        target: AttnTarget::Mana,
                    });
                } else if let Some(eta) = w.eta.filter(|d| d.num_hours() < 6) {
                    out.push(Attn {
                        severity: 1,
                        glyph: "⊘",
                        tone: Tone::Warn,
                        text: format!(
                            "{label}: {} used · runs out in {} ({})",
                            fmt::pct(w.estimate_now),
                            fmt::duration(eta),
                            (now + eta).with_timezone(&chrono::Local).format("%a %H:%M")
                        ),
                        since: None,
                        target: AttnTarget::Mana,
                    });
                } else if w.estimate_now >= 85.0 {
                    out.push(Attn {
                        severity: 6,
                        glyph: "⊘",
                        tone: Tone::Warn,
                        text: format!("{label}: {} used", fmt::pct(w.estimate_now)),
                        since: None,
                        target: AttnTarget::Mana,
                    });
                }
            }
        }
        for s in &self.analytics.seats {
            if s.state.unverified_since.is_some() {
                out.push(Attn {
                    severity: 5,
                    glyph: "⚿",
                    tone: Tone::Warn,
                    text: format!(
                        "seat {}: new login, account not known yet (start Claude Code there once)",
                        s.label
                    ),
                    since: s.state.unverified_since,
                    target: AttnTarget::Mana,
                });
            } else if s.state.fingerprint.is_some() && s.login.is_none() {
                out.push(Attn {
                    severity: 7,
                    glyph: "⚿",
                    tone: Tone::Info,
                    text: format!(
                        "seat {}: save this login to switch back to it later (c)",
                        s.label
                    ),
                    since: None,
                    target: AttnTarget::Mana,
                });
            }
        }
        for l in &self.vault.logins {
            if let Some(t) = l.meta.refresh_expires_at
                && (t - now).num_days() < 7
            {
                out.push(Attn {
                    severity: 6,
                    glyph: "⚿",
                    tone: Tone::Warn,
                    text: format!("login {} expires in {}", l.label, fmt::duration(t - now)),
                    since: None,
                    target: AttnTarget::Mana,
                });
            }
        }
        // Two agents editing the same file within the last half hour.
        for (file, n) in &self.snapshot.conflicts {
            let short: Vec<&str> = file.rsplit('/').take(2).collect();
            let short = if self.mask {
                "•••".to_string()
            } else {
                short.into_iter().rev().collect::<Vec<_>>().join("/")
            };
            out.push(Attn {
                severity: 3,
                glyph: "⚠",
                tone: Tone::Warn,
                text: format!("{n} agents edited {short} in the last 30 min"),
                since: None,
                target: AttnTarget::None,
            });
        }
        out.sort_by_key(|a| a.severity);
        out
    }

    fn jump_attention(&mut self, delta: isize) {
        let items = self.attention();
        if items.is_empty() {
            self.toast("nothing needs attention", Tone::Good);
            return;
        }
        let i =
            (self.cur(Panel::Attention) as isize + delta).rem_euclid(items.len() as isize) as usize;
        self.set_cur(Panel::Attention, i);
        self.act_on_attention(&items[i]);
    }

    fn act_on_attention(&mut self, a: &Attn) {
        match &a.target {
            AttnTarget::Agent(k) => {
                self.select_agent(k);
                self.focus_panel(Panel::Swarm);
            }
            AttnTarget::Mana => self.focus_panel(Panel::Mana),
            AttnTarget::None => {}
        }
    }

    // ───────────── messages ─────────────

    pub(super) fn log_event(&mut self, glyph: &'static str, text: String, tone: Tone) {
        self.events.push_front(EventLine {
            at: Utc::now(),
            glyph,
            text,
            tone,
            born: Instant::now(),
        });
        self.events.truncate(300);
    }

    fn ring(&self) {
        if self.bell {
            print!("\x07");
        }
    }

    pub fn on_msg(&mut self, m: Msg) {
        match m {
            Msg::Progress(p) => {
                if let Some(b) = &mut self.boot {
                    b.progress = p;
                }
            }
            Msg::Ready {
                seats,
                accounts,
                files,
            } => {
                if let Some(b) = &mut self.boot {
                    b.ready = Some((seats, accounts, files));
                    b.ready_at = Some(Instant::now());
                }
                self.log_event(
                    "◆",
                    format!(
                        "online · {seats} seats · {accounts} accounts · {} transcripts",
                        fmt::count(files as u64)
                    ),
                    Tone::Accent,
                );
            }
            Msg::Snapshot(s, live, sys) => {
                self.diff_events(&s);
                self.snapshot = *s;
                self.live = live;
                self.system = *sys;
                if self.selected_key.is_none()
                    || self
                        .selected_agent()
                        .is_none_or(|a| Some(a.key()) != self.selected_key)
                {
                    // Keep the selection when its agent is still listed, otherwise take the first.
                    let keep = self
                        .selected_key
                        .as_ref()
                        .is_some_and(|k| self.visible_agents().iter().any(|a| a.key() == *k));
                    if !keep {
                        self.selected_key = self.visible_agents().first().map(|a| a.key());
                    }
                }
            }
            Msg::Analytics(a) => {
                self.limit_alerts(&a);
                if self.vault.modal.is_none() && self.vault.picker.is_none() {
                    self.vault.logins = a.logins.clone();
                    self.vault.loaded = true;
                }
                self.analytics = *a;
            }
            Msg::Timeline(k, t, last, life) => {
                self.life = (k.clone(), life);
                self.timeline = (k, t, last);
            }
            Msg::Past(p) => {
                use crate::live::PastKind;
                // Old news: no highlight.
                let born = Instant::now()
                    .checked_sub(std::time::Duration::from_secs(600))
                    .unwrap_or_else(Instant::now);
                for e in p {
                    let (glyph, tone) = match e.kind {
                        PastKind::Spawned => ("+", Tone::Accent),
                        PastKind::Done => ("✓", Tone::Good),
                        PastKind::ApiError => ("!", Tone::Bad),
                        PastKind::Prompt => ("✎", Tone::Info),
                    };
                    self.events.push_back(EventLine {
                        at: e.at,
                        glyph,
                        text: e.text,
                        tone,
                        born,
                    });
                }
                self.events
                    .make_contiguous()
                    .sort_by(|a, b| b.at.cmp(&a.at));
                self.events.truncate(300);
            }
            Msg::Error(e) => self.error = Some(e),
        }
    }

    fn diff_events(&mut self, s: &Snapshot) {
        let first = self.prev_states.is_empty() && self.snapshot.taken_at.is_none();
        let mut next = HashMap::new();
        let mut ring = false;
        for a in &s.agents {
            let key = a.key();
            let name = if a.is_main() {
                format!("{} ({})", a.name, a.project)
            } else {
                a.name.clone()
            };
            if !first {
                match self.prev_states.get(&key) {
                    None if !a.is_main() && a.state != State::Done => {
                        let model = a
                            .model
                            .as_deref()
                            .map(crate::core::models::short_name)
                            .unwrap_or_default();
                        self.log_event(
                            "+",
                            format!(
                                "{name} spawned · {model}{}",
                                a.effort
                                    .as_deref()
                                    .map(|e| format!("/{e}"))
                                    .unwrap_or_default()
                            ),
                            Tone::Accent,
                        );
                        self.alerts.insert(key.clone(), Instant::now());
                    }
                    None if a.is_main() && a.state != State::Gone => {
                        self.log_event("◆", format!("{name} came online"), Tone::Info)
                    }
                    Some((prev, _, cost)) if *prev != a.state => match &a.state {
                        State::Done => {
                            let took = fmt::duration(a.last_activity - a.first_seen);
                            self.log_event(
                                "✓",
                                format!(
                                    "{name} done · {took} · {} (+{})",
                                    fmt::money(a.cost),
                                    fmt::money((a.cost - cost).max(0.0))
                                ),
                                Tone::Good,
                            );
                            self.alerts.insert(key.clone(), Instant::now());
                        }
                        State::NeedsYou(w) => {
                            self.log_event("▲", format!("{name} needs you: {w}"), Tone::Warn);
                            self.alerts.insert(key.clone(), Instant::now());
                            ring = true;
                        }
                        State::Stalled => {
                            self.log_event(
                                "✖",
                                format!("{name} stalled (no activity for a while)"),
                                Tone::Bad,
                            );
                            ring = true;
                        }
                        State::Gone if a.is_main() => {
                            self.log_event("○", format!("{name} exited"), Tone::Info)
                        }
                        _ => {}
                    },
                    _ => {}
                }
            }
            next.insert(key, (a.state.clone(), name, a.cost));
        }
        for (kind, n) in &s.errors_last_hour {
            let prev: u64 = self
                .snapshot
                .errors_last_hour
                .iter()
                .filter(|(k, _)| k == kind)
                .map(|(_, n)| *n)
                .sum();
            if !first && *n > prev {
                self.log_event("!", format!("API error: {kind} ×{}", n - prev), Tone::Bad);
            }
        }
        if ring {
            self.ring();
        }
        self.prev_states = next;
    }

    fn limit_alerts(&mut self, a: &Analytics) {
        let mut ring = false;
        for rep in &a.limits {
            for w in &rep.windows {
                let prev = self
                    .analytics
                    .limits
                    .iter()
                    .find(|r| r.account_uuid == rep.account_uuid)
                    .and_then(|r| r.windows.iter().find(|x| x.reading.kind == w.reading.kind))
                    .map(|x| x.estimate_now);
                if let Some(p) = prev {
                    for th in [80.0, 90.0, 100.0] {
                        if p < th && w.estimate_now >= th {
                            self.log_event(
                                "▲",
                                format!(
                                    "MANA {} {} crossed {th:.0}%",
                                    rep.name,
                                    w.reading.kind.label()
                                ),
                                Tone::Bad,
                            );
                            self.alerts
                                .insert(format!("mana:{}", rep.account_uuid), Instant::now());
                            ring |= th >= 90.0;
                        }
                    }
                    if p >= 50.0 && w.estimate_now < 5.0 {
                        self.log_event(
                            "◆",
                            format!("MANA RESTORED · {} {}", rep.name, w.reading.kind.label()),
                            Tone::Good,
                        );
                        self.alerts
                            .insert(format!("restored:{}", rep.account_uuid), Instant::now());
                    }
                }
            }
        }
        if ring {
            self.ring();
        }
    }

    pub fn toast(&mut self, msg: impl Into<String>, tone: Tone) {
        self.toast = Some((msg.into(), tone, Instant::now()));
    }

    pub fn tone_color(&self, t: Tone) -> Color {
        match t {
            Tone::Info => self.theme.dim,
            Tone::Good => self.theme.good,
            Tone::Warn => self.theme.warn,
            Tone::Bad => self.theme.bad,
            Tone::Accent => self.theme.accent,
        }
    }

    // ───────────── focus ─────────────

    /// Panels on screen in the current arrangement, in Tab order.
    pub fn visible_panels(&self) -> Vec<Panel> {
        if let Some(z) = self.zoom {
            return vec![z];
        }
        match self.shape {
            Shape::Wall => vec![
                Panel::Swarm,
                Panel::Agent,
                Panel::Mana,
                Panel::Attention,
                Panel::Burn,
                Panel::Projects,
                Panel::Models,
                Panel::History,
                Panel::Efficiency,
            ],
            Shape::Column => vec![
                Panel::Swarm,
                Panel::Agent,
                Panel::Mana,
                Panel::Attention,
                Panel::Projects,
                Panel::Efficiency,
            ],
            Shape::Tabs => match self.tab {
                Tab::Ops => vec![Panel::Swarm, Panel::Mana, Panel::Attention],
                Tab::Agent => vec![Panel::Swarm, Panel::Agent],
                Tab::Mana => vec![Panel::Mana],
                Tab::Spend => vec![
                    Panel::Projects,
                    Panel::Efficiency,
                    Panel::Models,
                    Panel::History,
                ],
                Tab::System => vec![Panel::System],
            },
        }
    }

    /// Focus a panel; if it is not on screen, zoom it (or switch to the tab that has it).
    pub fn focus_panel(&mut self, p: Panel) {
        self.focus = p;
        if self.zoom.is_some() && self.zoom != Some(p) {
            self.zoom = None;
        }
        if self.shape == Shape::Tabs {
            if !self.visible_panels().contains(&p) {
                self.tab = match p {
                    Panel::Agent => Tab::Agent,
                    Panel::Mana | Panel::Burn => Tab::Mana,
                    Panel::Projects | Panel::Models | Panel::History => Tab::Spend,
                    Panel::System => Tab::System,
                    _ => Tab::Ops,
                };
            }
            if !self.visible_panels().contains(&p) {
                self.zoom = Some(p);
            }
        } else if !self.visible_panels().contains(&p) {
            self.zoom = Some(p);
        }
        if p == Panel::Mana && !self.vault.loaded {
            self.vault_reload();
        }
    }

    fn cycle_focus(&mut self, delta: isize) {
        let v = self.visible_panels();
        let i = v.iter().position(|p| *p == self.focus).unwrap_or(0) as isize;
        self.focus = v[(i + delta).rem_euclid(v.len() as isize) as usize];
    }

    // ───────────── keys ─────────────

    /// Returns true to quit.
    pub fn on_key(&mut self, k: KeyEvent) -> bool {
        if let Some(b) = &mut self.boot {
            b.skip = true;
            return k.code == KeyCode::Char('q');
        }
        if let Some(cmd) = &mut self.command {
            match k.code {
                KeyCode::Esc => self.command = None,
                KeyCode::Enter => {
                    let c = std::mem::take(cmd);
                    self.command = None;
                    return self.run_command(&c);
                }
                KeyCode::Backspace => {
                    cmd.pop();
                }
                KeyCode::Char(c) => cmd.push(c),
                _ => {}
            }
            return false;
        }
        if self.filter_edit {
            let f = self.filter.get_or_insert_with(String::new);
            match k.code {
                KeyCode::Esc => {
                    self.filter = None;
                    self.filter_edit = false;
                }
                KeyCode::Enter => self.filter_edit = false,
                KeyCode::Backspace => {
                    f.pop();
                }
                KeyCode::Char(c) => f.push(c),
                _ => {}
            }
            return false;
        }
        if self.help {
            match k.code {
                KeyCode::Tab | KeyCode::Left | KeyCode::Right => self.help_page ^= 1,
                // `?` again: the legend on small screens, then close.
                KeyCode::Char('?') if self.help_page == 0 && self.shape == Shape::Tabs => {
                    self.help_page = 1
                }
                KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') => {
                    self.help = false;
                    self.help_page = 0;
                }
                _ => {}
            }
            return false;
        }
        // Dialogs of the vault take every key.
        if (self.vault.modal.is_some() || self.vault.picker.is_some()) && self.vault_key(k) {
            return false;
        }
        if self.global_key(k) {
            return true;
        }
        false
    }

    /// Returns true to quit.
    fn global_key(&mut self, k: KeyEvent) -> bool {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Char('q') => return true,
            KeyCode::Char('c') if ctrl => return true,
            KeyCode::Char('?') => self.help = true,
            KeyCode::Char(':') => self.command = Some(String::new()),
            KeyCode::Char('/') => {
                self.filter_edit = true;
                self.filter.get_or_insert_with(String::new);
            }
            KeyCode::Tab => self.cycle_focus(1),
            KeyCode::BackTab => self.cycle_focus(-1),
            KeyCode::Char(c @ '0'..='9')
                if self.shape == Shape::Tabs && ('1'..='5').contains(&c) && self.zoom.is_none() =>
            {
                let (t, _, p) = TABS[c as usize - '1' as usize];
                self.tab = t;
                self.focus = p;
            }
            KeyCode::Char(c @ '0'..='9') => {
                let p = Panel::from_number(c as u8 - b'0');
                if self.focus == p && self.visible_panels().len() > 1 {
                    self.zoom = Some(p);
                } else {
                    self.focus_panel(p);
                }
            }
            KeyCode::F(n) if n <= 10 => {
                let p = Panel::from_number((n % 10) as u8);
                self.focus_panel(p);
            }
            KeyCode::Char('z') => {
                self.zoom = if self.zoom.is_some() {
                    None
                } else {
                    Some(self.focus)
                }
            }
            KeyCode::Esc => {
                if self.zoom.is_some() {
                    self.zoom = None;
                } else if self.filter.is_some() {
                    self.filter = None;
                } else if self.shape == Shape::Tabs {
                    self.tab = Tab::Ops;
                    self.focus = Panel::Swarm;
                }
            }
            KeyCode::Char('n') => self.jump_attention(1),
            KeyCode::Char('N') => self.jump_attention(-1),
            KeyCode::Char('J') => self.jump(),
            KeyCode::Char('v') => {
                self.lanes = !self.lanes;
                self.toast(
                    if self.lanes {
                        "SWARM: activity of the last 30 min (thinking · tool · waiting for agents · waiting for you)"
                    } else {
                        "SWARM: spend of the last 15 min"
                    },
                    Tone::Info,
                );
            }
            KeyCode::Char('m') => {
                self.mask = !self.mask;
                self.toast(
                    if self.mask {
                        "mask on: names, emails and paths hidden"
                    } else {
                        "mask off"
                    },
                    Tone::Info,
                );
            }
            KeyCode::Char('p') => {
                self.paused = !self.paused;
                self.toast(
                    if self.paused {
                        "animations paused"
                    } else {
                        "animations on"
                    },
                    Tone::Info,
                );
            }
            KeyCode::Char('T') => {
                self.theme = self.theme.next();
                self.toast(format!("theme: {}", self.theme.name), Tone::Info);
            }
            KeyCode::Char('L') => {
                self.layout = match self.layout {
                    LayoutMode::Auto => LayoutMode::Wall,
                    LayoutMode::Wall => LayoutMode::Column,
                    LayoutMode::Column => LayoutMode::Tabs,
                    LayoutMode::Tabs => LayoutMode::Auto,
                };
                self.zoom = None;
                self.toast(
                    format!("layout: {:?}", self.layout).to_lowercase(),
                    Tone::Info,
                );
            }
            KeyCode::Char('t') => {
                let cur = PERIODS
                    .iter()
                    .position(|p| *p == period_key(&self.period))
                    .unwrap_or(1);
                self.period = PERIODS[(cur + 1) % PERIODS.len()]
                    .parse()
                    .unwrap_or(Period::Days(7));
                self.request_analytics();
                self.toast(format!("period: {}", self.period.label()), Tone::Info);
            }
            KeyCode::Char('g') => {
                let cur = GROUPS.iter().position(|g| *g == self.group).unwrap_or(0);
                self.group = GROUPS[(cur + 1) % GROUPS.len()];
                self.request_analytics();
                self.toast(
                    format!("spend grouped by {}", self.group.title().to_lowercase()),
                    Tone::Info,
                );
            }
            _ => self.panel_key(k),
        }
        false
    }

    fn list_len(&self, p: Panel) -> usize {
        match p {
            // Alerts, then the event log under them.
            Panel::Attention => self.attention().len() + self.events.len(),
            // Pages of the efficiency panel.
            Panel::Efficiency => 3,
            // Days of the daily bars (0 = today).
            Panel::History => 14,
            Panel::Projects => self.analytics.projects.len(),
            Panel::Agent => self.timeline.1.len(),
            _ => 0,
        }
    }

    fn panel_key(&mut self, k: KeyEvent) {
        let step: isize = match k.code {
            KeyCode::Down | KeyCode::Char('j') => 1,
            KeyCode::Up | KeyCode::Char('k') => -1,
            KeyCode::PageDown => 10,
            KeyCode::PageUp => -10,
            KeyCode::Home => -100_000,
            KeyCode::End => 100_000,
            _ => 0,
        };
        match self.focus {
            Panel::Swarm => {
                if step != 0 {
                    self.move_agent(step);
                } else if k.code == KeyCode::Enter {
                    self.focus_panel(Panel::Agent);
                }
            }
            Panel::Mana => {
                self.vault_key(k);
            }
            Panel::Attention if k.code == KeyCode::Enter => {
                let items = self.attention();
                if let Some(a) = items.get(self.cur(Panel::Attention)).cloned() {
                    self.act_on_attention(&a);
                }
            }
            Panel::Projects if k.code == KeyCode::Enter => {
                if let Some(p) = self.analytics.projects.get(self.cur(Panel::Projects)) {
                    self.filter = Some(format!("p:{}", p.name.to_lowercase()));
                    self.toast(format!("agents of {} (Esc clears)", p.name), Tone::Info);
                    self.focus_panel(Panel::Swarm);
                }
            }
            Panel::History
                if matches!(
                    k.code,
                    KeyCode::Left | KeyCode::Right | KeyCode::Char('h') | KeyCode::Char('l')
                ) =>
            {
                let step = if matches!(k.code, KeyCode::Left | KeyCode::Char('h')) {
                    1
                } else {
                    -1
                };
                let v = (self.cur(Panel::History) as isize + step).clamp(0, 13) as usize;
                self.set_cur(Panel::History, v);
            }
            p @ (Panel::Attention
            | Panel::Projects
            | Panel::Efficiency
            | Panel::Agent
            | Panel::History)
                if step != 0 =>
            {
                let n = self.list_len(p).max(1);
                let v = (self.cur(p) as isize + step).clamp(0, n as isize - 1) as usize;
                self.set_cur(p, v);
            }
            _ => {}
        }
    }

    pub fn on_mouse(&mut self, m: MouseEvent) {
        let hit = self
            .rects
            .iter()
            .find(|(_, r)| {
                m.column >= r.x
                    && m.column < r.x + r.width
                    && m.row >= r.y
                    && m.row < r.y + r.height
            })
            .map(|(p, r)| (*p, *r));
        let Some((p, r)) = hit else { return };
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.focus = p;
                // A click on an agent row selects it (rows start two lines below the border).
                if p == Panel::Swarm {
                    let row = m.row.saturating_sub(r.y + 2) as usize;
                    let keys: Vec<String> = self.visible_agents().iter().map(|a| a.key()).collect();
                    let first = self.swarm_offset(r.height.saturating_sub(3) as usize);
                    if let Some(k) = keys.get(first + row) {
                        self.selected_key = Some(k.clone());
                    }
                }
            }
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                let prev = self.focus;
                self.focus = p;
                let code = if m.kind == MouseEventKind::ScrollDown {
                    KeyCode::Down
                } else {
                    KeyCode::Up
                };
                self.panel_key(KeyEvent::new(code, KeyModifiers::NONE));
                self.focus = prev;
            }
            _ => {}
        }
    }

    /// First visible row of the agent list for a list `rows` tall (keeps the selection on screen).
    pub fn swarm_offset(&self, rows: usize) -> usize {
        let sel = self.selected_index();
        if rows == 0 || sel < rows {
            0
        } else {
            sel + 1 - rows
        }
    }

    /// Switch the terminal to the tmux pane of the selected agent's Claude Code.
    fn jump(&mut self) {
        let Some(a) = self.selected_agent() else {
            return;
        };
        let main = self
            .snapshot
            .agents
            .iter()
            .find(|x| x.is_main() && x.seat == a.seat && x.session_id == a.session_id);
        let Some(pane) = main.and_then(|m| m.tmux.clone()).or_else(|| a.tmux.clone()) else {
            self.toast("this session is not running inside tmux", Tone::Warn);
            return;
        };
        let (session, rest) = pane.split_once(':').unwrap_or(("", pane.as_str()));
        let (window, pane_id) = rest.split_once('.').unwrap_or((rest, ""));
        if std::env::var_os("TMUX").is_none() {
            self.toast(format!("not inside tmux; attach with: tmux attach -t {session} \\; select-pane -t {pane_id}"), Tone::Warn);
            return;
        }
        let ok = std::process::Command::new("tmux")
            .args([
                "select-window",
                "-t",
                window,
                ";",
                "select-pane",
                "-t",
                pane_id,
                ";",
                "switch-client",
                "-t",
                session,
            ])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            self.toast(format!("jumped to {pane}"), Tone::Good);
        } else {
            self.toast(format!("tmux could not switch to {pane}"), Tone::Bad);
        }
    }

    fn run_command(&mut self, line: &str) -> bool {
        let parts: Vec<&str> = line.split_whitespace().collect();
        match parts.as_slice() {
            ["q"] | ["quit"] => return true,
            ["period", p] => match p.parse::<Period>() {
                Ok(p) => {
                    self.period = p;
                    self.request_analytics();
                }
                Err(e) => self.toast(e, Tone::Bad),
            },
            ["theme", t] => self.theme = Theme::named(Some(t)),
            ["layout", l] => {
                self.layout = match *l {
                    "wall" => LayoutMode::Wall,
                    "column" => LayoutMode::Column,
                    "tabs" => LayoutMode::Tabs,
                    _ => LayoutMode::Auto,
                }
            }
            ["filter", rest @ ..] => self.filter = Some(rest.join(" ")),
            ["capture", seat] | ["capture", seat, _] => {
                let label = parts.get(2).copied();
                let res = self.with_vault(|v, seats| {
                    let s = seats.iter().find(|s| s.label == *seat).ok_or_else(|| anyhow::anyhow!("no seat '{seat}'"))?;
                    v.capture(s, label)?.ok_or_else(|| anyhow::anyhow!("seat '{seat}' has no login"))
                });
                match res {
                    Ok(_) => self.toast(format!("saved the login of seat '{seat}'"), Tone::Good),
                    Err(e) => self.toast(format!("{e:#}"), Tone::Bad),
                }
                self.vault_reload();
                self.request_analytics();
            }
            ["switch", seat, login] => self.request_switch(seat.to_string(), login.to_string()),
            [] => {}
            _ => self.toast("commands: switch <seat> <login> · capture <seat> [label] · period 7d · filter p:shop · layout wall|column|tabs · theme <name> · q", Tone::Info),
        }
        false
    }

    fn with_vault<T>(
        &mut self,
        f: impl FnOnce(&mut Vault, &[Seat]) -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        let mut v = Vault::open(&self.vault_dir)?;
        v.sync(&self.seats)?;
        f(&mut v, &self.seats)
    }

    pub fn clock(&self) -> String {
        Local::now().format("%H:%M:%S").to_string()
    }
}

fn period_key(p: &Period) -> &'static str {
    match p {
        Period::Today => "today",
        Period::Days(7) => "7d",
        Period::Days(30) => "30d",
        Period::All => "all",
        _ => "7d",
    }
}
