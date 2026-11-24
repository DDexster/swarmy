//! Where an agent works: its git checkout (a linked worktree, or the project's main checkout),
//! the branch, and how many live sessions work in that same checkout. Pure: the facts come from
//! the index (`projects`, `branches`), nothing is read from disk here.

use std::collections::{HashMap, HashSet};

use serde::Serialize;

use crate::index::live::ProjectDir;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Checkout {
    /// The checkout's root: the linked worktree's dir, else the project's key (or the dir itself
    /// when no project is known).
    pub root: String,
    /// The worktree's dir name; `None` in a main checkout.
    pub worktree: Option<String>,
    /// The git branch of the agent's last request with one.
    pub branch: Option<String>,
    /// Live sessions working in this checkout, this one included (set by [`count_sessions`]).
    pub sessions: u32,
}

impl Checkout {
    /// The checkout of an agent working in `cwd`, as `projects` names it.
    pub fn of(cwd: Option<&str>, dir: Option<&ProjectDir>, branch: Option<String>) -> Option<Self> {
        let (root, worktree) = match (dir, cwd) {
            (Some(d), _) => match &d.worktree {
                Some(w) => (w.clone(), Some(base_name(w).to_string())),
                None => (d.key.clone(), None),
            },
            (None, Some(c)) => (c.to_string(), None),
            (None, None) => return None,
        };
        Some(Checkout {
            root,
            worktree,
            branch,
            sessions: 0,
        })
    }

    /// `project/worktree` for a linked worktree, else the project.
    pub fn label(&self, project: &str) -> String {
        match &self.worktree {
            Some(w) => format!("{project}/{w}"),
            None => project.to_string(),
        }
    }

    /// The branch as shown: Claude Code records a detached HEAD as `HEAD`.
    pub fn branch_label(&self) -> Option<&str> {
        self.branch
            .as_deref()
            .map(|b| if b == "HEAD" { "detached HEAD" } else { b })
    }

    /// The branch where it says something: not `main`/`master` in a main checkout, and not
    /// when the worktree's name already says it (`fix-1` on `fix-1`, or Claude Code's
    /// `worktree-fix-1`).
    pub fn branch_to_show(&self) -> Option<&str> {
        let b = self.branch.as_deref()?;
        match &self.worktree {
            Some(w) if b == w || b.strip_prefix("worktree-") == Some(w.as_str()) => None,
            None if matches!(b, "main" | "master") => None,
            // Claude Code records a detached HEAD as `HEAD`.
            Some(_) | None if b == "HEAD" => Some("detached HEAD"),
            Some(_) | None => Some(b),
        }
    }

    /// More than one live session works in this checkout: their edits can collide.
    pub fn is_shared(&self) -> bool {
        self.sessions > 1
    }
}

fn base_name(path: &str) -> &str {
    path.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(path)
}

/// How many distinct sessions work in each checkout root, from (session key, root) pairs of the
/// agents at work (a session's subagents in the same checkout count once).
pub fn sessions_per_root<'a, K: Eq + std::hash::Hash>(
    working: impl IntoIterator<Item = (K, &'a str)>,
) -> HashMap<String, u32> {
    let mut by_root: HashMap<&str, HashSet<K>> = HashMap::new();
    for (k, root) in working {
        by_root.entry(root).or_default().insert(k);
    }
    by_root
        .into_iter()
        .map(|(r, s)| (r.to_string(), s.len() as u32))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(key: &str, wt: Option<&str>) -> ProjectDir {
        ProjectDir {
            key: key.into(),
            name: "shop".into(),
            worktree: wt.map(str::to_string),
        }
    }

    #[test]
    fn a_worktree_is_named_by_its_dir_and_a_repeated_branch_is_not_shown() {
        let d = dir("/w/shop", Some("/w/shop/.claude/worktrees/fix-1"));
        let c = Checkout::of(
            Some("/w/shop/.claude/worktrees/fix-1/src"),
            Some(&d),
            Some("worktree-fix-1".into()),
        )
        .unwrap();
        assert_eq!(c.root, "/w/shop/.claude/worktrees/fix-1");
        assert_eq!(c.label("shop"), "shop/fix-1");
        assert_eq!(c.branch_to_show(), None);
        let c = Checkout {
            branch: Some("feat/pay".into()),
            ..c
        };
        assert_eq!(c.branch_to_show(), Some("feat/pay"));

        let main = Checkout::of(
            Some("/w/shop/src"),
            Some(&dir("/w/shop", None)),
            Some("main".into()),
        )
        .unwrap();
        assert_eq!(
            (main.root.as_str(), main.label("shop")),
            ("/w/shop", "shop".into())
        );
        assert_eq!(
            main.branch_to_show(),
            None,
            "the usual branch of a main checkout says nothing"
        );
        let feat = Checkout {
            branch: Some("feat/pay".into()),
            ..main.clone()
        };
        assert_eq!(feat.branch_to_show(), Some("feat/pay"));
        assert_eq!(
            Checkout::of(Some("/tmp/x"), None, None).unwrap().root,
            "/tmp/x"
        );
        assert_eq!(Checkout::of(None, None, None), None);
    }

    #[test]
    fn sessions_are_counted_per_checkout_once_each() {
        let n = sessions_per_root([
            ("s1", "/w/shop"),
            // A subagent of the same session does not count twice.
            ("s1", "/w/shop"),
            ("s2", "/w/shop"),
            ("s3", "/w/shop-wt/x"),
        ]);
        assert_eq!(n.get("/w/shop"), Some(&2));
        assert_eq!(n.get("/w/shop-wt/x"), Some(&1));
        let c = Checkout {
            sessions: n["/w/shop"],
            ..Default::default()
        };
        assert!(c.is_shared());
    }
}
