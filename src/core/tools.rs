//! Claude Code's tools by what they mean here. One vocabulary for the transcript parser (which
//! tools name a file), the agent lanes (waiting on the user, on subagents), the efficiency figures
//! (questions, plans, commands, files edited) and the edit conflicts, so they cannot disagree.

/// What a tool call means for an agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolClass {
    /// Changes a file (its path is the call's target).
    Edit,
    /// Reads a file (its path is the call's target).
    Read,
    /// Runs a shell command.
    Shell,
    /// Asks the user a question: the agent waits on the user.
    AskUser,
    /// Presents a plan for approval: the agent waits on the user.
    Plan,
    /// Starts a subagent: the agent waits on it.
    Agent,
    Other,
}

/// Every tool with a class other than [`ToolClass::Other`].
const TOOLS: &[(&str, ToolClass)] = &[
    // Claude Code
    ("Edit", ToolClass::Edit),
    ("Write", ToolClass::Edit),
    ("MultiEdit", ToolClass::Edit),
    ("NotebookEdit", ToolClass::Edit),
    ("Read", ToolClass::Read),
    ("Bash", ToolClass::Shell),
    ("PowerShell", ToolClass::Shell),
    ("AskUserQuestion", ToolClass::AskUser),
    ("ExitPlanMode", ToolClass::Plan),
    ("Agent", ToolClass::Agent),
    ("Task", ToolClass::Agent),
    // Google Antigravity (`agy`)
    ("edit_file", ToolClass::Edit),
    ("write_file", ToolClass::Edit),
    ("write_to_file", ToolClass::Edit),
    ("replace_file_content", ToolClass::Edit),
    ("multi_replace_file_content", ToolClass::Edit),
    ("read_file", ToolClass::Read),
    ("view_file", ToolClass::Read),
    ("view_file_outline", ToolClass::Read),
    ("view_code_item", ToolClass::Read),
    ("run_command", ToolClass::Shell),
    ("ask_question", ToolClass::AskUser),
    ("invoke_subagent", ToolClass::Agent),
    ("swarm", ToolClass::Agent),
];

impl ToolClass {
    pub fn of(name: &str) -> ToolClass {
        TOOLS
            .iter()
            .find(|(n, _)| *n == name)
            .map_or(ToolClass::Other, |(_, c)| *c)
    }

    /// The tool names of this class (none for [`ToolClass::Other`]).
    pub fn names(self) -> impl Iterator<Item = &'static str> {
        TOOLS
            .iter()
            .filter(move |(_, c)| *c == self)
            .map(|(n, _)| *n)
    }

    /// While such a call is open, the agent waits for the user.
    pub fn waits_on_user(self) -> bool {
        matches!(self, ToolClass::AskUser | ToolClass::Plan)
    }

    /// The call works on a file, recorded as its target.
    pub fn has_target(self) -> bool {
        matches!(self, ToolClass::Edit | ToolClass::Read)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_has_one_class() {
        assert_eq!(ToolClass::of("NotebookEdit"), ToolClass::Edit);
        assert_eq!(ToolClass::of("Task"), ToolClass::Agent);
        assert_eq!(ToolClass::of("Grep"), ToolClass::Other);
        assert_eq!(
            ToolClass::Edit.names().collect::<Vec<_>>(),
            [
                "Edit",
                "Write",
                "MultiEdit",
                "NotebookEdit",
                "edit_file",
                "write_file",
                "write_to_file",
                "replace_file_content",
                "multi_replace_file_content",
            ]
        );
        assert_eq!(ToolClass::of("run_command"), ToolClass::Shell);
        assert_eq!(ToolClass::of("read_file"), ToolClass::Read);
        assert!(ToolClass::of("ask_question").waits_on_user());
        assert_eq!(ToolClass::of("invoke_subagent"), ToolClass::Agent);
        assert_eq!(ToolClass::Other.names().count(), 0);
        for (n, c) in TOOLS {
            assert_eq!(ToolClass::of(n), *c, "{n} is listed twice");
        }
        assert!(ToolClass::of("AskUserQuestion").waits_on_user());
        assert!(ToolClass::of("ExitPlanMode").waits_on_user());
        assert!(!ToolClass::of("Agent").waits_on_user());
        assert!(ToolClass::of("Read").has_target() && !ToolClass::of("Bash").has_target());
    }
}
