//! Draws the board in a `ratatui::Frame` (design §5.3). Pure: the time, time zone and path come
//! in as arguments.

use std::path::Path;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{KeyCode, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Padding, Paragraph, Widget, Wrap};

use super::model::{Session, State, Status, Unavailable, age};
use crate::memory::{IndexEntry, LogRecord};

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
    let mut stack = Stack {
        area,
        y: area.y,
        buf,
    };
    stack.push(
        1,
        Line::styled(format!("Project {}", ctx.project), bold().fg(Color::Cyan)),
    );
    render_status_body(&mut stack, board, ctx);
}

fn render_status_body(stack: &mut Stack<'_>, board: Result<&Status, &Unavailable>, ctx: &ViewCtx) {
    let width = stack.area.width;
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

/// Active tab in the interactive wiki board (`herdr-crew board` on `now.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WikiTab {
    #[default]
    Status,
    Index,
    Log,
}

impl WikiTab {
    pub fn next(self) -> Self {
        match self {
            Self::Status => Self::Index,
            Self::Index => Self::Log,
            Self::Log => Self::Status,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            Self::Status => Self::Log,
            Self::Index => Self::Status,
            Self::Log => Self::Index,
        }
    }
}

/// Loaded operational wiki data for the interactive `Index` and `Log` tabs.
#[derive(Debug, Clone)]
pub struct WikiData {
    pub index: Result<Vec<IndexEntry>, String>,
    pub log: Result<Vec<LogRecord>, String>,
}

impl Default for WikiData {
    fn default() -> Self {
        Self {
            index: Ok(Vec::new()),
            log: Ok(Vec::new()),
        }
    }
}

/// Open full-page Markdown preview inside the `Index` tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PagePreview {
    pub target: String,
    pub content: Result<String, String>,
    pub scroll: u16,
}

/// Interactive state for the wiki board (`1:Status`, `2:Index`, `3:Log`).
#[derive(Debug, Clone, Default)]
pub struct WikiState {
    pub tab: WikiTab,
    pub selected_index: usize,
    pub preview: Option<PagePreview>,
    pub log_scroll: u16,
}

/// Effect returned by `WikiState::handle_key`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WikiAction {
    None,
    Quit,
    Reload,
    Redraw,
    OpenPage(String),
}

const PAGE_SCROLL_STEP: u16 = 10;

impl WikiState {
    pub fn set_tab(&mut self, tab: WikiTab) {
        if self.tab != tab {
            self.tab = tab;
            self.preview = None;
        }
    }

    pub fn clamp_selection(&mut self, len: usize) {
        if len == 0 {
            self.selected_index = 0;
        } else if self.selected_index >= len {
            self.selected_index = len - 1;
        }
    }

    pub fn open_preview(&mut self, target: String, content: Result<String, String>) {
        self.preview = Some(PagePreview {
            target,
            content,
            scroll: 0,
        });
    }

    pub fn close_preview(&mut self) -> bool {
        self.preview.take().is_some()
    }

