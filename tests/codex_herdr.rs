//! Opt-in real Herdr lifecycle and Codex queues with temporary configuration.
#![cfg(unix)]

#[test]
#[ignore = "needs CREW_REAL_HERDR=1, herdr and python3; uses an isolated named test session"]
fn configured_codex_survives_a_real_herdr_restart() {
    if std::env::var_os("CREW_REAL_HERDR").is_none() {
        return;
    }
    let status = std::process::Command::new("python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/support/codex_herdr.py"
        ))
        .arg(env!("CARGO_BIN_EXE_herdr-crew"))
        .status()
        .expect("python3 must be installed for the real Herdr protocol test");
    assert!(status.success());
}

#[test]
#[ignore = "needs CREW_REAL_HERDR=1, CREW_REAL_CODEX=1, Herdr and Codex 0.163.0+; uses an isolated home and local mock provider"]
fn native_codex_queue_reaches_two_roles_and_survives_restore() {
    if std::env::var_os("CREW_REAL_HERDR").is_none()
        || std::env::var_os("CREW_REAL_CODEX").is_none()
    {
        return;
    }
    let status = std::process::Command::new("python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/support/codex_queue.py"
        ))
        .arg(env!("CARGO_BIN_EXE_herdr-crew"))
        .status()
        .expect("python3 must be installed for the real Codex queue test");
    assert!(status.success());
}
