//! `.herdr/crew.toml`: parsing and validation (design §2). Pure: it never touches the disk.

use std::fmt;
use std::ops::Range;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use toml::de::{DeTable, DeValue};

use crate::agent::{CODEX_KEYS, CodexOptions, Kind, PI_KEYS, PiOptions};
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
    pub kind: Kind,
    pub codex: CodexOptions,
    pub pi: PiOptions,
    pub label: String,
    pub board: Board,
    pub worktrees: Option<Worktrees>,
    /// Appended to every role's prompt.
    pub common_prompt: Option<String>,
    /// First message of a new conversation, unless the role has its own.
    pub start_message: Option<String>,
    pub roles: Vec<Role>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Board {
    pub tab: String,
    /// Relative to the root.
    pub file: String,
    pub writer: String,
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
    pub codex: CodexOptions,
    pub pi: PiOptions,
    pub name: String,
    pub prompt: String,
    pub worktree: bool,
    pub extra: bool,
    /// Overrides the top-level `start_message`; empty means none for this role.
    pub start_message: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default)]
    kind: Kind,
    #[serde(default)]
    codex: CodexOptions,
    #[serde(default)]
    pi: PiOptions,
    version: i64,
    workspace: RawWorkspace,
    board: RawBoard,
    worktrees: Option<RawWorktrees>,
    common_prompt: Option<String>,
    start_message: Option<String>,
    #[serde(default)]
    roles: Vec<RawRole>,
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
    codex: Option<CodexOptions>,
    pi: Option<PiOptions>,
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
    "version",
    "kind",
    "codex",
    "pi",
    "workspace",
    "board",
    "worktrees",
    "common_prompt",
    "start_message",
    "roles",
];
const TABLE_KEYS: &[(&str, &[&str])] = &[
    ("codex", CODEX_KEYS),
    ("pi", PI_KEYS),
    ("workspace", &["label"]),
    ("board", &["tab", "file", "writer"]),
    ("worktrees", &["dir", "base"]),
];
const ROLE_KEYS: &[&str] = &[
    "name",
    "prompt",
    "worktree",
    "extra",
    "start_message",
    "kind",
    "codex",
    "pi",
];

impl Config {
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
        for problem in raw.codex.problems() {
            err(format!("codex.{problem}"));
        }
        for problem in raw.pi.problems() {
            err(format!("pi.{problem}"));
        }
        for (i, r) in raw.roles.iter().enumerate() {
            if let Some(options) = &r.codex {
                if r.kind.unwrap_or(raw.kind) != Kind::Codex {
                    err(format!("roles[{i}].codex requires kind = \"codex\""));
                }
                for problem in options.problems() {
                    err(format!("roles[{i}].codex.{problem}"));
                }
            }
            if let Some(options) = &r.pi {
                if r.kind.unwrap_or(raw.kind) != Kind::Pi {
                    err(format!("roles[{i}].pi requires kind = \"pi\""));
                }
                for problem in options.problems() {
                    err(format!("roles[{i}].pi.{problem}"));
                }
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

        // Placeholders are looked up in the source text of each prompt, so that the error points
        // at its line and column in crew.toml.
        for (place, span) in prompt_spans(table.get_ref()) {
            let source = &text[span.clone()];
            for (offset, name) in prompt::unknown_placeholders(source) {
                errors.push(at(
                    Some(span.start + offset..span.start + offset),
                    format!("{place}: unknown placeholder {{{{{name}}}}}"),
                ));
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
            kind: raw.kind,
            codex: raw.codex.clone(),
            pi: raw.pi.clone(),
            label: raw.workspace.label,
            board: Board {
                tab: raw.board.tab,
                file: raw
                    .board
                    .file
                    .unwrap_or_else(|| DEFAULT_BOARD_FILE.to_string()),
                writer: raw.board.writer,
            },
            worktrees,
            common_prompt: raw.common_prompt,
            start_message: raw.start_message,
            roles: raw
                .roles
                .into_iter()
                .map(|r| Role {
                    kind: r.kind.unwrap_or(raw.kind),
                    codex: r.codex.unwrap_or_default().inherit(&raw.codex),
                    pi: r.pi.unwrap_or_default().inherit(&raw.pi),
                    name: r.name,
                    prompt: r.prompt,
                    worktree: r.worktree,
                    extra: r.extra,
                    start_message: r.start_message,
                })
                .collect(),
        })
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
        } else if key == "roles"
            && let DeValue::Array(items) = value.get_ref()
        {
            for (i, item) in items.iter().enumerate() {
                if let DeValue::Table(t) = item.get_ref() {
                    check(t, ROLE_KEYS, &format!("roles[{i}]"));
                    for (agent, known) in [("codex", CODEX_KEYS), ("pi", PI_KEYS)] {
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
    fn codex_options_require_a_codex_role_and_unknown_keys_have_positions() {
        assert!(
            errors(&format!("{MIN}[roles.codex]\nmodel = \"custom\"\n"))
                .iter()
                .any(|error| error.contains("requires kind"))
        );
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
    fn pi_roles_inherit_options_and_reject_foreign_ones() {
        let text = WORKTREES
            .replace("version = 1", "version = 1\nkind = \"pi\"")
            .replace(
                "[workspace]",
                "[pi]\nprovider = \"anthropic\"\nthinking = \"high\"\n[workspace]",
            );
        let c = Config::parse(&text, Path::new("/r/globex")).unwrap();
        assert!(c.roles.iter().all(|role| role.kind == Kind::Pi));
        let extra = c.session_role("globex-dev-2").unwrap();
        assert_eq!(extra.pi.provider.as_deref(), Some("anthropic"));
        assert_eq!(extra.pi.thinking.as_deref(), Some("high"));
        assert!(
            errors(&format!("{MIN}[roles.pi]\nmodel = \"sonnet\"\n"))
                .iter()
                .any(|error| error.contains("roles[0].pi requires kind = \"pi\""))
        );
        let e = errors(&format!(
            "{MIN}kind = \"pi\"\n[roles.pi]\nsandbox = \"x\"\n"
        ));
        assert!(
            e[0].contains("unknown key \"sandbox\" in roles[0].pi"),
            "{e:?}"
        );
        let e = errors(&format!(
            "{MIN}kind = \"pi\"\n[roles.pi]\nthinking = \"max\"\n"
        ));
        assert!(
            e[0].ends_with(
                "roles[0].pi.thinking: expected one of off, minimal, low, medium, high, xhigh"
            ),
            "{e:?}"
        );
    }
}