    pub fn selected_target<'a>(&self, data: &'a WikiData) -> Option<&'a str> {
        let entries = data.index.as_ref().ok()?;
        entries.get(self.selected_index).map(|e| e.target.as_str())
    }

    pub fn handle_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
        data: &WikiData,
    ) -> WikiAction {
        let index_len = data.index.as_ref().map_or(0, Vec::len);
        match code {
            KeyCode::Char('q') => WikiAction::Quit,
            KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => WikiAction::Quit,
            KeyCode::Esc => {
                if self.close_preview() {
                    WikiAction::Redraw
                } else {
                    WikiAction::Quit
                }
            }
            KeyCode::Backspace => {
                if self.close_preview() {
                    WikiAction::Redraw
                } else {
                    WikiAction::None
                }
            }
            KeyCode::Char('r') => WikiAction::Reload,
            KeyCode::Char('1') => {
                self.set_tab(WikiTab::Status);
                WikiAction::Redraw
            }
            KeyCode::Char('2') => {
                self.set_tab(WikiTab::Index);
                self.clamp_selection(index_len);
                WikiAction::Redraw
            }
            KeyCode::Char('3') => {
                self.set_tab(WikiTab::Log);
                WikiAction::Redraw
            }
            KeyCode::Tab | KeyCode::Right => {
                self.set_tab(self.tab.next());
                self.clamp_selection(index_len);
                WikiAction::Redraw
            }
            KeyCode::BackTab | KeyCode::Left => {
                self.set_tab(self.tab.prev());
                self.clamp_selection(index_len);
                WikiAction::Redraw
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.scroll_or_select_up(index_len, 1);
                WikiAction::Redraw
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.scroll_or_select_down(index_len, 1);
                WikiAction::Redraw
            }
            KeyCode::PageUp => {
                self.scroll_or_select_up(index_len, PAGE_SCROLL_STEP);
                WikiAction::Redraw
            }
            KeyCode::PageDown => {
                self.scroll_or_select_down(index_len, PAGE_SCROLL_STEP);
                WikiAction::Redraw
            }
            KeyCode::Enter if self.tab == WikiTab::Index && self.preview.is_none() => {
                self.clamp_selection(index_len);
                match self.selected_target(data) {
                    Some(target) => WikiAction::OpenPage(target.to_string()),
                    None => WikiAction::None,
                }
            }
            _ => WikiAction::None,
        }
    }

    fn scroll_or_select_up(&mut self, index_len: usize, step: u16) {
        match self.tab {
            WikiTab::Status => {}
            WikiTab::Index => {
                if let Some(preview) = &mut self.preview {
                    preview.scroll = preview.scroll.saturating_sub(step);
                } else if index_len > 0 {
                    self.selected_index = self.selected_index.saturating_sub(usize::from(step));
                }
            }
            WikiTab::Log => {
                self.log_scroll = self.log_scroll.saturating_sub(step);
            }
        }
    }

    fn scroll_or_select_down(&mut self, index_len: usize, step: u16) {
        match self.tab {
            WikiTab::Status => {}
            WikiTab::Index => {
                if let Some(preview) = &mut self.preview {
                    preview.scroll = preview.scroll.saturating_add(step);
                } else if index_len > 0 {
                    self.selected_index =
                        (self.selected_index + usize::from(step)).min(index_len - 1);
                }
            }
            WikiTab::Log => {
                self.log_scroll = self.log_scroll.saturating_add(step);
            }
        }
    }
}

/// Draws the interactive wiki board (`1:Status`, `2:Index`, `3:Log`) over the whole frame.
pub fn view_wiki(
    frame: &mut Frame,
    board: Result<&Status, &Unavailable>,
    data: &WikiData,
    state: &WikiState,
    ctx: &ViewCtx,
) {
    render_wiki(frame.area(), frame.buffer_mut(), board, data, state, ctx);
}

pub fn render_wiki(
    area: Rect,
    buf: &mut Buffer,
    board: Result<&Status, &Unavailable>,
    data: &WikiData,
    state: &WikiState,
    ctx: &ViewCtx,
) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let header_area = Rect {
        x: area.x,
        y: area.y,
        width: area.width,
        height: 1,
    };
    render_wiki_tab_bar(header_area, buf, ctx.project, state.tab);

    let has_footer = area.height >= 3;
    let body_height = area.height.saturating_sub(1 + u16::from(has_footer));
    let body_area = Rect {
        x: area.x,
        y: area.y + 1,
        width: area.width,
        height: body_height,
    };

    match state.tab {
        WikiTab::Status => {
            let mut stack = Stack {
                area: body_area,
                y: body_area.y,
                buf,
            };
            render_status_body(&mut stack, board, ctx);
        }
        WikiTab::Index => {
            if let Some(preview) = &state.preview {
                render_page_preview(body_area, buf, preview);
            } else {
                render_index_list(body_area, buf, &data.index, state.selected_index);
            }
        }
        WikiTab::Log => {
            render_log_view(body_area, buf, &data.log, state.log_scroll);
        }
    }

    if has_footer {
        let footer_area = Rect {
            x: area.x,
            y: area.bottom() - 1,
            width: area.width,
            height: 1,
        };
        let hint = match state.tab {
            WikiTab::Status => "1-3 tabs · r reload · q quit",
            WikiTab::Index if state.preview.is_some() => {
                "↑↓/jk/PgUp/PgDn scroll · Esc/Bksp back · r reload · q quit"
            }
            WikiTab::Index => "1-3 tabs · ↑↓/jk select · Enter open · r reload · q quit",
            WikiTab::Log => "1-3 tabs · ↑↓/jk/PgUp/PgDn scroll · r reload · q quit",
        };
        Line::styled(hint, dim()).render(footer_area, buf);
    }
}

