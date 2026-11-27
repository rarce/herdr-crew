//! `.herdr/crew.toml`: parsing and validation (design §2). Pure: it never touches the disk.

use std::fmt;
use std::ops::Range;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use toml::de::{DeTable, DeValue};

use crate::agent::{
    AGY_KEYS, AgentOptions, AgyOptions, CLAUDE_KEYS, CODEX_KEYS, ClaudeOptions, CodexOptions, Kind,
    PI_KEYS, PiOptions,
};
use crate::{files, prompt};

pub const CONFIG_FILE: &str = ".herdr/crew.toml";
pub const DEFAULT_BOARD_FILE: &str = ".herdr/status.json";
pub const SUPPORTED_VERSION: i64 = 1;
/// Longest `start_message`, in bytes.
pub const MAX_START_MESSAGE: usize = 200;

/// A configuration error, with the file it refers to and, when known, line and column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub file: PathBuf,
    pub pos: Option<(usize, usize)>,
    pub msg: String,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "herdr-crew: {}", self.file.display())?;
        if let Some((line, col)) = self.pos {
            write!(f, ":{line}:{col}")?;
        }
        write!(f, ": {}", self.msg)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub root: PathBuf,
    /// Prompt topology for the single-coordinator Codex native-subagent mode.
    pub codex_tree: bool,
    /// Prompt topology for the single-coordinator Antigravity native-subagent mode.
    pub agy_tree: bool,
    pub kind: Kind,
    pub claude: ClaudeOptions,
    pub codex: CodexOptions,
    pub pi: PiOptions,
    pub agy: AgyOptions,
    pub label: String,
    pub board: Board,
    pub memory: Memory,
    pub worktrees: Option<Worktrees>,
    /// Appended to every role's prompt.
    pub common_prompt: Option<String>,
    /// First message of a new conversation, unless the role has its own.
    pub start_message: Option<String>,
    pub roles: Vec<Role>,
    /// Launch options of the native subagents a `codex_tree` coordinator spawns, in file order.
    pub codex_workers: Vec<CodexWorker>,
}

