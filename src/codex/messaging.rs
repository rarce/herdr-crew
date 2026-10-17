//! Native delivery to a bound crew conversation. Codex 0.163.0's durable queue is consumed
//! by the interactive CLI even with `--no-daemon`; it does not require a shared server.

use std::process::Command;

use super::{
    Binding, Lock, RUNTIME, Session, context_key, pane_context, path, read, snapshot, text,
};
use crate::{config::Config, files, herdr::Herdr, process};

const MIN_QUEUE_VERSION: (u32, u32, u32) = (0, 163, 0);

/// `queue --thread` also accepts names. Only pass a UUID so a malformed/stale ID cannot
/// accidentally select an unrelated named conversation from the user's Codex home.
fn uuid(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(i, b)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
}

/// Returns false before delivery when the CLI predates the tested queue protocol or the
/// live pane has no crew binding. Errors after choosing native delivery never fall back to
/// terminal input: the message might already have reached the queue.
pub fn deliver(
    c: &Config,
    herdr: &Herdr,
    name: &str,
    pane: &str,
    message: &str,
) -> Result<bool, String> {
    let bindings = c.root.join(RUNTIME).join("bindings");
    files::validate(&c.root, &bindings, files::Kind::Directory)?;
    if !bindings.exists()
        || !process::detect("codex").is_ok_and(|version| version >= MIN_QUEUE_VERSION)
    {
        return Ok(false);
    }
    let status = herdr
        .call(&["status", "server", "--json"])
        .map_err(|e| e.to_string())?;
    let live = herdr
        .call(&["pane", "get", pane])
        .map_err(|e| e.to_string())?;
    let context = pane_context(&live, text(&status, "socket")?)?;
    let binding_path = path(&c.root, "bindings", &context_key(&context));
    files::validate(&c.root, &binding_path, files::Kind::File)?;
    if !binding_path.exists() {
        return Ok(false);
    }
    let _lock = Lock::acquire(&c.root)?;
    let binding: Binding = read(&c.root, &binding_path)?;
    if binding
        .codex_version
        .is_none_or(|version| version < MIN_QUEUE_VERSION)
    {
        return Ok(false);
    }
    let snapshot = snapshot(&c.root, &binding.token)?;
    let session: Session = read(&c.root, &path(&c.root, "sessions", &binding.token))?;
    let reference = &live["pane"]["agent_session"];
    let cwd = std::path::PathBuf::from(text(&live["pane"], "cwd")?);
    if binding.context != context
        || session.context != context
        || snapshot.name != name
        || snapshot.workspace != c.label
        || cwd.canonicalize().map_err(|e| e.to_string())?
            != snapshot.cwd.canonicalize().map_err(|e| e.to_string())?
        || reference["agent"] != "codex"
        || reference["kind"] != "id"
        || reference["value"] != session.id
        || !uuid(&session.id)
    {
        return Err(
            "the live Codex conversation does not match its saved crew binding; nothing was sent"
                .into(),
        );
    }
    let owner: String = read(&c.root, &path(&c.root, "claims", &session.id))?;
    if owner != binding.token {
        return Err(
            "the Codex conversation is owned by another crew role; nothing was sent".into(),
        );
    }
    let mut command = Command::new("codex");
    command
        .args(["queue", "--thread", &session.id, "--message", message])
        .current_dir(&snapshot.cwd)
        .env("CODEX_HOME", &snapshot.codex_home)
        .env_remove("CODEX_THREAD_ID")
        .env_remove("CREW_CODEX_BINDING");
    process::Inspector::spawn(&mut command)?
        .finish()
        .map_err(|e| {
            format!("native Codex delivery failed and was not retried through the terminal: {e}")
        })?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_targets_cannot_be_interpreted_as_session_names() {
        assert!(uuid("01a14ad6-4d8e-7de3-b791-46c0b4844fa1"));
        for id in [
            "dev",
            "../dev",
            "--last",
            "lead-id",
            "01a14ad6_4d8e-7de3-b791-46c0b4844fa1",
        ] {
            assert!(!uuid(id), "{id}");
        }
    }
}
