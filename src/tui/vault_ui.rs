//! The vault, operated inside the MANA · SEATS panel: seats first, then the logins parked in the
//! vault. Enter on a seat opens a picker of logins sorted by limit left (the best one first);
//! Enter on a parked login picks its seat. `S` puts the best login into the selected seat.

use ratatui::crossterm::event::{KeyCode, KeyEvent};

use super::app::{App, ManaRow, Panel, Tone};
use crate::vault::{Login, Vault};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputAction {
    Capture(String),
    Rename(String),
    Add,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfirmAction {
    Switch { seat: String, login: String },
    Remove(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Modal {
    Input {
        title: String,
        value: String,
        action: InputAction,
    },
    Confirm {
        text: String,
        action: ConfirmAction,
    },
}

/// What a picker chooses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickFor {
    /// A login for this seat.
    Seat(String),
    /// A seat for this login.
    Login(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picker {
    pub what: PickFor,
    /// Login labels (for a seat) or seat labels (for a login), best first.
    pub items: Vec<String>,
    pub sel: usize,
}

/// Work that needs the real terminal (runs `claude` for a new login).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum External {
    AddLogin(String),
}

#[derive(Debug, Default)]
pub struct VaultUi {
    pub logins: Vec<Login>,
    pub loaded: bool,
    pub picker: Option<Picker>,
    pub modal: Option<Modal>,
    pub external: Option<External>,
}

impl App {
    pub fn vault_reload(&mut self) {
        match Vault::open(&self.vault_dir).and_then(|mut v| {
            v.sync(&self.seats)?;
            Ok(v.logins().to_vec())
        }) {
            Ok(l) => {
                self.vault.logins = l;
                self.vault.loaded = true;
            }
            Err(e) => self.toast(format!("vault: {e:#}"), Tone::Bad),
        }
    }

    fn live_in(&self, seat: &str) -> usize {
        self.live.iter().filter(|l| l.seat == seat).count()
    }

    /// Limit used by a login's account: the larger of the 5-hour and weekly estimates (unknown
    /// sorts last).
    pub fn login_pressure(&self, l: &Login) -> f64 {
        let (five, week, _) = self.mana_of(l.account_uuid.as_deref());
        match (five, week) {
            (None, None) => 1_000.0,
            (f, w) => f.unwrap_or(0.0).max(w.unwrap_or(0.0)),
        }
    }

    /// Logins that could go into `seat`, the most limit left first.
    pub fn candidates_for(&self, _seat: &str) -> Vec<String> {
        // Logins mounted in another seat are left out: one grant in two seats breaks one of them.
        let mut v: Vec<&Login> = self
            .vault
            .logins
            .iter()
            .filter(|l| l.mounted.is_none())
            .collect();
        v.sort_by(|a, b| self.login_pressure(a).total_cmp(&self.login_pressure(b)));
        v.into_iter().map(|l| l.label.clone()).collect()
    }

    fn selected_row(&self) -> Option<ManaRow> {
        self.mana_rows().get(self.cur(Panel::Mana)).cloned()
    }

    /// Asks for confirmation (always naming what is saved first), then switches.
    pub fn request_switch(&mut self, seat: String, login: String) {
        self.vault.picker = None;
        let live = self.live_in(&seat);
        let current = self.analytics.seats.iter().find(|s| s.label == seat);
        let keeps = match current {
            Some(s) if s.login.is_some() => format!(
                "Its current login '{}' stays in the vault.",
                s.login.clone().unwrap_or_default()
            ),
            Some(s) if s.state.fingerprint.is_some() => {
                format!("Its current login is not saved yet: it is saved first, as '{seat}'.")
            }
            _ => String::new(),
        };
        let running = if live > 0 {
            format!(
                "\n\nClaude Code runs there ({live} session{}): it picks up the new login on its next request; restart it to refresh the account it shows.",
                if live == 1 { "" } else { "s" }
            )
        } else {
            String::new()
        };
        self.vault.modal = Some(Modal::Confirm {
            text: format!(
                "Put '{login}' into seat '{seat}'?\n\n{keeps}{running}\n\n[y] switch   [n] cancel"
            ),
            action: ConfirmAction::Switch { seat, login },
        });
    }

    fn do_switch(&mut self, seat: &str, login: &str) {
        let res = Vault::open(&self.vault_dir).and_then(|mut v| {
            v.sync(&self.seats)?;
            let s = self
                .seats
                .iter()
                .find(|s| s.label == seat)
                .ok_or_else(|| anyhow::anyhow!("no seat '{seat}'"))?;
            v.switch(s, login, false)
        });
        match res {
            Ok(r) if r.unchanged => self.toast(
                format!("seat '{}' already uses '{}'", r.seat, r.to),
                Tone::Info,
            ),
            Ok(r) => {
                if let Some(saved) = &r.auto_saved {
                    self.log_event(
                        "⚿",
                        format!("seat {}'s previous login saved as '{saved}'", r.seat),
                        Tone::Good,
                    );
                }
                self.log_event(
                    "⚿",
                    format!(
                        "seat {} → {} (was {})",
                        r.seat,
                        r.to,
                        r.from.as_deref().unwrap_or("-")
                    ),
                    Tone::Accent,
                );
                self.alerts
                    .insert("vault".into(), std::time::Instant::now());
                self.toast(format!("seat '{}' now uses '{}'", r.seat, r.to), Tone::Good);
            }
            Err(e) => self.toast(format!("{e:#}"), Tone::Bad),
        }
        self.vault_reload();
        self.request_analytics();
    }

    fn do_capture(&mut self, seat: &str, label: &str) {
        let label = label.trim();
        let res = Vault::open(&self.vault_dir).and_then(|mut v| {
            v.sync(&self.seats)?;
            let s = self
                .seats
                .iter()
                .find(|s| s.label == seat)
                .ok_or_else(|| anyhow::anyhow!("no seat '{seat}'"))?;
            v.capture(s, (!label.is_empty()).then_some(label))?
                .ok_or_else(|| anyhow::anyhow!("seat '{seat}' has no login"))
        });
        match res {
            Ok(_) => {
                self.log_event("⚿", format!("saved the login of seat {seat}"), Tone::Accent);
                self.toast(format!("saved the login of seat '{seat}'"), Tone::Good);
            }
            Err(e) => self.toast(format!("{e:#}"), Tone::Bad),
        }
        self.vault_reload();
        self.request_analytics();
    }

    /// Keys of the MANA · SEATS panel and of its dialogs. Returns true when the key was used.
    pub fn vault_key(&mut self, k: KeyEvent) -> bool {
        if !self.vault.loaded {
            self.vault_reload();
        }
        if let Some(m) = self.vault.modal.clone() {
            self.modal_key(m, k);
            return true;
        }
        if let Some(mut p) = self.vault.picker.clone() {
            match k.code {
                KeyCode::Esc => self.vault.picker = None,
                KeyCode::Down | KeyCode::Char('j') => {
                    p.sel = (p.sel + 1).min(p.items.len().saturating_sub(1));
                    self.vault.picker = Some(p);
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    p.sel = p.sel.saturating_sub(1);
                    self.vault.picker = Some(p);
                }
                KeyCode::Enter => {
                    if let Some(item) = p.items.get(p.sel).cloned() {
                        match p.what {
                            PickFor::Seat(seat) => self.request_switch(seat, item),
                            PickFor::Login(login) => self.request_switch(item, login),
                        }
                    }
                }
                _ => {}
            }
            return true;
        }
        let rows = self.mana_rows();
        let cur = self.cur(Panel::Mana).min(rows.len().saturating_sub(1));
        match k.code {
            KeyCode::Down | KeyCode::Char('j') => {
                self.cursor
                    .insert(Panel::Mana, (cur + 1).min(rows.len().saturating_sub(1)));
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.cursor.insert(Panel::Mana, cur.saturating_sub(1));
            }
            KeyCode::Enter => match self.selected_row() {
                Some(ManaRow::Seat(seat)) => {
                    let items = self.candidates_for(&seat);
                    if items.is_empty() {
                        self.toast("no free saved login (the others are in seats): c saves this seat's login, a adds one", Tone::Warn);
                    } else {
                        self.vault.picker = Some(Picker {
                            what: PickFor::Seat(seat),
                            items,
                            sel: 0,
                        });
                    }
                }
                Some(ManaRow::Login(login)) => {
                    let items: Vec<String> = self.seats.iter().map(|s| s.label.clone()).collect();
                    self.vault.picker = Some(Picker {
                        what: PickFor::Login(login),
                        items,
                        sel: 0,
                    });
                }
                None => {}
            },
            KeyCode::Char('S') => {
                if let Some(ManaRow::Seat(seat)) = self.selected_row() {
                    match self.candidates_for(&seat).first().cloned() {
                        Some(best) => self.request_switch(seat, best),
                        None => self.toast("no other saved login to switch to", Tone::Warn),
                    }
                }
            }
            KeyCode::Char('c') => {
                if let Some(ManaRow::Seat(s)) = self.selected_row() {
                    self.vault.modal = Some(Modal::Input {
                        title: format!("Save the login of seat '{s}' as"),
                        value: s.clone(),
                        action: InputAction::Capture(s),
                    });
                } else {
                    self.toast("select a seat to save its login", Tone::Info);
                }
            }
            KeyCode::Char('a') => {
                self.vault.modal = Some(Modal::Input {
                    title: "Label for the new login (Claude Code opens: /login, then /exit)".into(),
                    value: String::new(),
                    action: InputAction::Add,
                });
            }
            KeyCode::Char('r') => {
                let label = match self.selected_row() {
                    Some(ManaRow::Login(l)) => Some(l),
                    Some(ManaRow::Seat(s)) => self
                        .analytics
                        .seats
                        .iter()
                        .find(|x| x.label == s)
                        .and_then(|x| x.login.clone()),
                    None => None,
                };
                if let Some(l) = label {
                    self.vault.modal = Some(Modal::Input {
                        title: format!("Rename '{l}' to"),
                        value: l.clone(),
                        action: InputAction::Rename(l),
                    });
                }
            }
            KeyCode::Char('d') | KeyCode::Delete => {
                if let Some(ManaRow::Login(label)) = self.selected_row() {
                    self.vault.modal = Some(Modal::Confirm {
                        text: format!(
                            "Forget the saved login '{label}'?\n\nIts tokens are deleted from the vault.\n\n[y] forget   [n] cancel"
                        ),
                        action: ConfirmAction::Remove(label),
                    });
                } else {
                    self.toast(
                        "select a saved login (below the seats) to forget it",
                        Tone::Info,
                    );
                }
            }
            KeyCode::Char('R') => {
                self.vault_reload();
                self.request_analytics();
                self.toast("re-reading seats and logins…", Tone::Info);
            }
            _ => return false,
        }
        true
    }

    fn modal_key(&mut self, m: Modal, k: KeyEvent) {
        match m {
            Modal::Input {
                title,
                mut value,
                action,
            } => match k.code {
                KeyCode::Esc => self.vault.modal = None,
                KeyCode::Enter => {
                    self.vault.modal = None;
                    match action {
                        InputAction::Capture(seat) => self.do_capture(&seat, &value),
                        InputAction::Rename(old) => {
                            let res = Vault::open(&self.vault_dir)
                                .and_then(|mut v| v.rename(&old, value.trim()));
                            match res {
                                Ok(()) => {
                                    self.toast(format!("renamed to '{}'", value.trim()), Tone::Good)
                                }
                                Err(e) => self.toast(format!("{e:#}"), Tone::Bad),
                            }
                            self.vault_reload();
                            self.request_analytics();
                        }
                        InputAction::Add => {
                            if value.trim().is_empty() {
                                self.toast("a label is needed", Tone::Warn);
                            } else {
                                self.vault.external =
                                    Some(External::AddLogin(value.trim().to_string()));
                            }
                        }
                    }
                }
                KeyCode::Backspace => {
                    value.pop();
                    self.vault.modal = Some(Modal::Input {
                        title,
                        value,
                        action,
                    });
                }
                KeyCode::Char(c) if !c.is_control() => {
                    value.push(c);
                    self.vault.modal = Some(Modal::Input {
                        title,
                        value,
                        action,
                    });
                }
                _ => {}
            },
            Modal::Confirm { action, .. } => match k.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                    self.vault.modal = None;
                    match action {
                        ConfirmAction::Switch { seat, login } => self.do_switch(&seat, &login),
                        ConfirmAction::Remove(login) => {
                            let res = Vault::open(&self.vault_dir)
                                .and_then(|mut v| v.remove(&login, true));
                            match res {
                                Ok(l) => self.toast(format!("forgot '{}'", l.label), Tone::Good),
                                Err(e) => self.toast(format!("{e:#}"), Tone::Bad),
                            }
                            self.vault_reload();
                            self.request_analytics();
                        }
                    }
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => self.vault.modal = None,
                _ => {}
            },
        }
    }

    /// Runs `claude` in a scratch seat for `/login`, then saves the login. Called with the
    /// dashboard suspended (the terminal belongs to Claude Code meanwhile).
    pub fn run_add_login(&mut self, label: &str) {
        let scratch = self
            .vault_dir
            .join(format!("new-{}", chrono::Utc::now().format("%Y%m%d%H%M%S")));
        println!(
            "\nswarmy: starting Claude Code in a scratch seat for '{label}'. Run /login, choose the account, then /exit.\n"
        );
        let res = (|| -> anyhow::Result<()> {
            std::fs::create_dir_all(&scratch)?;
            std::process::Command::new("claude")
                .env("CLAUDE_CONFIG_DIR", &scratch)
                .status()
                .map_err(|e| anyhow::anyhow!("cannot start `claude`: {e}"))?;
            let seat = crate::claude::seats::Seat {
                label: format!("scratch-{label}"),
                claude_json: scratch.join(".claude.json"),
                dir: scratch.clone(),
                is_default: false,
                found_by: vec![],
            };
            let s = crate::vault::read_seat(&seat)?;
            let secret = s
                .secret
                .ok_or_else(|| anyhow::anyhow!("no login was made"))?;
            let mut v = Vault::open(&self.vault_dir)?;
            v.put(
                &secret,
                s.oauth_account,
                s.account.as_ref(),
                Some(label),
                None,
            )?;
            Ok(())
        })();
        let _ = std::fs::remove_dir_all(&scratch);
        match res {
            Ok(()) => {
                self.log_event("⚿", format!("new login saved: {label}"), Tone::Accent);
                self.toast(format!("saved '{label}'"), Tone::Good);
            }
            Err(e) => self.toast(format!("{e:#}"), Tone::Bad),
        }
        self.vault_reload();
        self.request_analytics();
    }
}