fn render_wiki_tab_bar(area: Rect, buf: &mut Buffer, project: &str, active: WikiTab) {
    let tab_span = |label: &'static str, tab: WikiTab| {
        if active == tab {
            Span::styled(
                format!(" {label} "),
                Style::new()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(format!(" {label} "), dim())
        }
    };
    let line = Line::from(vec![
        Span::styled(format!("Project {project}"), bold().fg(Color::Cyan)),
        Span::raw("  "),
        tab_span("[1] Status", WikiTab::Status),
        Span::raw(" "),
        tab_span("[2] Index", WikiTab::Index),
        Span::raw(" "),
        tab_span("[3] Log", WikiTab::Log),
    ]);
    line.render(area, buf);
}

fn render_index_list(
    area: Rect,
    buf: &mut Buffer,
    index: &Result<Vec<IndexEntry>, String>,
    selected: usize,
) {
    if area.height == 0 {
        return;
    }
    let block = boxed(Line::styled("Index", bold()), Style::new().fg(Color::Cyan));
    let inner = block.inner(area);
    block.render(area, buf);
    if inner.height == 0 || inner.width == 0 {
        return;
    }
    let entries = match index {
        Ok(e) => e,
        Err(err) => {
            Paragraph::new(Text::styled(err.as_str(), Style::new().fg(Color::Yellow)))
                .wrap(Wrap { trim: true })
                .render(inner, buf);
            return;
        }
    };
    if entries.is_empty() {
        Line::styled("No pages indexed yet.", dim()).render(inner, buf);
        return;
    }

    let mut lines: Vec<Line<'_>> = Vec::new();
    let mut current_section: Option<&str> = None;
    let mut selected_line = 0usize;

    for (idx, entry) in entries.iter().enumerate() {
        if current_section != Some(entry.section.as_str()) {
            if current_section.is_some() {
                lines.push(Line::default());
            }
            lines.push(Line::styled(entry.section.as_str(), bold().fg(Color::Cyan)));
            current_section = Some(entry.section.as_str());
        }
        let is_selected = idx == selected.min(entries.len().saturating_sub(1));
        if is_selected {
            selected_line = lines.len();
        }
        let mut spans = if is_selected {
            vec![
                Span::styled(
                    "▸ ",
                    Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("[[{}]]", entry.target),
                    Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                ),
            ]
        } else {
            vec![
                Span::raw("  "),
                Span::styled(format!("[[{}]]", entry.target), bold()),
            ]
        };
        if !entry.summary.is_empty() {
            spans.push(Span::raw("  "));
            spans.push(Span::styled(
                entry.summary.as_str(),
                if is_selected {
                    Style::new().fg(Color::Yellow)
                } else {
                    dim()
                },
            ));
        }
        lines.push(Line::from(spans));
    }

    let visible = usize::from(inner.height);
    let scroll = if selected_line >= visible {
        u16::try_from(selected_line + 1 - visible).unwrap_or(u16::MAX)
    } else {
        0
    };
    Paragraph::new(Text::from(lines))
        .scroll((scroll, 0))
        .render(inner, buf);
}

