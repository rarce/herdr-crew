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

fn read(path: &Path, c: &Config, tz: &TimeZone) -> Result<Status, Unavailable> {
    match std::fs::read(path) {
        Ok(bytes) if is_markdown(path) => {
            let log_bytes = path
                .parent()
                .map(|p| p.join("log.md"))
                .and_then(|p| std::fs::read(p).ok());
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

fn watch_signature_wiki(path: &Path, preview_target: Option<&str>) -> [FileSig; 4] {
    let dir = path.parent();
    let index_sig = dir.and_then(|p| signature(&p.join("index.md")));
    let log_sig = dir.and_then(|p| signature(&p.join("log.md")));
    let page_sig = preview_target.and_then(|target| {
        let rel = if target.ends_with(".md") {
            target.to_string()
        } else {
            format!("{target}.md")
        };
        dir.and_then(|p| signature(&p.join(rel)))
    });
    [signature(path), index_sig, log_sig, page_sig]
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
    let mem_dir = path.parent().unwrap_or_else(|| Path::new("."));
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
                            seen = watch_signature_wiki(
                                path,
                                state.preview.as_ref().map(|p| p.target.as_str()),
                            );
                            dirty = true;
                        }
                        WikiAction::OpenPage(target) => {
                            let content = memory::show(mem_dir, &target);
                            state.open_preview(target, content);
                            seen = watch_signature_wiki(
                                path,
                                state.preview.as_ref().map(|p| p.target.as_str()),
                            );
                            dirty = true;
                        }
                        WikiAction::Redraw => {
                            seen = watch_signature_wiki(
                                path,
                                state.preview.as_ref().map(|p| p.target.as_str()),
                            );
                            dirty = true;
                        }
                        WikiAction::None => {}
                    }
                }
                Event::Resize(..) => dirty = true,
                _ => {}
            }
        }
        let now = watch_signature_wiki(path, state.preview.as_ref().map(|p| p.target.as_str()));
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
