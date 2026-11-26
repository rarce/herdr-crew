//! Agent selection and typed, additive launch options for Claude, Codex, pi and agy.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    #[default]
    Claude,
    Codex,
    Pi,
    Agy,
}

impl Kind {
    pub const ALL: [Self; 4] = [Self::Claude, Self::Codex, Self::Pi, Self::Agy];

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            "pi" => Some(Self::Pi),
            "agy" => Some(Self::Agy),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Pi => "pi",
            Self::Agy => "agy",
        }
    }
}

/// The agent a role starts, with the options of its kind only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentOptions {
    Claude(ClaudeOptions),
    Codex(CodexOptions),
    Pi(PiOptions),
    Agy(AgyOptions),
}

impl AgentOptions {
    pub fn kind(&self) -> Kind {
        match self {
            Self::Claude(_) => Kind::Claude,
            Self::Codex(_) => Kind::Codex,
            Self::Pi(_) => Kind::Pi,
            Self::Agy(_) => Kind::Agy,
        }
    }

    /// Appends an extra runtime `--add-dir` directory (e.g. out-of-repo operational memory) for
    /// agents that support it (`claude`, `codex`, `agy`), without mutating `crew.toml` or
    /// duplicating an already-configured path.
    pub fn with_extra_dir(mut self, dir: PathBuf) -> Self {
        match &mut self {
            Self::Claude(o) => o.add_extra_dir(dir),
            Self::Codex(o) => o.add_extra_dir(dir),
            Self::Agy(o) => o.add_extra_dir(dir),
            Self::Pi(_) => {}
        }
        self
    }
}

/// Claude Code's model selection: `--model`, `--effort` and `--fallback-model`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ClaudeOptions {
    pub model: Option<String>,
    pub effort: Option<String>,
    pub fallback_model: Option<String>,
    #[serde(skip)]
    pub extra_dirs: Vec<PathBuf>,
}

pub const CLAUDE_KEYS: &[&str] = &["model", "effort", "fallback_model"];
/// `claude --effort` levels.
pub const CLAUDE_EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];
/// Codex `model_reasoning_effort` values; each model supports a subset.
pub const CODEX_EFFORTS: &[&str] = &[
    "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
];
/// `pi --thinking` levels; pi clamps them to what the model supports.
pub const PI_THINKING: &[&str] = &["off", "minimal", "low", "medium", "high", "xhigh", "max"];
/// `agy --effort` levels.
pub const AGY_EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];
/// `agy --mode` values.
pub const AGY_MODES: &[&str] = &["accept-edits", "auto", "plan"];

/// A value passed as its own argument: nonempty, one line, and not readable as an option.
fn plain_value_problem(key: &str, value: &str) -> Option<String> {
    (value.is_empty() || value.starts_with('-') || value.chars().any(char::is_control)).then(|| {
        format!("{key}: must be a nonempty value without control characters or a leading '-'")
    })
}

fn same_dir(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => false,
    }
}

fn resolve_extra_dirs(extra_dirs: &mut Vec<PathBuf>, additional_dirs: Option<&[PathBuf]>) {
    let existing = additional_dirs.unwrap_or(&[]);
    let mut deduped: Vec<PathBuf> = Vec::with_capacity(extra_dirs.len());
    for dir in std::mem::take(extra_dirs) {
        let resolved = dir.canonicalize().unwrap_or(dir);
        if existing.iter().any(|d| same_dir(d, &resolved))
            || deduped.iter().any(|d| same_dir(d, &resolved))
        {
            continue;
        }
        deduped.push(resolved);
    }
    *extra_dirs = deduped;
}

impl ClaudeOptions {
    pub fn inherit(&self, defaults: &Self) -> Self {
        Self {
            model: self.model.clone().or_else(|| defaults.model.clone()),
            effort: self.effort.clone().or_else(|| defaults.effort.clone()),
            fallback_model: self
                .fallback_model
                .clone()
                .or_else(|| defaults.fallback_model.clone()),
            extra_dirs: self.extra_dirs.clone(),
        }
    }

    pub fn add_extra_dir(&mut self, dir: PathBuf) {
        if !self
            .extra_dirs
            .iter()
            .any(|existing| same_dir(existing, &dir))
        {
            self.extra_dirs.push(dir);
        }
    }

