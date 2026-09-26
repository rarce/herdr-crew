//! Draws the board in a `ratatui::Frame` (design §5.3). Pure: the time, time zone and path come
//! in as arguments.

use std::path::Path;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Padding, Paragraph, Widget, Wrap};

use super::model::{Session, State, Status, Unavailable, age};

pub struct ViewCtx<'a> {
    pub now: Timestamp,
    pub tz: TimeZone,
    /// Path of the board, for the footer and the messages.
    pub path: &'a Path,
    /// Name of the project's root directory.
    pub project: &'a str,
    pub writer: &'a str,
}

const MAX_RECENT: usize = 5;
const MAX_ERRORS: usize = 5;

fn bold() -> Style {
    Style::new().add_modifier(Modifier::BOLD)
}

fn dim() -> Style {
    Style::new().add_modifier(Modifier::DIM)
}

/// Title style and border style of a session's state.
fn state_style(state: State) -> (Style, Style) {
    let color = match state {
        State::Working => Color::Green,
        State::Waiting => Color::Yellow,
        State::Blocked => Color::Red,
        State::Idle => return (dim(), dim()),
    };
    (
        Style::new().fg(color).add_modifier(Modifier::BOLD),
        Style::new().fg(color),
    )
}

/// Stacks blocks from top to bottom and cuts at the bottom what does not fit.
struct Stack<'b> {
    area: Rect,
    y: u16,
    buf: &'b mut Buffer,
}

impl Stack<'_> {
    fn push(&mut self, height: u16, widget: impl Widget) {
        let bottom = self.area.bottom();
        if self.y >= bottom || height == 0 {
            return;
        }
        let h = height.min(bottom - self.y);
        widget.render(
            Rect {
                x: self.area.x,
                y: self.y,
                width: self.area.width,
                height: h,
            },
            self.buf,
        );
        self.y += h;
    }
}

/// "label  value" rows with the value wrapped: a two-column grid.
struct Grid<'t> {
    label_width: u16,
    rows: Vec<(Span<'t>, Text<'t>)>,
}

impl<'t> Grid<'t> {
    fn value_width(&self, width: u16) -> u16 {
        width.saturating_sub(self.label_width + 1).max(1)
    }

    fn row_height(&self, value: &Text<'t>, width: u16) -> u16 {
        let p = Paragraph::new(value.clone()).wrap(Wrap { trim: true });
        u16::try_from(p.line_count(self.value_width(width)))
            .unwrap_or(u16::MAX)
            .max(1)
    }

    fn height(&self, width: u16) -> u16 {
        self.rows
            .iter()
            .map(|(_, v)| self.row_height(v, width))
            .sum()
    }
}

impl Widget for Grid<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let mut y = area.y;
        for (label, value) in &self.rows {
            if y >= area.bottom() {
                break;
            }
            let h = self.row_height(value, area.width).min(area.bottom() - y);
            Line::from(label.clone()).render(
                Rect {
                    x: area.x,
                    y,
                    width: self.label_width,
                    height: 1,
                },
                buf,
            );
            Paragraph::new(value.clone())
                .wrap(Wrap { trim: true })
                .render(
                    Rect {
                        x: area.x + self.label_width + 1,
                        y,
                        width: self.value_width(area.width),
                        height: h,
                    },
                    buf,
                );
            y += h;
        }
    }
}

/// A rounded box with the title between spaces and a column of margin on each side.
fn boxed<'t>(mut title: Line<'t>, border: Style) -> Block<'t> {
    title.spans.insert(0, Span::raw(" "));
    title.spans.push(Span::raw(" "));
    Block::bordered()
        .border_type(BorderType::Rounded)
        .title(title)
        .border_style(border)
        .padding(Padding::horizontal(1))
}

fn boxed_height(inner: u16) -> u16 {
    inner + 2
}

fn inner_width(width: u16) -> u16 {
    width.saturating_sub(4)
}

struct Framed<'t, W> {
    block: Block<'t>,
    content: W,
}

impl<W: Widget> Widget for Framed<'_, W> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let inner = self.block.inner(area);
        self.block.render(area, buf);
        self.content.render(inner, buf);
    }
}

