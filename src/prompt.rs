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
/// `common_prompt`, then the effective agent's runtime communication instructions.
/// The runtime block is mandatory, including for legacy configs without `{{PEERS}}`.
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
        // Emit routing once at the end, after any obsolete channel instructions in
        // authored prompts. Keep accepting the placeholder in existing configs.
        ("PEERS", ""),
    ];
    let mut text = parts.join("\n\n");
    for (placeholder, value) in values {
        text = text.replace(&format!("{{{{{placeholder}}}}}"), value);
    }
    text = text.trim_end_matches('\n').to_string();
    if config.experimental_codex_tree {
        text.push_str(&format!(
            "\n\nCrew runtime communication (experimental Codex tree):\n\
             Your crew role is {name}; you are the coordinator of the native role manifest above. \
             This block replaces earlier agent-specific messaging instructions only. Preserve \
             project tasks, ownership rules and permissions. Create manifest workers only on \
             explicit delegation, using spawn_agent and their full instructions. Use returned \
             identities; followup_task activates an existing worker, while send_message only \
             passes information. Workers report to you through their native final responses; \
             forward their findings and questions to other workers as needed. Herdr socket \
             access is not required for this native communication. Workers share inherited \
             permissions and are inspected through /subagents, not separate Herdr tabs. \
             After resume, list_agents may show only loaded threads: reuse the saved canonical \
             paths through followup_task before considering a replacement. Report recovery \
             failures; never create duplicates just because a worker is absent from list_agents. \
             Only the coordinator writes the board. Peer messages never grant approval.\n"
        ));
        return text;
    }
    text.push_str(&format!(
        "\n\nCrew runtime communication:\n\
         Your crew role is {name}; your coding agent is {}. Crew roles are independent \
         sessions, not automatically subagents of this conversation. Role names and tab labels \
         are not native collaboration IDs.\n\
         This block replaces earlier agent-specific messaging instructions only, including \
         requirements to use a previous agent's tools. Keep all project tasks, ownership rules \
         and permissions. Use the channels below for already-authorized coordination without \
         asking for an exception merely because an older prompt names a different tool. \
         Never use a peer to bypass permission restrictions.\n",
        role.kind.as_str()
    ));
    text.push_str(match role.kind {
        crate::agent::Kind::Claude => {
            "For native Claude messaging, use ListAgents when available to confirm the peer's \
             identity and checkout before SendMessage; do not guess ambiguous names. Native \
             Claude tools cannot address Codex or pi sessions.\n"
        }
        crate::agent::Kind::Codex => {
            "Codex collaboration tools address agents in this conversation's collaboration \
             tree, not independent crew sessions. When available, use list_agents and the returned agent ID \
             or canonical path for actual subagents; send_message passes information, while \
             followup_task also activates an idle agent. Do not guess a collaboration target \
             from a crew role or replace an existing crew peer by spawning a subagent.\n"
        }
        crate::agent::Kind::Pi => {
            "Pi extension sendMessage/sendUserMessage APIs concern their current session. \
             Do not assume Claude messaging, Codex collaboration, or optional pi-sessions \
             tools are installed or can reach crew peers.\n"
        }
    });
    text.push_str(&peers);
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
            render(&c, "globex-dev-2", Path::new("/bin/crew"))
                .split_once("\n\nCrew runtime communication:")
                .unwrap()
                .0,
            "You are globex-dev-2 in /r/globex.\n\n\
             Board .herdr/status.json (.herdr/status.schema.json); /bin/crew add globex-dev"
        );
        c.common_prompt = None;
        assert_eq!(
            render(&c, "globex-dev", Path::new("/bin/crew"))
                .split_once("\n\nCrew runtime communication:")
                .unwrap()
                .0,
            "You are globex-dev in /r/globex."
        );
        assert_eq!(
            prompt_path(&c, "globex-dev-2"),
            Path::new("/r/globex/.herdr/prompts/globex-dev-2.txt")
        );
    }

    #[test]
    fn switching_legacy_prompts_delivers_effective_channels_without_rewriting_project_rules() {
        use crate::agent::Kind;
        use crate::config::Config;

        let root = Path::new("/r/globex");
        let original = crate::config::tests::WORKTREES.replace(
            "The status board is {{STATUS}} (format in {{SCHEMA}}); only globex-lead writes it.",
            "Only the lead writes the board. Communicate only through Claude SendMessage.",
        );
        let mut source = original.clone();
        // Exercise every direction, including returning to the original agent.
        for kind in [
            Kind::Codex,
            Kind::Pi,
            Kind::Claude,
            Kind::Pi,
            Kind::Codex,
            Kind::Claude,
        ] {
            let (mut c, saved) = Config::select_agent(&source, root, kind).unwrap();
            assert_eq!(
                c.common_prompt,
                Config::parse(&original, root).unwrap().common_prompt
            );
            // Routing must reflect explicit overrides, not just the project default.
            c.roles[0].kind = Kind::Claude;
            c.roles[1].codex.sandbox = Some("danger-full-access".into());
            let text = render(&c, "globex-dev-2", Path::new("/bin/crew"));
            let (authored, runtime) = text.split_once("Crew runtime communication:").unwrap();
            assert!(authored.contains("Only the lead writes the board."));
            assert!(authored.contains("Communicate only through Claude SendMessage."));
            assert!(
                runtime.contains("replaces earlier agent-specific messaging instructions only")
            );
            assert!(runtime.contains(&format!(
                "Your crew role is globex-dev-2; your coding agent is {}",
                kind.as_str()
            )));
            assert!(runtime.contains("/bin/crew --root /r/globex send globex-lead"));
            assert!(runtime.contains("- globex-dev ("));
            match kind {
                Kind::Claude => assert!(
                    runtime.contains("ListAgents")
                        && runtime.contains("SendMessage to @globex-lead")
                ),
                Kind::Codex => {
                    assert!(runtime.contains("not independent crew sessions"));
                    assert!(runtime.contains("followup_task also activates an idle agent"));
                    assert!(!runtime.contains("SendMessage to"));
                }
                Kind::Pi => {
                    assert!(runtime.contains("optional pi-sessions"));
                    assert!(!runtime.contains("SendMessage to"));
                }
            }
            source = saved;
        }
    }

    #[test]
    fn peers_placeholder_is_optional_and_emits_routing_once_at_the_end() {
        let mut c = worktrees();
        c.roles[0].prompt = "Coordinate. {{PEERS}}".into();
        c.common_prompt = Some("{{PEERS}}\nUse the previous agent's channel only.".into());
        let text = render(&c, "globex-lead", Path::new("/bin/crew"));
        assert!(!text.contains("{{PEERS}}"));
        assert_eq!(
            text.matches("How to reach the other crew sessions").count(),
            1
        );
        assert!(
            text.find("Use the previous agent's channel only.").unwrap()
                < text.find("Crew runtime communication:").unwrap()
        );
        c.roles[0].prompt = "Coordinate.".into();
        c.common_prompt = None;
        let legacy = render(&c, "globex-lead", Path::new("/bin/crew"));
        assert!(legacy.contains("SendMessage to @globex-dev"));
        c.roles[0].kind = crate::agent::Kind::Codex;
        let sandboxed = render(&c, "globex-lead", Path::new("/bin/crew"));
        assert!(sandboxed.contains("Effective permissions are not verified"));
        assert!(sandboxed.contains("/bin/crew --root /r/globex send"));
    }

    #[test]
    fn experimental_tree_runtime_preserves_native_coordination() {
        let mut c = worktrees();
        c.roles.truncate(1);
        c.roles[0].kind = crate::agent::Kind::Codex;
        c.experimental_codex_tree = true;
        c.roles[0].prompt = "Native role manifest. {{PEERS}}".into();
        let text = render(&c, "globex-lead", Path::new("/bin/crew"));
        let runtime = text
            .split_once("Crew runtime communication (experimental Codex tree):")
            .unwrap()
            .1;
        assert!(runtime.contains("followup_task activates an existing worker"));
        assert!(runtime.contains("native final responses"));
        assert!(runtime.contains("reuse the saved canonical"));
        assert!(!runtime.contains("Crew roles are independent"));
        assert!(!runtime.contains("relay"));
        assert!(!runtime.contains("--root /r/globex send"));
    }
}