    pub fn problems(&self) -> Vec<String> {
        let mut errors = Vec::new();
        for (key, value) in [
            ("model", &self.model),
            ("effort", &self.effort),
            ("fallback_model", &self.fallback_model),
        ] {
            let Some(value) = value else { continue };
            if let Some(problem) = plain_value_problem(key, value) {
                errors.push(problem);
            } else if key == "effort" && !CLAUDE_EFFORTS.contains(&value.as_str()) {
                errors.push(format!(
                    "effort: expected one of {}",
                    CLAUDE_EFFORTS.join(", ")
                ));
            } else if key == "fallback_model" && value.split(',').any(|m| m.trim().is_empty()) {
                errors.push("fallback_model: a comma-separated list of model names".into());
            }
        }
        errors
    }

    /// Separate arguments, never interpolated by a shell.
    pub fn args(&self) -> Vec<String> {
        let mut args = Vec::new();
        for (flag, value) in [
            ("--model", &self.model),
            ("--effort", &self.effort),
            ("--fallback-model", &self.fallback_model),
        ] {
            if let Some(value) = value {
                args.extend([flag.to_string(), value.clone()]);
            }
        }
        let mut seen: Vec<&Path> = Vec::new();
        for dir in &self.extra_dirs {
            if !seen.iter().any(|existing| same_dir(existing, dir)) {
                seen.push(dir);
                args.extend(["--add-dir".into(), dir.display().to_string()]);
            }
        }
        args
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
    #[serde(skip)]
    pub extra_dirs: Vec<PathBuf>,
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
            extra_dirs: self.extra_dirs.clone(),
        }
    }

    pub fn add_extra_dir(&mut self, dir: PathBuf) {
        let already = self
            .additional_dirs
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .chain(self.extra_dirs.iter())
            .any(|existing| same_dir(existing, &dir));
        if !already {
            self.extra_dirs.push(dir);
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
                    "reasoning_effort" => Some(CODEX_EFFORTS),
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
        resolve_extra_dirs(&mut self.extra_dirs, self.additional_dirs.as_deref());
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
        let configured = self.additional_dirs.as_deref().unwrap_or(&[]);
        let mut seen: Vec<&Path> = configured.iter().map(PathBuf::as_path).collect();
        for dir in &self.extra_dirs {
            if !seen.iter().any(|existing| same_dir(existing, dir)) {
                seen.push(dir);
                args.extend(["--add-dir".into(), dir.display().to_string()]);
            }
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
            if key == "thinking" && !PI_THINKING.contains(&value.as_str()) {
                errors.push(format!(
                    "thinking: expected one of {}",
                    PI_THINKING.join(", ")
                ));
            }
        }
        errors
    }

    /// Check launch requirements after defaults and role options have been merged.
    /// Partial options for inactive agents can remain saved until that agent is selected.
    pub fn launch_problem(&self) -> Option<&'static str> {
        (self.provider.is_some() && self.model.is_none())
            .then_some("provider: requires an effective model in [pi] or [roles.pi]")
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

/// Antigravity CLI (`agy`) launch options: `--model`, `--effort`, `--mode`, `--sandbox`, `--add-dir`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgyOptions {
    pub model: Option<String>,
    pub effort: Option<String>,
    pub mode: Option<String>,
    pub sandbox: Option<bool>,
    pub additional_dirs: Option<Vec<PathBuf>>,
    #[serde(skip)]
    pub extra_dirs: Vec<PathBuf>,
}

pub const AGY_KEYS: &[&str] = &["model", "effort", "mode", "sandbox", "additional_dirs"];

impl AgyOptions {
    pub fn inherit(&self, defaults: &Self) -> Self {
        Self {
            model: self.model.clone().or_else(|| defaults.model.clone()),
            effort: self.effort.clone().or_else(|| defaults.effort.clone()),
            mode: self.mode.clone().or_else(|| defaults.mode.clone()),
            sandbox: self.sandbox.or(defaults.sandbox),
            additional_dirs: self
                .additional_dirs
                .clone()
                .or_else(|| defaults.additional_dirs.clone()),
            extra_dirs: self.extra_dirs.clone(),
        }
    }

    pub fn add_extra_dir(&mut self, dir: PathBuf) {
        let already = self
            .additional_dirs
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .chain(self.extra_dirs.iter())
            .any(|existing| same_dir(existing, &dir));
        if !already {
            self.extra_dirs.push(dir);
        }
    }