fn session_box<'t>(s: &'t Session, now: Timestamp, width: u16) -> (u16, impl Widget + 't) {
    let (title_style, border) = state_style(s.state);
    let mut rows = vec![(Span::styled("Now", bold()), Text::from(s.now.as_str()))];
    if !s.next.is_empty() {
        let lines: Vec<Line> = s
            .next
            .iter()
            .enumerate()
            .map(|(i, n)| Line::from(format!("{}. {n}", i + 1)))
            .collect();
        rows.push((Span::styled("Next", bold()), Text::from(lines)));
    }
    if let Some(note) = &s.note {
        rows.push((
            Span::styled("Note", bold()),
            Text::styled(note.as_str(), Style::new().add_modifier(Modifier::ITALIC)),
        ));
    }
    let grid = Grid {
        label_width: 4,
        rows,
    };
    let height = boxed_height(grid.height(inner_width(width)));
    let title = Line::from(vec![
        Span::styled(s.role.as_str(), bold()),
        Span::raw("  "),
        Span::styled(format!("● {}", s.state.label()), title_style),
    ]);
    let mut block = boxed(title, border);
    if let Some(t) = s.updated_at {
        block = block.title_bottom(Line::from(format!(" {} ", age(now, t))).right_aligned());
    }
    (
        height,
        Framed {
            block,
            content: grid,
        },
    )
}

enum OwnerContent<'t> {
    Nothing,
    Grid(Grid<'t>),
}

impl Widget for OwnerContent<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        match self {
            OwnerContent::Nothing => Line::styled("Nothing.", dim()).render(area, buf),
            OwnerContent::Grid(g) => g.render(area, buf),
        }
    }
}

