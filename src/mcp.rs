//! Zero-daemon JSON-RPC 2.0 MCP server over newline-delimited `stdio` (`herdr-crew mem mcp`).
//!
//! Exposes read/write tools over the project's out-of-repo operational `llmwiki` memory store
//! without requiring any background daemon or extra dependencies.

use std::io::{self, BufRead, Read, Write};
use std::path::PathBuf;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use serde_json::{Value, json};

use crate::config::Config;
use crate::memory;

const SUPPORTED_PROTOCOL_VERSIONS: [&str; 3] = ["2024-11-05", "2025-03-26", "2025-06-18"];
const LATEST_PROTOCOL_VERSION: &str = "2025-06-18";
const MAX_LINE_BYTES: usize = 4 * 1024 * 1024;
const ERR_PARSE: i64 = -32700;
const ERR_INVALID_REQUEST: i64 = -32600;
const ERR_METHOD_NOT_FOUND: i64 = -32601;
const ERR_INVALID_PARAMS: i64 = -32602;

/// Initializes `<dir>` if absent (with `--write`) and runs the newline-delimited JSON-RPC 2.0 MCP
/// server on stdio. Read-only by default: without `--write` it never creates or modifies files.
/// `--write` is a guardrail against accidental writes, not access control: any process that can
/// run `mem mcp --write` (or edit the directory, which `--add-dir` allows) can write.
pub fn serve(config: &Config, dir: PathBuf, write: bool) -> Result<(), String> {
    let read_only = !write;
    eprintln!(
        "herdr-crew: memory MCP server for {} ({})",
        dir.display(),
        if write {
            "read-write: --write"
        } else {
            "read-only; pass --write to enable mem_status_update and mem_upsert"
        }
    );
    let tz = TimeZone::system();
    let stdin = io::stdin();
    let stdout = io::stdout();
    serve_with_io(config, dir, read_only, tz, stdin.lock(), stdout.lock())
}

fn serve_with_io<R: BufRead, W: Write>(
    config: &Config,
    dir: PathBuf,
    read_only: bool,
    tz: TimeZone,
    reader: R,
    writer: W,
) -> Result<(), String> {
    if !read_only {
        let now = Timestamp::now()
            .to_zoned(tz.clone())
            .strftime("%Y-%m-%dT%H:%M:%S%:z")
            .to_string();
        memory::ensure_initialized(&dir, config, &now, memory::LOCK_WAIT)?;
    }
    serve_io(config, dir, read_only, tz, reader, writer)
}

/// Pure `BufRead` + `Write` JSON-RPC 2.0 loop for both live `stdio` and in-memory unit tests.
pub fn serve_io<R: BufRead, W: Write>(
    config: &Config,
    dir: PathBuf,
    read_only: bool,
    tz: TimeZone,
    reader: R,
    writer: W,
) -> Result<(), String> {
    serve_io_with_limit(config, dir, read_only, tz, reader, writer, MAX_LINE_BYTES)
}

enum BoundedLine {
    Eof,
    Line(String),
    TooLong,
    InvalidUtf8(String),
}

fn read_bounded_line<R: BufRead>(reader: &mut R, max_bytes: usize) -> Result<BoundedLine, String> {
    let mut buf = Vec::new();
    let n = reader
        .by_ref()
        .take((max_bytes + 1) as u64)
        .read_until(b'\n', &mut buf)
        .map_err(|e| format!("failed reading MCP input: {e}"))?;
    if n == 0 {
        return Ok(BoundedLine::Eof);
    }
    if buf.len() > max_bytes {
        if buf.last() != Some(&b'\n') {
            loop {
                let chunk = reader
                    .fill_buf()
                    .map_err(|e| format!("failed reading MCP input: {e}"))?;
                if chunk.is_empty() {
                    break;
                }
                if let Some(pos) = chunk.iter().position(|&b| b == b'\n') {
                    reader.consume(pos + 1);
                    break;
                }
                let len = chunk.len();
                reader.consume(len);
            }
        }
        return Ok(BoundedLine::TooLong);
    }
    match String::from_utf8(buf) {
        Ok(line) => Ok(BoundedLine::Line(line)),
        Err(e) => Ok(BoundedLine::InvalidUtf8(e.to_string())),
    }
}

