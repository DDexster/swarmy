//! The VAULT screen, driven from the keyboard: pick a seat or a login, save, switch, rename,
//! forget, log into a new account.

use ratatui::crossterm::event::{KeyCode, KeyEvent};

use super::app::{App, Tone};
use crate::vault::{Login, Vault};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Focus {
    #[default]
    Seats,
    Logins,
}

/// A two-step switch: the first choice waits for its counterpart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pick {
    /// A seat was chosen; now choose the login to put into it.
    LoginFor(String),
    /// A login was chosen; now choose the seat to put it into.
    SeatFor(String),
}

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

/// Work that needs the real terminal (runs `claude` for a new login).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum External {
    AddLogin(String),
}

#[derive(Debug, Default)]
pub struct VaultUi {
    pub focus: Focus,
    pub seat_sel: usize,
    pub login_sel: usize,
    pub pick: Option<Pick>,
    pub logins: Vec<Login>,
    pub loaded: bool,
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
        self.vault.login_sel = self
            .vault
            .login_sel
            .min(self.vault.logins.len().saturating_sub(1));
    }

    fn selected_seat_label(&self) -> Option<String> {
        self.seats.get(self.vault.seat_sel).map(|s| s.label.clone())
    }

    fn selected_login_label(&self) -> Option<String> {
        self.vault
            .logins
            .get(self.vault.login_sel)
            .map(|l| l.label.clone())
    }

    fn live_in(&self, seat: &str) -> usize {
        self.live.iter().filter(|l| l.seat == seat).count()
    }

    /// Asks for confirmation when Claude Code runs in the seat, otherwise switches at once.
    fn request_switch(&mut self, seat: String, login: String) {
        self.vault.pick = None;
        let live = self.live_in(&seat);
        let text = if live > 0 {
            format!(
                "Put '{login}' into seat '{seat}'?\n\nClaude Code runs there ({live} session{}). It re-reads the login when the file changes \
                 and goes on under the new account; the account it shows stays stale until restart.\n\n[y] switch   [n] cancel",
                if live == 1 { "" } else { "s" }
            )
        } else {
            format!(
                "Put '{login}' into seat '{seat}'?\n\nThe seat's current login is saved first.\n\n[y] switch   [n] cancel"
            )
        };
        self.vault.modal = Some(Modal::Confirm {
            text,
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
                        format!("seat {}'s previous login auto-saved as '{saved}'", r.seat),
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
        self.request_analytics();
        self.vault_reload();
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
        self.request_analytics();
        self.vault_reload();
    }

    /// Keys of the VAULT screen. Returns true when the key was used.
    pub fn vault_key(&mut self, k: KeyEvent) -> bool {
        if !self.vault.loaded {
            self.vault_reload();
        }
        if let Some(m) = self.vault.modal.clone() {
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
                                    Ok(()) => self.toast(
                                        format!("renamed to '{}'", value.trim()),
                                        Tone::Good,
                                    ),
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
                                    Ok(l) => {
                                        self.toast(format!("forgot '{}'", l.label), Tone::Good)
                                    }
                                    Err(e) => self.toast(format!("{e:#}"), Tone::Bad),
                                }
                                self.vault_reload();
                                self.request_analytics();
                            }
                        }
                    }
                    KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                        self.vault.modal = None
                    }
                    _ => {}
                },
            }
            return true;
        }
        let v = &mut self.vault;
        match k.code {
            KeyCode::Up | KeyCode::Char('k') => {
                match v.focus {
                    Focus::Seats => v.seat_sel = v.seat_sel.saturating_sub(1),
                    Focus::Logins => v.login_sel = v.login_sel.saturating_sub(1),
                }
                true
            }
            KeyCode::Down | KeyCode::Char('j') => {
                match v.focus {
                    Focus::Seats => {
                        v.seat_sel = (v.seat_sel + 1).min(self.seats.len().saturating_sub(1))
                    }
                    Focus::Logins => {
                        v.login_sel = (v.login_sel + 1).min(v.logins.len().saturating_sub(1))
                    }
                }
                true
            }
            KeyCode::Left | KeyCode::Char('h') => {
                v.focus = Focus::Seats;
                true
            }
            KeyCode::Right | KeyCode::Char('l') => {
                v.focus = Focus::Logins;
                true
            }
            KeyCode::Esc if v.pick.is_some() => {
                v.pick = None;
                true
            }
            KeyCode::Enter => {
                match (v.focus, v.pick.clone()) {
                    (Focus::Logins, Some(Pick::LoginFor(seat))) => {
                        if let Some(l) = self.selected_login_label() {
                            self.request_switch(seat, l);
                        }
                    }
                    (Focus::Seats, Some(Pick::SeatFor(login))) => {
                        if let Some(s) = self.selected_seat_label() {
                            self.request_switch(s, login);
                        }
                    }
                    (Focus::Seats, _) => {
                        if self.vault.logins.is_empty() {
                            self.toast("no saved logins yet: press c to save this seat's login, a to add one", Tone::Warn);
                        } else if let Some(s) = self.selected_seat_label() {
                            self.vault.pick = Some(Pick::LoginFor(s));
                            self.vault.focus = Focus::Logins;
                        }
                    }
                    (Focus::Logins, _) => {
                        if let Some(l) = self.selected_login_label() {
                            self.vault.pick = Some(Pick::SeatFor(l));
                            self.vault.focus = Focus::Seats;
                        }
                    }
                }
                true
            }
            KeyCode::Char('c') => {
                if let Some(s) = self.selected_seat_label() {
                    self.vault.modal = Some(Modal::Input {
                        title: format!("Save the login of seat '{s}' as"),
                        value: s.clone(),
                        action: InputAction::Capture(s),
                    });
                }
                true
            }
            KeyCode::Char('R') => {
                self.vault_reload();
                self.request_analytics();
                self.toast("re-reading seats and logins…", Tone::Info);
                true
            }
            KeyCode::Char('a') => {
                self.vault.modal = Some(Modal::Input {
                    title: "Label for the new login (Claude Code opens: /login, then /exit)".into(),
                    value: String::new(),
                    action: InputAction::Add,
                });
                true
            }
            KeyCode::Char('r') if v.focus == Focus::Logins => {
                if let Some(l) = self.selected_login_label() {
                    self.vault.modal = Some(Modal::Input {
                        title: format!("Rename '{l}' to"),
                        value: l.clone(),
                        action: InputAction::Rename(l),
                    });
                }
                true
            }
            KeyCode::Char('d') | KeyCode::Delete if v.focus == Focus::Logins => {
                if let Some(l) = self.vault.logins.get(self.vault.login_sel).cloned() {
                    let note = match &l.mounted {
                        Some(s) => format!(
                            "\n\nIt is in use in seat '{s}'; the seat keeps working, the vault forgets its copy."
                        ),
                        None => String::new(),
                    };
                    self.vault.modal = Some(Modal::Confirm {
                        text: format!(
                            "Forget the saved login '{}'?{note}\n\n[y] forget   [n] cancel",
                            l.label
                        ),
                        action: ConfirmAction::Remove(l.label),
                    });
                }
                true
            }
            _ => false,
        }
    }

    /// Runs `claude` in a scratch seat for `/login`, then saves the login. Called with the
    /// dashboard suspended (the terminal belongs to Claude Code meanwhile).
    pub fn run_add_login(&mut self, label: &str) {
        let scratch = self
            .vault_dir
            .join(format!("new-{}", chrono::Utc::now().format("%Y%m%d%H%M%S")));
        println!(
            "\nagentmana: starting Claude Code in a scratch seat for '{label}'. Run /login, choose the account, then /exit.\n"
        );
        let res = (|| -> anyhow::Result<()> {
            std::fs::create_dir_all(&scratch)?;
            let status = std::process::Command::new("claude")
                .env("CLAUDE_CONFIG_DIR", &scratch)
                .status()
                .map_err(|e| anyhow::anyhow!("cannot start `claude`: {e}"))?;
            let _ = status;
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
