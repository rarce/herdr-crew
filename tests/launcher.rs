//! Launcher and install lifecycle tests. All tools, prefixes and registries are temporary.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::json;

const LAUNCHER: &str = env!("CARGO_BIN_EXE_herdr-crew-launcher");
const CREW: &str = env!("CARGO_BIN_EXE_herdr-crew");
static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Fixture {
    root: PathBuf,
    plugin: PathBuf,
    project: PathBuf,
    prefix: PathBuf,
    registry: PathBuf,
    herdr: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = PathBuf::from("/tmp").join(format!(
            "crew-launcher-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let fixture = Self {
            plugin: root.join("plugin with spaces"),
            project: root.join("project with spaces"),
            prefix: root.join("public bin"),
            registry: root.join("registry.json"),
            herdr: root.join("tools/herdr"),
            root,
        };
        fs::create_dir(&fixture.plugin).unwrap();
        fs::create_dir(&fixture.project).unwrap();
        executable(
            &fixture.herdr,
            "#!/bin/sh\n[ \"$*\" = 'plugin list --plugin herdr-crew --json' ] || exit 99\ncat \"$CREW_TEST_REGISTRY\"\n",
        );
        fixture.register(&fixture.plugin);
        fixture
    }

    fn command(&self, program: impl AsRef<Path>) -> Command {
        let mut command = Command::new(program.as_ref());
        command
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &self.root)
            .env("HERDR_BIN_PATH", &self.herdr)
            .env("CREW_TEST_REGISTRY", &self.registry)
            .current_dir(&self.project);
        command
    }

    fn register(&self, root: &Path) {
        fs::write(
            &self.registry,
            json!({ "result": { "plugins": [{
                "plugin_id": "herdr-crew", "plugin_root": root
            }] } })
            .to_string(),
        )
        .unwrap();
    }

    fn install(&self) -> PathBuf {
        success(
            self.command(LAUNCHER)
                .arg("--install-launcher")
                .arg("--bin-dir")
                .arg(&self.prefix)
                .output()
                .unwrap(),
        );
        self.prefix.join("herdr-crew")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn executable(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn success(output: Output) -> String {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn forwards_arguments_environment_working_directory_and_exit_status() {
    let fixture = Fixture::new();
    executable(
        &fixture.plugin.join("bin/herdr-crew"),
        "#!/bin/sh\nprintf 'cwd=<%s>\n' \"$(pwd -P)\"\nprintf 'env=<%s>\n' \"$CREW_TEST_VALUE\"\nprintf 'arg=<%s>\n' \"$@\"\nexit 23\n",
    );
    let output = fixture
        .command(LAUNCHER)
        .env("CREW_TEST_VALUE", "keep this value")
        .args(["--root", "/repo with spaces", "add", "a'\"$HOME;`pwd`"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(23));
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!(
            "cwd=<{}>\nenv=<keep this value>\narg=<--root>\narg=</repo with spaces>\narg=<add>\narg=<a'\"$HOME;`pwd`>\n",
            fixture.project.canonicalize().unwrap().display()
        )
    );
}

#[test]
fn resolves_the_current_registry_and_prefers_prepared_binary() {
    let fixture = Fixture::new();
    let installed = fixture.install();
    executable(
        &fixture.plugin.join("bin/herdr-crew"),
        "#!/bin/sh\necho first\n",
    );
    executable(
        &fixture.plugin.join("target/release/herdr-crew"),
        "#!/bin/sh\necho legacy\n",
    );
    assert_eq!(
        success(fixture.command(&installed).output().unwrap()),
        "first\n"
    );
    let new_root = fixture.root.join("new plugin");
    executable(
        &new_root.join("bin/herdr-crew"),
        "#!/bin/sh\necho updated\n",
    );
    fixture.register(&new_root);
    assert_eq!(
        success(fixture.command(&installed).output().unwrap()),
        "updated\n"
    );
}

#[test]
fn supports_installed_plugins_with_the_previous_binary_location() {
    let fixture = Fixture::new();
    executable(
        &fixture.plugin.join("target/release/herdr-crew"),
        "#!/bin/sh\necho legacy\n",
    );
    assert_eq!(
        success(fixture.command(LAUNCHER).output().unwrap()),
        "legacy\n"
    );
}

#[test]
fn reports_missing_registration_malformed_output_and_failed_herdr() {
    let fixture = Fixture::new();
    for (response, message) in [
        (r#"{"result":{"plugins":[]}}"#, "plugin not installed"),
        ("not json", "invalid response"),
        (r#"{"result":{}}"#, "plugins array"),
        (
            r#"{"error":{"message":"registry error"}}"#,
            "registry error",
        ),
        (
            r#"{"result":{"plugins":[{"plugin_id":"herdr-crew","plugin_root":"relative"}]}}"#,
            "relative plugin_root",
        ),
    ] {
        fs::write(&fixture.registry, response).unwrap();
        let output = fixture.command(LAUNCHER).output().unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains(message));
    }
    executable(&fixture.herdr, "#!/bin/sh\necho unavailable >&2\nexit 1\n");
    let output = fixture.command(LAUNCHER).output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unavailable"));
}

#[test]
fn missing_binary_and_recursive_registration_fail_without_starting_a_server() {
    let fixture = Fixture::new();
    let output = fixture.command(LAUNCHER).output().unwrap();
    assert!(String::from_utf8_lossy(&output.stderr).contains("no crew binary"));
    fs::create_dir(fixture.plugin.join("bin")).unwrap();
    symlink(LAUNCHER, fixture.plugin.join("bin/herdr-crew")).unwrap();
    let output = fixture.command(LAUNCHER).output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("points back"));
}

#[test]
fn installs_replaces_and_removes_owned_launcher_without_the_plugin() {
    let fixture = Fixture::new();
    let installed = fixture.install();
    assert!(installed.is_file());
    assert_eq!(
        fs::metadata(&installed).unwrap().permissions().mode() & 0o777,
        0o755
    );
    fixture.install();
    fs::remove_file(&fixture.registry).unwrap();
    let output = fixture
        .command(&installed)
        .arg("--uninstall-launcher")
        .output()
        .unwrap();
    success(output);
    assert!(!installed.exists());
    success(
        fixture
            .command(LAUNCHER)
            .arg("--uninstall-launcher")
            .arg("--bin-dir")
            .arg(&fixture.prefix)
            .output()
            .unwrap(),
    );
}

#[test]
fn refuses_to_overwrite_or_remove_unrelated_commands_and_symlinks() {
    let fixture = Fixture::new();
    let destination = fixture.prefix.join("herdr-crew");
    executable(&destination, "#!/bin/sh\necho unrelated\n");
    for operation in ["--install-launcher", "--uninstall-launcher"] {
        let output = fixture
            .command(LAUNCHER)
            .arg(operation)
            .arg("--bin-dir")
            .arg(&fixture.prefix)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("refusing"));
        assert_eq!(
            fs::read_to_string(&destination).unwrap(),
            "#!/bin/sh\necho unrelated\n"
        );
    }
    fs::remove_file(&destination).unwrap();
    symlink(LAUNCHER, &destination).unwrap();
    let output = fixture
        .command(LAUNCHER)
        .args(["--install-launcher", "--bin-dir"])
        .arg(&fixture.prefix)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        fs::symlink_metadata(destination)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn install_directory_defaults_environment_and_explicit_flag_are_respected() {
    let fixture = Fixture::new();
    success(
        fixture
            .command(LAUNCHER)
            .arg("--install-launcher")
            .output()
            .unwrap(),
    );
    assert!(fixture.root.join(".local/bin/herdr-crew").is_file());
    success(
        fixture
            .command(LAUNCHER)
            .env("CREW_BIN_DIR", &fixture.prefix)
            .arg("--install-launcher")
            .output()
            .unwrap(),
    );
    assert!(fixture.prefix.join("herdr-crew").is_file());
    let override_dir = fixture.root.join("override");
    success(
        fixture
            .command(LAUNCHER)
            .env("CREW_BIN_DIR", &fixture.prefix)
            .args(["--install-launcher", "--bin-dir"])
            .arg(&override_dir)
            .output()
            .unwrap(),
    );
    assert!(override_dir.join("herdr-crew").is_file());
}

fn fake_cargo(fixture: &Fixture) -> PathBuf {
    let cargo = fixture.root.join("tools/cargo");
    executable(
        &cargo,
        "#!/bin/sh\n[ \"$*\" = 'build --release --locked --bins --target-dir target' ] || exit 99\nmkdir -p target/release\ncp \"$CREW_TEST_RUNTIME\" target/release/herdr-crew\ncp \"$CREW_TEST_LAUNCHER\" target/release/herdr-crew-launcher\n",
    );
    cargo
}

fn build_command(fixture: &Fixture) -> Command {
    let scripts = fixture.plugin.join("scripts");
    fs::create_dir_all(&scripts).unwrap();
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/build.sh"),
        scripts.join("build.sh"),
    )
    .unwrap();
    fake_cargo(fixture);
    let mut command = fixture.command("/bin/sh");
    command
        .arg(scripts.join("build.sh"))
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", fixture.root.join("tools").display()),
        )
        .env("CREW_TEST_RUNTIME", CREW)
        .env("CREW_TEST_LAUNCHER", LAUNCHER)
        .env("CREW_BIN_DIR", &fixture.prefix);
    command
}

#[test]
fn install_script_survives_checkout_relocation_and_launcher_outlives_uninstall() {
    let fixture = Fixture::new();
    success(
        build_command(&fixture)
            .arg("--install-launcher")
            .output()
            .unwrap(),
    );
    let final_root = fixture.root.join("managed checkout");
    fs::rename(&fixture.plugin, &final_root).unwrap();
    fixture.register(&final_root);
    let installed = fixture.prefix.join("herdr-crew");
    assert_eq!(
        success(
            fixture
                .command(&installed)
                .arg("--version")
                .output()
                .unwrap()
        ),
        format!("herdr-crew {}\n", env!("CARGO_PKG_VERSION"))
    );
    fs::remove_dir_all(final_root).unwrap();
    let output = fixture
        .command(&installed)
        .arg("--version")
        .output()
        .unwrap();
    assert!(!output.status.success());
    success(
        fixture
            .command(&installed)
            .arg("--uninstall-launcher")
            .output()
            .unwrap(),
    );
}

#[test]
fn plugin_install_rejects_relative_or_empty_bin_directory_before_building() {
    for directory in ["tools/bin", ""] {
        let fixture = Fixture::new();
        let output = build_command(&fixture)
            .arg("--install-launcher")
            .env("CREW_BIN_DIR", directory)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("CREW_BIN_DIR must be an absolute path")
        );
        assert!(!fixture.plugin.join("target").exists());
        assert!(!fixture.plugin.join("bin").exists());
        assert!(!fixture.plugin.join("tools/bin").exists());
        assert!(!fixture.prefix.exists());
    }
}