/// One `[[codex_workers]]` entry: the model and reasoning effort the coordinator passes to
/// `spawn_agent` for the manifest worker `name` (its `task_name`). Unset values inherit the
/// coordinator's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexWorker {
    pub name: String,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Board {
    pub tab: String,
    /// Relative to the root.
    pub file: String,
    pub writer: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MemoryMode {
    #[default]
    Json,
    Wiki,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Memory {
    pub mode: MemoryMode,
    /// The resolved out-of-repo memory directory in wiki mode. Not a `crew.toml` key: the
    /// adapter (`memory::resolve`) sets it after parsing, so the core never reads the
    /// environment or the disk to find it.
    pub dir: Option<PathBuf>,
    /// Why the adapter could not resolve `dir` (wiki mode only); surfaced only by the
    /// commands that use memory.
    pub unresolved: Option<String>,
    /// The pin the next writing command (`up`, `mem init`) records, when one is due.
    pub pin: Option<crate::memory::PinWrite>,
    /// A warning about the resolution (`CREW_MEMORY_DIR` ignored because of the pin).
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktrees {
    /// Relative to the root.
    pub dir: String,
    pub remote: String,
    pub branch: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Role {
    pub kind: Kind,
    pub claude: ClaudeOptions,
    pub codex: CodexOptions,
    pub pi: PiOptions,
    pub agy: AgyOptions,
    pub name: String,
    pub prompt: String,
    pub worktree: bool,
    pub extra: bool,
    /// Overrides the top-level `start_message`; empty means none for this role.
    pub start_message: Option<String>,
}

impl Role {
    /// The agent this role starts, with only its kind's options.
    pub fn agent(&self) -> AgentOptions {
        match self.kind {
            Kind::Claude => AgentOptions::Claude(self.claude.clone()),
            Kind::Codex => AgentOptions::Codex(self.codex.clone()),
            Kind::Pi => AgentOptions::Pi(self.pi.clone()),
            Kind::Agy => AgentOptions::Agy(self.agy.clone()),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default)]
    codex_tree: Option<bool>,
    #[serde(default)]
    experimental_codex_tree: Option<bool>,
    #[serde(default)]
    agy_tree: Option<bool>,
    #[serde(default)]
    experimental_agy_tree: Option<bool>,
    #[serde(default)]
    kind: Kind,
    #[serde(default)]
    claude: ClaudeOptions,
    #[serde(default)]
    codex: CodexOptions,
    #[serde(default)]
    pi: PiOptions,
    #[serde(default)]
    agy: AgyOptions,
    version: i64,
    workspace: RawWorkspace,
    board: RawBoard,
    #[serde(default)]
    memory: RawMemory,
    worktrees: Option<RawWorktrees>,
    common_prompt: Option<String>,
    start_message: Option<String>,
    #[serde(default)]
    roles: Vec<RawRole>,
    #[serde(default)]
    codex_workers: Vec<RawCodexWorker>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCodexWorker {
    name: String,
    model: Option<String>,
    reasoning_effort: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawWorkspace {
    label: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBoard {
    tab: String,
    file: Option<String>,
    writer: String,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawMemory {
    mode: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawWorktrees {
    dir: String,
    base: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRole {
    kind: Option<Kind>,
    claude: Option<ClaudeOptions>,
    codex: Option<CodexOptions>,
    pi: Option<PiOptions>,
    agy: Option<AgyOptions>,
    name: String,
    prompt: String,
    #[serde(default)]
    worktree: bool,
    #[serde(default)]
    extra: bool,
    start_message: Option<String>,
}

/// Known keys per table; checked before `serde` so that all of them are reported at once.
const ROOT_KEYS: &[&str] = &[
    "codex_tree",
    "experimental_codex_tree",
    "agy_tree",
    "experimental_agy_tree",
    "version",
    "kind",
    "claude",
    "codex",
    "pi",
    "agy",
    "workspace",
    "board",
    "memory",
    "worktrees",
    "common_prompt",
    "start_message",
    "roles",
    "codex_workers",
];
const CODEX_WORKER_KEYS: &[&str] = &["name", "model", "reasoning_effort"];
const TABLE_KEYS: &[(&str, &[&str])] = &[
    ("claude", CLAUDE_KEYS),
    ("codex", CODEX_KEYS),
    ("pi", PI_KEYS),
    ("agy", AGY_KEYS),
    ("workspace", &["label"]),
    ("board", &["tab", "file", "writer"]),
    ("memory", &["mode"]),
    ("worktrees", &["dir", "base"]),
];
const ROLE_KEYS: &[&str] = &[
    "name",
    "prompt",
    "worktree",
    "extra",
    "start_message",
    "kind",
    "claude",
    "codex",
    "pi",
    "agy",
];

impl Config {
    /// Change only the project default, preserving comments, formatting and role overrides.
    pub fn select_agent(text: &str, root: &Path, kind: Kind) -> Result<(Self, String), Vec<Error>> {
        let original = Self::parse(text, root)?;
        if original.kind == kind {
            return Ok((original, text.to_string()));
        }
        let table = DeTable::parse(text).expect("validated TOML");
        let mut updated = text.to_string();
        if let Some((_, span, _)) = string_spans(table.get_ref(), "kind")
            .into_iter()
            .find(|(place, _, _)| place == "kind")
        {
            updated.replace_range(span, &format!("\"{}\"", kind.as_str()));
        } else {
            updated.insert_str(0, &format!("kind = \"{}\"\n", kind.as_str()));
        }
        let config = Self::parse(&updated, root)?;
        Ok((config, updated))
    }

    /// Parses and validates `text`, the content of `<root>/.herdr/crew.toml`.
    pub fn parse(text: &str, root: &Path) -> Result<Config, Vec<Error>> {
        let path = root.join(CONFIG_FILE);
        let at = |span: Option<Range<usize>>, msg: String| Error {
            file: path.clone(),
            pos: span.map(|s| line_col(text, s.start)),
            msg,
        };

        let table =
            DeTable::parse(text).map_err(|e| vec![at(e.span(), e.message().to_string())])?;
        let mut unknown = unknown_keys(table.get_ref());
        if !unknown.is_empty() {
            unknown.sort_by_key(|(span, _)| span.start);
            return Err(unknown
                .into_iter()
                .map(|(span, msg)| at(Some(span), msg))
                .collect());
        }
        let raw: RawConfig =
            toml::from_str(text).map_err(|e| vec![at(e.span(), e.message().to_string())])?;

        let mut errors = Vec::new();
        let mut err = |msg: String| errors.push(at(None, msg));

        for (key, value) in [
            ("board.file", raw.board.file.as_deref()),
            (
                "worktrees.dir",
                raw.worktrees.as_ref().map(|w| w.dir.as_str()),
            ),
        ] {
            if let Some(value) = value
                && let Some(why) = files::relative_path_problem(value)
            {
                err(format!("{key} {value:?}: {why}"));
            }
        }

        if raw.version != SUPPORTED_VERSION {
            err(format!(
                "version {} is not supported by herdr-crew {} (supports {SUPPORTED_VERSION})",
                raw.version,
                env!("CARGO_PKG_VERSION")
            ));
        }
        if raw.roles.is_empty() {
            err("roles[] is empty: at least one role is needed".to_string());
        }
        let mut tree_contradiction = false;
        let codex_tree = match (raw.codex_tree, raw.experimental_codex_tree) {
            (Some(a), Some(b)) if a != b => {
                tree_contradiction = true;
                err(
                    "codex_tree and experimental_codex_tree cannot have contradictory values"
                        .into(),
                );
                false
            }
            (Some(v), _) | (_, Some(v)) => v,
            (None, None) => false,
        };
        let agy_tree = match (raw.agy_tree, raw.experimental_agy_tree) {
            (Some(a), Some(b)) if a != b => {
                err("agy_tree and experimental_agy_tree cannot have contradictory values".into());
                false
            }
            (Some(v), _) | (_, Some(v)) => v,
            (None, None) => false,
        };
        if codex_tree && agy_tree {
            err("codex_tree and agy_tree cannot both be enabled".into());
        }
        if codex_tree
            && (raw.roles.len() != 1
                || raw
                    .roles
                    .iter()
                    .any(|r| r.kind.unwrap_or(raw.kind) != Kind::Codex || r.worktree || r.extra))
        {
            err(
                "codex_tree requires exactly one Codex coordinator without worktree or extra"
                    .into(),
            );
        }
        if agy_tree
            && (raw.roles.len() != 1
                || raw
                    .roles
                    .iter()
                    .any(|r| r.kind.unwrap_or(raw.kind) != Kind::Agy || r.worktree || r.extra))
        {
            err("agy_tree requires exactly one agy coordinator without worktree or extra".into());
        }
        if !codex_tree && !tree_contradiction && !raw.codex_workers.is_empty() {
            err("codex_workers applies only with codex_tree = true".into());
        }
        for (i, w) in raw.codex_workers.iter().enumerate() {
            if !is_worker_name(&w.name) {
                err(format!(
                    "codex_workers[{i}].name \"{}\": must match [a-z][a-z0-9_]{{0,31}}",
                    w.name
                ));
            }
            if let Some(j) = raw.codex_workers[..i].iter().position(|o| o.name == w.name) {
                err(format!(
                    "codex_workers[{i}].name \"{}\" is repeated (already in codex_workers[{j}])",
                    w.name
                ));
            }
            let options = CodexOptions {
                model: w.model.clone(),
                reasoning_effort: w.reasoning_effort.clone(),
                ..CodexOptions::default()
            };
            for problem in options.problems() {
                err(format!("codex_workers[{i}].{problem}"));
            }
        }
        for problem in raw.claude.problems() {
            err(format!("claude.{problem}"));
        }
        for problem in raw.codex.problems() {
            err(format!("codex.{problem}"));
        }
        for problem in raw.pi.problems() {
            err(format!("pi.{problem}"));
        }
        for problem in raw.agy.problems() {
            err(format!("agy.{problem}"));
        }
        for (i, r) in raw.roles.iter().enumerate() {
            if let Some(options) = &r.claude {
                for problem in options.problems() {
                    err(format!("roles[{i}].claude.{problem}"));
                }
            }
            if let Some(options) = &r.codex {
                for problem in options.problems() {
                    err(format!("roles[{i}].codex.{problem}"));
                }
            }
            if let Some(options) = &r.pi {
                for problem in options.problems() {
                    err(format!("roles[{i}].pi.{problem}"));
                }
            }
            if let Some(options) = &r.agy {
                for problem in options.problems() {
                    err(format!("roles[{i}].agy.{problem}"));
                }
            }
            if r.kind.unwrap_or(raw.kind) == Kind::Pi
                && let Some(problem) =
                    r.pi.clone()
                        .unwrap_or_default()
                        .inherit(&raw.pi)
                        .launch_problem()
            {
                err(format!("roles[{i}].pi.{problem}"));
            }
            if r.kind.unwrap_or(raw.kind) == Kind::Agy
                && let Some(problem) = r
                    .agy
                    .clone()
                    .unwrap_or_default()
                    .inherit(&raw.agy)
                    .launch_problem()
            {
                err(format!("roles[{i}].agy.{problem}"));
            }
            // pi reads a positional argument that starts with "@" as a file to attach.
            if r.kind.unwrap_or(raw.kind) == Kind::Pi
                && r.start_message
                    .as_deref()
                    .or(raw.start_message.as_deref())
                    .is_some_and(|m| m.starts_with('@'))
            {
                err(format!(
                    "roles[{i}]: the start_message of a pi role must not start with \"@\", which \
                     pi would read as a file to attach"
                ));
            }
            if !is_agent_name(&r.name) {
                err(format!(
                    "roles[{i}].name \"{}\": must match [a-z][a-z0-9_-]{{0,31}}",
                    r.name
                ));
            } else if r.extra && r.name.len() > 29 {
                err(format!(
                    "roles[{i}].name \"{}\": with extra = true, at most 29 characters to leave room for -NN",
                    r.name
                ));
            }
            if let Some(j) = raw.roles[..i].iter().position(|o| o.name == r.name) {
                err(format!(
                    "roles[{i}].name \"{}\" is repeated (already in roles[{j}])",
                    r.name
                ));
            }
            for o in raw.roles.iter().filter(|o| o.extra) {
                if extra_number(&r.name, &o.name).is_some() {
                    err(format!(
                        "roles[{i}].name \"{}\" clashes with the extra instances of {}",
                        r.name, o.name
                    ));
                }
            }
            if r.name == raw.board.tab {
                err(format!(
                    "board.tab \"{}\" is the same as the name of roles[{i}]",
                    raw.board.tab
                ));
            }
            if r.worktree && raw.worktrees.is_none() {
                err(format!(
                    "roles[{i}] {} asks for a worktree but [worktrees] is missing",
                    r.name
                ));
            }
        }
        if !raw.roles.is_empty() && !raw.roles.iter().any(|r| r.name == raw.board.writer) {
            err(format!(
                "board.writer \"{}\" is not a role in roles[]",
                raw.board.writer
            ));
        }
        let worktrees = raw
            .worktrees
            .as_ref()
            .and_then(|w| match w.base.split_once('/') {
                Some((remote, branch)) if !remote.is_empty() && !branch.is_empty() => {
                    Some(Worktrees {
                        dir: w.dir.clone(),
                        remote: remote.to_string(),
                        branch: branch.to_string(),
                    })
                }
                _ => {
                    err(format!(
                        "worktrees.base \"{}\": must look like remote/branch",
                        w.base
                    ));
                    None
                }
            });

        let memory_mode = match raw.memory.mode.as_deref() {
            None | Some("json") => MemoryMode::Json,
            Some("wiki") => MemoryMode::Wiki,
            Some(other) => {
                errors.push(at(
                    None,
                    format!("memory.mode \"{other}\": expected \"json\" or \"wiki\""),
                ));
                MemoryMode::Json
            }
        };

        // Placeholders are looked up in the source text of each prompt, so that the error points
        // at its line and column in crew.toml.
        for (place, span) in prompt_spans(table.get_ref()) {
            let source = &text[span.clone()];
            for (offset, name) in prompt::placeholders(source) {
                let why = if !prompt::PLACEHOLDERS.contains(&name.as_str()) {
                    format!("{place}: unknown placeholder {{{{{name}}}}}")
                } else if memory_mode == MemoryMode::Json
                    && prompt::WIKI_PLACEHOLDERS.contains(&name.as_str())
                {
                    format!("{place}: placeholder {{{{{name}}}}} needs [memory] mode = \"wiki\"")
                } else {
                    continue;
                };
                errors.push(at(Some(span.start + offset..span.start + offset), why));
            }
        }

        for (place, span, value) in string_spans(table.get_ref(), "start_message") {
            if let Some(why) = start_message_problem(&value) {
                errors.push(at(Some(span), format!("{place}: {why}")));
            }
        }

        if !errors.is_empty() {
            return Err(errors);
        }
        Ok(Config {
            root: root.to_path_buf(),
            codex_tree,
            agy_tree,
            kind: raw.kind,
            claude: raw.claude.clone(),
            codex: raw.codex.clone(),
            pi: raw.pi.clone(),
            agy: raw.agy.clone(),
            label: raw.workspace.label,
            board: Board {
                tab: raw.board.tab,
                file: raw
                    .board
                    .file
                    .unwrap_or_else(|| DEFAULT_BOARD_FILE.to_string()),
                writer: raw.board.writer,
            },
            memory: Memory {
                mode: memory_mode,
                ..Memory::default()
            },
            worktrees,
            common_prompt: raw.common_prompt,
            start_message: raw.start_message,
            roles: raw
                .roles
                .into_iter()
                .map(|r| Role {
                    kind: r.kind.unwrap_or(raw.kind),
                    claude: r.claude.unwrap_or_default().inherit(&raw.claude),
                    codex: r.codex.unwrap_or_default().inherit(&raw.codex),
                    pi: r.pi.unwrap_or_default().inherit(&raw.pi),
                    agy: r.agy.unwrap_or_default().inherit(&raw.agy),
                    name: r.name,
                    prompt: r.prompt,
                    worktree: r.worktree,
                    extra: r.extra,
                    start_message: r.start_message,
                })
                .collect(),
            codex_workers: raw
                .codex_workers
                .into_iter()
                .map(|w| CodexWorker {
                    name: w.name,
                    model: w.model,
                    reasoning_effort: w.reasoning_effort,
                })
                .collect(),
        })
    }

    /// The `spawn_agent` model and reasoning effort the coordinator passes for a worker. A
    /// worker that sets either gets both, the missing one from the coordinator, because Codex
    /// would otherwise pick its own default for it; a worker that sets neither gets none.
    pub fn codex_worker_params(&self, worker: &CodexWorker) -> (Option<String>, Option<String>) {
        if worker.model.is_none() && worker.reasoning_effort.is_none() {
            return (None, None);
        }
        let coordinator = self.roles.first().map(|r| &r.codex);
        (
            worker
                .model
                .clone()
                .or_else(|| coordinator.and_then(|c| c.model.clone())),
            worker
                .reasoning_effort
                .clone()
                .or_else(|| coordinator.and_then(|c| c.reasoning_effort.clone())),
        )
    }

    /// Whether this crew runs in single-coordinator native tree mode.
    pub fn is_tree(&self) -> bool {
        self.agy_tree || self.codex_tree
    }

    pub fn role(&self, name: &str) -> Option<&Role> {
        self.roles.iter().find(|r| r.name == name)
    }

    /// If `name` is an extra instance (`<role with extra>-N`, N ≥ 2), its base role and number.
    pub fn extra_instance(&self, name: &str) -> Option<(&Role, u32)> {
        self.roles
            .iter()
            .filter(|r| r.extra)
            .find_map(|r| extra_number(name, &r.name).map(|n| (r, n)))
    }

    /// The role that defines a session: the role itself or the base role of an extra instance.
    pub fn session_role(&self, name: &str) -> Option<&Role> {
        self.role(name)
            .or_else(|| self.extra_instance(name).map(|(r, _)| r))
    }

    /// Position of a session in the board and tab order: the role's index in `roles[]` and the
    /// instance number (1 for the base role).
    pub fn session_order(&self, name: &str) -> Option<(usize, u32)> {
        if let Some(i) = self.roles.iter().position(|r| r.name == name) {
            return Some((i, 1));
        }
        let (base, n) = self.extra_instance(name)?;
        let i = self.roles.iter().position(|r| r.name == base.name)?;
        Some((i, n))
    }

    /// Tabs that `up` recognizes as the project's own: roles, the board and extra instances.
    pub fn is_known_tab(&self, label: &str) -> bool {
        label == self.board.tab || self.session_order(label).is_some()
    }

    /// The first message of a new conversation of `session`: its role's, else the top-level
    /// one. An empty role value means none.
    pub fn start_message(&self, session: &str) -> Option<&str> {
        let role = self.session_role(session)?;
        role.start_message
            .as_deref()
            .or(self.start_message.as_deref())
            .filter(|m| !m.is_empty())
    }

    pub fn board_path(&self) -> PathBuf {
        self.root.join(&self.board.file)
    }

    pub fn uses_wiki_memory(&self) -> bool {
        self.memory.mode == MemoryMode::Wiki
    }

    /// The resolved memory directory, only in wiki mode. Commands that use memory check
    /// `memory_ready` first, so the core sees wiki mode with a directory.
    pub fn wiki_dir(&self) -> Option<&Path> {
        match self.memory.mode {
            MemoryMode::Wiki => self.memory.dir.as_deref(),
            MemoryMode::Json => None,
        }
    }

    /// In wiki mode, the resolved directory or why it could not be resolved.
    pub fn memory_ready(&self) -> Result<Option<&Path>, String> {
        match self.memory.mode {
            MemoryMode::Json => Ok(None),
            MemoryMode::Wiki => match (&self.memory.dir, &self.memory.unresolved) {
                (Some(dir), _) => Ok(Some(dir)),
                (None, Some(why)) => Err(why.clone()),
                (None, None) => Err("the operational memory directory is not resolved".into()),
            },
        }
    }

    /// The live board file: `<memory_dir>/now.md` in wiki mode, or `<root>/<board.file>` in json mode.
    pub fn effective_board_path(&self) -> PathBuf {
        match self.wiki_dir() {
            Some(dir) => dir.join("now.md"),
            None => self.board_path(),
        }
    }

    pub fn worktree_path(&self, session: &str) -> Option<PathBuf> {
        let w = self.worktrees.as_ref()?;
        Some(self.root.join(&w.dir).join(session))
    }
}

/// `[a-z][a-z0-9_-]{0,31}`, the shape of a herdr agent name.
pub fn is_agent_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some('a'..='z'))
        && name.len() <= 32
        && chars.all(|c| matches!(c, 'a'..='z' | '0'..='9' | '_' | '-'))
}

/// A native Codex `task_name`: like an agent name but without '-'.
fn is_worker_name(name: &str) -> bool {
    is_agent_name(name) && !name.contains('-')
}

/// The number N if `name` is `<base>-N` with N ≥ 2 written without leading zeros.
pub fn extra_number(name: &str, base: &str) -> Option<u32> {
    let digits = name.strip_prefix(base)?.strip_prefix('-')?;
    if digits.is_empty() || digits.starts_with('0') || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok().filter(|n| *n >= 2)
}

fn unknown_keys(root: &DeTable<'_>) -> Vec<(Range<usize>, String)> {
    let mut out = Vec::new();
    let mut check = |table: &DeTable<'_>, known: &[&str], place: &str| {
        for key in table.keys() {
            if !known.contains(&key.get_ref().as_ref()) {
                out.push((
                    key.span(),
                    format!("unknown key \"{}\" in {place}", key.get_ref()),
                ));
            }
        }
    };
    check(root, ROOT_KEYS, "the top level");
    for (key, value) in root.iter() {
        let key = key.get_ref().as_ref();
        if let Some((_, known)) = TABLE_KEYS.iter().find(|(k, _)| *k == key) {
            if let DeValue::Table(t) = value.get_ref() {
                check(t, known, &format!("[{key}]"));
            }
        } else if key == "codex_workers"
            && let DeValue::Array(items) = value.get_ref()
        {
            for (i, item) in items.iter().enumerate() {
                if let DeValue::Table(t) = item.get_ref() {
                    check(t, CODEX_WORKER_KEYS, &format!("codex_workers[{i}]"));
                }
            }
        } else if key == "roles"
            && let DeValue::Array(items) = value.get_ref()
        {
            for (i, item) in items.iter().enumerate() {
                if let DeValue::Table(t) = item.get_ref() {
                    check(t, ROLE_KEYS, &format!("roles[{i}]"));
                    for (agent, known) in [
                        ("claude", CLAUDE_KEYS),
                        ("codex", CODEX_KEYS),
                        ("pi", PI_KEYS),
                        ("agy", AGY_KEYS),
                    ] {
                        if let Some(value) = t.get(agent)
                            && let DeValue::Table(options) = value.get_ref()
                        {
                            check(options, known, &format!("roles[{i}].{agent}"));
                        }
                    }
                }
            }
        }
    }
    out
}

/// Why a `start_message` cannot be passed as an agent's initial prompt, if it cannot: herdr types
/// the command into the pane's shell, which rejects newlines, so no control character is
/// accepted, and a leading `-` would read as an option. An empty value is allowed: in a role it
/// means no message.
fn start_message_problem(m: &str) -> Option<String> {
    if m.chars().any(char::is_control) {
        Some("must be one line, without control characters".into())
    } else if m.len() > MAX_START_MESSAGE {
        Some(format!("{} bytes, at most {MAX_START_MESSAGE}", m.len()))
    } else if m.starts_with('-') {
        Some("must not start with \"-\", which the agent would read as an option".into())
    } else {
        None
    }
}

/// The top-level `key` and each `roles[i].<key>` that are strings: place, span of the value and
/// value.
fn string_spans(root: &DeTable<'_>, key: &str) -> Vec<(String, Range<usize>, String)> {
    let mut out = Vec::new();
    for (k, value) in root.iter() {
        match (k.get_ref().as_ref(), value.get_ref()) {
            (k, DeValue::String(v)) if k == key => {
                out.push((key.to_string(), value.span(), v.to_string()))
            }
            ("roles", DeValue::Array(items)) => {
                for (i, item) in items.iter().enumerate() {
                    let DeValue::Table(t) = item.get_ref() else {
                        continue;
                    };
                    for (rk, v) in t.iter() {
                        if let (true, DeValue::String(text)) =
                            (rk.get_ref().as_ref() == key, v.get_ref())
                        {
                            out.push((format!("roles[{i}].{key}"), v.span(), text.to_string()));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    out.sort_by_key(|(_, span, _)| span.start);
    out
}

/// `common_prompt` and each `roles[i].prompt` with the span of its value in the source text.
fn prompt_spans(root: &DeTable<'_>) -> Vec<(String, Range<usize>)> {
    let mut out = Vec::new();
    for (key, value) in root.iter() {
        match (key.get_ref().as_ref(), value.get_ref()) {
            ("common_prompt", DeValue::String(_)) => {
                out.push(("common_prompt".to_string(), value.span()))
            }
            ("roles", DeValue::Array(items)) => {
                for (i, item) in items.iter().enumerate() {
                    let DeValue::Table(t) = item.get_ref() else {
                        continue;
                    };
                    for (k, v) in t.iter() {
                        if k.get_ref().as_ref() == "prompt"
                            && matches!(v.get_ref(), DeValue::String(_))
                        {
                            out.push((format!("roles[{i}].prompt"), v.span()));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    out.sort_by_key(|(_, span)| span.start);
    out
}

fn line_col(text: &str, offset: usize) -> (usize, usize) {
    let before = &text[..offset.min(text.len())];
    let line = before.matches('\n').count() + 1;
    let col = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
    (line, col)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub const BASIC: &str = include_str!("../tests/fixtures/crew-basic.toml");
    pub const WORKTREES: &str = include_str!("../tests/fixtures/crew-worktrees.toml");

    /// A project with four roles in the main checkout.
    pub fn basic() -> Config {
        Config::parse(BASIC, Path::new("/r/acme")).unwrap()
    }

    /// A project whose developer and reviewer work in their own worktrees and have extra instances.
    pub fn worktrees() -> Config {
        Config::parse(WORKTREES, Path::new("/r/globex")).unwrap()
    }

    fn errors(text: &str) -> Vec<String> {
        Config::parse(text, Path::new("/r/p"))
            .unwrap_err()
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    const MIN: &str = r#"version = 1
[workspace]
label = "p"
[board]
tab = "p-status"
writer = "p-lead"
[[roles]]
name = "p-lead"
prompt = '''You are {{NAME}}.'''
"#;

    #[test]
    fn accepts_both_examples() {
        let b = basic();
        assert_eq!(b.label, "acme");
        let names: Vec<_> = b.roles.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(
            names,
            ["acme-lead", "acme-reviewer", "acme-designer", "acme-dev"]
        );
        assert!(b.worktrees.is_none());
        assert!(b.common_prompt.as_deref().unwrap().contains("{{STATUS}}"));
        let w = worktrees();
        assert_eq!(w.board.tab, "globex-status");
        let t = w.worktrees.as_ref().unwrap();
        assert_eq!((t.remote.as_str(), t.branch.as_str()), ("origin", "main"));
        let dev = w.role("globex-dev").unwrap();
        assert!(dev.extra && dev.worktree);
    }

    #[test]
    fn memory_table_defaults_to_json_and_validates_mode() {
        let default_cfg = Config::parse(MIN, Path::new("/r/p")).unwrap();
        assert!(!default_cfg.uses_wiki_memory());
        assert_eq!(default_cfg.memory.mode, MemoryMode::Json);
        assert_eq!(
            default_cfg.effective_board_path(),
            Path::new("/r/p/.herdr/status.json")
        );

        let wiki_text = MIN.replace("[workspace]", "[memory]\nmode = \"wiki\"\n[workspace]");
        let mut wiki_cfg = Config::parse(&wiki_text, Path::new("/r/p")).unwrap();
        assert!(wiki_cfg.uses_wiki_memory());
        assert_eq!(wiki_cfg.memory.mode, MemoryMode::Wiki);
        // The adapter resolves the directory; until then wiki commands refuse to run.
        assert!(wiki_cfg.wiki_dir().is_none());
        assert!(wiki_cfg.memory_ready().is_err());
        wiki_cfg.memory.dir = Some(PathBuf::from("/m/p-0123abcd"));
        assert_eq!(
            wiki_cfg.effective_board_path(),
            Path::new("/m/p-0123abcd/now.md")
        );
        assert_eq!(
            wiki_cfg.memory_ready().unwrap(),
            Some(Path::new("/m/p-0123abcd"))
        );

        // `{{MEMORY}}` and `{{MEMORY_SCHEMA}}` only have a value in wiki mode.
        let json_memory = MIN.replace("You are {{NAME}}.", "Memory {{MEMORY}} {{MEMORY_SCHEMA}}");
        let errs = errors(&json_memory);
        assert!(
            errs.iter()
                .any(|e| e.contains("placeholder {{MEMORY}} needs [memory] mode = \"wiki\"")),
            "{errs:?}"
        );
        assert!(
            errs.iter()
                .any(|e| e.contains("placeholder {{MEMORY_SCHEMA}} needs [memory] mode")),
            "{errs:?}"
        );
        let wiki_memory =
            json_memory.replace("[workspace]", "[memory]\nmode = \"wiki\"\n[workspace]");
        assert!(Config::parse(&wiki_memory, Path::new("/r/p")).is_ok());

        let bad_mode = MIN.replace("[workspace]", "[memory]\nmode = \"sqlite\"\n[workspace]");
        assert!(
            errors(&bad_mode)
                .iter()
                .any(|e| e.contains("memory.mode \"sqlite\": expected \"json\" or \"wiki\""))
        );

        let unknown_key = MIN.replace(
            "[workspace]",
            "[memory]\nmode = \"wiki\"\nbackend = \"local\"\n[workspace]",
        );
        assert!(
            errors(&unknown_key)
                .iter()
                .any(|e| e.contains("unknown key \"backend\" in [memory]"))
        );
    }

    #[test]
    fn board_file_defaults() {
        let c = Config::parse(MIN, Path::new("/r/p")).unwrap();
        assert_eq!(c.board.file, ".herdr/status.json");
        assert_eq!(c.board_path(), Path::new("/r/p/.herdr/status.json"));
        assert_eq!(c.common_prompt, None);
    }

    #[test]
    fn basic_strings_are_accepted_too() {
        let text = MIN.replace(
            "'''You are {{NAME}}.'''",
            "\"\"\"You are {{NAME}}.\\nBye.\"\"\"",
        );
        let c = Config::parse(&text, Path::new("/r/p")).unwrap();
        assert_eq!(c.roles[0].prompt, "You are {{NAME}}.\nBye.");
    }

    #[test]
    fn unknown_keys_are_reported_with_position() {
        let text = format!("{MIN}worktre = true\n");
        assert_eq!(
            errors(&text),
            ["herdr-crew: /r/p/.herdr/crew.toml:10:1: unknown key \"worktre\" in roles[0]"]
        );
    }

    #[test]
    fn args_remain_unsupported() {
        let e = errors(&format!("{MIN}kind = \"codex\"\nargs = [\"-x\"]\n"));
        assert_eq!(e.len(), 1);
        assert!(e[0].ends_with("unknown key \"args\" in roles[0]"), "{e:?}");
    }

    #[test]
    fn agent_defaults_role_overrides_and_extras() {
        let text = WORKTREES
            .replace("version = 1", "version = 1\nkind = \"codex\"")
            .replace(
                "[workspace]",
                "[codex]\nmodel = \"account-model\"\nadditional_dirs = [\"/board\"]\n[workspace]",
            )
            .replace(
                "name = \"globex-lead\"",
                "name = \"globex-lead\"\nkind = \"claude\"",
            );
        let c = Config::parse(&text, Path::new("/r/globex")).unwrap();
        assert_eq!(c.roles[0].kind, Kind::Claude);
        assert_eq!(c.roles[1].kind, Kind::Codex);
        assert_eq!(
            c.session_role("globex-dev-2")
                .unwrap()
                .codex
                .model
                .as_deref(),
            Some("account-model")
        );
        assert!(basic().roles.iter().all(|role| role.kind == Kind::Claude));
    }

    #[test]
    fn selecting_agent_preserves_source_role_overrides_and_inactive_options() {
        let text = format!("# Crew «á»\nkind = 'claude' # default\n{WORKTREES}\n[roles.codex]\nmodel = 'saved-model'\n").replace("name = \"globex-lead\"", "name = \"globex-lead\"\nkind = 'claude' # keep this role");
        let root = Path::new("/r/globex");
        let (selected, updated) = Config::select_agent(&text, root, Kind::Codex).unwrap();
        assert_eq!(
            updated,
            text.replacen("kind = 'claude'", "kind = \"codex\"", 1)
        );
        assert_eq!(selected.roles[0].kind, Kind::Claude);
        assert!(
            selected.roles[1..]
                .iter()
                .all(|role| role.kind == Kind::Codex)
        );
        assert_eq!(
            Config::select_agent(&updated, root, Kind::Codex).unwrap().1,
            updated
        );
        let (restored, back) = Config::select_agent(&updated, root, Kind::Claude).unwrap();
        assert!(restored.roles.iter().all(|role| role.kind == Kind::Claude));
        assert!(back.contains("model = 'saved-model'"));
        let (pi, _) = Config::select_agent(WORKTREES, root, Kind::Pi).unwrap();
        assert!(pi.roles.iter().all(|role| role.kind == Kind::Pi));
        let file_message = WORKTREES.replace("version = 1", "version = 1\nstart_message = '@file'");
        assert!(Config::select_agent(&file_message, root, Kind::Pi).is_err());
    }

    #[test]
    fn claude_options_have_defaults_role_overrides_and_positions() {
        let text = MIN.replace(
            "version = 1",
            "version = 1\n[claude]\nmodel = \"opus\"\neffort = \"high\"",
        );
        let config = Config::parse(
            &format!("{text}[roles.claude]\nmodel = \"sonnet\"\n"),
            Path::new("/r/p"),
        )
        .unwrap();
        let AgentOptions::Claude(options) = config.roles[0].agent() else {
            panic!("a Claude role");
        };
        assert_eq!(options.model.as_deref(), Some("sonnet"));
        assert_eq!(options.effort.as_deref(), Some("high"));
        let e = errors(&format!("{text}[roles.claude]\neffrot = \"low\"\n"));
        assert_eq!(e.len(), 1);
        assert!(
            e[0].contains("unknown key \"effrot\" in roles[0].claude"),
            "{e:?}"
        );
        let e = errors(&format!("{MIN}[roles.claude]\neffort = \"ultra\"\n"));
        assert!(
            e[0].contains("roles[0].claude.effort: expected one of"),
            "{e:?}"
        );
    }

    #[test]
    fn inactive_codex_options_are_preserved_and_unknown_keys_have_positions() {
        let config = Config::parse(
            &format!("{MIN}[roles.codex]\nmodel = \"custom\"\n"),
            Path::new("/r/p"),
        )
        .unwrap();
        assert_eq!(
            config.roles[0].agent(),
            AgentOptions::Claude(Default::default())
        );
        assert_eq!(config.roles[0].codex.model.as_deref(), Some("custom"));
        let errors = errors(&format!(
            "{MIN}kind = \"codex\"\n[roles.codex]\nmodle = \"custom\"\n"
        ));
        assert_eq!(errors.len(), 1);
        assert!(
            errors[0].contains(":12:1: unknown key \"modle\" in roles[0].codex"),
            "{errors:?}"
        );
    }

    #[test]
    fn prompt_files_are_gone() {
        let e = errors(&MIN.replace("'''You are {{NAME}}.'''", "[\"main.txt\"]"));
        assert_eq!(e.len(), 1);
        assert!(e[0].contains("/r/p/.herdr/crew.toml:9:"), "{e:?}");
    }

    #[test]
    fn invalid_toml() {
        let e = errors("version = \n");
        assert_eq!(e.len(), 1);
        assert!(
            e[0].starts_with("herdr-crew: /r/p/.herdr/crew.toml:1:"),
            "{e:?}"
        );
    }

    #[test]
    fn version() {
        assert_eq!(
            errors(&MIN.replace("version = 1", "version = 2")),
            [format!(
                "herdr-crew: /r/p/.herdr/crew.toml: version 2 is not supported by herdr-crew {} \
                 (supports 1)",
                env!("CARGO_PKG_VERSION")
            )]
        );
    }

    #[test]
    fn role_name_shape() {
        let e = errors(&MIN.replace("\"p-lead\"", "\"Lead_1\""));
        assert!(
            e.iter()
                .any(|m| m.ends_with("roles[0].name \"Lead_1\": must match [a-z][a-z0-9_-]{0,31}")),
            "{e:?}"
        );
        let long = format!(
            "{}extra = true\n",
            MIN.replace(
                "name = \"p-lead\"",
                &format!("name = \"{}\"", "a".repeat(30))
            )
        );
        assert!(
            errors(&long)
                .iter()
                .any(|m| m.contains("at most 29 characters"))
        );
    }

    #[test]
    fn unique_names_extra_clash_and_board_tab() {
        let text = format!(
            "{MIN}extra = true\n[[roles]]\nname = \"p-lead-2\"\nprompt = ''\n[[roles]]\nname = \"p-lead\"\nprompt = ''\n[[roles]]\nname = \"p-status\"\nprompt = ''\n"
        );
        let e = errors(&text);
        assert!(
            e.iter().any(|m| m.ends_with(
                "roles[1].name \"p-lead-2\" clashes with the extra instances of p-lead"
            )),
            "{e:?}"
        );
        assert!(
            e.iter()
                .any(|m| m.ends_with("roles[2].name \"p-lead\" is repeated (already in roles[0])")),
            "{e:?}"
        );
        assert!(
            e.iter()
                .any(|m| m.ends_with("board.tab \"p-status\" is the same as the name of roles[3]")),
            "{e:?}"
        );
    }

    #[test]
    fn at_least_one_role_and_writer_is_a_role() {
        let e = errors(&MIN.replace("writer = \"p-lead\"", "writer = \"boss\""));
        assert!(
            e.iter()
                .any(|m| m.ends_with("board.writer \"boss\" is not a role in roles[]")),
            "{e:?}"
        );
        let e = errors(MIN.split("[[roles]]").next().unwrap());
        assert!(
            e.iter()
                .any(|m| m.ends_with("roles[] is empty: at least one role is needed")),
            "{e:?}"
        );
    }

    #[test]
    fn worktree_needs_table_and_base_shape() {
        let e = errors(&format!("{MIN}worktree = true\n"));
        assert!(
            e.iter()
                .any(|m| m
                    .ends_with("roles[0] p-lead asks for a worktree but [worktrees] is missing")),
            "{e:?}"
        );
        let e = errors(&MIN.replace(
            "[[roles]]",
            "[worktrees]\ndir = \"w\"\nbase = \"main\"\n[[roles]]",
        ));
        assert!(
            e.iter()
                .any(|m| m.ends_with("worktrees.base \"main\": must look like remote/branch")),
            "{e:?}"
        );
    }

    #[test]
    fn generated_paths_stay_inside_the_project_and_outside_git_metadata() {
        for path in [
            "",
            ".",
            "..",
            "../outside.json",
            "sub/../../outside",
            "/tmp/outside",
            ".git/hooks/hook",
        ] {
            for table in ["board", "worktrees"] {
                let text = if table == "board" {
                    MIN.replace("[board]", &format!("[board]\nfile = {path:?}"))
                } else {
                    MIN.replace(
                        "[[roles]]",
                        &format!("[worktrees]\ndir = {path:?}\nbase = 'origin/main'\n[[roles]]"),
                    )
                };
                assert!(
                    errors(&text)
                        .iter()
                        .any(|e| e.contains(&format!("{table}."))),
                    "{text}"
                );
            }
        }
        let valid = MIN.replace("[board]", "[board]\nfile = './nested/status.json'");
        assert!(Config::parse(&valid, Path::new("/r/p")).is_ok());
    }

    #[test]
    fn unknown_placeholders_point_at_their_line_and_column() {
        let text = MIN
            .replace(
                "[workspace]",
                "common_prompt = '''\nBoard: {{STATUS}}.\nRun {{LAUNCH}}.\n'''\n[workspace]",
            )
            .replace(
                "'''You are {{NAME}}.'''",
                "'''You are {{NAME}} in {{ROOT}}.'''",
            );
        assert_eq!(
            errors(&text),
            [
                "herdr-crew: /r/p/.herdr/crew.toml:4:5: common_prompt: unknown placeholder {{LAUNCH}}",
                "herdr-crew: /r/p/.herdr/crew.toml:13:33: roles[0].prompt: unknown placeholder {{ROOT}}",
            ]
        );
    }

    #[test]
    fn all_errors_at_once() {
        let text = MIN
            .replace("version = 1", "version = 3")
            .replace("writer = \"p-lead\"", "writer = \"x\"");
        assert_eq!(errors(&text).len(), 2);
    }

    #[test]
    fn start_message_top_level_and_per_role() {
        let text = MIN.replace(
            "[workspace]",
            "start_message = '''Confirma tu rol «ahora» y espera.'''\n[workspace]",
        ) + "[[roles]]\nname = \"p-dev\"\nprompt = ''\nstart_message = 'Só tú'\n\
              [[roles]]\nname = \"p-quiet\"\nprompt = ''\nstart_message = ''\n";
        let c = Config::parse(&text, Path::new("/r/p")).unwrap();
        assert_eq!(
            c.start_message("p-lead"),
            Some("Confirma tu rol «ahora» y espera.")
        );
        assert_eq!(c.start_message("p-dev"), Some("Só tú"));
        assert_eq!(c.start_message("p-quiet"), None);
        assert_eq!(
            Config::parse(MIN, Path::new("/r/p"))
                .unwrap()
                .start_message("p-lead"),
            None
        );
    }

    #[test]
    fn start_message_is_one_short_line() {
        let text = MIN.replace(
            "[workspace]",
            "start_message = '''Línea «uno»\ny dos'''\n[workspace]",
        ) + &format!("start_message = '{}'\n", "é".repeat(101));
        assert_eq!(
            errors(&text),
            [
                "herdr-crew: /r/p/.herdr/crew.toml:2:17: start_message: must be one line, without control characters",
                "herdr-crew: /r/p/.herdr/crew.toml:12:17: roles[0].start_message: 202 bytes, at most 200",
            ]
        );
        // 200 bytes of accented text is accepted; a leading dash is not.
        let ok = format!("{MIN}start_message = '{}'\n", "é".repeat(100));
        assert!(Config::parse(&ok, Path::new("/r/p")).is_ok());
        for control in ["a\\tb", "a\\u001bb", "a\\u007fb"] {
            let text = format!("{MIN}start_message = \"{control}\"\n");
            assert_eq!(
                errors(&text),
                [
                    "herdr-crew: /r/p/.herdr/crew.toml:10:17: roles[0].start_message: must be one line, without control characters"
                ],
                "{control}"
            );
        }
        let dash = format!("{MIN}start_message = '--help'\n");
        assert!(
            errors(&dash)[0]
                .ends_with("must not start with \"-\", which the agent would read as an option")
        );
    }

    #[test]
    fn sessions_and_order() {
        let w = worktrees();
        assert_eq!(w.session_order("globex-reviewer"), Some((2, 1)));
        assert_eq!(w.session_order("globex-dev-3"), Some((1, 3)));
        assert_eq!(w.session_order("globex-lead-2"), None);
        assert_eq!(w.session_order("globex-dev-1"), None);
        assert_eq!(w.session_order("globex-dev-02"), None);
        assert!(
            w.is_known_tab("globex-status")
                && w.is_known_tab("globex-reviewer-2")
                && !w.is_known_tab("6")
        );
        assert_eq!(
            w.worktree_path("globex-dev-2").unwrap(),
            Path::new("/r/globex/.worktrees/globex-dev-2")
        );
    }

    #[test]
    fn pi_roles_inherit_options_and_reject_unknown_ones() {
        let text = WORKTREES
            .replace("version = 1", "version = 1\nkind = \"pi\"")
            .replace(
                "[workspace]",
                "[pi]\nprovider = \"anthropic\"\nmodel = \"sonnet\"\nthinking = \"high\"\n[workspace]",
            );
        let c = Config::parse(&text, Path::new("/r/globex")).unwrap();
        assert!(c.roles.iter().all(|role| role.kind == Kind::Pi));
        let extra = c.session_role("globex-dev-2").unwrap();
        assert_eq!(extra.pi.provider.as_deref(), Some("anthropic"));
        assert_eq!(extra.pi.thinking.as_deref(), Some("high"));
        let inactive = Config::parse(
            &format!("{MIN}[roles.pi]\nmodel = \"sonnet\"\n"),
            Path::new("/r/p"),
        )
        .unwrap();
        assert_eq!(
            inactive.roles[0].agent(),
            AgentOptions::Claude(Default::default())
        );
        assert_eq!(inactive.roles[0].pi.model.as_deref(), Some("sonnet"));
        let e = errors(&format!(
            "{MIN}kind = \"pi\"\n[roles.pi]\nsandbox = \"x\"\n"
        ));
        assert!(
            e[0].contains("unknown key \"sandbox\" in roles[0].pi"),
            "{e:?}"
        );
        let e = errors(&format!(
            "{MIN}kind = \"pi\"\n[roles.pi]\nthinking = \"ultra\"\n"
        ));
        assert!(
            e[0].ends_with(
                "roles[0].pi.thinking: expected one of off, minimal, low, medium, high, xhigh, max"
            ),
            "{e:?}"
        );
        // pi reads a leading "@" as a file to attach; Claude passes it on as text.
        let mention = "start_message = \"@p-lead: confirm your role\"\n";
        let e = errors(&format!("{MIN}kind = \"pi\"\n{mention}"));
        assert!(
            e.iter()
                .any(|error| error.contains("roles[0]: the start_message of a pi role")),
            "{e:?}"
        );
        assert!(Config::parse(&format!("{MIN}{mention}"), Path::new("/r/p")).is_ok());
    }

    #[test]
    fn pi_launch_requirements_use_effective_options_and_agent_kind() {
        let root = Path::new("/r/p");
        let defaults = format!("{MIN}[pi]\nprovider = \"anthropic\"\n");
        // Inactive options are preserved, but switching to pi validates the launch.
        assert!(Config::parse(&defaults, root).is_ok());
        let errors = Config::select_agent(&defaults, root, Kind::Pi).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| e.msg.contains("requires an effective model"))
        );
        let role_model = format!("{defaults}[roles.pi]\nmodel = \"sonnet\"\nthinking = \"max\"\n");
        let (c, _) = Config::select_agent(&role_model, root, Kind::Pi).unwrap();
        assert_eq!(c.roles[0].pi.provider.as_deref(), Some("anthropic"));
        assert_eq!(c.roles[0].pi.thinking.as_deref(), Some("max"));
        let default_model =
            format!("{MIN}[pi]\nmodel = \"sonnet\"\n[roles.pi]\nprovider = \"anthropic\"\n");
        assert!(Config::select_agent(&default_model, root, Kind::Pi).is_ok());
        let role_provider = format!("{MIN}[roles.pi]\nprovider = \"anthropic\"\n");
        assert!(Config::select_agent(&role_provider, root, Kind::Pi).is_err());
        // A role override to Claude leaves partial pi options inactive.
        let mixed = role_provider.replace("prompt =", "kind = \"claude\"\nprompt =");
        assert!(Config::select_agent(&mixed, root, Kind::Pi).is_ok());
    }

    #[test]
    fn codex_workers_set_subagent_options_only_in_codex_tree() {
        let root = Path::new("/r/p");
        let tree = format!("codex_tree = true\nkind = \"codex\"\n{MIN}");
        let workers = "[[codex_workers]]\nname = \"dev\"\nmodel = \"gpt-6-luna\"\n\
                       [[codex_workers]]\nname = \"reviewer\"\nreasoning_effort = \"high\"\n\
                       [[codex_workers]]\nname = \"researcher_a\"\n";
        let parsed = Config::parse(&format!("{tree}{workers}"), root).unwrap();
        assert_eq!(
            parsed.codex_workers,
            [
                CodexWorker {
                    name: "dev".into(),
                    model: Some("gpt-6-luna".into()),
                    reasoning_effort: None,
                },
                CodexWorker {
                    name: "reviewer".into(),
                    model: None,
                    reasoning_effort: Some("high".into()),
                },
                CodexWorker {
                    name: "researcher_a".into(),
                    model: None,
                    reasoning_effort: None,
                },
            ]
        );
        // Subagents are not roles or tabs.
        assert_eq!(parsed.roles.len(), 1);
        assert!(!parsed.is_known_tab("dev"));

        let e = errors(&format!("kind = \"codex\"\n{MIN}{workers}"));
        assert!(
            e.iter()
                .any(|e| e.ends_with("codex_workers applies only with codex_tree = true")),
            "{e:?}"
        );
        // A contradictory tree flag is reported once, without a misleading codex_workers error.
        let e = errors(&format!(
            "codex_tree = true\nexperimental_codex_tree = false\nkind = \"codex\"\n{MIN}{workers}"
        ));
        assert!(
            !e.iter().any(|e| e.contains("codex_workers applies")),
            "{e:?}"
        );
        // A worker that sets one value gets the other from the coordinator.
        let with_lead = format!(
            "{tree}[roles.codex]\nmodel = \"gpt-6.2-astra\"\nreasoning_effort = \"xhigh\"\n{workers}"
        );
        let parsed = Config::parse(&with_lead, root).unwrap();
        let params: Vec<_> = parsed
            .codex_workers
            .iter()
            .map(|w| parsed.codex_worker_params(w))
            .collect();
        assert_eq!(
            params,
            [
                (Some("gpt-6-luna".into()), Some("xhigh".into())),
                (Some("gpt-6.2-astra".into()), Some("high".into())),
                (None, None),
            ]
        );
        let e = errors(&format!(
            "{tree}[[codex_workers]]\nname = \"dev-a\"\n[[codex_workers]]\nname = \"dev-a\"\n\
             reasoning_effort = \"huge\"\n"
        ));
        assert!(
            e.iter()
                .any(|e| e.contains("codex_workers[0].name \"dev-a\": must match")),
            "{e:?}"
        );
        assert!(
            e.iter()
                .any(|e| e.contains("codex_workers[1].name \"dev-a\" is repeated")),
            "{e:?}"
        );
        assert!(
            e.iter()
                .any(|e| e.contains("codex_workers[1].reasoning_effort: expected one of")),
            "{e:?}"
        );
        let e = errors(&format!(
            "{tree}[[codex_workers]]\nname = \"dev\"\nsandbox = \"read-only\"\n"
        ));
        assert!(
            e.iter()
                .any(|e| e.ends_with("unknown key \"sandbox\" in codex_workers[0]")),
            "{e:?}"
        );
    }

    #[test]
    fn codex_tree_and_legacy_alias_require_one_shared_codex_coordinator() {
        let root = Path::new("/r/p");
        for key in ["codex_tree", "experimental_codex_tree"] {
            let text = format!("{key} = true\nkind = \"codex\"\n{MIN}");
            let parsed = Config::parse(&text, root).unwrap();
            assert!(parsed.codex_tree, "{key}");
            assert!(parsed.is_tree(), "{key}");
            assert_eq!(parsed.session_order("dev"), None);
            assert!(!parsed.is_known_tab("dev"));
            assert!(Config::select_agent(&text, root, Kind::Pi).is_err());
            assert!(Config::select_agent(&text, root, Kind::Claude).is_err());
            for suffix in [
                "extra = true\n",
                "worktree = true\n",
                "[[roles]]\nname = \"second\"\nprompt = \"Worker\"\n",
            ] {
                assert!(
                    errors(&format!("{text}{suffix}"))
                        .iter()
                        .any(|error| error
                            .contains("codex_tree requires exactly one Codex coordinator")),
                    "{key} + {suffix}"
                );
            }
            let both = format!("agy_tree = true\n{text}");
            assert!(
                errors(&both)
                    .iter()
                    .any(|error| error.contains("codex_tree and agy_tree cannot both be enabled")),
                "{key}"
            );
        }
        // Agreeing values are accepted; contradictory values fail validation.
        let both_true =
            format!("codex_tree = true\nexperimental_codex_tree = true\nkind = \"codex\"\n{MIN}");
        assert!(Config::parse(&both_true, root).unwrap().codex_tree);
        let both_false = format!("codex_tree = false\nexperimental_codex_tree = false\n{MIN}");
        assert!(!Config::parse(&both_false, root).unwrap().codex_tree);
        for conflict in [
            format!("codex_tree = true\nexperimental_codex_tree = false\nkind = \"codex\"\n{MIN}"),
            format!("codex_tree = false\nexperimental_codex_tree = true\nkind = \"codex\"\n{MIN}"),
        ] {
            assert!(
                errors(&conflict)
                    .iter()
                    .any(|error| error.contains("contradictory")),
                "{conflict}"
            );
        }
        let plain = Config::parse(MIN, root).unwrap();
        assert!(!plain.codex_tree);
        assert!(!plain.is_tree());
    }

    #[test]
    fn agy_tree_and_legacy_alias_require_one_shared_agy_coordinator() {
        let root = Path::new("/r/p");
        for key in ["agy_tree", "experimental_agy_tree"] {
            let text = format!("{key} = true\nkind = \"agy\"\n{MIN}");
            assert!(Config::parse(&text, root).unwrap().agy_tree, "{key}");
            assert!(Config::select_agent(&text, root, Kind::Claude).is_err());
            for suffix in [
                "extra = true\n",
                "worktree = true\n",
                "[[roles]]\nname = \"second\"\nprompt = \"Worker\"\n",
            ] {
                assert!(
                    errors(&format!("{text}{suffix}"))
                        .iter()
                        .any(|error| error.contains("exactly one agy coordinator")),
                    "{key} + {suffix}"
                );
            }
            let both = format!("experimental_codex_tree = true\n{text}");
            assert!(
                errors(&both)
                    .iter()
                    .any(|error| error.contains("cannot both be enabled")),
                "{key}"
            );
        }
        // Agreeing values are accepted; contradictory values fail validation.
        let both_true =
            format!("agy_tree = true\nexperimental_agy_tree = true\nkind = \"agy\"\n{MIN}");
        assert!(Config::parse(&both_true, root).unwrap().agy_tree);
        let both_false = format!("agy_tree = false\nexperimental_agy_tree = false\n{MIN}");
        assert!(!Config::parse(&both_false, root).unwrap().agy_tree);
        for conflict in [
            format!("agy_tree = true\nexperimental_agy_tree = false\nkind = \"agy\"\n{MIN}"),
            format!("agy_tree = false\nexperimental_agy_tree = true\nkind = \"agy\"\n{MIN}"),
        ] {
            assert!(
                errors(&conflict)
                    .iter()
                    .any(|error| error.contains("contradictory")),
                "{conflict}"
            );
        }
        let multi_role_conflict = format!(
            "agy_tree = true\nexperimental_agy_tree = false\n{MIN}[[roles]]\nname = \"p-dev\"\nprompt = \"Dev\"\n"
        );
        let conflict_errors = errors(&multi_role_conflict);
        assert_eq!(conflict_errors.len(), 1, "{conflict_errors:?}");
        assert!(
            conflict_errors[0].contains("contradictory"),
            "{conflict_errors:?}"
        );
        assert!(
            !conflict_errors
                .iter()
                .any(|error| error.contains("exactly one agy coordinator")),
            "{conflict_errors:?}"
        );
        assert!(!Config::parse(MIN, root).unwrap().agy_tree);
        let fixture = include_str!("../tests/fixtures/crew-agy-tree.toml");
        let parsed = Config::parse(fixture, root).unwrap();
        assert!(parsed.agy_tree);
        assert!(parsed.is_tree());
        assert_eq!(parsed.kind, Kind::Agy);
        assert_eq!(parsed.roles.len(), 1);
        assert_eq!(parsed.session_order("agy-lead"), Some((0, 1)));
        assert_eq!(parsed.session_order("dev"), None);
        assert!(!parsed.is_known_tab("dev"));
    }

    #[test]
    fn config_editor_rejects_changing_agent_while_agy_tree_is_active() {
        use crate::tui::editor::Editor;
        use crate::tui::testing::{draw, send};

        let root = Path::new("/r/p");
        for text in [
            format!("agy_tree = true\nkind = \"agy\"\n{MIN}"),
            include_str!("../tests/fixtures/crew-agy-tree.toml").to_string(),
        ] {
            let mut editor = Editor::new(root, &text).unwrap();
            // Defaults row (row 0, col 0 = Agent): try switching from agy to claude (index 0).
            send(&mut editor, "{enter}1{enter}");
            let screen = draw(&mut editor, 120, 30);
            assert!(
                screen.contains("agy_tree crews run one agy coordinator"),
                "{screen}"
            );
            assert!(!screen.contains("unsaved"), "{screen}");
            // Coordinator row (row 1, col 0 = Agent): try overriding role agent to codex (index 2).
            send(&mut editor, "{down}{enter}3{enter}");
            let screen = draw(&mut editor, 120, 30);
            assert!(
                screen.contains("agy_tree crews run one agy coordinator"),
                "{screen}"
            );
            assert!(!screen.contains("unsaved"), "{screen}");
            // Changing the coordinator's effort still works normally.
            send(&mut editor, "{right}{right}{enter}2{enter}");
            let screen = draw(&mut editor, 120, 30);
            assert!(screen.contains("unsaved"), "{screen}");
        }
    }
}