fn serve_io_with_limit<R: BufRead, W: Write>(
    config: &Config,
    dir: PathBuf,
    read_only: bool,
    tz: TimeZone,
    mut reader: R,
    mut writer: W,
    max_line_bytes: usize,
) -> Result<(), String> {
    let mut server = Server {
        config,
        dir,
        read_only,
        tz,
    };
    loop {
        let response = match read_bounded_line(&mut reader, max_line_bytes)? {
            BoundedLine::Eof => break,
            BoundedLine::TooLong => Some(json_rpc_error(
                Value::Null,
                ERR_PARSE,
                "request line exceeds maximum size".into(),
            )),
            BoundedLine::InvalidUtf8(e) => Some(json_rpc_error(
                Value::Null,
                ERR_PARSE,
                format!("Parse error: invalid UTF-8: {e}"),
            )),
            BoundedLine::Line(line) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                match serde_json::from_str::<Value>(trimmed) {
                    Ok(req) => server.handle_message(req),
                    Err(e) => Some(json_rpc_error(
                        Value::Null,
                        ERR_PARSE,
                        format!("Parse error: {e}"),
                    )),
                }
            }
        };
        if let Some(resp) = response {
            let encoded = serde_json::to_string(&resp).map_err(|e| e.to_string())?;
            writeln!(writer, "{encoded}").map_err(|e| format!("failed writing MCP output: {e}"))?;
            writer
                .flush()
                .map_err(|e| format!("failed flushing MCP output: {e}"))?;
        }
    }
    Ok(())
}

struct Server<'a> {
    config: &'a Config,
    dir: PathBuf,
    read_only: bool,
    tz: TimeZone,
}

