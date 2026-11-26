//! Zero-dependency YAML frontmatter extractor and subset parser for `now.md`. Pure.

use serde_json::{Map, Number, Value};

/// Leading `---\n...\n---` frontmatter line with its 1-based line number and indentation.
#[derive(Debug, Clone)]
struct Line<'a> {
    line_no: usize,
    indent: usize,
    text: &'a str,
}

/// Extracts and parses a leading YAML frontmatter block (`---\n...\n---`) from `markdown`.
/// Returns the parsed frontmatter as a `serde_json::Value` and the remaining Markdown body.
pub fn parse_frontmatter(markdown: &str) -> Result<(Value, &str), String> {
    let source = markdown.strip_prefix('\u{feff}').unwrap_or(markdown);
    let Some((first_line, rest_after_open)) = split_first_line(source) else {
        return Err("line 1: missing opening YAML frontmatter delimiter '---'".into());
    };
    if first_line.trim_end_matches([' ', '\t']) != "---" {
        return Err("line 1: missing opening YAML frontmatter delimiter '---'".into());
    }

    let mut offset = 0usize;
    let mut line_no = 2usize;
    let mut raw_lines: Vec<(usize, &str)> = Vec::new();
    let mut body: Option<&str> = None;

    while offset <= rest_after_open.len() {
        let slice = &rest_after_open[offset..];
        if slice.is_empty() {
            break;
        }
        let (line, consumed) = match slice.find('\n') {
            Some(pos) => {
                let raw = &slice[..pos];
                let trimmed = raw.strip_suffix('\r').unwrap_or(raw);
                (trimmed, pos + 1)
            }
            None => (slice.strip_suffix('\r').unwrap_or(slice), slice.len()),
        };
        let marker = line.trim_end_matches([' ', '\t']);
        if marker == "---" || marker == "..." {
            body = Some(&rest_after_open[offset + consumed..]);
            break;
        }
        raw_lines.push((line_no, line));
        offset += consumed;
        line_no += 1;
    }

    let Some(body) = body else {
        return Err("missing closing YAML frontmatter delimiter '---'".into());
    };

    let mut lines = Vec::new();
    for (no, raw) in raw_lines {
        let stripped = strip_comment(raw);
        let trimmed_end = stripped.trim_end_matches([' ', '\t']);
        if trimmed_end.trim_start_matches([' ', '\t']).is_empty() {
            continue;
        }
        let leading = trimmed_end.len() - trimmed_end.trim_start_matches(' ').len();
        let after_spaces = &trimmed_end[leading..];
        if after_spaces.starts_with('\t') {
            return Err(format!(
                "line {no}: tabs are not allowed for YAML indentation"
            ));
        }
        lines.push(Line {
            line_no: no,
            indent: leading,
            text: after_spaces,
        });
    }

    if lines.is_empty() {
        return Ok((Value::Object(Map::new()), body));
    }

    let mut idx = 0;
    let root_indent = lines[0].indent;
    if root_indent != 0 {
        return Err(format!(
            "line {}: unexpected leading indentation at top level",
            lines[0].line_no
        ));
    }
    let value = parse_node(&lines, &mut idx, root_indent)?;
    if idx < lines.len() {
        return Err(format!(
            "line {}: unexpected trailing content",
            lines[idx].line_no
        ));
    }
    Ok((value, body))
}

fn split_first_line(s: &str) -> Option<(&str, &str)> {
    if s.is_empty() {
        return None;
    }
    match s.find('\n') {
        Some(pos) => {
            let line = s[..pos].strip_suffix('\r').unwrap_or(&s[..pos]);
            Some((line, &s[pos + 1..]))
        }
        None => Some((s.strip_suffix('\r').unwrap_or(s), "")),
    }
}

/// Removes an unquoted `# ...` comment from a YAML line.
fn strip_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut i = 0;
    let mut in_single = false;
    let mut in_double = false;
    while i < bytes.len() {
        let b = bytes[i];
        if in_double {
            if b == b'\\' && i + 1 < bytes.len() {
                i += 2;
                continue;
            }
            if b == b'"' {
                in_double = false;
            }
        } else if in_single {
            if b == b'\'' {
                if i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                    i += 2;
                    continue;
                }
                in_single = false;
            }
        } else {
            match b {
                b'"' => in_double = true,
                b'\'' => in_single = true,
                b'#' if i == 0 || bytes[i - 1] == b' ' || bytes[i - 1] == b'\t' => {
                    return &line[..i];
                }
                _ => {}
            }
        }
        i += 1;
    }
    line
}

