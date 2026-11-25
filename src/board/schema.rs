//! The JSON Schema of `status.json`, generated from the configuration, and the seeded board
//! (design §5.1). The descriptions are the instructions for whoever writes the board. Pure.

use serde_json::{Value, json};

use crate::config::Config;

/// Content of `.herdr/status.schema.json`.
pub fn generate(c: &Config) -> String {
    let roles: Vec<&str> = c.roles.iter().map(|r| r.name.as_str()).collect();
    let extras: Vec<&str> = c
        .roles
        .iter()
        .filter(|r| r.extra)
        .map(|r| r.name.as_str())
        .collect();
    let writer = &c.board.writer;
    let file = &c.board.file;

    let (sessions_text, sessions_description, role_schema) = if c.is_tree() {
        (
            format!(
                "Status board of the crew coordinator ({}) and any native subagent workers it \
                 delegates to.",
                roles.join(", ")
            ),
            "One entry for the coordinator plus any tracked native subagent workers, each name at \
             most once. The board orders the coordinator first, followed by workers in listed \
             order.",
            json!({
                "description": format!(
                    "Session name: the coordinator role ({}) or a valid worker name \
                     ([a-z][a-z0-9_-]{{0,31}}, e.g. dev, reviewer).",
                    roles.join(", ")
                ),
                "anyOf": [
                    { "enum": roles },
                    {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": 32,
                        "pattern": "^[a-z][a-z0-9_-]{0,31}$",
                    },
                ],
            }),
        )
    } else if extras.is_empty() {
        (
            format!(
                "Status board of the crew sessions, one per role ({}).",
                roles.join(", ")
            ),
            "One entry per session, each name at most once. The board orders them as roles[] in .herdr/crew.toml, each extra instance after its role by number.",
            json!({ "description": "The session's role.", "enum": roles }),
        )
    } else {
        (
            format!(
                "Status board of the crew sessions, one per role ({}), plus the extra \
                 instances <role>-N of {} that {writer} starts with `herdr-crew add <role>` once \
                 the user agrees.",
                roles.join(", "),
                extras.join(", ")
            ),
            "One entry per session, each name at most once. The board orders them as roles[] in .herdr/crew.toml, each extra instance after its role by number.",
            json!({
                "description": format!(
                    "Session name: a role ({}) or an extra instance <role>-N (N >= 2) of {}.",
                    roles.join(", "),
                    extras.join(", ")
                ),
                "anyOf": [
                    { "enum": roles },
                    { "type": "string", "pattern": format!("^({})-([2-9]|[1-9][0-9]+)$", extras.join("|")) },
                ],
            }),
        )
    };

    let string_list = json!({ "type": "array", "items": { "type": "string", "minLength": 1 } });
    let with = |description: &str, base: &Value| {
        let mut v = base.clone();
        v["description"] = json!(description);
        v
    };

    let schema = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": "status.schema.json",
        "title": "Status of the herdr sessions",
        "description": format!(
            "{sessions_text} It lives in {file} (not versioned) and `herdr-crew board` draws it. \
             ONLY {writer} writes it, on every state change of any session; the others report to \
             {writer} and never edit this file. Atomic writes: write the whole JSON to {file}.tmp \
             and rename it to {file} (mv), never edit in place. Short texts, one line each."
        ),
        "type": "object",
        "additionalProperties": false,
        "required": ["version", "updatedAt", "updatedBy", "sessions", "owner"],
        "properties": {
            "$schema": {
                "description": "Relative path to this schema so that editors validate the file; from .herdr/status.json it is \"status.schema.json\".",
                "type": "string",
            },
            "version": { "description": "Format version. Always 1 today.", "const": 1 },
            "updatedAt": {
                "description": "Time of the last write, ISO 8601 with an offset (e.g. 2026-09-25T09:10:00-03:00). Updated on every write.",
                "type": "string",
                "format": "date-time",
            },
            "updatedBy": {
                "description": format!("Who wrote the file. Only {writer}."),
                "const": writer,
            },
            "sessions": {
                "description": sessions_description,
                "type": "array",
                "items": { "$ref": "#/$defs/session" },
            },
            "owner": {
                "description": "What the sessions are waiting for from the user (the \"Waiting on you\" section). Empty lists when there is nothing.",
                "type": "object",
                "additionalProperties": false,
                "required": ["urgent", "beforeMain", "optional"],
                "properties": {
                    "urgent": with("What blocks work right now and needs the user.", &string_list),
                    "beforeMain": with("What the user must do before the next merge into main.", &string_list),
                    "optional": with("What the user can do whenever; it blocks nothing.", &string_list),
                },
            },
            "recent": {
                "description": "Optional: latest relevant events, newest first; keep about 5 at most.",
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["at", "text"],
                    "properties": {
                        "at": { "type": "string", "format": "date-time" },
                        "text": { "type": "string", "minLength": 1 },
                    },
                },
            },
        },
        "$defs": {
            "session": {
                "type": "object",
                "additionalProperties": false,
                "required": ["role", "state", "now", "next"],
                "properties": {
                    "role": role_schema,
                    "state": {
                        "description": "working: working on \"now\". waiting: waits for another session or for the user (say which in \"now\" or \"note\"). idle: free, nothing assigned. blocked: cannot go on (say why in \"note\").",
                        "enum": ["working", "waiting", "idle", "blocked"],
                    },
                    "now": {
                        "description": "What it is doing now, in one line (with the name of the change or task, if any). \"idle\" when idle.",
                        "type": "string",
                        "minLength": 1,
                    },
                    "next": with("Its queue in order; an empty list when there is none.", &string_list),
                    "note": {
                        "description": "Optional: a short extra fact (a blocker, a quota, who it waits for).",
                        "type": "string",
                    },
                    "updatedAt": {
                        "description": "Optional: when this session's entry last changed (ISO 8601 with an offset); the board shows its age.",
                        "type": "string",
                        "format": "date-time",
                    },
                },
            },
        },
    });
    let mut text = serde_json::to_string_pretty(&schema).expect("serializable JSON");
    text.push('\n');
    text
}

