//! Bounded local CLI inspection. Drop always stops and reaps the inspection process.

use std::ffi::OsStr;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use serde_json::Value;

const MAX_OUTPUT: usize = 2 * 1024 * 1024;
pub struct Inspector {
    child: Child,
    input: ChildStdin,
    lines: Receiver<Result<String, String>>,
    stderr: Receiver<String>,
    deadline: Instant,
}

impl Inspector {
    pub fn spawn(command: &mut Command) -> Result<Self, String> {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("could not start local inspection: {e}"))?;
        let input = child.stdin.take().ok_or("inspection has no stdin")?;
        let stdout = child.stdout.take().ok_or("inspection has no stdout")?;
        let mut stderr = child.stderr.take().ok_or("inspection has no stderr")?;
        let (error_sender, errors) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let mut text = String::new();
            let _ = stderr.by_ref().take(8 * 1024).read_to_string(&mut text);
            let _ = error_sender.send(text);
            let _ = std::io::copy(&mut stderr, &mut std::io::sink());
        });
        let (sender, lines) = mpsc::sync_channel(64);
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut total = 0;
            loop {
                let mut line = Vec::new();
                let result = reader
                    .by_ref()
                    .take((MAX_OUTPUT + 1) as u64)
                    .read_until(b'\n', &mut line);
                match result {
                    Ok(0) => break,
                    Ok(_) => {
                        total += line.len();
                        if total > MAX_OUTPUT {
                            let _ =
                                sender.send(Err("local inspection output exceeded 2 MiB".into()));
                            break;
                        }
                        if sender
                            .send(String::from_utf8(line).map_err(|e| e.to_string()))
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(e) => {
                        let _ = sender.send(Err(e.to_string()));
                        break;
                    }
                }
            }
        });
        Ok(Self {
            child,
            input,
            lines,
            stderr: errors,
            deadline: Instant::now() + Duration::from_secs(10),
        })
    }

    pub fn send(&mut self, value: Value) -> Result<(), String> {
        if let Err(error) = writeln!(self.input, "{value}").and_then(|()| self.input.flush()) {
            return Err(format!("{error}: {}", self.failure()));
        }
        Ok(())
    }

    fn left(&self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }

    pub fn line(&mut self) -> Result<String, String> {
        match self.lines.recv_timeout(self.left()) {
            Ok(result) => result,
            Err(_) => Err(self.failure()),
        }
    }

    fn failure(&mut self) -> String {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let detail = self
            .stderr
            .recv_timeout(Duration::from_millis(100))
            .unwrap_or_default();
        if detail.trim().is_empty() {
            "local inspection ended or timed out after 10 s".into()
        } else {
            format!("local inspection failed: {}", detail.trim())
        }
    }

    pub fn response(&mut self, id: u64) -> Result<Value, String> {
        loop {
            let value: Value = serde_json::from_str(&self.line()?)
                .map_err(|e| format!("invalid inspection response: {e}"))?;
            if value["id"].as_u64() == Some(id) {
                if let Some(error) = value.get("error") {
                    return Err(format!("inspection rejected the request: {error}"));
                }
                return value
                    .get("result")
                    .cloned()
                    .ok_or("inspection response has no result".into());
            }
        }
    }
}

impl Drop for Inspector {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The first `X.Y.Z` that `<program> --version` prints, within the inspection deadline: on the
/// first stdout line or, when stdout closes empty and the program exits successfully, on stderr
/// (pi prints its version there).
pub fn detect(program: impl AsRef<OsStr>) -> Result<(u32, u32, u32), String> {
    let name = program.as_ref().to_string_lossy().into_owned();
    let mut process = Inspector::spawn(Command::new(program.as_ref()).arg("--version"))?;
    let line = match process.lines.recv_timeout(process.left()) {
        Ok(result) => result?,
        Err(RecvTimeoutError::Timeout) => return Err(process.failure()),
        Err(RecvTimeoutError::Disconnected) => {
            let status = loop {
                match process.child.try_wait() {
                    Ok(Some(status)) => break status,
                    Ok(None) if !process.left().is_zero() => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    _ => return Err(process.failure()),
                }
            };
            let stderr = process
                .stderr
                .recv_timeout(process.left())
                .unwrap_or_default();
            if !status.success() {
                return Err(format!("`--version` failed ({status}): {}", stderr.trim()));
            }
            stderr
        }
    };
    line.split_whitespace()
        .find_map(|word| {
            let parts: Vec<_> = word.split('.').map(str::parse::<u32>).collect();
            match parts.as_slice() {
                [Ok(a), Ok(b), Ok(c)] => Some((*a, *b, *c)),
                _ => None,
            }
        })
        .ok_or_else(|| format!("cannot determine {name} version"))
}

pub fn version(program: &str, minimum: (u32, u32, u32)) -> Result<(), String> {
    if detect(program)? < minimum {
        return Err(format!(
            "{program} {}.{}.{} or newer is required",
            minimum.0, minimum.1, minimum.2
        ));
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn script(name: &str, body: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("crew-detect-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tool");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn versions_come_from_stdout_or_from_stderr_of_a_successful_run() {
        assert_eq!(detect(script("out", "echo 'tool 0.9.3'")), Ok((0, 9, 3)));
        assert_eq!(detect(script("err", "echo 1.1.0 >&2")), Ok((1, 1, 0)));
    }

    #[test]
    fn a_failing_run_is_not_read_as_a_version() {
        let error =
            detect(script("fail", "echo 'requires Node.js 20.6.0' >&2; exit 1")).unwrap_err();
        assert!(error.contains("requires Node.js 20.6.0"), "{error}");
    }
}