#[test]
fn local_build_does_not_install_public_launcher_and_failed_build_keeps_previous_files() {
    let fixture = Fixture::new();
    success(build_command(&fixture).output().unwrap());
    assert!(fixture.plugin.join("bin/herdr-crew").is_file());
    assert!(!fixture.prefix.exists());
    let installed = fixture.install();
    let previous = fs::read(&installed).unwrap();
    let mut command = build_command(&fixture);
    executable(&fixture.root.join("tools/cargo"), "#!/bin/sh\nexit 7\n");
    let output = command.arg("--install-launcher").output().unwrap();
    assert_eq!(output.status.code(), Some(7));
    assert_eq!(fs::read(installed).unwrap(), previous);
}

#[test]
#[ignore = "needs CREW_REAL_HERDR=1 and herdr on PATH; uses only an isolated offline registry"]
fn real_herdr_resolves_linked_plugin_without_a_server() {
    assert_eq!(std::env::var("CREW_REAL_HERDR").as_deref(), Ok("1"));
    let real_herdr = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|directory| directory.join("herdr"))
        .find(|path| path.is_file())
        .expect("herdr on PATH");
    let fixture = Fixture::new();
    success(build_command(&fixture).output().unwrap());
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("herdr-plugin.toml"),
        fixture.plugin.join("herdr-plugin.toml"),
    )
    .unwrap();
    let socket = fixture.root.join("offline.sock");
    let mut link = fixture.command(&real_herdr);
    link.env("XDG_CONFIG_HOME", fixture.root.join("config"))
        .env("XDG_STATE_HOME", fixture.root.join("state"))
        .env("HERDR_SOCKET_PATH", &socket)
        .args(["plugin", "link"])
        .arg(&fixture.plugin);
    success(link.output().unwrap());
    let installed = fixture.install();
    let output = fixture
        .command(installed)
        .env("HERDR_BIN_PATH", real_herdr)
        .env("XDG_CONFIG_HOME", fixture.root.join("config"))
        .env("XDG_STATE_HOME", fixture.root.join("state"))
        .env("HERDR_SOCKET_PATH", &socket)
        .arg("--version")
        .output()
        .unwrap();
    assert_eq!(
        success(output),
        format!("herdr-crew {}\n", env!("CARGO_PKG_VERSION"))
    );
    assert!(!socket.exists(), "no server should have been started");
}
