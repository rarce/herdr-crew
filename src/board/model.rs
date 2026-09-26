//! `status.json`: types, validation, order and age (design §5.1 and §5.2). Pure.

use jiff::Timestamp;
use serde::Deserialize;
use serde::de::IgnoredAny;
use serde_json::Value;

use crate::config::Config;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Working,
    Waiting,
    Idle,
    Blocked,
}

impl State {
    pub fn label(self) -> &'static str {
        match self {
            State::Working => "working",
            State::Waiting => "waiting",
            State::Idle => "idle",
            State::Blocked => "blocked",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Status {
    pub updated_at: Timestamp,
    pub updated_by: String,
    /// In `roles[]` order, each extra instance after its base role by number.
    pub sessions: Vec<Session>,
    pub owner: Owner,
    pub recent: Vec<Recent>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    pub role: String,
    pub state: State,
    pub now: String,
    pub next: Vec<String>,
    pub note: Option<String>,
    pub updated_at: Option<Timestamp>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Owner {
    pub urgent: Vec<String>,
    pub before_main: Vec<String>,
    pub optional: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Recent {
    pub at: Timestamp,
    pub text: String,
}

/// Why the board cannot be drawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unavailable {
    Missing,
    /// Syntax error, with line and column.
    Json(String),
    /// Schema errors, `<field path>: <message>`.
    Schema(Vec<String>),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RawStatus {
    #[serde(rename = "$schema")]
    _schema: Option<IgnoredAny>,
    version: Value,
    updated_at: String,
    updated_by: String,
    sessions: Vec<Value>,
    owner: Value,
    #[serde(default)]
    recent: Vec<Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RawSession {
    role: String,
    state: State,
    now: String,
    next: Vec<String>,
    note: Option<String>,
    updated_at: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRecent {
    at: String,
    text: String,
}

/// Reads and validates the content of `status.json` against the configuration.
pub fn load(bytes: &[u8], c: &Config) -> Result<Status, Unavailable> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|e| Unavailable::Json(e.to_string()))?;
    let raw: RawStatus = serde_json::from_value(value)
        .map_err(|e| Unavailable::Schema(vec![format!("(root): {e}")]))?;
    let mut errors = Vec::new();

    if raw.version != 1 {
        errors.push(format!("version: must be 1, not {}", raw.version));
    }
    let updated_at = date(&mut errors, "updatedAt", &raw.updated_at);
    if raw.updated_by != c.board.writer {
        errors.push(format!(
            "updatedBy: must be \"{}\", not \"{}\"",
            c.board.writer, raw.updated_by
        ));
    }

    let mut sessions: Vec<Session> = Vec::new();
    for (i, v) in raw.sessions.into_iter().enumerate() {
        let path = format!("sessions[{i}]");
        let s: RawSession = match serde_json::from_value(v) {
            Ok(s) => s,
            Err(e) => {
                errors.push(format!("{path}: {e}"));
                continue;
            }
        };
        if c.session_order(&s.role).is_none() {
            errors.push(format!(
                "{path}.role: \"{}\" is not a role or an extra instance of roles[]",
                s.role
            ));
        } else if sessions.iter().any(|o| o.role == s.role) {
            errors.push(format!("{path}.role: \"{}\" is repeated", s.role));
        }
        non_empty(&mut errors, &format!("{path}.now"), &s.now);
        for (j, n) in s.next.iter().enumerate() {
            non_empty(&mut errors, &format!("{path}.next[{j}]"), n);
        }
        let updated_at = s
            .updated_at
            .as_deref()
            .and_then(|d| date(&mut errors, &format!("{path}.updatedAt"), d));
        sessions.push(Session {
            role: s.role,
            state: s.state,
            now: s.now,
            next: s.next,
            note: s.note,
            updated_at,
        });
    }

    let owner: Option<Owner> = match serde_json::from_value(raw.owner) {
        Ok(o) => Some(o),
        Err(e) => {
            errors.push(format!("owner: {e}"));
            None
        }
    };
    if let Some(o) = &owner {
        for (key, list) in [
            ("urgent", &o.urgent),
            ("beforeMain", &o.before_main),
            ("optional", &o.optional),
        ] {
            for (j, item) in list.iter().enumerate() {
                non_empty(&mut errors, &format!("owner.{key}[{j}]"), item);
            }
        }
    }

    let mut recent = Vec::new();
    for (i, v) in raw.recent.into_iter().enumerate() {
        match serde_json::from_value::<RawRecent>(v) {
            Ok(r) => {
                non_empty(&mut errors, &format!("recent[{i}].text"), &r.text);
                if let Some(at) = date(&mut errors, &format!("recent[{i}].at"), &r.at) {
                    recent.push(Recent { at, text: r.text });
                }
            }
            Err(e) => errors.push(format!("recent[{i}]: {e}")),
        }
    }

    match (errors.is_empty(), updated_at, owner) {
        (true, Some(updated_at), Some(owner)) => {
            sessions.sort_by_key(|s| c.session_order(&s.role));
            Ok(Status {
                updated_at,
                updated_by: raw.updated_by,
                sessions,
                owner,
                recent,
            })
        }
        _ => Err(Unavailable::Schema(errors)),
    }
}

fn date(errors: &mut Vec<String>, path: &str, value: &str) -> Option<Timestamp> {
    match value.parse::<Timestamp>() {
        Ok(t) => Some(t),
        Err(_) => {
            errors.push(format!(
                "{path}: \"{value}\" is not an RFC 3339 date with an offset"
            ));
            None
        }
    }
}

fn non_empty(errors: &mut Vec<String>, path: &str, value: &str) {
    if value.is_empty() {
        errors.push(format!("{path}: must not be empty"));
    }
}

/// `<1 min ago`, `N min ago`, `N h MM min ago` or `N d ago`.
pub fn age(now: Timestamp, then: Timestamp) -> String {
    let secs = now.duration_since(then).as_secs();
    if secs < 60 {
        "<1 min ago".into()
    } else if secs < 3600 {
        format!("{} min ago", secs / 60)
    } else if secs < 86400 {
        format!("{} h {:02} min ago", secs / 3600, secs % 3600 / 60)
    } else {
        format!("{} d ago", secs / 86400)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::path::Path;

    use super::*;
    use crate::config::tests::{basic, worktrees};

    pub fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures")
                .join(name),
        )
        .unwrap()
    }

    fn schema_errors(json: &str, c: &Config) -> Vec<String> {
        match load(json.as_bytes(), c) {
            Err(Unavailable::Schema(e)) => e,
            other => panic!("expected a schema error: {other:?}"),
        }
    }

    const BASE: &str = r#"{"version": 1, "updatedAt": "2026-09-25T09:10:00-03:00", "updatedBy": "globex-lead",
        "sessions": SESSIONS, "owner": {"urgent": [], "beforeMain": [], "optional": []}}"#;

    fn with_sessions(roles: &[&str]) -> String {
        let s: Vec<String> = roles
            .iter()
            .map(|r| format!(r#"{{"role": "{r}", "state": "idle", "now": "idle", "next": []}}"#))
            .collect();
        BASE.replace("SESSIONS", &format!("[{}]", s.join(",")))
    }

    #[test]
    fn accepts_the_example_boards() {
        let s = load(&fixture("status-basic.json"), &basic()).unwrap();
        assert_eq!(s.updated_by, "acme-lead");
        load(&fixture("status-basic-busy.json"), &basic()).unwrap();
        let s = load(&fixture("status-worktrees.json"), &worktrees()).unwrap();
        assert_eq!(s.sessions.len(), 4);
    }

    #[test]
    fn orders_by_roles_with_extras_after_their_base() {
        let json = with_sessions(&[
            "globex-reviewer-2",
            "globex-dev-10",
            "globex-reviewer",
            "globex-dev-2",
            "globex-lead",
            "globex-dev",
        ]);
        let s = load(json.as_bytes(), &worktrees()).unwrap();
        let roles: Vec<_> = s.sessions.iter().map(|s| s.role.as_str()).collect();
        assert_eq!(
            roles,
            [
                "globex-lead",
                "globex-dev",
                "globex-dev-2",
                "globex-dev-10",
                "globex-reviewer",
                "globex-reviewer-2"
            ]
        );
    }

    #[test]
    fn rejects_repeated_unknown_and_non_extra_roles() {
        let c = worktrees();
        assert_eq!(
            schema_errors(&with_sessions(&["globex-lead", "globex-lead"]), &c),
            ["sessions[1].role: \"globex-lead\" is repeated"]
        );
        assert_eq!(
            schema_errors(&with_sessions(&["globex-boss"]), &c),
            ["sessions[0].role: \"globex-boss\" is not a role or an extra instance of roles[]"]
        );
        assert_eq!(
            schema_errors(&with_sessions(&["globex-lead-2"]), &c),
            ["sessions[0].role: \"globex-lead-2\" is not a role or an extra instance of roles[]"]
        );
    }

    #[test]
    fn rejects_unknown_fields() {
        let c = worktrees();
        let e = schema_errors(
            &with_sessions(&[]).replace("\"version\": 1", "\"version\": 1, \"extra\": true"),
            &c,
        );
        assert!(e[0].starts_with("(root): unknown field `extra`"), "{e:?}");
        let e = schema_errors(
            &with_sessions(&["globex-lead"]).replace("\"now\"", "\"color\": 1, \"now\""),
            &c,
        );
        assert!(
            e[0].starts_with("sessions[0]: unknown field `color`"),
            "{e:?}"
        );
    }

    #[test]
    fn other_checks() {
        let json = with_sessions(&["globex-lead"])
            .replace("\"state\": \"idle\"", "\"state\": \"asleep\"")
            .replace("09:10:00-03:00", "09:10:00")
            .replace(
                "\"updatedBy\": \"globex-lead\"",
                "\"updatedBy\": \"globex-dev\"",
            );
        let e = schema_errors(&json, &worktrees());
        assert_eq!(e.len(), 3, "{e:?}");
        assert_eq!(
            e[0],
            "updatedAt: \"2026-09-25T09:10:00\" is not an RFC 3339 date with an offset"
        );
        assert_eq!(
            e[1],
            "updatedBy: must be \"globex-lead\", not \"globex-dev\""
        );
        assert!(
            e[2].starts_with("sessions[0]: unknown variant `asleep`"),
            "{e:?}"
        );
        let e = schema_errors(
            &with_sessions(&["globex-lead"]).replace("\"now\": \"idle\"", "\"now\": \"\""),
            &worktrees(),
        );
        assert_eq!(e, ["sessions[0].now: must not be empty"]);
    }

    #[test]
    fn syntax_errors_keep_line_and_column() {
        let Err(Unavailable::Json(msg)) = load(br#"{"version": 1, "sess"#, &worktrees()) else {
            panic!()
        };
        assert!(msg.contains("line 1 column"), "{msg}");
    }

    #[test]
    fn ages() {
        let t = |s: &str| s.parse::<Timestamp>().unwrap();
        let now = t("2026-09-26T12:00:00Z");
        assert_eq!(age(now, t("2026-09-26T11:59:30Z")), "<1 min ago");
        assert_eq!(age(now, t("2026-09-26T12:00:30Z")), "<1 min ago");
        assert_eq!(age(now, t("2026-09-26T11:15:00Z")), "45 min ago");
        assert_eq!(age(now, t("2026-09-26T09:55:00Z")), "2 h 05 min ago");
        assert_eq!(age(now, t("2026-09-23T12:00:00Z")), "3 d ago");
    }
}