fn render_page_preview(area: Rect, buf: &mut Buffer, preview: &PagePreview) {
    if area.height == 0 {
        return;
    }
    let block = boxed(
        Line::styled(format!("Page: {}", preview.target), bold()),
        Style::new().fg(Color::Cyan),
    );
    let inner = block.inner(area);
    block.render(area, buf);
    if inner.height == 0 || inner.width == 0 {
        return;
    }
    match &preview.content {
        Err(err) => {
            Paragraph::new(Text::styled(err.as_str(), Style::new().fg(Color::Yellow)))
                .wrap(Wrap { trim: true })
                .render(inner, buf);
        }
        Ok(content) => {
            let lines: Vec<Line<'_>> = content
                .lines()
                .map(|raw| {
                    let line = raw.strip_suffix('\r').unwrap_or(raw);
                    if line.starts_with("# ") || line.starts_with("## ") || line.starts_with("### ")
                    {
                        Line::styled(line, bold().fg(Color::Cyan))
                    } else if line.trim_start() == "---" {
                        Line::styled(line, dim())
                    } else {
                        Line::from(line)
                    }
                })
                .collect();
            Paragraph::new(Text::from(lines))
                .wrap(Wrap { trim: false })
                .scroll((preview.scroll, 0))
                .render(inner, buf);
        }
    }
}

fn log_kind_style(kind: &str) -> Style {
    match kind {
        "init" => Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        "task" | "merge" => Style::new().fg(Color::Green).add_modifier(Modifier::BOLD),
        "review" => Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        "fix" => Style::new().fg(Color::Magenta).add_modifier(Modifier::BOLD),
        "decision" => Style::new().fg(Color::Blue).add_modifier(Modifier::BOLD),
        "blocker" => Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
        _ => bold(),
    }
}

