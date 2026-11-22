//! Switching the user's tmux client to the pane where a Claude Code runs. Claude Code records
//! its pane in the session registry as `session:@window.%pane`.

/// Outcome of a jump, for the caller to report.
pub enum Jump {
    Done,
    NotInTmux { hint: String },
    Failed,
}

pub fn switch_to(pane: &str) -> Jump {
    let (session, rest) = pane.split_once(':').unwrap_or(("", pane));
    let (window, pane_id) = rest.split_once('.').unwrap_or((rest, ""));
    if std::env::var_os("TMUX").is_none() {
        return Jump::NotInTmux {
            hint: format!(r"tmux attach -t {session} \; select-pane -t {pane_id}"),
        };
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
    if ok { Jump::Done } else { Jump::Failed }
}
