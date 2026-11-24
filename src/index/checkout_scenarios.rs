//! Scenario tests of checkouts: git worktrees and branches, read from synthetic transcripts in a
//! temp dir that holds a real repository layout (`.git` dir, a linked worktree's `.git` file).
//! Checked by request counts; the money rules are proved elsewhere.

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::core::period::Span;
    use crate::core::pricing::PriceBook;
    use crate::index::compact::CompactOpts;
    use crate::index::live::AgentKey;
    use crate::index::query::{Dim, Filter};
    use crate::index::scenarios::{World, assistant, at};

    /// `assistant` with the `gitBranch` Claude Code writes on every record.
    fn on_branch(line: String, branch: &str) -> String {
        let mut v: serde_json::Value = serde_json::from_str(&line).unwrap();
        v["gitBranch"] = branch.into();
        v.to_string()
    }

    /// A repository at `<root>/shop` and its linked worktree `.claude/worktrees/fix-1`.
    fn repo(root: &Path) -> (String, String) {
        let repo = root.join("repos/shop");
        std::fs::create_dir_all(repo.join(".git/worktrees/fix-1")).unwrap();
        let wt = repo.join(".claude/worktrees/fix-1");
        std::fs::create_dir_all(wt.join("src")).unwrap();
        std::fs::write(
            wt.join(".git"),
            format!("gitdir: {}\n", repo.join(".git/worktrees/fix-1").display()),
        )
        .unwrap();
        (
            repo.to_string_lossy().into_owned(),
            wt.join("src").to_string_lossy().into_owned(),
        )
    }

    fn by(idx: &crate::index::Index, dim: Dim, f: &Filter) -> Vec<(String, u64)> {
        let span = Span {
            start: at("2026-10-01T00:00:00Z"),
            end: at("2026-11-20T00:00:00Z"),
        };
        let mut v: Vec<(String, u64)> = idx
            .spend_by(dim, span, f)
            .unwrap()
            .into_iter()
            .map(|r| (r.label, r.requests))
            .collect();
        v.sort();
        v
    }

    #[test]
    fn worktrees_and_branches_are_reported_and_survive_compaction() {
        let w = World::new(&["work"]);
        let (main_dir, wt_dir) = repo(w.dir.path());
        let m = "claude-sonnet-5-5";
        // The main checkout: one request on `main`, then two on `feat` (the branch was switched
        // in the session), an old day that will be rolled up and a recent one.
        w.append(
            "work",
            "s1",
            &[
                on_branch(
                    assistant("a1", "2026-10-02T10:00:00Z", m, &main_dir, (10, 1), &[]),
                    "main",
                ),
                on_branch(
                    assistant("a2", "2026-10-02T10:40:00Z", m, &main_dir, (10, 1), &[]),
                    "feat",
                ),
                on_branch(
                    assistant("a3", "2026-11-18T09:00:00Z", m, &main_dir, (10, 1), &[]),
                    "feat",
                ),
            ],
        );
        // A session in the worktree, on Claude Code's `worktree-fix-1`.
        w.append(
            "work",
            "s2",
            &[
                on_branch(
                    assistant("b1", "2026-10-03T11:00:00Z", m, &wt_dir, (10, 1), &[]),
                    "worktree-fix-1",
                ),
                on_branch(
                    assistant("b2", "2026-11-18T11:00:00Z", m, &wt_dir, (10, 1), &[]),
                    "worktree-fix-1",
                ),
            ],
        );
        let mut idx = w.index("i.sqlite");
        let prices = PriceBook::builtin();
        w.refresh(&mut idx, &[], &prices);

        let all = Filter::default();
        let checkouts = vec![("shop".to_string(), 3), ("shop/fix-1".to_string(), 2)];
        let branches = vec![
            ("shop ⎇ feat".to_string(), 2),
            ("shop ⎇ main".to_string(), 1),
            ("shop ⎇ worktree-fix-1".to_string(), 2),
        ];
        assert_eq!(by(&idx, Dim::Worktree, &all), checkouts);
        assert_eq!(by(&idx, Dim::Branch, &all), branches);
        let only = |in_worktree| Filter {
            in_worktree: Some(in_worktree),
            ..Filter::default()
        };
        assert_eq!(
            by(&idx, Dim::Project, &only(true)),
            vec![("shop".into(), 2)]
        );
        assert_eq!(
            by(&idx, Dim::Project, &only(false)),
            vec![("shop".into(), 3)]
        );

        // One row per change of branch; reading the file again adds none.
        let s1 = idx.agent_branch(AgentKey {
            seat: &w.id("work"),
            session_id: &"s1".into(),
            agent_id: "",
        });
        assert_eq!(s1.unwrap().as_deref(), Some("feat"));
        let rows = |idx: &crate::index::Index| {
            crate::index::scenarios::dump(
                idx,
                "SELECT session_id, branch FROM branches ORDER BY 1, from_ts",
            )
        };
        assert_eq!(rows(&idx), ["s1|main", "s1|feat", "s2|worktree-fix-1"]);
        w.append(
            "work",
            "s1",
            &[on_branch(
                assistant("a4", "2026-11-18T09:30:00Z", m, &main_dir, (10, 1), &[]),
                "feat",
            )],
        );
        w.refresh(&mut idx, &[], &prices);
        assert_eq!(rows(&idx).len(), 3);

        // The live view's facts: the worktree's dir as `projects` recorded it.
        let dirs = idx.project_dirs().unwrap();
        let wt = dirs.get(&wt_dir).unwrap();
        assert!(wt.worktree.as_deref().is_some_and(|p| p.ends_with("fix-1")));
        assert_eq!(dirs.get(&main_dir).unwrap().worktree, None);

        // October is rolled up; every report reads the same from the rollups.
        let before: Vec<_> = [Dim::Worktree, Dim::Branch]
            .iter()
            .map(|d| by(&idx, *d, &all))
            .collect();
        let report = idx
            .compact(&CompactOpts {
                keep_raw_days: 14,
                now: at("2026-11-19T00:00:00Z"),
                prices: &prices,
                seats: &w.seats,
                max_chunks: None,
            })
            .unwrap();
        assert_eq!(report.requests, 3, "October's requests are rolled up");
        let after: Vec<_> = [Dim::Worktree, Dim::Branch]
            .iter()
            .map(|d| by(&idx, *d, &all))
            .collect();
        assert_eq!(before, after);
        assert_eq!(
            by(&idx, Dim::Project, &only(true)),
            vec![("shop".into(), 2)]
        );
    }
}