fn is_seq_item(text: &str) -> bool {
    text == "-" || text.starts_with("- ")
}

fn parse_node(lines: &[Line<'_>], idx: &mut usize, indent: usize) -> Result<Value, String> {
    if is_seq_item(lines[*idx].text) {
        parse_sequence(lines, idx, indent)
    } else {
        parse_mapping(lines, idx, indent)
    }
}

fn parse_mapping(lines: &[Line<'_>], idx: &mut usize, base_indent: usize) -> Result<Value, String> {
    let mut map = Map::new();
    while *idx < lines.len() {
        let line = &lines[*idx];
        if line.indent < base_indent {
            break;
        }
        if line.indent > base_indent {
            return Err(format!("line {}: unexpected indentation", line.line_no));
        }
        if is_seq_item(line.text) {
            break;
        }
        let Some((key, rest)) = try_split_key_value(line.line_no, line.text)? else {
            return Err(format!(
                "line {}: expected 'key: value' mapping entry",
                line.line_no
            ));
        };
        let line_no = line.line_no;
        *idx += 1;
        let value = parse_mapping_value(lines, idx, line_no, base_indent, rest)?;
        if map.insert(key.clone(), value).is_some() {
            return Err(format!("line {line_no}: duplicate key \"{key}\""));
        }
    }
    Ok(Value::Object(map))
}

fn parse_mapping_value(
    lines: &[Line<'_>],
    idx: &mut usize,
    line_no: usize,
    parent_indent: usize,
    rest: &str,
) -> Result<Value, String> {
    let rest = rest.trim();
    if !rest.is_empty() {
        return parse_inline_value(line_no, rest);
    }
    if *idx < lines.len() {
        let next = &lines[*idx];
        if next.indent > parent_indent {
            return parse_node(lines, idx, next.indent);
        }
        if next.indent == parent_indent && is_seq_item(next.text) {
            return parse_sequence(lines, idx, parent_indent);
        }
    }
    Ok(Value::Null)
}

fn parse_sequence(
    lines: &[Line<'_>],
    idx: &mut usize,
    base_indent: usize,
) -> Result<Value, String> {
    let mut items = Vec::new();
    while *idx < lines.len() {
        let line = &lines[*idx];
        if line.indent < base_indent {
            break;
        }
        if line.indent > base_indent {
            return Err(format!("line {}: unexpected indentation", line.line_no));
        }
        if !is_seq_item(line.text) {
            break;
        }
        let line_no = line.line_no;
        let raw_after = if line.text == "-" {
            ""
        } else {
            &line.text[1..]
        };
        let trimmed_after = raw_after.trim_start_matches(' ');
        let entry_indent = base_indent + 1 + (raw_after.len() - trimmed_after.len());
        *idx += 1;

        if trimmed_after.is_empty() {
            if *idx < lines.len() && lines[*idx].indent > base_indent {
                let child_indent = lines[*idx].indent;
                items.push(parse_node(lines, idx, child_indent)?);
            } else {
                items.push(Value::Null);
            }
            continue;
        }

        if let Some((first_key, first_rest)) = try_split_key_value(line_no, trimmed_after)? {
            let mut map = Map::new();
            let first_val = parse_mapping_value(lines, idx, line_no, entry_indent, first_rest)?;
            map.insert(first_key, first_val);
            while *idx < lines.len() {
                let next = &lines[*idx];
                if next.indent < entry_indent {
                    break;
                }
                if next.indent > entry_indent {
                    return Err(format!("line {}: unexpected indentation", next.line_no));
                }
                if is_seq_item(next.text) {
                    return Err(format!(
                        "line {}: unexpected sequence item inside mapping",
                        next.line_no
                    ));
                }
                let Some((key, rest)) = try_split_key_value(next.line_no, next.text)? else {
                    return Err(format!(
                        "line {}: expected 'key: value' mapping entry",
                        next.line_no
                    ));
                };
                let next_no = next.line_no;
                *idx += 1;
                let value = parse_mapping_value(lines, idx, next_no, entry_indent, rest)?;
                if map.insert(key.clone(), value).is_some() {
                    return Err(format!("line {next_no}: duplicate key \"{key}\""));
                }
            }
            items.push(Value::Object(map));
        } else {
            items.push(parse_inline_value(line_no, trimmed_after)?);
        }
    }
    Ok(Value::Array(items))
}

/// Splits `key: value` when `text` starts with a bare or quoted key followed by `:` and
/// either end-of-line or space/tab. Returns `Ok(None)` when `text` is not a mapping entry.
fn try_split_key_value(line_no: usize, text: &str) -> Result<Option<(String, &str)>, String> {
    if text.starts_with('[') || text.starts_with('{') {
        return Ok(None);
    }
    if text.starts_with('"') {
        let (key, rest) = parse_double_quoted_prefix(line_no, text)?;
        let rest = rest.trim_start_matches(' ');
        if let Some(after) = rest.strip_prefix(':')
            && (after.is_empty() || after.starts_with(' ') || after.starts_with('\t'))
        {
            return Ok(Some((key, after)));
        }
        return Ok(None);
    }
    if text.starts_with('\'') {
        let (key, rest) = parse_single_quoted_prefix(line_no, text)?;
        let rest = rest.trim_start_matches(' ');
        if let Some(after) = rest.strip_prefix(':')
            && (after.is_empty() || after.starts_with(' ') || after.starts_with('\t'))
        {
            return Ok(Some((key, after)));
        }
        return Ok(None);
    }
    let bytes = text.as_bytes();
    for i in 0..bytes.len() {
        if bytes[i] == b':' {
            let next_is_sep = i + 1 == bytes.len() || bytes[i + 1] == b' ' || bytes[i + 1] == b'\t';
            if next_is_sep {
                let key = text[..i].trim_end_matches([' ', '\t']);
                if key.is_empty() {
                    return Err(format!("line {line_no}: empty mapping key"));
                }
                return Ok(Some((key.to_string(), &text[i + 1..])));
            }
        }
    }
    Ok(None)
}

fn parse_inline_value(line_no: usize, text: &str) -> Result<Value, String> {
    let text = text.trim();
    if text.starts_with('[') {
        return parse_flow_seq(line_no, text);
    }
    if text.starts_with('{') {
        return parse_flow_map(line_no, text);
    }
    if text.starts_with('"') {
        let (s, rest) = parse_double_quoted_prefix(line_no, text)?;
        if !rest.trim().is_empty() {
            return Err(format!(
                "line {line_no}: unexpected characters after closing quote"
            ));
        }
        return Ok(Value::String(s));
    }
    if text.starts_with('\'') {
        let (s, rest) = parse_single_quoted_prefix(line_no, text)?;
        if !rest.trim().is_empty() {
            return Err(format!(
                "line {line_no}: unexpected characters after closing quote"
            ));
        }
        return Ok(Value::String(s));
    }
    Ok(parse_plain_scalar(text))
}

fn parse_flow_seq(line_no: usize, text: &str) -> Result<Value, String> {
    let inner = text
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .ok_or_else(|| format!("line {line_no}: unclosed '[' in flow sequence"))?;
    let inner = inner.trim();
    if inner.is_empty() {
        return Ok(Value::Array(Vec::new()));
    }
    let parts = split_flow_items(line_no, inner)?;
    let mut out = Vec::with_capacity(parts.len());
    for part in parts {
        out.push(parse_inline_value(line_no, part)?);
    }
    Ok(Value::Array(out))
}

fn parse_flow_map(line_no: usize, text: &str) -> Result<Value, String> {
    let inner = text
        .strip_prefix('{')
        .and_then(|s| s.strip_suffix('}'))
        .ok_or_else(|| format!("line {line_no}: unclosed '{{' in flow mapping"))?;
    let inner = inner.trim();
    if inner.is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    let parts = split_flow_items(line_no, inner)?;
    let mut map = Map::new();
    for part in parts {
        let Some((key, rest)) = try_split_key_value(line_no, part.trim())? else {
            return Err(format!(
                "line {line_no}: expected 'key: value' inside flow mapping"
            ));
        };
        let val_str = rest.trim();
        let value = if val_str.is_empty() {
            Value::Null
        } else {
            parse_inline_value(line_no, val_str)?
        };
        if map.insert(key.clone(), value).is_some() {
            return Err(format!("line {line_no}: duplicate key \"{key}\""));
        }
    }
    Ok(Value::Object(map))
}

fn split_flow_items(line_no: usize, text: &str) -> Result<Vec<&str>, String> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;
    let mut depth_bracket = 0usize;
    let mut depth_brace = 0usize;
    let mut in_single = false;
    let mut in_double = false;

    while i < bytes.len() {
        let b = bytes[i];
        if in_double {
            if b == b'\\' && i + 1 < bytes.len() {
                i += 2;
                continue;
            }
            if b == b'"' {
                in_double = false;
            }
        } else if in_single {
            if b == b'\'' {
                if i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                    i += 2;
                    continue;
                }
                in_single = false;
            }
        } else {
            match b {
                b'"' => in_double = true,
                b'\'' => in_single = true,
                b'[' => depth_bracket += 1,
                b']' => {
                    if depth_bracket == 0 {
                        return Err(format!("line {line_no}: unmatched ']'"));
                    }
                    depth_bracket -= 1;
                }
                b'{' => depth_brace += 1,
                b'}' => {
                    if depth_brace == 0 {
                        return Err(format!("line {line_no}: unmatched '}}'"));
                    }
                    depth_brace -= 1;
                }
                b',' if depth_bracket == 0 && depth_brace == 0 => {
                    let piece = text[start..i].trim();
                    if piece.is_empty() {
                        return Err(format!("line {line_no}: empty item in flow collection"));
                    }
                    out.push(piece);
                    start = i + 1;
                }
                _ => {}
            }
        }
        i += 1;
    }
    if in_single || in_double {
        return Err(format!("line {line_no}: unclosed quoted string"));
    }
    if depth_bracket != 0 || depth_brace != 0 {
        return Err(format!("line {line_no}: unclosed nested flow collection"));
    }
    let tail = text[start..].trim();
    if tail.is_empty() {
        // Allow trailing comma? Reject to stay strict and predictable.
        return Err(format!("line {line_no}: trailing comma in flow collection"));
    }
    out.push(tail);
    Ok(out)
}

fn parse_double_quoted_prefix(line_no: usize, text: &str) -> Result<(String, &str), String> {
    let mut chars = text.char_indices();
    debug_assert_eq!(chars.next().map(|(_, c)| c), Some('"'));
    let mut out = String::new();
    while let Some((idx, ch)) = chars.next() {
        match ch {
            '"' => return Ok((out, &text[idx + 1..])),
            '\\' => {
                let Some((_, esc)) = chars.next() else {
                    return Err(format!(
                        "line {line_no}: unterminated escape in double-quoted string"
                    ));
                };
                match esc {
                    '"' => out.push('"'),
                    '\\' => out.push('\\'),
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    't' => out.push('\t'),
                    other => {
                        return Err(format!(
                            "line {line_no}: unsupported escape '\\{other}' in double-quoted string"
                        ));
                    }
                }
            }
            c => out.push(c),
        }
    }
    Err(format!("line {line_no}: unclosed double-quoted string"))
}

fn parse_single_quoted_prefix(line_no: usize, text: &str) -> Result<(String, &str), String> {
    let mut chars = text.char_indices().peekable();
    debug_assert_eq!(chars.next().map(|(_, c)| c), Some('\''));
    let mut out = String::new();
    while let Some((idx, ch)) = chars.next() {
        if ch == '\'' {
            if let Some(&(_, '\'')) = chars.peek() {
                chars.next();
                out.push('\'');
            } else {
                return Ok((out, &text[idx + 1..]));
            }
        } else {
            out.push(ch);
        }
    }
    Err(format!("line {line_no}: unclosed single-quoted string"))
}

fn parse_plain_scalar(text: &str) -> Value {
    match text {
        "null" | "~" => Value::Null,
        "true" => Value::Bool(true),
        "false" => Value::Bool(false),
        _ => {
            let looks_int = !text.is_empty()
                && (text == "0"
                    || (!text.starts_with('0') && !text.starts_with("-0"))
                        && text
                            .strip_prefix('-')
                            .unwrap_or(text)
                            .bytes()
                            .all(|b| b.is_ascii_digit()));
            if looks_int && let Ok(n) = text.parse::<i64>() {
                return Value::Number(n.into());
            }
            if text.contains('.')
                && text
                    .strip_prefix('-')
                    .unwrap_or(text)
                    .bytes()
                    .all(|b| b.is_ascii_digit() || b == b'.')
                && let Ok(f) = text.parse::<f64>()
                && let Some(n) = Number::from_f64(f)
            {
                return Value::Number(n);
            }
            Value::String(text.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn parses_now_md_with_nested_mappings_sequences_dates_and_quotes() {
        let md = "\
---
# Operational status frontmatter
version: 1
updatedAt: 2026-11-26T02:24:54-03:00
updatedBy: acme-lead
sessions:
  - role: acme-lead
    state: working
    now: \"Coordinating: \\\"POC\\\"\\tphase\"
    next:
      - 'Review ''dev'' delivery'
      - Merge branch
    updatedAt: 2026-11-26T02:20:00-03:00
  - role: dev
    state: idle
    now: idle
    next: []
owner:
  urgent: []
  beforeMain: [\"Approve PR #35\", Run smoke test]
  optional: []
recent: []
---
# Current Focus

Body content here.
";
        let (val, body) = parse_frontmatter(md).unwrap();
        assert_eq!(body, "# Current Focus\n\nBody content here.\n");
        assert_eq!(
            val,
            json!({
                "version": 1,
                "updatedAt": "2026-11-26T02:24:54-03:00",
                "updatedBy": "acme-lead",
                "sessions": [
                    {
                        "role": "acme-lead",
                        "state": "working",
                        "now": "Coordinating: \"POC\"\tphase",
                        "next": ["Review 'dev' delivery", "Merge branch"],
                        "updatedAt": "2026-11-26T02:20:00-03:00"
                    },
                    {
                        "role": "dev",
                        "state": "idle",
                        "now": "idle",
                        "next": []
                    }
                ],
                "owner": {
                    "urgent": [],
                    "beforeMain": ["Approve PR #35", "Run smoke test"],
                    "optional": []
                },
                "recent": []
            })
        );
    }

    #[test]
    fn handles_crlf_comments_and_flow_mappings() {
        let md = "---\r\nversion: 1 # inline comment\r\nflag: true\r\nempty: {}\r\ninline: {a: 1, b: \"x # not a comment\"}\r\nnil: null\r\n---\r\nBody\r\n";
        let (val, body) = parse_frontmatter(md).unwrap();
        assert_eq!(body, "Body\r\n");
        assert_eq!(
            val,
            json!({
                "version": 1,
                "flag": true,
                "empty": {},
                "inline": {"a": 1, "b": "x # not a comment"},
                "nil": null
            })
        );
    }

    #[test]
    fn reports_line_numbered_errors_on_malformed_frontmatter() {
        assert!(
            parse_frontmatter("# No frontmatter\n")
                .unwrap_err()
                .contains("line 1")
        );
        assert!(
            parse_frontmatter("---\nversion: 1\n")
                .unwrap_err()
                .contains("closing")
        );
        let err = parse_frontmatter("---\nversion: 1\n\tbad: true\n---\n").unwrap_err();
        assert!(err.contains("line 3") && err.contains("tabs"), "{err}");
        let err = parse_frontmatter("---\nversion: \"unclosed\n---\n").unwrap_err();
        assert!(err.contains("line 2") && err.contains("unclosed"), "{err}");
        let err = parse_frontmatter("---\nversion: 1\nversion: 2\n---\n").unwrap_err();
        assert!(err.contains("line 3") && err.contains("duplicate"), "{err}");
        let err = parse_frontmatter("---\nnot a mapping\n---\n").unwrap_err();
        assert!(err.contains("line 2"), "{err}");
    }
}