    pub fn problems(&self) -> Vec<String> {
        let mut errors = Vec::new();
        for (key, value) in [
            ("model", &self.model),
            ("effort", &self.effort),
            ("mode", &self.mode),
        ] {
            let Some(value) = value else { continue };
            if let Some(problem) = plain_value_problem(key, value) {
                errors.push(problem);
            } else if key == "effort" && !AGY_EFFORTS.contains(&value.as_str()) {
                errors.push(format!(
                    "effort: expected one of {}",
                    AGY_EFFORTS.join(", ")
                ));
            } else if key == "mode" && !AGY_MODES.contains(&value.as_str()) {
                errors.push(format!("mode: expected one of {}", AGY_MODES.join(", ")));
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

    /// Check launch compatibility after defaults and role options have been merged.
    pub fn launch_problem(&self) -> Option<&'static str> {
        let (Some(model), Some(_)) = (&self.model, &self.effort) else {
            return None;
        };
        if model.starts_with("gemini-pro-orchestrator-") {
            return Some("effort: orchestrator models manage reasoning automatically; omit effort");
        }
        if AGY_EFFORTS
            .iter()
            .any(|level| model.ends_with(&format!("-{level}")))
        {
            return Some(
                "effort: conflicts with a model slug that already ends in an effort level; use \
                 the base model name or omit effort",
            );
        }
        None
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
        resolve_extra_dirs(&mut self.extra_dirs, self.additional_dirs.as_deref());
        Ok(())
    }

    /// Separate arguments, never interpolated by a shell.
    pub fn args(&self) -> Vec<String> {
        let mut args = Vec::new();
        for (flag, value) in [
            ("--model", &self.model),
            ("--effort", &self.effort),
            ("--mode", &self.mode),
        ] {
            if let Some(value) = value {
                args.extend([flag.to_string(), value.clone()]);
            }
        }
        if self.sandbox == Some(true) {
            args.push("--sandbox".into());
        }
        let mut seen: Vec<&Path> = Vec::new();
        if let Some(dirs) = &self.additional_dirs {
            for dir in dirs {
                if !seen.iter().any(|existing| same_dir(existing, dir)) {
                    seen.push(dir);
                    args.extend(["--add-dir".into(), dir.display().to_string()]);
                }
            }
        }
        for dir in &self.extra_dirs {
            if !seen.iter().any(|existing| same_dir(existing, dir)) {
                seen.push(dir);
                args.extend(["--add-dir".into(), dir.display().to_string()]);
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
            Kind::Agy => 3,
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
            thinking: Some("ultra".into()),
            provider: Some("a\nb".into()),
        };
        assert_eq!(bad.problems().len(), 3);
        assert!(PiOptions::default().args().is_empty());
    }

    #[test]
    fn claude_options_inherit_validate_and_become_flags() {
        let defaults = ClaudeOptions {
            model: Some("opus".into()),
            effort: Some("high".into()),
            ..Default::default()
        };
        let role = ClaudeOptions {
            model: Some("sonnet".into()),
            fallback_model: Some("opus,haiku".into()),
            ..Default::default()
        }
        .inherit(&defaults);
        assert_eq!(
            role.args(),
            [
                "--model",
                "sonnet",
                "--effort",
                "high",
                "--fallback-model",
                "opus,haiku"
            ]
        );
        assert!(role.problems().is_empty());
        let bad = ClaudeOptions {
            model: Some("--dangerously-skip-permissions".into()),
            effort: Some("ultra".into()),
            fallback_model: Some("opus,".into()),
            ..Default::default()
        };
        assert_eq!(bad.problems().len(), 3);
        assert!(ClaudeOptions::default().args().is_empty());
    }

    #[test]
    fn extra_dir_appends_add_dir_without_duplication_for_claude_codex_and_agy() {
        let mem = PathBuf::from("/tmp/mem-store");
        let claude = AgentOptions::Claude(ClaudeOptions::default())
            .with_extra_dir(mem.clone())
            .with_extra_dir(mem.clone());
        let AgentOptions::Claude(c) = claude else {
            panic!();
        };
        assert_eq!(c.args(), ["--add-dir", "/tmp/mem-store"]);

        let codex = AgentOptions::Codex(CodexOptions {
            additional_dirs: Some(vec![mem.clone()]),
            ..Default::default()
        })
        .with_extra_dir(mem.clone());
        let AgentOptions::Codex(cx) = codex else {
            panic!();
        };
        assert!(!cx.args().contains(&"--add-dir".to_string()));

        let codex_new = AgentOptions::Codex(CodexOptions::default()).with_extra_dir(mem.clone());
        let AgentOptions::Codex(cx2) = codex_new else {
            panic!();
        };
        assert_eq!(cx2.args(), ["--no-daemon", "--add-dir", "/tmp/mem-store"]);

        let agy = AgentOptions::Agy(AgyOptions {
            additional_dirs: Some(vec![mem.clone()]),
            ..Default::default()
        })
        .with_extra_dir(mem.clone());
        let AgentOptions::Agy(a) = agy else {
            panic!();
        };
        assert_eq!(a.args(), ["--add-dir", "/tmp/mem-store"]);
    }

    #[test]
    fn resolve_dirs_and_args_deduplicate_canonical_and_relative_extra_dirs() {
        let root = PathBuf::from(format!("/tmp/crew-agent-dedup-{}", std::process::id()));
        let sub = root.join("mem");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&sub).unwrap();
        let canonical_sub = sub.canonicalize().unwrap();

        let mut claude = ClaudeOptions::default();
        claude.add_extra_dir(sub.clone());
        claude.add_extra_dir(canonical_sub.clone());
        assert_eq!(
            claude
                .args()
                .iter()
                .filter(|arg| arg.as_str() == "--add-dir")
                .count(),
            1
        );

        let mut codex = CodexOptions {
            additional_dirs: Some(vec![PathBuf::from("mem")]),
            ..Default::default()
        };
        codex.add_extra_dir(sub.clone());
        codex.resolve_dirs(&root).unwrap();
        assert!(!codex.args().contains(&"--add-dir".to_string()));

        let mut agy = AgyOptions {
            additional_dirs: Some(vec![PathBuf::from("mem")]),
            ..Default::default()
        };
        agy.add_extra_dir(sub.clone());
        agy.resolve_dirs(&root).unwrap();
        assert_eq!(
            agy.args()
                .iter()
                .filter(|arg| arg.as_str() == "--add-dir")
                .count(),
            1
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn codex_accepts_ultra_effort() {
        let options = CodexOptions {
            reasoning_effort: Some("ultra".into()),
            ..Default::default()
        };
        assert!(options.problems().is_empty());
    }

    #[test]
    fn pi_accepts_max_and_requires_a_model_only_with_an_explicit_provider() {
        let mut options = PiOptions {
            thinking: Some("max".into()),
            ..Default::default()
        };
        assert!(options.problems().is_empty());
        assert!(options.launch_problem().is_none());
        assert_eq!(options.args(), ["--thinking", "max"]);
        options.provider = Some("anthropic".into());
        assert!(options.launch_problem().is_some());
        options.model = Some("sonnet".into());
        assert!(options.launch_problem().is_none());
    }

    #[test]
    fn agy_options_inherit_validate_and_detect_effort_conflicts() {
        let defaults = AgyOptions {
            model: Some("gemini-4-neon".into()),
            effort: Some("high".into()),
            sandbox: Some(true),
            ..Default::default()
        };
        let role = AgyOptions {
            mode: Some("auto".into()),
            additional_dirs: Some(vec!["/board".into()]),
            ..Default::default()
        }
        .inherit(&defaults);
        assert!(role.problems().is_empty());
        assert!(role.launch_problem().is_none());
        assert_eq!(
            role.args(),
            [
                "--model",
                "gemini-4-neon",
                "--effort",
                "high",
                "--mode",
                "auto",
                "--sandbox",
                "--add-dir",
                "/board",
            ]
        );
        let suffixed = AgyOptions {
            model: Some("gemini-4-neon-high".into()),
            effort: Some("low".into()),
            ..Default::default()
        };
        assert!(suffixed.launch_problem().is_some());
        let orchestrator = AgyOptions {
            model: Some("gemini-pro-orchestrator-neon".into()),
            effort: Some("high".into()),
            ..Default::default()
        };
        assert!(orchestrator.launch_problem().is_some());
        let bad = AgyOptions {
            model: Some("--print".into()),
            effort: Some("ultra".into()),
            mode: Some("turbo".into()),
            ..Default::default()
        };
        assert_eq!(bad.problems().len(), 3);
    }
}
