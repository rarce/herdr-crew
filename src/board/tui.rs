//! The board's terminal loop (design §5.4): polls the file and redraws (adapter).

use std::io::{self, IsTerminal, Write};
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use jiff::Timestamp;
use jiff::tz::TimeZone;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::Rect;

use super::model::{self, Status, Unavailable};
use super::view::{self, ViewCtx, WikiAction, WikiData, WikiState};
use crate::config::Config;
use crate::memory;

const REDRAW_EVERY: Duration = Duration::from_secs(30);

fn is_markdown(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()) == Some("md")
}

fn parent_dir(path: &Path) -> &Path {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn read(path: &Path, c: &Config, tz: &TimeZone) -> Result<Status, Unavailable> {
    match std::fs::read(path) {
        Ok(bytes) if is_markdown(path) => {
            let log_bytes = std::fs::read(parent_dir(path).join("log.md")).ok();
            model::load_markdown(&bytes, log_bytes.as_deref(), c, tz)
        }
        Ok(bytes) => model::load(&bytes, c),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Err(Unavailable::Missing),
        Err(e) => Err(Unavailable::Json(e.to_string())),
    }
}

fn read_wiki_data(dir: &Path, tz: &TimeZone) -> WikiData {
    WikiData {
        index: memory::list_index_entries(dir),
        log: memory::read_log_records(dir, tz),
    }
}

fn signature(path: &Path) -> Option<(SystemTime, u64)> {
    let m = std::fs::metadata(path).ok()?;
    Some((m.modified().ok()?, m.len()))
}

type FileSig = Option<(SystemTime, u64)>;

fn watched_preview_target(state: &WikiState) -> Option<&str> {
    state
        .preview
        .as_ref()
        .filter(|p| p.content.is_ok())
        .map(|p| p.target.as_str())
}

fn preview_signature(path: &Path, preview_target: Option<&str>) -> FileSig {
    let dir = parent_dir(path);
    preview_target.and_then(|target| {
        let rel = if target.ends_with(".md") {
            target.to_string()
        } else {
            format!("{target}.md")
        };
        signature(&dir.join(rel))
    })
}

fn watch_signature_wiki(path: &Path, preview_target: Option<&str>) -> [FileSig; 4] {
    let dir = parent_dir(path);
    [
        signature(path),
        signature(&dir.join("index.md")),
        signature(&dir.join("log.md")),
        preview_signature(path, preview_target),
    ]
}

fn project(c: &Config) -> String {
    c.root.file_name().map_or_else(
        || c.root.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// Draws once to stdout, without the alternate screen.
pub fn once(path: &Path, c: &Config) -> io::Result<()> {
    let width = if io::stdout().is_terminal() {
        ratatui::crossterm::terminal::size().map_or(100, |(w, _)| w)
    } else {
        100
    };
    let project = project(c);
    let tz = TimeZone::system();
    let ctx = ViewCtx {
        now: Timestamp::now(),
        tz: tz.clone(),
        path,
        project: &project,
        writer: &c.board.writer,
    };
    let area = Rect::new(0, 0, width, 500);
    let mut buf = Buffer::empty(area);
    view::render(area, &mut buf, read(path, c, &tz).as_ref(), &ctx);
    let text = view::buffer_text(&buf);
    writeln!(io::stdout().lock(), "{}", text.trim_end_matches('\n'))
}

/// Full screen until `q`, `Esc` or `Ctrl-C`; `r` rereads.
pub fn run(path: &Path, c: &Config, interval: Duration) -> io::Result<()> {
    // `try_init` enters the alternate screen and raw mode, and installs a panic hook that
    // restores them.
    let mut terminal = ratatui::try_init()?;
    let result = if is_markdown(path) {
        run_wiki(&mut terminal, path, c, interval)
    } else {
        run_json(&mut terminal, path, c, interval)
    };
    ratatui::restore();
    result
}

fn run_json(
    terminal: &mut ratatui::DefaultTerminal,
    path: &Path,
    c: &Config,
    interval: Duration,
) -> io::Result<()> {
    let project = project(c);
    let tz = TimeZone::system();
    let mut seen = signature(path);
    let mut board = read(path, c, &tz);
    let mut last_draw: Option<Instant> = None;
    let mut dirty = true;
    loop {
        if dirty || last_draw.is_none_or(|t| t.elapsed() >= REDRAW_EVERY) {
            let ctx = ViewCtx {
                now: Timestamp::now(),
                tz: tz.clone(),
                path,
                project: &project,
                writer: &c.board.writer,
            };
            terminal.draw(|f| view::view(f, board.as_ref(), &ctx))?;
            last_draw = Some(Instant::now());
            dirty = false;
        }
        if event::poll(interval)? {
            match event::read()? {
                Event::Key(k) if k.kind == KeyEventKind::Press => match k.code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                    KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                        return Ok(());
                    }
                    KeyCode::Char('r') => {
                        board = read(path, c, &tz);
                        dirty = true;
                    }
                    _ => {}
                },
                Event::Resize(..) => dirty = true,
                _ => {}
            }
        }
        let now = signature(path);
        if now != seen {
            seen = now;
            board = read(path, c, &tz);
            dirty = true;
        }
    }
}

fn run_wiki(
    terminal: &mut ratatui::DefaultTerminal,
    path: &Path,
    c: &Config,
    interval: Duration,
) -> io::Result<()> {
    let project = project(c);
    let tz = TimeZone::system();
    let mem_dir = parent_dir(path);
    let mut state = WikiState::default();
    let mut seen = watch_signature_wiki(path, None);
    let mut board = read(path, c, &tz);
    let mut wiki_data = read_wiki_data(mem_dir, &tz);
    let mut last_draw: Option<Instant> = None;
    let mut dirty = true;
    loop {
        if dirty || last_draw.is_none_or(|t| t.elapsed() >= REDRAW_EVERY) {
            let ctx = ViewCtx {
                now: Timestamp::now(),
                tz: tz.clone(),
                path,
                project: &project,
                writer: &c.board.writer,
            };
            terminal.draw(|f| view::view_wiki(f, board.as_ref(), &wiki_data, &state, &ctx))?;
            last_draw = Some(Instant::now());
            dirty = false;
        }
        if event::poll(interval)? {
            match event::read()? {
                Event::Key(k) if k.kind == KeyEventKind::Press => {
                    match state.handle_key(k.code, k.modifiers, &wiki_data) {
                        WikiAction::Quit => return Ok(()),
                        WikiAction::Reload => {
                            board = read(path, c, &tz);
                            wiki_data = read_wiki_data(mem_dir, &tz);
                            state.clamp_selection(wiki_data.index.as_ref().map_or(0, Vec::len));
                            if let Some(preview) = &mut state.preview {
                                preview.content = memory::show(mem_dir, &preview.target);
                            }
                            seen = watch_signature_wiki(path, watched_preview_target(&state));
                            dirty = true;
                        }
                        WikiAction::OpenPage(target) => {
                            let content = memory::show(mem_dir, &target);
                            state.open_preview(target, content);
                            seen[3] = preview_signature(path, watched_preview_target(&state));
                            dirty = true;
                        }
                        WikiAction::Redraw => {
                            seen[3] = preview_signature(path, watched_preview_target(&state));
                            dirty = true;
                        }
                        WikiAction::None => {}
                    }
                }
                Event::Resize(..) => dirty = true,
                _ => {}
            }
        }
        let now = watch_signature_wiki(path, watched_preview_target(&state));
        if now != seen {
            seen = now;
            board = read(path, c, &tz);
            wiki_data = read_wiki_data(mem_dir, &tz);
            state.clamp_selection(wiki_data.index.as_ref().map_or(0, Vec::len));
            if let Some(preview) = &mut state.preview {
                preview.content = memory::show(mem_dir, &preview.target);
            }
            dirty = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn parent_dir_and_preview_signature_preserve_board_watch_slots() {
        assert_eq!(parent_dir(Path::new("now.md")), Path::new("."));
        assert_eq!(parent_dir(Path::new("/a/b/now.md")), Path::new("/a/b"));

        let dir = PathBuf::from(format!("/tmp/crew-tui-watch-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("tasks")).unwrap();
        let now_path = dir.join("now.md");
        fs::write(&now_path, "v1").unwrap();
        fs::write(dir.join("index.md"), "# Index\n").unwrap();
        fs::write(dir.join("log.md"), "# Log\n").unwrap();
        fs::write(dir.join("tasks/t1.md"), "# Task 1\n").unwrap();

        let mut seen = watch_signature_wiki(&now_path, None);
        assert!(seen[3].is_none());

        // Simulate a concurrent edit to now.md followed by opening a preview (updating only seen[3]).
        fs::write(&now_path, "v2 longer content").unwrap();
        let mut state = WikiState::default();
        state.open_preview("tasks/t1".into(), Ok("# Task 1\n".into()));
        seen[3] = preview_signature(&now_path, watched_preview_target(&state));
        assert!(seen[3].is_some());
        assert_ne!(
            watch_signature_wiki(&now_path, watched_preview_target(&state)),
            seen,
            "updating only seen[3] must not swallow concurrent edits to now.md"
        );

        // Errored previews are not watched.
        state.open_preview("../outside".into(), Err("invalid path".into()));
        assert_eq!(watched_preview_target(&state), None);
        assert!(preview_signature(&now_path, watched_preview_target(&state)).is_none());

        let _ = fs::remove_dir_all(&dir);
    }
}