fn render_log_view(
    area: Rect,
    buf: &mut Buffer,
    log: &Result<Vec<LogRecord>, String>,
    scroll: u16,
) {
    if area.height == 0 {
        return;
    }
    let block = boxed(
        Line::styled("Activity Log", bold()),
        Style::new().fg(Color::Cyan),
    );
    let inner = block.inner(area);
    block.render(area, buf);
    if inner.height == 0 || inner.width == 0 {
        return;
    }
    let records = match log {
        Ok(r) => r,
        Err(err) => {
            Paragraph::new(Text::styled(err.as_str(), Style::new().fg(Color::Yellow)))
                .wrap(Wrap { trim: true })
                .render(inner, buf);
            return;
        }
    };
    if records.is_empty() {
        Line::styled("No log entries yet.", dim()).render(inner, buf);
        return;
    }

    let mut lines: Vec<Line<'_>> = Vec::new();
    for (idx, record) in records.iter().enumerate() {
        if idx > 0 {
            lines.push(Line::default());
        }
        lines.push(Line::from(vec![
            Span::styled(record.timestamp_raw.as_str(), dim()),
            Span::raw("  "),
            Span::styled(format!("[{}]", record.kind), log_kind_style(&record.kind)),
            Span::raw(" "),
            Span::styled(record.title.as_str(), bold()),
        ]));
        for body_line in &record.body {
            lines.push(Line::from(format!("  {body_line}")));
        }
    }

    Paragraph::new(Text::from(lines))
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0))
        .render(inner, buf);
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

    #[test]
    fn wiki_tabs_navigation_index_preview_and_log_render() {
        let c = basic();
        let tz = TimeZone::fixed(jiff::tz::offset(-3));
        let now_md = "---\nversion: 1\nupdatedAt: 2026-09-25T09:10:00-03:00\nupdatedBy: acme-lead\nsessions:\n  - role: acme-lead\n    state: working\n    now: Coordinating\n    next: []\nowner:\n  urgent: []\n  beforeMain: []\n  optional: []\n---\n";
        let log_md = "# Log\n\n## [2026-09-25 09:00] init | Bootstrapped\n\nSeeded store.\n\n## [2026-09-25 09:05] task | Implemented wiki views\n\nAdded tabs and preview.\n";
        let loaded =
            crate::board::model::load_markdown(now_md.as_bytes(), Some(log_md.as_bytes()), &c, &tz);
        let index_entries = crate::memory::parse_index_entries(
            "# Index\n\n## Active\n\n- [[now]] — live status\n\n## Tasks\n\n- [[tasks/t1]] — first task\n",
            &["tasks/t1.md".into(), "notes/extra.md".into()],
        );
        let mut records = crate::memory::parse_log_records(log_md, &tz);
        records.sort_by(|a, b| b.at.cmp(&a.at).then_with(|| a.line.cmp(&b.line)));
        let data = WikiData {
            index: Ok(index_entries),
            log: Ok(records),
        };
        let path = PathBuf::from("/tmp/mem/now.md");
        let ctx = ViewCtx {
            now: NOW.parse().unwrap(),
            tz,
            path: &path,
            project: "acme",
            writer: &c.board.writer,
        };
        let draw_wiki = |state: &WikiState| {
            let mut t = Terminal::new(TestBackend::new(100, 24)).unwrap();
            t.draw(|f| view_wiki(f, loaded.as_ref(), &data, state, &ctx))
                .unwrap();
            buffer_text(t.backend().buffer())
        };

        let mut state = WikiState::default();
        let status_view = draw_wiki(&state);
        assert!(status_view.contains("[1] Status") && status_view.contains("[2] Index"));
        assert!(status_view.contains("acme-lead  ● working"));
        assert!(status_view.contains("1-3 tabs · r reload · q quit"));

        // Switch to Index via Tab, move selection down to tasks/t1, open preview, scroll, close.
        assert_eq!(
            state.handle_key(KeyCode::Tab, KeyModifiers::NONE, &data),
            WikiAction::Redraw
        );
        assert_eq!(state.tab, WikiTab::Index);
        assert_eq!(
            state.handle_key(KeyCode::Char('j'), KeyModifiers::NONE, &data),
            WikiAction::Redraw
        );
        assert_eq!(state.selected_index, 1);
        let index_view = draw_wiki(&state);
        assert!(
            index_view.contains("▸ [[tasks/t1]]  first task"),
            "{index_view}"
        );
        assert!(
            index_view.contains("[[notes/extra]]  (not listed in index.md)"),
            "{index_view}"
        );

        assert_eq!(
            state.handle_key(KeyCode::Enter, KeyModifiers::NONE, &data),
            WikiAction::OpenPage("tasks/t1".into())
        );
        state.open_preview(
            "tasks/t1".into(),
            Ok("# Task 1\n\nLine 1\nLine 2\nLine 3\n".into()),
        );
        let preview_view = draw_wiki(&state);
        assert!(preview_view.contains("Page: tasks/t1"), "{preview_view}");
        assert!(preview_view.contains("Line 1"), "{preview_view}");

        assert_eq!(
            state.handle_key(KeyCode::Down, KeyModifiers::NONE, &data),
            WikiAction::Redraw
        );
        assert_eq!(state.preview.as_ref().unwrap().scroll, 1);

        // Esc closes preview without quitting; a second Esc quits.
        assert_eq!(
            state.handle_key(KeyCode::Esc, KeyModifiers::NONE, &data),
            WikiAction::Redraw
        );
        assert!(state.preview.is_none());
        assert_eq!(
            state.handle_key(KeyCode::Esc, KeyModifiers::NONE, &data),
            WikiAction::Quit
        );

        // Switch to Log via '3', verify newest-first rendering and scrolling.
        assert_eq!(
            state.handle_key(KeyCode::Char('3'), KeyModifiers::NONE, &data),
            WikiAction::Redraw
        );
        assert_eq!(state.tab, WikiTab::Log);
        let log_view = draw_wiki(&state);
        let pos_new = log_view.find("[task] Implemented wiki views").unwrap();
        let pos_old = log_view.find("[init] Bootstrapped").unwrap();
        assert!(pos_new < pos_old, "{log_view}");
        assert!(log_view.contains("Added tabs and preview."), "{log_view}");
        assert_eq!(
            state.handle_key(KeyCode::PageDown, KeyModifiers::NONE, &data),
            WikiAction::Redraw
        );
        assert_eq!(state.log_scroll, PAGE_SCROLL_STEP);
        assert_eq!(
            state.handle_key(KeyCode::Char('q'), KeyModifiers::NONE, &data),
            WikiAction::Quit
        );
    }
}
