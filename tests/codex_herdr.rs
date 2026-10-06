//! Opt-in real Herdr lifecycle with a guarded fake Codex and temporary configuration.
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
