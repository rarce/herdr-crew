//! Agent selection and typed, additive Codex and pi launch options.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    #[default]
    Claude,
    Codex,
    Pi,
}

impl Kind {
    pub const ALL: [Self; 3] = [Self::Claude, Self::Codex, Self::Pi];

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            "pi" => Some(Self::Pi),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Pi => "pi",
        }
    }
}

/// The agent a role starts, with the options of its kind only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentOptions {
    Claude,
    Codex(CodexOptions),
    Pi(PiOptions),
}

impl AgentOptions {
    pub fn kind(&self) -> Kind {
        match self {
            Self::Claude => Kind::Claude,
            Self::Codex(_) => Kind::Codex,
            Self::Pi(_) => Kind::Pi,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CodexOptions {
    pub model: Option<String>,
    pub profile: Option<String>,
    pub reasoning_effort: Option<String>,
    pub sandbox: Option<String>,
    pub approval_policy: Option<String>,
    pub search: Option<String>,
    pub additional_dirs: Option<Vec<PathBuf>>,
}

pub const CODEX_KEYS: &[&str] = &[
    "model",
    "profile",
    "reasoning_effort",
    "sandbox",
    "approval_policy",
    "search",
    "additional_dirs",
];

impl CodexOptions {
    pub fn inherit(&self, defaults: &Self) -> Self {
        Self {
            model: self.model.clone().or_else(|| defaults.model.clone()),
            profile: self.profile.clone().or_else(|| defaults.profile.clone()),
            reasoning_effort: self
                .reasoning_effort
                .clone()
                .or_else(|| defaults.reasoning_effort.clone()),
            sandbox: self.sandbox.clone().or_else(|| defaults.sandbox.clone()),
            approval_policy: self
                .approval_policy
                .clone()
                .or_else(|| defaults.approval_policy.clone()),
            search: self.search.clone().or_else(|| defaults.search.clone()),
            additional_dirs: self
                .additional_dirs
                .clone()
                .or_else(|| defaults.additional_dirs.clone()),
        }
    }

    pub fn problems(&self) -> Vec<String> {
        let mut errors = Vec::new();
        for (key, value) in [
            ("model", &self.model),
            ("profile", &self.profile),
            ("reasoning_effort", &self.reasoning_effort),
            ("sandbox", &self.sandbox),
            ("approval_policy", &self.approval_policy),
            ("search", &self.search),
        ] {
            if let Some(value) = value {
                if value.is_empty() || value.starts_with('-') || value.chars().any(char::is_control)
                {
                    errors.push(format!("{key}: must be a nonempty value without control characters or a leading '-'") );
                    continue;
                }
                let choices: Option<&[&str]> = match key {
                    "reasoning_effort" => {
                        Some(&["none", "minimal", "low", "medium", "high", "xhigh", "max"])
                    }
                    "sandbox" => Some(&["read-only", "workspace-write", "danger-full-access"]),
                    "approval_policy" => Some(&["on-request", "never"]),
                    "search" => Some(&["disabled", "cached", "live"]),
                    _ => None,
                };
                if let Some(choices) = choices
                    && !choices.contains(&value.as_str())
                {
                    errors.push(format!("{key}: expected one of {}", choices.join(", ")));
                }
                if key == "profile"
                    && !value
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
                {
                    errors.push("profile: use only letters, digits, '-' and '_'".into());
                }
            }
        }
        if let Some(dirs) = &self.additional_dirs {
            for dir in dirs {
                if dir.as_os_str().is_empty()
                    || dir.to_str().is_none_or(|s| s.chars().any(char::is_control))
                {
                    errors.push(
                        "additional_dirs: paths must be nonempty UTF-8 without control characters"
                            .into(),
                    );
                } else if !dir.is_absolute()
                    && let Some(problem) =
                        crate::files::relative_path_problem(dir.to_str().unwrap())
                {
                    errors.push(format!("additional_dirs: {problem}"));
                }
            }
        }
        errors
    }

    pub fn resolve_dirs(&mut self, root: &Path) -> Result<(), String> {
        if let Some(dirs) = &mut self.additional_dirs {
            for dir in dirs {
                let path = if dir.is_absolute() {
                    dir.clone()
                } else {
                    root.join(&*dir)
                };
                *dir = path
                    .canonicalize()
                    .map_err(|e| format!("additional directory {}: {e}", path.display()))?;
                if !dir.is_dir() {
                    return Err(format!("{} is not a directory", dir.display()));
                }
            }
        }
        Ok(())
    }

    /// No shell interpolation; config string values use TOML-compatible JSON escaping.
    pub fn args(&self) -> Vec<String> {
        let mut args = vec!["--no-daemon".into()];
        if let Some(profile) = &self.profile {
            args.extend(["--profile".into(), profile.clone()]);
        }
        for (key, value) in [
            ("model", &self.model),
            ("model_reasoning_effort", &self.reasoning_effort),
            ("sandbox_mode", &self.sandbox),
            ("approval_policy", &self.approval_policy),
            ("web_search", &self.search),
        ] {
            if let Some(value) = value {
                args.extend([
                    "-c".into(),
                    format!("{key}={}", serde_json::to_string(value).unwrap()),
                ]);
            }
        }
        if let Some(dirs) = &self.additional_dirs {
            // An explicit empty list also overrides a profile's additional writable roots.
            args.extend([
                "-c".into(),
                format!(
                    "sandbox_workspace_write.writable_roots={}",
                    serde_json::to_string(dirs).unwrap()
                ),
            ]);
        }
        args
    }
}

/// pi's model selection. The role prompt itself travels through the crew's pi extension
/// (`crate::pi`), never as a launch option.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PiOptions {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub thinking: Option<String>,
}

pub const PI_KEYS: &[&str] = &["provider", "model", "thinking"];

impl PiOptions {
    pub fn inherit(&self, defaults: &Self) -> Self {
        Self {
            provider: self.provider.clone().or_else(|| defaults.provider.clone()),
            model: self.model.clone().or_else(|| defaults.model.clone()),
            thinking: self.thinking.clone().or_else(|| defaults.thinking.clone()),
        }
    }

    pub fn problems(&self) -> Vec<String> {
        let mut errors = Vec::new();
        for (key, value) in [
            ("provider", &self.provider),
            ("model", &self.model),
            ("thinking", &self.thinking),
        ] {
            let Some(value) = value else { continue };
            if value.is_empty() || value.starts_with('-') || value.chars().any(char::is_control) {
                errors.push(format!(
                    "{key}: must be a nonempty value without control characters or a leading '-'"
                ));
                continue;
            }
            let choices = ["off", "minimal", "low", "medium", "high", "xhigh"];
            if key == "thinking" && !choices.contains(&value.as_str()) {
                errors.push(format!("thinking: expected one of {}", choices.join(", ")));
            }
        }
        errors
    }

    /// Separate arguments, never interpolated by a shell.
    pub fn args(&self) -> Vec<String> {
        let mut args = Vec::new();
        for (flag, value) in [
            ("--provider", &self.provider),
            ("--model", &self.model),
            ("--thinking", &self.thinking),
        ] {
            if let Some(value) = value {
                args.extend([flag.to_string(), value.clone()]);
            }
        }
        args
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_lists_every_kind_once() {
        // Adding a variant breaks this match, a reminder to add it to `Kind::ALL` as well.
        let position = |kind: Kind| match kind {
            Kind::Claude => 0,
            Kind::Codex => 1,
            Kind::Pi => 2,
        };
        for (i, kind) in Kind::ALL.into_iter().enumerate() {
            assert_eq!(position(kind), i);
            assert_eq!(Kind::parse(kind.as_str()), Some(kind));
        }
    }
    #[test]
    fn individual_overrides_and_empty_directories_are_preserved() {
        let defaults = CodexOptions {
            model: Some("custom-model".into()),
            additional_dirs: Some(vec!["/board".into()]),
            ..Default::default()
        };
        let role = CodexOptions {
            profile: Some("dev".into()),
            additional_dirs: Some(vec![]),
            ..Default::default()
        }
        .inherit(&defaults);
        assert_eq!(role.model, defaults.model);
        assert_eq!(role.additional_dirs, Some(vec![]));
        assert!(
            role.args()
                .contains(&"sandbox_workspace_write.writable_roots=[]".into())
        );
    }
    #[test]
    fn invalid_options_cannot_inject_a_subcommand() {
        let options = CodexOptions {
            profile: Some("../other".into()),
            model: Some("--exec".into()),
            approval_policy: Some("untrusted".into()),
            ..Default::default()
        };
        assert_eq!(options.problems().len(), 3);
    }
    #[test]
    fn pi_options_inherit_and_reject_flags() {
        let defaults = PiOptions {
            provider: Some("anthropic".into()),
            thinking: Some("high".into()),
            ..Default::default()
        };
        let role = PiOptions {
            model: Some("sonnet".into()),
            thinking: Some("low".into()),
            ..Default::default()
        }
        .inherit(&defaults);
        assert_eq!(
            role.args(),
            [
                "--provider",
                "anthropic",
                "--model",
                "sonnet",
                "--thinking",
                "low"
            ]
        );
        let bad = PiOptions {
            model: Some("--session".into()),
            thinking: Some("max".into()),
            provider: Some("a\nb".into()),
        };
        assert_eq!(bad.problems().len(), 3);
        assert!(PiOptions::default().args().is_empty());
    }
}