impl Server<'_> {
    fn handle_message(&mut self, req: Value) -> Option<Value> {
        let Some(obj) = req.as_object() else {
            return Some(json_rpc_error(
                Value::Null,
                ERR_INVALID_REQUEST,
                "Invalid Request: expected JSON object".into(),
            ));
        };
        // JSON-RPC notifications have no `id` and must not receive a response.
        let id = obj.get("id")?.clone();
        let Some(method) = obj.get("method").and_then(Value::as_str) else {
            return Some(json_rpc_error(
                id,
                ERR_INVALID_REQUEST,
                "Invalid Request: missing string method".into(),
            ));
        };
        let params = obj.get("params").cloned().unwrap_or_else(|| json!({}));

        match method {
            "initialize" => {
                // Answer with the client's version when supported, else the newest we speak.
                let version = params
                    .get("protocolVersion")
                    .and_then(Value::as_str)
                    .filter(|v| SUPPORTED_PROTOCOL_VERSIONS.contains(v))
                    .unwrap_or(LATEST_PROTOCOL_VERSION);
                Some(json_rpc_ok(
                    id,
                    json!({
                        "protocolVersion": version,
                        "capabilities": { "tools": {} },
                        "serverInfo": {
                            "name": "herdr-crew-memory",
                            "version": env!("CARGO_PKG_VERSION"),
                        },
                    }),
                ))
            }
            "ping" => Some(json_rpc_ok(id, json!({}))),
            "tools/list" => Some(json_rpc_ok(
                id,
                json!({ "tools": tool_specs(self.read_only) }),
            )),
            "tools/call" => Some(self.handle_tool_call(id, params)),
            other => Some(json_rpc_error(
                id,
                ERR_METHOD_NOT_FOUND,
                format!("Method not found: {other}"),
            )),
        }
    }

    fn handle_tool_call(&mut self, id: Value, params: Value) -> Value {
        let Some(obj) = params.as_object() else {
            return json_rpc_error(
                id,
                ERR_INVALID_PARAMS,
                "Invalid params: expected object".into(),
            );
        };
        let Some(name) = obj.get("name").and_then(Value::as_str) else {
            return json_rpc_error(
                id,
                ERR_INVALID_PARAMS,
                "Invalid params: missing tool name".into(),
            );
        };
        let args = match obj.get("arguments") {
            None | Some(Value::Null) => json!({}),
            Some(v) if v.is_object() => v.clone(),
            Some(_) => {
                return json_rpc_error(
                    id,
                    ERR_INVALID_PARAMS,
                    "Invalid params: arguments must be an object".into(),
                );
            }
        };

        let result = match name {
            "mem_search" => self.tool_search(&args),
            "mem_timeline" => self.tool_timeline(&args),
            "mem_get" => self.tool_get(&args),
            "mem_status_update" => {
                if self.read_only {
                    Err(
                        "write tool mem_status_update is disabled: the server is read-only; \
                         run it as `herdr-crew mem mcp --write` (for the board writer)"
                            .into(),
                    )
                } else {
                    self.tool_status_update(&args)
                }
            }
            "mem_upsert" => {
                if self.read_only {
                    Err(
                        "write tool mem_upsert is disabled: the server is read-only; \
                         run it as `herdr-crew mem mcp --write` (for the board writer)"
                            .into(),
                    )
                } else {
                    self.tool_upsert(&args)
                }
            }
            unknown => {
                return json_rpc_error(id, ERR_INVALID_PARAMS, format!("Unknown tool: {unknown}"));
            }
        };

        match result {
            Ok(text) => json_rpc_ok(
                id,
                json!({
                    "content": [{ "type": "text", "text": text }]
                }),
            ),
            Err(err) => json_rpc_ok(
                id,
                json!({
                    "content": [{ "type": "text", "text": err }],
                    "isError": true
                }),
            ),
        }
    }

    fn tool_search(&self, args: &Value) -> Result<String, String> {
        let query = require_str(args, "query")?;
        if query.trim().is_empty() {
            return Err("query must not be empty".into());
        }
        let limit = optional_limit(args, "limit", 20, 100)?;
        let hits = memory::search(&self.dir, query)?;
        if hits.is_empty() {
            return Ok("No matches found.".into());
        }
        Ok(hits.into_iter().take(limit).collect::<Vec<_>>().join("\n"))
    }

    fn tool_timeline(&self, args: &Value) -> Result<String, String> {
        let limit = optional_limit(args, "limit", 10, 100)?;
        let around = optional_str(args, "around")?;
        let text = memory::timeline_around(&self.dir, limit, around, &self.tz)?;
        if text.is_empty() {
            Ok("No matching timeline entries.".into())
        } else {
            Ok(text)
        }
    }

    fn tool_get(&self, args: &Value) -> Result<String, String> {
        let page = require_str(args, "page")?;
        memory::show(&self.dir, page)
    }

    fn tool_status_update(&self, args: &Value) -> Result<String, String> {
        let sessions = args
            .get("sessions")
            .filter(|v| v.is_array())
            .cloned()
            .ok_or_else(|| "sessions must be an array".to_string())?;
        let owner = args
            .get("owner")
            .filter(|v| v.is_object())
            .cloned()
            .ok_or_else(|| "owner must be an object".to_string())?;
        let recent = match args.get("recent") {
            None | Some(Value::Null) => None,
            Some(v) if v.is_array() => Some(v.clone()),
            Some(_) => return Err("recent must be an array".into()),
        };
        let focus = optional_str(args, "focus")?;
        let updated_at = optional_str(args, "updatedAt")?;
        let count = memory::update_status(
            &self.dir,
            self.config,
            sessions,
            owner,
            recent,
            focus,
            updated_at,
            &self.tz,
        )?;
        Ok(format!(
            "Updated {} ({count} sessions).",
            self.dir.join("now.md").display()
        ))
    }

    fn tool_upsert(&self, args: &Value) -> Result<String, String> {
        let page = require_str(args, "page")?;
        let content = require_str(args, "content")?;
        let summary = optional_str(args, "summary")?;
        let log_type = optional_str(args, "log_type")?;
        let log_title = optional_str(args, "log_title")?;
        let res = memory::upsert_page(
            &self.dir,
            self.config,
            page,
            content,
            summary,
            log_type,
            log_title,
            &self.tz,
        )?;
        let mut lines = vec![format!("Upserted {}", res.path)];
        if res.index_updated {
            lines.push("Updated index.md".into());
        }
        if res.log_updated {
            lines.push("Prepended log.md".into());
        }
        if res.lint.is_ok() {
            lines.push(format!(
                "Lint ok ({} pages, {} links, {} log entries)",
                res.lint.pages_checked, res.lint.links_checked, res.lint.log_entries_checked
            ));
        } else {
            lines.push(format!("Lint issues ({}):", res.lint.issues.len()));
            for issue in res.lint.issues {
                lines.push(format!("- {issue}"));
            }
        }
        Ok(lines.join("\n"))
    }
}