fn owner_box(status: &Status, width: u16) -> (u16, Framed<'_, OwnerContent<'_>>) {
    let o = &status.owner;
    let sections = [
        (
            "Urgent",
            &o.urgent,
            Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        (
            "Before main",
            &o.before_main,
            Style::new().fg(Color::Yellow),
        ),
        ("Optional", &o.optional, dim()),
    ];
    let rows: Vec<(Span, Text)> = sections
        .into_iter()
        .filter(|(_, list, _)| !list.is_empty())
        .map(|(name, list, style)| {
            let lines: Vec<Line> = list.iter().map(|x| Line::from(format!("• {x}"))).collect();
            (Span::styled(name, style), Text::from(lines))
        })
        .collect();
    let content = if rows.is_empty() {
        OwnerContent::Nothing
    } else {
        OwnerContent::Grid(Grid {
            label_width: 11,
            rows,
        })
    };
    let inner = match &content {
        OwnerContent::Nothing => 1,
        OwnerContent::Grid(g) => g.height(inner_width(width)),
    };
    let border = if o.urgent.is_empty() {
        Style::new().fg(Color::Cyan)
    } else {
        Style::new().fg(Color::Red)
    };
    (
        boxed_height(inner),
        Framed {
            block: boxed(Line::styled("Waiting on you", bold()), border),
            content,
        },
    )
}

fn wrapped(text: Text<'_>, width: u16) -> (u16, Paragraph<'_>) {
    let p = Paragraph::new(text).wrap(Wrap { trim: true });
    let h = u16::try_from(p.line_count(width.max(1))).unwrap_or(u16::MAX);
    (h, p)
}

fn local(t: Timestamp, tz: &TimeZone, format: &str) -> String {
    t.to_zoned(tz.clone()).strftime(format).to_string()
}

/// The message of the "Status unavailable" box.
pub fn unavailable_text(u: &Unavailable, path: &Path, writer: &str) -> String {
    match u {
        Unavailable::Missing => format!(
            "{} does not exist. {writer} creates it when it updates the status.",
            path.display()
        ),
        Unavailable::Json(e) => format!("Unreadable JSON in {}: {e}", path.display()),
        Unavailable::Schema(errors) => {
            let mut s = "Does not match the schema:".to_string();
            for e in errors.iter().take(MAX_ERRORS) {
                s.push_str(&format!("\n• {e}"));
            }
            s
        }
    }
}

/// Draws the board, or why it cannot be drawn, over the whole frame.
pub fn view(frame: &mut Frame, board: Result<&Status, &Unavailable>, ctx: &ViewCtx) {
    let area = frame.area();
    render(area, frame.buffer_mut(), board, ctx);
}

pub fn render(area: Rect, buf: &mut Buffer, board: Result<&Status, &Unavailable>, ctx: &ViewCtx) {
    let width = area.width;
    let mut stack = Stack {
        area,
        y: area.y,
        buf,
    };
    stack.push(
        1,
        Line::styled(format!("Project {}", ctx.project), bold().fg(Color::Cyan)),
    );

    match board {
        Err(u) => {
            let text = Text::styled(
                unavailable_text(u, ctx.path, ctx.writer),
                Style::new().fg(Color::Yellow),
            );
            let (h, p) = wrapped(text, inner_width(width));
            let block = boxed(
                Line::styled("Status unavailable", bold()),
                Style::new().fg(Color::Yellow),
            );
            stack.push(boxed_height(h), Framed { block, content: p });
        }
        Ok(status) => {
            for s in &status.sessions {
                let (h, w) = session_box(s, ctx.now, width);
                stack.push(h, w);
            }
            let (h, w) = owner_box(status, width);
            stack.push(h, w);
            if !status.recent.is_empty() {
                let lines: Vec<Line> = status
                    .recent
                    .iter()
                    .take(MAX_RECENT)
                    .map(|r| {
                        Line::from(format!(
                            "{}  {}",
                            local(r.at, &ctx.tz, "%m-%d %H:%M"),
                            r.text
                        ))
                    })
                    .collect();
                let (h, p) = wrapped(Text::from(lines), inner_width(width));
                stack.push(
                    boxed_height(h),
                    Framed {
                        block: boxed(Line::from("Recent"), dim()),
                        content: p,
                    },
                );
            }
            let footer = format!(
                "Updated {} by {} ({})",
                local(status.updated_at, &ctx.tz, "%Y-%m-%d %H:%M"),
                status.updated_by,
                age(ctx.now, status.updated_at)
            );
            let (h, p) = wrapped(Text::styled(footer, dim()), width);
            stack.push(h, p);
        }
    }
    let checked = format!(
        "{}  ·  checked {}",
        ctx.path.display(),
        local(ctx.now, &ctx.tz, "%H:%M:%S")
    );
    let (h, p) = wrapped(Text::styled(checked, dim()), width);
    stack.push(h, p);
}

/// The content of a buffer as text, one line per row without trailing spaces.
pub fn buffer_text(buf: &Buffer) -> String {
    let area = buf.area;
    let mut out = String::new();
    for y in area.top()..area.bottom() {
        let mut line = String::new();
        for x in area.left()..area.right() {
            line.push_str(buf[(x, y)].symbol());
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::board::model::load;
    use crate::board::model::tests::fixture;
    use crate::board::schema::seed;
    use crate::config::Config;
    use crate::config::tests::{basic, worktrees};

    const NOW: &str = "2026-09-25T12:34:56Z";

    fn draw(c: &Config, bytes: Option<&[u8]>, project: &str) -> String {
        let loaded = match bytes {
            None => Err(Unavailable::Missing),
            Some(b) => load(b, c),
        };
        let path = PathBuf::from(format!("/r/{project}/.herdr/status.json"));
        let ctx = ViewCtx {
            now: NOW.parse().unwrap(),
            tz: TimeZone::fixed(jiff::tz::offset(-3)),
            path: &path,
            project,
            writer: &c.board.writer,
        };
        let mut t = Terminal::new(TestBackend::new(100, 40)).unwrap();
        t.draw(|f| view(f, loaded.as_ref(), &ctx)).unwrap();
        buffer_text(t.backend().buffer())
    }

    /// Compares with `tests/snapshots/<name>.txt`; with `UPDATE_SNAPSHOTS=1` it rewrites it.
    fn snapshot(name: &str, actual: &str) {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/snapshots")
            .join(format!("{name}.txt"));
        if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
            std::fs::write(&path, actual).unwrap();
            return;
        }
        let expected = std::fs::read_to_string(&path).unwrap_or_else(|_| {
            panic!(
                "{} is missing; generate it with UPDATE_SNAPSHOTS=1",
                path.display()
            )
        });
        assert_eq!(actual, expected, "snapshot {name} changed:\n{actual}");
    }

    fn fixture_text(name: &str) -> String {
        String::from_utf8(fixture(name)).unwrap()
    }

    #[test]
    fn basic_example() {
        let text = draw(&basic(), Some(&fixture("status-basic.json")), "acme");
        assert!(text.contains("acme-dev  ● working") && text.contains("Waiting on you"));
        assert!(!text.contains("Does not match"));
        snapshot("basic-example", &text);
    }

    #[test]
    fn worktrees_example() {
        let text = draw(
            &worktrees(),
            Some(&fixture("status-worktrees.json")),
            "globex",
        );
        assert!(text.contains("globex-dev-2  ● waiting") && text.contains("Note PR #430"));
        snapshot("worktrees-example", &text);
    }

    #[test]
    fn missing_file() {
        let text = draw(&basic(), None, "acme");
        assert!(text.contains("/r/acme/.herdr/status.json does not exist. acme-lead creates it"));
        snapshot("missing-file", &text);
    }

    #[test]
    fn truncated_json() {
        let text = draw(&basic(), Some(br#"{"version": 1, "sess"#), "acme");
        assert!(text.contains("Unreadable JSON"));
        snapshot("truncated-json", &text);
    }

    #[test]
    fn unknown_state_names_the_value() {
        let json = fixture_text("status-basic.json").replacen("\"idle\"", "\"asleep\"", 1);
        let text = draw(&basic(), Some(json.as_bytes()), "acme");
        assert!(text.contains("Does not match") && text.contains("asleep"));
        snapshot("unknown-state", &text);
    }

    #[test]
    fn date_without_offset() {
        let json = fixture_text("status-basic.json").replacen(
            "\"2026-09-25T09:10:00-03:00\"",
            "\"2026-09-25T09:10:00\"",
            1,
        );
        let text = draw(&basic(), Some(json.as_bytes()), "acme");
        assert!(text.contains("Does not match") && text.contains("updatedAt"));
        snapshot("date-without-offset", &text);
    }

    #[test]
    fn updated_by_is_not_the_writer() {
        let json = fixture_text("status-basic.json").replace(
            "\"updatedBy\": \"acme-lead\"",
            "\"updatedBy\": \"acme-dev\"",
        );
        let text = draw(&basic(), Some(json.as_bytes()), "acme");
        assert!(text.contains("updatedBy: must be \"acme-lead\", not \"acme-dev\""));
        snapshot("wrong-writer", &text);
    }

    #[test]
    fn busy_board() {
        let text = draw(&basic(), Some(&fixture("status-basic-busy.json")), "acme");
        assert!(text.contains("Urgent") && text.contains("Recent") && text.contains("● blocked"));
        snapshot("busy-board", &text);
    }

    #[test]
    fn order_with_extra_instances_and_project_name() {
        let json = r##"{"version": 1, "updatedAt": "2026-09-25T09:10:00-03:00", "updatedBy": "globex-lead",
            "sessions": [
              {"role": "globex-reviewer-2", "state": "blocked", "now": "review", "next": [], "note": "no quota"},
              {"role": "globex-dev-3", "state": "working", "now": "#1 three", "next": ["a", "b"]},
              {"role": "globex-reviewer", "state": "idle", "now": "idle", "next": []},
              {"role": "globex-dev", "state": "working", "now": "#2 base", "next": [], "updatedAt": "2026-09-23T09:00:00-03:00"},
              {"role": "globex-lead", "state": "waiting", "now": "waiting for the user", "next": []}
            ],
            "owner": {"urgent": ["decide the migration"], "beforeMain": [], "optional": ["x"]}}"##;
        let text = draw(&worktrees(), Some(json.as_bytes()), "my-repo");
        assert!(text.starts_with("Project my-repo\n"));
        let pos = |s: &str| text.find(s).unwrap_or_else(|| panic!("missing {s}"));
        assert!(pos("globex-lead  ●") < pos("globex-dev  ●"));
        assert!(pos("globex-dev  ●") < pos("globex-dev-3  ●"));
        assert!(pos("globex-dev-3  ●") < pos("globex-reviewer  ●"));
        assert!(pos("globex-reviewer  ●") < pos("globex-reviewer-2  ●"));
        assert!(text.contains("2 d ago"));
        snapshot("order-with-extras", &text);
    }

    #[test]
    fn empty_owner_says_nothing_and_content_is_cut_at_the_bottom() {
        let c = worktrees();
        let sessions: Vec<String> = (2..20)
            .map(|n| {
                format!(
                    r#"{{"role": "globex-dev-{n}", "state": "idle", "now": "idle", "next": []}}"#
                )
            })
            .collect();
        let json = format!(
            r#"{{"version": 1, "updatedAt": "2026-09-25T09:10:00-03:00", "updatedBy": "globex-lead", "sessions": [{}],
            "owner": {{"urgent": [], "beforeMain": [], "optional": []}}}}"#,
            sessions.join(",")
        );
        let text = draw(&c, Some(json.as_bytes()), "globex");
        assert_eq!(text.lines().count(), 40);
        assert!(!text.contains("checked"));
        let small = draw(
            &c,
            Some(seed(&c, "2026-09-25T09:10:00-03:00").as_bytes()),
            "globex",
        );
        assert!(small.contains("Nothing."));
    }
}
