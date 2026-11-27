//! Edits of `crew.toml` that keep the user's comments, order and formatting (used by `config`).
//! Callers validate the result with `Config::parse` before saving it.

use toml_edit::{DocumentMut, Item, Table, TableLike, value};

use crate::agent::Kind;

/// The defaults at the top level, one entry of `[[roles]]`, or one entry of
/// `[[codex_workers]]` (whose options sit directly in the entry, without an agent table).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Defaults,
    Role(usize),
    Worker(usize),
}

pub fn parse(text: &str) -> Result<DocumentMut, String> {
    text.parse::<DocumentMut>().map_err(|e| e.to_string())
}

fn table(doc: &DocumentMut, target: Target) -> Option<&dyn TableLike> {
    match target {
        Target::Defaults => Some(doc.as_table()),
        Target::Role(i) => doc
            .get("roles")?
            .as_array_of_tables()?
            .get(i)
            .map(|t| t as &dyn TableLike),
        // `[[codex_workers]]` or an inline `codex_workers = [{ … }]`.
        Target::Worker(i) => match doc.get("codex_workers")? {
            Item::ArrayOfTables(a) => a.get(i).map(|t| t as &dyn TableLike),
            item => item
                .as_array()?
                .get(i)?
                .as_inline_table()
                .map(|t| t as &dyn TableLike),
        },
    }
}

fn table_mut(doc: &mut DocumentMut, target: Target) -> Option<&mut dyn TableLike> {
    match target {
        Target::Defaults => Some(doc.as_table_mut()),
        Target::Role(i) => doc
            .get_mut("roles")?
            .as_array_of_tables_mut()?
            .get_mut(i)
            .map(|t| t as &mut dyn TableLike),
        Target::Worker(i) => match doc.get_mut("codex_workers")? {
            Item::ArrayOfTables(a) => a.get_mut(i).map(|t| t as &mut dyn TableLike),
            item => item
                .as_array_mut()?
                .get_mut(i)?
                .as_inline_table_mut()
                .map(|t| t as &mut dyn TableLike),
        },
    }
}

/// Assigns through an existing value, keeping its comments.
fn assign(options: &mut dyn TableLike, key: &str, new: &str) {
    match options.get_mut(key) {
        Some(item) if item.is_value() => {
            let decor = item.as_value().unwrap().decor().clone();
            *item = value(new);
            *item.as_value_mut().unwrap().decor_mut() = decor;
        }
        _ => {
            options.insert(key, value(new));
        }
    }
}

/// The `kind` written at the target, if any.
pub fn kind(doc: &DocumentMut, target: Target) -> Option<String> {
    table(doc, target)?
        .get("kind")?
        .as_str()
        .map(str::to_string)
}

/// Writes or, with `None`, removes the target's `kind`.
pub fn set_kind(doc: &mut DocumentMut, target: Target, kind: Option<Kind>) {
    if matches!(target, Target::Worker(_)) {
        return;
    }
    let Some(t) = table_mut(doc, target) else {
        return;
    };
    match kind {
        Some(kind) => match t.get_mut("kind") {
            // Assigning through the existing value keeps its comments.
            Some(item) if item.is_value() => {
                let decor = item.as_value().unwrap().decor().clone();
                *item = value(kind.as_str());
                *item.as_value_mut().unwrap().decor_mut() = decor;
            }
            _ => {
                t.insert("kind", value(kind.as_str()));
            }
        },
        None => {
            t.remove("kind");
        }
    }
}

/// The value of `[<agent>].<key>` or `[roles.<agent>].<key>` written at the target.
pub fn option(doc: &DocumentMut, target: Target, agent: Kind, key: &str) -> Option<String> {
    if matches!(target, Target::Worker(_)) {
        return table(doc, target)?.get(key)?.as_str().map(str::to_string);
    }
    table(doc, target)?
        .get(agent.as_str())?
        .as_table_like()?
        .get(key)?
        .as_str()
        .map(str::to_string)
}

/// Writes or, with `None`, removes one agent option at the target. A table left empty is
/// removed with it.
pub fn set_option(
    doc: &mut DocumentMut,
    target: Target,
    agent: Kind,
    key: &str,
    new: Option<&str>,
) {
    let position = first_table_position(doc);
    let Some(t) = table_mut(doc, target) else {
        return;
    };
    // A worker entry keeps its keys inline and stays, even empty, so that it remains listed.
    if matches!(target, Target::Worker(_)) {
        match new {
            Some(new) => assign(t, key, new),
            None => {
                t.remove(key);
            }
        }
        return;
    }
    let name = agent.as_str();
    match new {
        Some(new) => {
            if t.get(name).and_then(Item::as_table_like).is_none() {
                let mut options = Table::new();
                // A new top-level table goes before the workspace and the roles, where the
                // other defaults are; a role's own goes after the role's keys.
                if target == Target::Defaults
                    && let Some(position) = position
                {
                    options.set_position(Some(position));
                }
                t.insert(name, Item::Table(options));
            }
            let options = t.get_mut(name).unwrap().as_table_like_mut().unwrap();
            assign(options, key, new);
            if let Some(inline) = t.get_mut(name).and_then(Item::as_inline_table_mut) {
                inline.fmt();
            }
        }
        None => {
            if let Some(options) = t.get_mut(name).and_then(Item::as_table_like_mut) {
                options.remove(key);
                if options.is_empty() {
                    t.remove(name);
                }
            }
        }
    }
}

