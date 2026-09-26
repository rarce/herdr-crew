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
use super::view::{self, ViewCtx};
use crate::config::Config;

const REDRAW_EVERY: Duration = Duration::from_secs(30);

fn read(path: &Path, c: &Config) -> Result<Status, Unavailable> {
    match std::fs::read(path) {
        Ok(bytes) => model::load(&bytes, c),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Err(Unavailable::Missing),
        Err(e) => Err(Unavailable::Json(e.to_string())),
    }
}

fn signature(path: &Path) -> Option<(SystemTime, u64)> {
    let m = std::fs::metadata(path).ok()?;
    Some((m.modified().ok()?, m.len()))
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
    let ctx = ViewCtx {
        now: Timestamp::now(),
        tz: TimeZone::system(),
        path,
        project: &project,
        writer: &c.board.writer,
    };
    let area = Rect::new(0, 0, width, 500);
    let mut buf = Buffer::empty(area);
    view::render(area, &mut buf, read(path, c).as_ref(), &ctx);
    let text = view::buffer_text(&buf);
    writeln!(io::stdout().lock(), "{}", text.trim_end_matches('\n'))
}

/// Full screen until `q`, `Esc` or `Ctrl-C`; `r` rereads.
pub fn run(path: &Path, c: &Config, interval: Duration) -> io::Result<()> {
    // `try_init` enters the alternate screen and raw mode, and installs a panic hook that
    // restores them.
    let mut terminal = ratatui::try_init()?;
    let result = (|| -> io::Result<()> {
        let project = project(c);
        let tz = TimeZone::system();
        let mut seen = signature(path);
        let mut board = read(path, c);
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
                            board = read(path, c);
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
                board = read(path, c);
                dirty = true;
            }
        }
    })();
    ratatui::restore();
    result
}
