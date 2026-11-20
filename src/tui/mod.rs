//! Full-screen setup and configuration screens (adapter). Each screen is a state machine that
//! takes keys and draws itself, so tests drive it without a terminal.

pub mod editor;
pub mod wizard;

use std::io::{self, IsTerminal};

use ratatui::Frame;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// Whether a person can answer full-screen questions here.
pub fn interactive() -> bool {
    io::stdin().is_terminal() && io::stdout().is_terminal()
}

/// What a screen wants after a key.
pub enum Flow<T> {
    Stay,
    Done(T),
    Cancel,
}

pub trait Screen {
    type Output;
    fn draw(&mut self, frame: &mut Frame);
    fn key(&mut self, key: KeyEvent) -> Flow<Self::Output>;
}

/// Runs a screen until it finishes; `None` when cancelled. `Ctrl-C` always cancels.
pub fn run<S: Screen>(screen: &mut S) -> io::Result<Option<S::Output>> {
    let mut terminal = ratatui::try_init()?;
    let result = (|| loop {
        terminal.draw(|f| screen.draw(f))?;
        if let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                return Ok(None);
            }
            match screen.key(key) {
                Flow::Stay => {}
                Flow::Done(value) => return Ok(Some(value)),
                Flow::Cancel => return Ok(None),
            }
        }
    })();
    ratatui::restore();
    result
}

pub fn accent() -> Style {
    Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD)
}

pub fn bold() -> Style {
    Style::new().add_modifier(Modifier::BOLD)
}

pub fn dim() -> Style {
    Style::new().add_modifier(Modifier::DIM)
}

pub fn error() -> Style {
    Style::new().fg(Color::Red).add_modifier(Modifier::BOLD)
}

pub fn warn() -> Style {
    Style::new().fg(Color::Yellow)
}

pub fn selected() -> Style {
    Style::new().fg(Color::Black).bg(Color::Cyan)
}

/// A footer line of `key action` pairs.
pub fn hints(pairs: &[(&str, &str)]) -> Line<'static> {
    let mut spans = Vec::new();
    for (i, (key, action)) in pairs.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  ·  ", dim()));
        }
        spans.push(Span::styled(key.to_string(), accent()));
        spans.push(Span::raw(format!(" {action}")));
    }
    Line::from(spans)
}

/// A rectangle of at most `width` × `height` in the middle of `area`.
pub fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let [row] = Layout::vertical([Constraint::Length(height.min(area.height))])
        .flex(Flex::Center)
        .areas(area);
    let [cell] = Layout::horizontal([Constraint::Length(width.min(area.width))])
        .flex(Flex::Center)
        .areas(row);
    cell
}

/// A one-line text field with a cursor.
#[derive(Clone, Debug, Default)]
pub struct TextInput {
    pub value: String,
    /// In characters.
    cursor: usize,
}

impl TextInput {
    pub fn new(value: &str) -> Self {
        Self {
            value: value.to_string(),
            cursor: value.chars().count(),
        }
    }

    fn byte(&self, chars: usize) -> usize {
        self.value
            .char_indices()
            .nth(chars)
            .map_or(self.value.len(), |(i, _)| i)
    }

    /// Applies an editing key; false when the key is not an edit.
    pub fn key(&mut self, key: KeyEvent) -> bool {
        let len = self.value.chars().count();
        match key.code {
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.value.clear();
                self.cursor = 0;
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                let at = self.byte(self.cursor);
                self.value.insert(at, c);
                self.cursor += 1;
            }
            KeyCode::Backspace if self.cursor > 0 => {
                self.cursor -= 1;
                let at = self.byte(self.cursor);
                self.value.remove(at);
            }
            KeyCode::Delete if self.cursor < len => {
                let at = self.byte(self.cursor);
                self.value.remove(at);
            }
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(len),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = len,
            _ => return false,
        }
        true
    }

    /// The text with a block cursor.
    pub fn line(&self) -> Line<'static> {
        let at = self.byte(self.cursor);
        let (before, rest) = self.value.split_at(at);
        let mut chars = rest.chars();
        let under = chars.next().map_or(" ".to_string(), |c| c.to_string());
        Line::from(vec![
            Span::raw(before.to_string()),
            Span::styled(under, Style::new().add_modifier(Modifier::REVERSED)),
            Span::raw(chars.as_str().to_string()),
        ])
    }
}

/// Moves a list index up or down with the arrow and vi keys; false for other keys.
pub fn move_in_list(key: KeyEvent, index: &mut usize, len: usize) -> bool {
    if len == 0 {
        return false;
    }
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => *index = (*index + len - 1) % len,
        KeyCode::Down | KeyCode::Char('j') => *index = (*index + 1) % len,
        KeyCode::Home => *index = 0,
        KeyCode::End => *index = len - 1,
        KeyCode::Char(c @ '1'..='9') if (c as usize - '1' as usize) < len => {
            *index = c as usize - '1' as usize;
        }
        _ => return false,
    }
    true
}

#[cfg(test)]
pub(crate) mod testing {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::{Flow, Screen};

    pub fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// Sends the keys of `script`: each character is typed, except the names in braces such
    /// as `{enter}`, `{esc}`, `{up}`, `{down}`, `{left}`, `{right}`, `{bs}` and `{tab}`.
    pub fn send<S: Screen>(screen: &mut S, script: &str) -> Option<Flow<S::Output>> {
        let mut last = None;
        let mut rest = script;
        while !rest.is_empty() {
            let code = if let Some(after) = rest.strip_prefix('{') {
                let (name, tail) = after.split_once('}').expect("closed key name");
                rest = tail;
                match name {
                    "enter" => KeyCode::Enter,
                    "esc" => KeyCode::Esc,
                    "up" => KeyCode::Up,
                    "down" => KeyCode::Down,
                    "left" => KeyCode::Left,
                    "right" => KeyCode::Right,
                    "bs" => KeyCode::Backspace,
                    "tab" => KeyCode::Tab,
                    other => panic!("unknown key {other}"),
                }
            } else {
                let c = rest.chars().next().unwrap();
                rest = &rest[c.len_utf8()..];
                KeyCode::Char(c)
            };
            last = Some(screen.key(key(code)));
            if !matches!(last, Some(Flow::Stay)) {
                break;
            }
        }
        last
    }

    /// The screen drawn at `width` × `height`, as text.
    pub fn draw<S: Screen>(screen: &mut S, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| screen.draw(f)).unwrap();
        let buffer = terminal.backend().buffer();
        let mut text = String::new();
        for y in 0..height {
            for x in 0..width {
                text.push_str(buffer[(x, y)].symbol());
            }
            text.push('\n');
        }
        text
    }
}
