//! Dashboard state and input handling.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::time::Instant;

use chrono::{DateTime, Local, Utc};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::style::Color;

use super::theme::Theme;
use super::{Analytics, Msg, SystemView};
use crate::claude::registry::LiveSession;
use crate::claude::seats::Seat;
use crate::core::period::Period;
use crate::index::Progress;
use crate::index::query::Dim;
use crate::live::{Agent, Snapshot, State, ToolLine};
use crate::ui::fmt;
use crate::vault::Vault;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Ops,
    Agents,
    Spend,
    Models,
    Mana,
    Vault,
    System,
}

pub const SCREENS: [(Screen, &str); 7] = [
    (Screen::Ops, "OPS"),
    (Screen::Agents, "AGENTS"),
    (Screen::Spend, "SPEND"),
    (Screen::Models, "MODELS"),
    (Screen::Mana, "MANA"),
    (Screen::Vault, "VAULT"),
    (Screen::System, "SYSTEM"),
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

pub struct App {
    pub theme: Theme,
    pub mask: bool,
    pub paused: bool,
    /// Low-power mode (on battery or --eco).
    pub eco: bool,
    pub screen: Screen,
    pub help: bool,
    pub zoom: bool,
    pub boot: Option<Boot>,
    pub started: Instant,
    pub snapshot: Snapshot,
    pub live: Vec<LiveSession>,
    pub system: SystemView,
    pub analytics: Analytics,
    pub timeline: (String, Vec<ToolLine>),
    pub events: VecDeque<EventLine>,
    prev_states: HashMap<String, (State, String, f64)>,
    pub selected: usize,
    pub period: Period,
    pub group: Dim,
    pub command: Option<String>,
    pub filter: Option<String>,
    pub filter_edit: bool,
    pub toast: Option<(String, Tone, Instant)>,
    pub error: Option<String>,
    pub alerts: HashMap<String, Instant>,
    analytics_request: bool,
    pub(super) seats: Vec<Seat>,
    pub(super) vault_dir: PathBuf,
    pub vault: super::vault_ui::VaultUi,
    pub home: PathBuf,
}

const PERIODS: [&str; 5] = ["today", "week", "7d", "30d", "all"];
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
            paused: false,
            eco: false,
            screen: Screen::Ops,
            help: false,
            zoom: false,
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
            timeline: (String::new(), Vec::new()),
            events: VecDeque::new(),
            prev_states: HashMap::new(),
            selected: 0,
            period: Period::LimitWeek,
            group: Dim::Project,
            command: None,
            filter: None,
            filter_edit: false,
            toast: None,
            error: None,
            alerts: HashMap::new(),
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

    /// Seconds since start, frozen while paused: drives every animation.
    pub fn t(&self) -> f64 {
        self.started.elapsed().as_secs_f64()
    }

    pub fn request_analytics(&mut self) {
        self.analytics_request = true;
    }

    pub fn take_analytics_request(&mut self) -> bool {
        std::mem::take(&mut self.analytics_request)
    }

    pub fn visible_agents(&self) -> Vec<&Agent> {
        let f = self.filter.as_deref().map(str::to_lowercase);
        self.snapshot
            .agents
            .iter()
            .filter(|a| match &f {
                Some(f) if !f.is_empty() => {
                    a.name.to_lowercase().contains(f)
                        || a.project.to_lowercase().contains(f)
                        || a.model.as_deref().unwrap_or("").contains(f.as_str())
                }
                _ => true,
            })
            .collect()
    }

    pub fn selected_agent(&self) -> Option<&Agent> {
        let v = self.visible_agents();
        v.get(self.selected.min(v.len().saturating_sub(1))).copied()
    }

    pub(super) fn log_event(&mut self, glyph: &'static str, text: String, tone: Tone) {
        self.events.push_front(EventLine {
            at: Utc::now(),
            glyph,
            text,
            tone,
            born: Instant::now(),
        });
        self.events.truncate(200);
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
                let n = self.visible_agents().len();
                if n > 0 && self.selected >= n {
                    self.selected = n - 1;
                }
            }
            Msg::Analytics(a) => {
                self.limit_alerts(&a);
                // The worker re-reads the vault with every analytics pass (and at once when a
                // login file changes), so the VAULT screen follows changes made elsewhere.
                if self.vault.modal.is_none() {
                    self.vault.logins = a.logins.clone();
                    self.vault.loaded = true;
                    self.vault.login_sel = self
                        .vault
                        .login_sel
                        .min(self.vault.logins.len().saturating_sub(1));
                }
                self.analytics = *a;
            }
            Msg::Timeline(k, t) => self.timeline = (k, t),
            Msg::Error(e) => self.error = Some(e),
        }
    }

    fn diff_events(&mut self, s: &Snapshot) {
        let first = self.prev_states.is_empty() && self.snapshot.taken_at.is_none();
        let mut next = HashMap::new();
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
                            self.log_event(
                                "✓",
                                format!(
                                    "{name} done · {} · +{}",
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
                            if self.theme.animate {
                                print!("\x07");
                            }
                        }
                        State::Stalled => self.log_event(
                            "✖",
                            format!("{name} stalled (no activity for a while)"),
                            Tone::Bad,
                        ),
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
        self.prev_states = next;
    }

    fn limit_alerts(&mut self, a: &Analytics) {
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

    /// Returns true to quit.
    pub fn on_key(&mut self, k: KeyEvent) -> bool {
        if let Some(b) = &mut self.boot {
            b.skip = true;
            if k.code == KeyCode::Char('q') {
                return true;
            }
            return false;
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
            self.selected = 0;
            return false;
        }
        if self.help {
            self.help = false;
            return false;
        }
        if self.screen == Screen::Vault && self.vault_key(k) {
            return false;
        }
        let before = self.screen;
        let quit = self.global_key(k);
        if self.screen == Screen::Vault && before != Screen::Vault {
            self.vault_reload();
        }
        quit
    }

    fn global_key(&mut self, k: KeyEvent) -> bool {
        let n = self.visible_agents().len();
        match k.code {
            KeyCode::Char('q') => return true,
            KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => return true,
            KeyCode::Esc => {
                if self.zoom {
                    self.zoom = false;
                } else if self.filter.is_some() {
                    self.filter = None;
                } else if self.screen != Screen::Ops {
                    self.screen = Screen::Ops;
                }
            }
            KeyCode::Char('?') => self.help = true,
            KeyCode::F(i) if (1..=7).contains(&i) => self.screen = SCREENS[i as usize - 1].0,
            KeyCode::Char(c @ '1'..='7') => self.screen = SCREENS[c as usize - '1' as usize].0,
            KeyCode::Tab => {
                let i = SCREENS
                    .iter()
                    .position(|(s, _)| *s == self.screen)
                    .unwrap_or(0);
                self.screen = SCREENS[(i + 1) % SCREENS.len()].0;
            }
            KeyCode::BackTab => {
                let i = SCREENS
                    .iter()
                    .position(|(s, _)| *s == self.screen)
                    .unwrap_or(0);
                self.screen = SCREENS[(i + SCREENS.len() - 1) % SCREENS.len()].0;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = (self.selected + 1).min(n.saturating_sub(1))
            }
            KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
            KeyCode::PageDown => self.selected = (self.selected + 10).min(n.saturating_sub(1)),
            KeyCode::PageUp => self.selected = self.selected.saturating_sub(10),
            KeyCode::Home => self.selected = 0,
            KeyCode::End | KeyCode::Char('G') => self.selected = n.saturating_sub(1),
            KeyCode::Enter => {
                if self.screen == Screen::Ops {
                    self.screen = Screen::Agents;
                }
            }
            KeyCode::Char('z') => self.zoom = !self.zoom,
            KeyCode::Char('/') => {
                self.filter_edit = true;
                self.filter.get_or_insert_with(String::new);
            }
            KeyCode::Char(':') => self.command = Some(String::new()),
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
            KeyCode::Char('t') => {
                let cur = PERIODS
                    .iter()
                    .position(|p| *p == period_key(&self.period))
                    .unwrap_or(1);
                self.period = PERIODS[(cur + 1) % PERIODS.len()]
                    .parse()
                    .unwrap_or(Period::LimitWeek);
                self.analytics_request = true;
                self.toast(format!("period: {}", self.period.label()), Tone::Info);
            }
            KeyCode::Char('s') if self.screen == Screen::Spend => {
                let cur = GROUPS.iter().position(|g| *g == self.group).unwrap_or(0);
                self.group = GROUPS[(cur + 1) % GROUPS.len()];
                self.analytics_request = true;
                self.toast(
                    format!("group by {}", self.group.title().to_lowercase()),
                    Tone::Info,
                );
            }
            KeyCode::Char('J') => self.jump(),
            _ => {}
        }
        false
    }

    pub fn on_mouse(&mut self, m: MouseEvent) {
        match m.kind {
            MouseEventKind::ScrollDown => {
                self.selected =
                    (self.selected + 1).min(self.visible_agents().len().saturating_sub(1))
            }
            MouseEventKind::ScrollUp => self.selected = self.selected.saturating_sub(1),
            _ => {}
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
        // "0:@0.%6": session 0, window @0, pane %6.
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
                    self.analytics_request = true;
                }
                Err(e) => self.toast(e, Tone::Bad),
            },
            ["theme", t] => self.theme = Theme::named(Some(t)),
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
                self.analytics_request = true;
            }
            ["switch", seat, login] | ["switch", seat, login, "force"] => {
                let force = parts.len() == 4;
                let live_here = self.live.iter().filter(|l| l.seat == *seat).count();
                if live_here > 0 && !force {
                    self.toast(format!("Claude Code runs in '{seat}' ({live_here}); add `force` to switch anyway"), Tone::Warn);
                    return false;
                }
                let res = self.with_vault(|v, seats| {
                    let s = seats.iter().find(|s| s.label == *seat).ok_or_else(|| anyhow::anyhow!("no seat '{seat}'"))?;
                    v.switch(s, login, false)
                });
                match res {
                    Ok(r) => {
                        self.log_event("⚿", format!("seat {} → {} (was {})", r.seat, r.to, r.from.as_deref().unwrap_or("-")), Tone::Accent);
                        self.alerts.insert("vault".into(), Instant::now());
                        self.toast(format!("seat '{}' now uses '{}'", r.seat, r.to), Tone::Good);
                    }
                    Err(e) => self.toast(format!("{e:#}"), Tone::Bad),
                }
                self.analytics_request = true;
            }
            [] => {}
            _ => self.toast("commands: switch <seat> <login> [force] · capture <seat> [label] · period <p> · theme <name> · q", Tone::Info),
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
        Period::LimitWeek => "week",
        Period::Days(7) => "7d",
        Period::Days(30) => "30d",
        Period::All => "all",
        _ => "week",
    }
}
