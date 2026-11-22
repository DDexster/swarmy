//! `agentmana shell-alias`: a short command name as a user alias.
//!
//! Packages ship only `agentmana`: two-letter commands collide (`am` is already the AppImage
//! manager AM on Linux and Android's activity manager), and distributions rename colliding
//! binaries. An alias in the user's own shell rc file collides with nothing.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Context;
use clap::{Args, ValueEnum};

use crate::ui::style::{Painter, Tone};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
    Powershell,
}

#[derive(Args, Debug, Clone)]
pub struct AliasArgs {
    /// The alias name.
    #[arg(long, default_value = "am")]
    pub name: String,
    /// Shell to configure (default: from $SHELL).
    #[arg(long, value_enum)]
    pub shell: Option<Shell>,
    /// Only print the line to add.
    #[arg(long)]
    pub print: bool,
    /// Add the alias even if a command with that name exists.
    #[arg(long)]
    pub force: bool,
}

fn detect_shell() -> Option<Shell> {
    if cfg!(windows) {
        return Some(Shell::Powershell);
    }
    let sh = std::env::var("SHELL").ok()?;
    let base = Path::new(&sh).file_name()?.to_string_lossy().into_owned();
    match base.as_str() {
        "zsh" => Some(Shell::Zsh),
        "bash" => Some(Shell::Bash),
        "fish" => Some(Shell::Fish),
        "pwsh" | "powershell" => Some(Shell::Powershell),
        _ => None,
    }
}

pub fn alias_line(shell: Shell, name: &str) -> String {
    match shell {
        Shell::Bash | Shell::Zsh => format!("alias {name}='agentmana'"),
        Shell::Fish => format!("alias {name} agentmana"),
        Shell::Powershell => format!("Set-Alias -Name {name} -Value agentmana"),
    }
}

fn rc_file(shell: Shell, home: &Path) -> Option<PathBuf> {
    match shell {
        Shell::Zsh => Some(
            std::env::var_os("ZDOTDIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.to_path_buf())
                .join(".zshrc"),
        ),
        Shell::Bash => Some(home.join(".bashrc")),
        Shell::Fish => Some(home.join(".config/fish/config.fish")),
        Shell::Powershell => None,
    }
}

/// An executable called `name` somewhere on PATH.
pub fn on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        for cand in [dir.join(name), dir.join(format!("{name}.exe"))] {
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    None
}

pub fn run(a: &AliasArgs, p: &Painter) -> anyhow::Result<()> {
    if a.name.is_empty()
        || !a
            .name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        anyhow::bail!("alias name must be letters, digits, '-' or '_'");
    }
    let shell = a
        .shell
        .or_else(detect_shell)
        .context("cannot tell the shell from $SHELL; pass --shell")?;
    let line = alias_line(shell, &a.name);
    if a.print {
        println!("{line}");
        return Ok(());
    }
    if let Some(existing) = on_path(&a.name)
        && !a.force
    {
        anyhow::bail!(
            "'{}' already exists ({}). Pick another name with --name, or --force to shadow it in your shell.",
            a.name,
            existing.display()
        );
    }
    let home = directories::BaseDirs::new()
        .context("no home directory")?
        .home_dir()
        .to_path_buf();
    let Some(rc) = rc_file(shell, &home) else {
        println!("Add this line to your PowerShell profile ($PROFILE):\n\n    {line}\n");
        return Ok(());
    };
    let current = std::fs::read_to_string(&rc).unwrap_or_default();
    if current.lines().any(|l| l.trim() == line) {
        println!(
            "{} {} already in {}",
            p.paint("[ OK ]", Tone::Good),
            line,
            rc.display()
        );
        return Ok(());
    }
    if let Some(dir) = rc.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&rc)
        .with_context(|| format!("cannot open {}", rc.display()))?;
    let sep = if current.is_empty() || current.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    write!(
        f,
        "{sep}\n# Short name for agentmana (added by `agentmana shell-alias`)\n{line}\n"
    )?;
    println!(
        "{} added `{}` to {}; open a new shell or run: source {}",
        p.paint("[ OK ]", Tone::Good),
        line,
        rc.display(),
        rc.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines() {
        assert_eq!(alias_line(Shell::Zsh, "am"), "alias am='agentmana'");
        assert_eq!(alias_line(Shell::Fish, "am"), "alias am agentmana");
        assert_eq!(
            alias_line(Shell::Powershell, "am"),
            "Set-Alias -Name am -Value agentmana"
        );
    }
}
