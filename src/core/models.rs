//! Model families and short display names.

use super::pricing::strip_date_suffix;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Family {
    Fable,
    Mythos,
    Opus,
    Sonnet,
    Haiku,
    Other,
}

impl Family {
    pub fn of(model: &str) -> Family {
        let m = model.to_ascii_lowercase();
        if m.contains("fable") {
            Family::Fable
        } else if m.contains("mythos") {
            Family::Mythos
        } else if m.contains("opus") {
            Family::Opus
        } else if m.contains("sonnet") {
            Family::Sonnet
        } else if m.contains("haiku") {
            Family::Haiku
        } else {
            Family::Other
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Family::Fable => "fable",
            Family::Mythos => "mythos",
            Family::Opus => "opus",
            Family::Sonnet => "sonnet",
            Family::Haiku => "haiku",
            Family::Other => "other",
        }
    }
}

/// Context window in tokens: 1M for the current models, 200K for Haiku 4.5 and older ones.
pub fn context_window(model: &str) -> u64 {
    let m = model.to_ascii_lowercase();
    let old = m.contains("claude-3")
        || m.contains("haiku-4")
        || [
            "opus-4-1",
            "opus-4-5",
            "sonnet-4-5",
            "opus-4-2025",
            "sonnet-4-2025",
        ]
        .iter()
        .any(|x| m.contains(x));
    if old { 200_000 } else { 1_000_000 }
}

/// `claude-opus-5-5` → `opus-5.5`, `claude-haiku-4-5-20251001` → `haiku-4.5`,
/// `claude-3-5-sonnet-20241022` → `sonnet-3.5`. Unknown ids are returned unchanged.
pub fn short_name(model: &str) -> String {
    let base = strip_date_suffix(model).unwrap_or(model);
    let Some(rest) = base.strip_prefix("claude-") else {
        return model.to_string();
    };
    let parts: Vec<&str> = rest.split('-').collect();
    let is_num = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    // New scheme: family first (`opus-5-5`); old scheme: version first (`3-5-sonnet`).
    let (family, version): (&str, Vec<&str>) = if parts.first().is_some_and(|p| !is_num(p)) {
        (
            parts[0],
            parts[1..]
                .iter()
                .copied()
                .take_while(|p| is_num(p))
                .collect(),
        )
    } else {
        let v: Vec<&str> = parts.iter().copied().take_while(|p| is_num(p)).collect();
        match parts.get(v.len()) {
            Some(f) => (f, v),
            None => return model.to_string(),
        }
    };
    if version.is_empty() {
        family.to_string()
    } else {
        format!("{family}-{}", version.join("."))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_names() {
        assert_eq!(short_name("claude-opus-5-5"), "opus-5.5");
        assert_eq!(short_name("claude-fable-5-2"), "fable-5.2");
        assert_eq!(short_name("claude-opus-5"), "opus-5");
        assert_eq!(short_name("claude-haiku-4-5-20251001"), "haiku-4.5");
        assert_eq!(short_name("claude-3-5-sonnet-20241022"), "sonnet-3.5");
        assert_eq!(short_name("claude-opus-4-20250514"), "opus-4");
        assert_eq!(short_name("<synthetic>"), "<synthetic>");
        assert_eq!(Family::of("claude-fable-5-2"), Family::Fable);
        assert_eq!(Family::of("opus"), Family::Opus);
    }
}
