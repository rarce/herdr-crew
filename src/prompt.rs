//! Role prompts: placeholders and the combined prompt (design §2.2). Pure.

use std::path::{Path, PathBuf};

use crate::config::Config;

pub const PLACEHOLDERS: [&str; 6] = ["NAME", "REPO", "STATUS", "SCHEMA", "LAUNCHER", "PEERS"];
pub const SCHEMA_FILE: &str = ".herdr/status.schema.json";
pub const PROMPTS_DIR: &str = ".herdr/prompts";

/// Byte offset and content of every `{{…}}` in `text` that is not a known placeholder.
pub fn unknown_placeholders(text: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(start) = text[from..].find("{{").map(|i| from + i) {
        let Some(end) = text[start + 2..].find("}}").map(|i| start + 2 + i) else {
            break;
        };
        let inner = &text[start + 2..end];
        if !PLACEHOLDERS.contains(&inner) {
            out.push((start, inner.to_string()));
        }
        from = end + 2;
    }
    out
}

/// The full prompt of session `name` (a role or an extra instance): the role's prompt, then
/// `common_prompt`, separated by a blank line, with the placeholders replaced.
pub fn render(config: &Config, name: &str, launcher: &Path) -> String {
    let role = config
        .session_role(name)
        .expect("a session of a known role");
    let parts: Vec<&str> = [Some(role.prompt.as_str()), config.common_prompt.as_deref()]
        .into_iter()
        .flatten()
        .map(|p| p.trim_matches('\n'))
        .filter(|p| !p.is_empty())
        .collect();
    let root = config.root.display().to_string();
    let peers = crate::send::peers(config, name, launcher);
    let launcher = launcher.display().to_string();
    let values = [
        ("NAME", name),
        ("REPO", root.as_str()),
        ("STATUS", config.board.file.as_str()),
        ("SCHEMA", SCHEMA_FILE),
        ("LAUNCHER", launcher.as_str()),
        ("PEERS", peers.as_str()),
    ];
    let mut text = parts.join("\n\n");
    for (placeholder, value) in values {
        text = text.replace(&format!("{{{{{placeholder}}}}}"), value);
    }
    text.push('\n');
    text
}

/// Where the combined prompt of a session is written, for `--append-system-prompt-file`.
pub fn prompt_path(config: &Config, name: &str) -> PathBuf {
    config.root.join(PROMPTS_DIR).join(format!("{name}.txt"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::tests::worktrees;

    #[test]
    fn unknown() {
        assert_eq!(
            unknown_placeholders("a {{NAME}} b {{LAUNCH}} {{}} {{REPO"),
            [(13, "LAUNCH".to_string()), (24, String::new())]
        );
        assert!(
            unknown_placeholders("{{NAME}}{{REPO}}{{STATUS}}{{SCHEMA}}{{LAUNCHER}}{{PEERS}}")
                .is_empty()
        );
    }

    #[test]
    fn role_prompt_then_common_prompt_with_every_placeholder_replaced() {
        let mut c = worktrees();
        c.roles[1].prompt = "\nYou are {{NAME}} in {{REPO}}.\n".into();
        c.common_prompt =
            Some("Board {{STATUS}} ({{SCHEMA}}); {{LAUNCHER}} add globex-dev\n".into());
        assert_eq!(
            render(&c, "globex-dev-2", Path::new("/bin/crew")),
            "You are globex-dev-2 in /r/globex.\n\n\
             Board .herdr/status.json (.herdr/status.schema.json); /bin/crew add globex-dev\n"
        );
        c.common_prompt = None;
        assert_eq!(
            render(&c, "globex-dev", Path::new("/bin/crew")),
            "You are globex-dev in /r/globex.\n"
        );
        assert_eq!(
            prompt_path(&c, "globex-dev-2"),
            Path::new("/r/globex/.herdr/prompts/globex-dev-2.txt")
        );
    }
}