/// The position of the first top-level table, so that a new one can be placed before it.
fn first_table_position(doc: &DocumentMut) -> Option<isize> {
    doc.as_table()
        .iter()
        .filter_map(|(_, item)| item.as_table()?.position())
        .min()
        .map(|p| p - 1)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::config::Config;

    const TEXT: &str = r#"# My crew
version = 1
kind = "claude" # project default

[workspace]
label = "p"

[board]
tab = "p-status"
writer = "p-lead"

[[roles]]
name = "p-lead"
prompt = "Lead." # keep me

[[roles]]
name = "p-dev"
prompt = "Develop."

[roles.codex]
model = "gpt-6.2-sol"
"#;

    fn check(text: &str) -> Config {
        Config::parse(text, Path::new("/r/p")).unwrap_or_else(|e| panic!("{e:?}\n{text}"))
    }

    #[test]
    fn options_are_added_changed_and_removed_keeping_comments() {
        let mut doc = parse(TEXT).unwrap();
        set_option(
            &mut doc,
            Target::Role(0),
            Kind::Claude,
            "model",
            Some("opus"),
        );
        set_option(
            &mut doc,
            Target::Role(0),
            Kind::Claude,
            "effort",
            Some("high"),
        );
        set_option(
            &mut doc,
            Target::Defaults,
            Kind::Claude,
            "effort",
            Some("medium"),
        );
        set_option(
            &mut doc,
            Target::Role(1),
            Kind::Codex,
            "model",
            Some("gpt-6-luna"),
        );
        set_kind(&mut doc, Target::Defaults, Some(Kind::Codex));
        set_kind(&mut doc, Target::Role(0), Some(Kind::Claude));
        let text = doc.to_string();
        assert!(text.starts_with("# My crew\n"), "{text}");
        assert!(
            text.contains("kind = \"codex\" # project default"),
            "{text}"
        );
        assert!(text.contains("prompt = \"Lead.\" # keep me"), "{text}");
        // The defaults stay above the workspace; the role's table follows the role.
        let defaults = text.find("[claude]").unwrap();
        assert!(defaults < text.find("[workspace]").unwrap(), "{text}");
        let lead = text.find("name = \"p-lead\"").unwrap();
        let lead_options = text.find("[roles.claude]").unwrap();
        assert!(lead < lead_options && lead_options < text.find("name = \"p-dev\"").unwrap());
        let config = check(&text);
        assert_eq!(config.kind, Kind::Codex);
        assert_eq!(config.roles[0].kind, Kind::Claude);
        assert_eq!(config.roles[0].claude.model.as_deref(), Some("opus"));
        assert_eq!(config.roles[0].claude.effort.as_deref(), Some("high"));
        assert_eq!(config.roles[1].codex.model.as_deref(), Some("gpt-6-luna"));
        assert_eq!(
            option(&doc, Target::Defaults, Kind::Claude, "effort").as_deref(),
            Some("medium")
        );
        assert_eq!(kind(&doc, Target::Role(1)), None);

        set_option(&mut doc, Target::Role(0), Kind::Claude, "model", None);
        set_option(&mut doc, Target::Role(0), Kind::Claude, "effort", None);
        set_option(&mut doc, Target::Defaults, Kind::Claude, "effort", None);
        set_kind(&mut doc, Target::Role(0), None);
        set_kind(&mut doc, Target::Defaults, Some(Kind::Claude));
        set_option(
            &mut doc,
            Target::Role(1),
            Kind::Codex,
            "model",
            Some("gpt-6.2-sol"),
        );
        assert_eq!(doc.to_string(), TEXT);
    }

    #[test]
    fn inline_option_tables_are_edited_in_place() {
        let text = TEXT.replace(
            "prompt = \"Lead.\" # keep me",
            "prompt = \"Lead.\"\nclaude = { model = \"sonnet\" }",
        );
        let mut doc = parse(&text).unwrap();
        set_option(
            &mut doc,
            Target::Role(0),
            Kind::Claude,
            "effort",
            Some("low"),
        );
        let out = doc.to_string();
        assert!(
            out.contains("claude = { model = \"sonnet\", effort = \"low\" }"),
            "{out}"
        );
        check(&out);
    }
}