/// A minimal valid board that `up` seeds when there is none.
pub fn seed(c: &Config, now: &str) -> String {
    format!(
        "{{\n  \"$schema\": \"status.schema.json\",\n  \"version\": 1,\n  \"updatedAt\": {},\n  \
         \"updatedBy\": {},\n  \"sessions\": [],\n  \"owner\": {{ \"urgent\": [], \"beforeMain\": [], \"optional\": [] }}\n}}\n",
        json!(now),
        json!(c.board.writer)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::model::load;
    use crate::board::model::tests::fixture;
    use crate::config::tests::{basic, worktrees};

    #[test]
    fn schema_follows_the_config() {
        let v: Value = serde_json::from_str(&generate(&basic())).unwrap();
        assert_eq!(v["properties"]["updatedBy"]["const"], "acme-lead");
        assert_eq!(
            v["$defs"]["session"]["properties"]["role"]["enum"],
            json!(["acme-lead", "acme-reviewer", "acme-designer", "acme-dev"])
        );
        assert!(v["properties"]["sessions"].get("maxItems").is_none());
        let v: Value = serde_json::from_str(&generate(&worktrees())).unwrap();
        assert_eq!(
            v["$defs"]["session"]["properties"]["role"]["anyOf"][1]["pattern"],
            "^(globex-dev|globex-reviewer)-([2-9]|[1-9][0-9]+)$"
        );
        let desc = v["description"].as_str().unwrap();
        assert!(desc.contains("Status board of the crew sessions"));
        assert!(!desc.contains("Claude Code"));
        assert!(desc.contains("ONLY globex-lead writes it"));
    }

    #[test]
    fn seed_is_a_valid_board() {
        let c = worktrees();
        let s = load(seed(&c, "2026-09-26T10:00:00-03:00").as_bytes(), &c).unwrap();
        assert!(s.sessions.is_empty());
        assert_eq!(s.updated_by, "globex-lead");
    }

    /// The viewer enforces every presence, type and emptiness rule the generated schema states
    /// for the fields of each object, so a writer that follows the schema is never surprised.
    /// Dates, `updatedBy`, roles and repeated roles have their own tests in `model`.
    #[test]
    fn viewer_enforces_the_schema_rules() {
        let c = worktrees();
        let schema: Value = serde_json::from_str(&generate(&c)).unwrap();
        let mut board: Value = serde_json::from_slice(&fixture("status-worktrees.json")).unwrap();
        board["sessions"][0]["note"] = json!("a note");
        let accepts = |b: &Value| load(b.to_string().as_bytes(), &c).is_ok();
        assert!(accepts(&board));
        let with = |at: &str, change: &dyn Fn(&mut Value)| {
            let mut b = board.clone();
            change(b.pointer_mut(at).unwrap());
            b
        };

        let objects = [
            ("", &schema),
            ("/owner", &schema["properties"]["owner"]),
            ("/sessions/0", &schema["$defs"]["session"]),
            ("/recent/0", &schema["properties"]["recent"]["items"]),
        ];
        for (at, object) in objects {
            assert_eq!(object["additionalProperties"], false, "{at}");
            let b = with(at, &|o| o["unknown"] = json!(1));
            assert!(!accepts(&b), "{at}: unknown field");
            let required = object["required"].as_array().unwrap();
            let properties = object["properties"].as_object().unwrap();
            assert!(
                required
                    .iter()
                    .all(|r| properties.contains_key(r.as_str().unwrap()))
            );
            for (key, property) in properties {
                let path = format!("{at}/{key}");
                let b = with(at, &|o| {
                    o.as_object_mut().unwrap().remove(key);
                });
                assert_eq!(
                    accepts(&b),
                    !required.contains(&json!(key)),
                    "{path}: absent"
                );
                for wrong in [
                    json!(null),
                    json!(true),
                    json!(123),
                    json!({}),
                    json!([123]),
                ] {
                    let b = with(&path, &|v| *v = wrong.clone());
                    assert!(!accepts(&b), "{path}: {wrong}");
                }
                if property["type"] == "string" {
                    let empty_allowed = property.get("minLength").is_none();
                    let b = with(&path, &|v| *v = json!(""));
                    assert_eq!(
                        accepts(&b),
                        empty_allowed && property.get("format").is_none(),
                        "{path}: empty"
                    );
                }
                if property["items"]["type"] == "string" {
                    assert_eq!(property["items"]["minLength"], 1, "{path}");
                    let b = with(&path, &|v| *v = json!([""]));
                    assert!(!accepts(&b), "{path}: empty item");
                }
            }
        }
        let b = with("/version", &|v| *v = json!(1.0));
        assert!(accepts(&b), "the const 1 compares by value");
    }

    #[test]
    fn tree_schema_allows_coordinator_and_native_subagent_workers() {
        let agy = Config::parse(
            include_str!("../../tests/fixtures/crew-agy-tree.toml"),
            std::path::Path::new("/r/agy-tree"),
        )
        .unwrap();
        let mut codex = worktrees();
        codex.roles.truncate(1);
        codex.roles[0].kind = crate::agent::Kind::Codex;
        codex.roles[0].worktree = false;
        codex.roles[0].extra = false;
        codex.experimental_codex_tree = true;

        for (c, coordinator) in [(&agy, "agy-lead"), (&codex, "globex-lead")] {
            let v: Value = serde_json::from_str(&generate(c)).unwrap();
            let desc = v["description"].as_str().unwrap();
            assert!(
                desc.contains(&format!("crew coordinator ({coordinator})")),
                "{desc}"
            );
            assert!(desc.contains("native subagent workers"), "{desc}");
            let sessions_desc = v["properties"]["sessions"]["description"].as_str().unwrap();
            assert!(
                sessions_desc.contains("coordinator plus any tracked native subagent workers"),
                "{sessions_desc}"
            );
            assert!(
                sessions_desc.contains("coordinator first, followed by workers in listed order"),
                "{sessions_desc}"
            );
            let role = &v["$defs"]["session"]["properties"]["role"];
            assert_eq!(role["anyOf"][0]["enum"], json!([coordinator]));
            assert_eq!(
                role["anyOf"][1],
                json!({
                    "type": "string",
                    "minLength": 1,
                    "maxLength": 32,
                    "pattern": "^[a-z][a-z0-9_-]{0,31}$",
                })
            );
        }
    }

    #[test]
    fn tree_viewer_matches_schema_role_and_field_rules() {
        let c = Config::parse(
            include_str!("../../tests/fixtures/crew-agy-tree.toml"),
            std::path::Path::new("/r/agy-tree"),
        )
        .unwrap();
        let schema: Value = serde_json::from_str(&generate(&c)).unwrap();
        let worker_rule = &schema["$defs"]["session"]["properties"]["role"]["anyOf"][1];
        assert_eq!(worker_rule["minLength"], 1);
        assert_eq!(worker_rule["maxLength"], 32);
        assert_eq!(worker_rule["pattern"], "^[a-z][a-z0-9_-]{0,31}$");

        let board = json!({
            "$schema": "status.schema.json",
            "version": 1,
            "updatedAt": "2026-09-26T10:00:00-03:00",
            "updatedBy": "agy-lead",
            "sessions": [
                {
                    "role": "agy-lead",
                    "state": "working",
                    "now": "coordinating",
                    "next": [],
                    "note": "active",
                    "updatedAt": "2026-09-26T09:59:00-03:00"
                },
                {
                    "role": "dev",
                    "state": "working",
                    "now": "implementing feature",
                    "next": ["run tests"]
                },
                {
                    "role": "reviewer",
                    "state": "waiting",
                    "now": "waiting for dev",
                    "next": []
                }
            ],
            "owner": { "urgent": [], "beforeMain": [], "optional": [] },
            "recent": [{ "at": "2026-09-26T09:58:00-03:00", "text": "spawned dev" }]
        });
        let accepts = |b: &Value| load(b.to_string().as_bytes(), &c).is_ok();
        assert!(accepts(&board));

        for (candidate, expected) in [
            ("dev-a", true),
            ("herdr-crew-dev", true),
            ("a", true),
            (&"a".repeat(32), true),
            ("", false),
            (&"a".repeat(33), false),
            ("Dev", false),
            ("1dev", false),
            ("-dev", false),
            ("dev 2", false),
            ("dev/2", false),
        ] {
            let mut b = board.clone();
            b["sessions"][1]["role"] = json!(candidate);
            assert_eq!(accepts(&b), expected, "role {candidate:?}");
        }
    }
}