fn require_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing required string argument \"{key}\""))
}

fn optional_str<'a>(args: &'a Value, key: &str) -> Result<Option<&'a str>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.as_str())),
        Some(_) => Err(format!("argument \"{key}\" must be a string")),
    }
}

fn optional_limit(args: &Value, key: &str, default: usize, max: usize) -> Result<usize, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(v) => {
            let n = v
                .as_u64()
                .ok_or_else(|| format!("argument \"{key}\" must be a positive integer"))?;
            if n == 0 {
                return Err(format!("argument \"{key}\" must be at least 1"));
            }
            Ok(usize::try_from(n).unwrap_or(max).min(max))
        }
    }
}

fn json_rpc_ok(id: Value, result: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    })
}

fn json_rpc_error(id: Value, code: i64, message: String) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message,
        },
    })
}

fn tool_specs(read_only: bool) -> Vec<Value> {
    let mut tools = vec![
        json!({
            "name": "mem_search",
            "description": "Case-insensitive search across operational memory pages (skipping SCHEMA.md).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Non-empty search query" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 100, "default": 20 }
                },
                "required": ["query"]
            }
        }),
        json!({
            "name": "mem_timeline",
            "description": "Read chronological entries from log.md (newest first, or centered around a match).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "limit": { "type": "integer", "minimum": 1, "maximum": 100, "default": 10 },
                    "around": { "type": "string", "description": "Optional substring to center timeline results around" }
                }
            }
        }),
        json!({
            "name": "mem_get",
            "description": "Read a single operational memory page (e.g. now, index, log, tasks/issue-33).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "page": { "type": "string", "description": "Relative page name inside the memory directory" }
                },
                "required": ["page"]
            }
        }),
    ];
    if !read_only {
        tools.push(json!({
            "name": "mem_status_update",
            "description": "Validate and atomically rewrite now.md YAML frontmatter (and optional Markdown focus body).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "sessions": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "role": { "type": "string" },
                                "state": { "type": "string", "enum": ["working", "waiting", "idle", "blocked"] },
                                "now": { "type": "string" },
                                "next": { "type": "array", "items": { "type": "string" } },
                                "note": { "type": "string" },
                                "updatedAt": { "type": "string" }
                            },
                            "required": ["role", "state", "now", "next"]
                        }
                    },
                    "owner": {
                        "type": "object",
                        "properties": {
                            "urgent": { "type": "array", "items": { "type": "string" } },
                            "beforeMain": { "type": "array", "items": { "type": "string" } },
                            "optional": { "type": "array", "items": { "type": "string" } }
                        },
                        "required": ["urgent", "beforeMain", "optional"]
                    },
                    "recent": {
                        "type": "array",
                        "description": "Optional curated Recent box; when omitted the board shows the newest log.md entries",
                        "items": {
                            "type": "object",
                            "properties": {
                                "at": { "type": "string", "description": "RFC 3339 timestamp with offset" },
                                "text": { "type": "string" }
                            },
                            "required": ["at", "text"]
                        }
                    },
                    "focus": { "type": "string", "description": "Optional Markdown body below frontmatter; preserves the existing body when omitted, and an empty string clears it" },
                    "updatedAt": { "type": "string", "description": "Optional RFC 3339 timestamp with offset; defaults to now" }
                },
                "required": ["sessions", "owner"]
            }
        }));
        tools.push(json!({
            "name": "mem_upsert",
            "description": "Atomically create/update a page under tasks/, decisions/, or notes/, update index.md, optionally prepend log.md, and run lint.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "page": { "type": "string", "description": "Relative path under tasks/, decisions/, or notes/ (e.g. tasks/issue-36)" },
                    "content": { "type": "string", "description": "Full Markdown content for the page" },
                    "summary": { "type": "string", "description": "Optional 1-line summary for index.md" },
                    "log_type": { "type": "string", "description": "Optional log entry type (must pair with log_title)" },
                    "log_title": { "type": "string", "description": "Optional log entry title (must pair with log_type)" }
                },
                "required": ["page", "content"]
            }
        }));
    }
    tools
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Cursor;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::config::tests::basic;

    static NEXT: AtomicUsize = AtomicUsize::new(0);

    fn temp_mem(tag: &str) -> PathBuf {
        let dir = PathBuf::from("/tmp").join(format!(
            "crew-mcp-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn run_session(config: &Config, dir: &Path, read_only: bool, lines: &[&str]) -> Vec<Value> {
        let input = format!("{}\n", lines.join("\n"));
        let mut output = Vec::new();
        let tz = TimeZone::fixed(jiff::tz::offset(-3));
        serve_io(
            config,
            dir.to_path_buf(),
            read_only,
            tz,
            Cursor::new(input.into_bytes()),
            &mut output,
        )
        .unwrap();
        let text = String::from_utf8(output).unwrap();
        text.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str::<Value>(l).unwrap())
            .collect()
    }

    #[test]
    fn mcp_lifecycle_tools_errors_and_read_only_mode() {
        let root = temp_mem("lifecycle");
        let mem = root.join("store");
        let c = basic();
        memory::ensure_initialized(&mem, &c, "2026-11-26T09:00:00-03:00", memory::LOCK_WAIT)
            .unwrap();

        let responses = run_session(
            &c,
            &mem,
            false,
            &[
                r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}"#,
                r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
                r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#,
                r#"{"jsonrpc":"2.0","id":3,"method":"tools/list"}"#,
                r#"{not json"#,
                r#"{"jsonrpc":"2.0","id":4,"method":"unknown/method"}"#,
                r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"does_not_exist","arguments":{}}}"#,
            ],
        );

        // Notification produces no response -> 6 responses total.
        assert_eq!(responses.len(), 6, "{responses:?}");
        assert_eq!(responses[0]["id"], 1);
        assert_eq!(responses[0]["result"]["protocolVersion"], "2025-03-26");
        // An unknown client version gets the newest supported one, not an echo.
        let negotiated = run_session(
            &c,
            &mem,
            true,
            &[
                r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"1999-01-01"}}"#,
            ],
        );
        assert_eq!(
            negotiated[0]["result"]["protocolVersion"],
            LATEST_PROTOCOL_VERSION
        );
        assert_eq!(
            responses[0]["result"]["serverInfo"]["name"],
            "herdr-crew-memory"
        );
        assert_eq!(responses[1]["id"], 2);
        assert_eq!(responses[1]["result"], json!({}));

        let rw_tools: Vec<&str> = responses[2]["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            rw_tools,
            [
                "mem_search",
                "mem_timeline",
                "mem_get",
                "mem_status_update",
                "mem_upsert"
            ]
        );
        assert_eq!(responses[3]["error"]["code"], ERR_PARSE);
        assert_eq!(responses[4]["id"], 4);
        assert_eq!(responses[4]["error"]["code"], ERR_METHOD_NOT_FOUND);
        assert_eq!(responses[5]["id"], 5);
        assert_eq!(responses[5]["error"]["code"], ERR_INVALID_PARAMS);

        // Read-only mode lists only the 3 read tools and rejects write tools.
        let ro_responses = run_session(
            &c,
            &mem,
            true,
            &[
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
                r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"mem_status_update","arguments":{"sessions":[],"owner":{"urgent":[],"beforeMain":[],"optional":[]}}}}"#,
                r##"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"mem_upsert","arguments":{"page":"tasks/x","content":"# X"}}}"##,
            ],
        );
        let ro_tools: Vec<&str> = ro_responses[0]["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(ro_tools, ["mem_search", "mem_timeline", "mem_get"]);
        assert_eq!(ro_responses[1]["result"]["isError"], true);
        assert!(
            ro_responses[1]["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("mem mcp --write")
        );
        assert_eq!(ro_responses[2]["result"]["isError"], true);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn mcp_status_update_upsert_search_timeline_and_get_round_trip() {
        let root = temp_mem("roundtrip");
        let mem = root.join("store");
        let c = basic();
        let tz = TimeZone::fixed(jiff::tz::offset(-3));
        memory::ensure_initialized(&mem, &c, "2026-11-26T09:00:00-03:00", memory::LOCK_WAIT)
            .unwrap();

        let status_update = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "mem_status_update",
                "arguments": {
                    "updatedAt": "2026-11-26T09:15:00-03:00",
                    "sessions": [
                        {
                            "role": "acme-lead",
                            "state": "working",
                            "now": "Review: PR #42 'dev's fix' [ready]",
                            "next": ["Check: {edge} #cases", "Merge"],
                            "note": "true"
                        }
                    ],
                    "owner": {
                        "urgent": ["Approve: PR #42"],
                        "beforeMain": [],
                        "optional": []
                    }
                }
            }
        });
        let bad_status = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "mem_status_update",
                "arguments": {
                    "sessions": [
                        {
                            "role": "unknown-role",
                            "state": "working",
                            "now": "Broken",
                            "next": []
                        }
                    ],
                    "owner": { "urgent": [], "beforeMain": [], "optional": [] }
                }
            }
        });

        let r1 = run_session(
            &c,
            &mem,
            false,
            &[&status_update.to_string(), &bad_status.to_string()],
        );
        assert!(r1[0]["result"].get("isError").is_none(), "{:?}", r1[0]);
        assert_eq!(r1[1]["result"]["isError"], true, "{:?}", r1[1]);

        // `now.md` was written cleanly by request 1, preserved body, and was NOT corrupted by request 2.
        let now_bytes = fs::read(mem.join("now.md")).unwrap();
        let loaded = crate::board::model::load_markdown(&now_bytes, None, &c, &tz).unwrap();
        assert_eq!(loaded.sessions.len(), 1);
        assert_eq!(loaded.sessions[0].now, "Review: PR #42 'dev's fix' [ready]");
        assert_eq!(loaded.sessions[0].next, ["Check: {edge} #cases", "Merge"]);
        assert_eq!(loaded.sessions[0].note.as_deref(), Some("true"));
        assert!(
            String::from_utf8(now_bytes)
                .unwrap()
                .contains("See [[index]]")
        );

        // Upsert page twice (verify idempotency in index.md), prepend log, reject bad targets.
        let upsert1 = json!({
            "jsonrpc": "2.0",
            "id": 10,
            "method": "tools/call",
            "params": {
                "name": "mem_upsert",
                "arguments": {
                    "page": "tasks/issue-36",
                    "content": "# Issue 36\n\nMCP server needle keyword.\n",
                    "summary": "Initial summary",
                    "log_type": "task",
                    "log_title": "Started issue 36"
                }
            }
        });
        let upsert2 = json!({
            "jsonrpc": "2.0",
            "id": 11,
            "method": "tools/call",
            "params": {
                "name": "mem_upsert",
                "arguments": {
                    "page": "tasks/issue-36",
                    "content": "# Issue 36\n\nUpdated MCP server needle keyword.\n",
                    "summary": "Updated summary"
                }
            }
        });
        let bad_root = json!({
            "jsonrpc": "2.0",
            "id": 12,
            "method": "tools/call",
            "params": {
                "name": "mem_upsert",
                "arguments": { "page": "now", "content": "# Nope" }
            }
        });
        let bad_traversal = json!({
            "jsonrpc": "2.0",
            "id": 13,
            "method": "tools/call",
            "params": {
                "name": "mem_upsert",
                "arguments": { "page": "tasks/../../escape", "content": "# Nope" }
            }
        });
        let search_req = json!({
            "jsonrpc": "2.0",
            "id": 14,
            "method": "tools/call",
            "params": {
                "name": "mem_search",
                "arguments": { "query": "needle", "limit": 5 }
            }
        });
        let timeline_req = json!({
            "jsonrpc": "2.0",
            "id": 15,
            "method": "tools/call",
            "params": {
                "name": "mem_timeline",
                "arguments": { "limit": 2, "around": "issue 36" }
            }
        });
        let get_req = json!({
            "jsonrpc": "2.0",
            "id": 16,
            "method": "tools/call",
            "params": {
                "name": "mem_get",
                "arguments": { "page": "tasks/issue-36" }
            }
        });

        let r2 = run_session(
            &c,
            &mem,
            false,
            &[
                &upsert1.to_string(),
                &upsert2.to_string(),
                &bad_root.to_string(),
                &bad_traversal.to_string(),
                &search_req.to_string(),
                &timeline_req.to_string(),
                &get_req.to_string(),
            ],
        );

        assert!(r2[0]["result"].get("isError").is_none(), "{:?}", r2[0]);
        assert!(
            r2[0]["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("Lint ok"),
            "{:?}",
            r2[0]
        );
        assert!(r2[1]["result"].get("isError").is_none(), "{:?}", r2[1]);
        let index_text = fs::read_to_string(mem.join("index.md")).unwrap();
        assert_eq!(
            index_text.matches("[[tasks/issue-36]]").count(),
            1,
            "{index_text}"
        );
        assert!(index_text.contains("Updated summary"), "{index_text}");
        assert!(!index_text.contains("Initial summary"), "{index_text}");

        assert_eq!(r2[2]["result"]["isError"], true);
        assert_eq!(r2[3]["result"]["isError"], true);

        let search_text = r2[4]["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            search_text.contains("tasks/issue-36.md:3:"),
            "{search_text}"
        );

        let timeline_text = r2[5]["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            timeline_text.contains("task | Started issue 36"),
            "{timeline_text}"
        );

        let get_text = r2[6]["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            get_text.contains("Updated MCP server needle keyword."),
            "{get_text}"
        );

        let report = memory::lint(&mem, &c, &tz);
        assert!(report.is_ok(), "{:?}", report.issues);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn read_only_startup_does_not_create_files_and_oversized_lines_return_parse_error() {
        let root = temp_mem("ro-and-limit");
        let mem = root.join("uninitialized-store");
        let c = basic();
        let tz = TimeZone::fixed(jiff::tz::offset(-3));

        // 1. `--read-only` startup must not create `mem` or any files inside it.
        let input = concat!(
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"mem_search","arguments":{"query":"anything"}}}"#,
            "\n",
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"mem_get","arguments":{"page":"index"}}}"#,
            "\n",
        );
        let mut out = Vec::new();
        serve_with_io(
            &c,
            mem.clone(),
            true,
            tz.clone(),
            Cursor::new(input.as_bytes()),
            &mut out,
        )
        .unwrap();
        assert!(!mem.exists(), "read-only serve must not create memory dir");
        let responses: Vec<Value> = String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(responses.len(), 2);
        assert_eq!(
            responses[0]["result"]["content"][0]["text"],
            "No matches found."
        );
        assert_eq!(responses[1]["result"]["isError"], true);

        // 2. Oversized line is discarded cleanly and returns -32700 without breaking next request.
        let oversized = format!("{{\"padding\":\"{}\"}}\n", "a".repeat(200));
        let next = "{\"jsonrpc\":\"2.0\",\"id\":9,\"method\":\"ping\"}\n";
        let combined = format!("{oversized}{next}");
        let mut out2 = Vec::new();
        serve_io_with_limit(
            &c,
            mem.clone(),
            true,
            tz,
            Cursor::new(combined.into_bytes()),
            &mut out2,
            64,
        )
        .unwrap();
        let res2: Vec<Value> = String::from_utf8(out2)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(res2.len(), 2, "{res2:?}");
        assert_eq!(res2[0]["error"]["code"], ERR_PARSE);
        assert!(
            res2[0]["error"]["message"]
                .as_str()
                .unwrap()
                .contains("maximum size")
        );
        assert_eq!(res2[1]["id"], 9);
        assert_eq!(res2[1]["result"], json!({}));

        let _ = fs::remove_dir_all(&root);
    }
}
