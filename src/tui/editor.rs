//! `config`: a full-screen editor for the agent, model and effort of each role and of the
//! project defaults. Every edit is validated with `Config::parse` and refused if invalid;
//! saving keeps the file's comments and formatting.

use std::io;
use std::path::{Path, PathBuf};

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Cell, Clear, Paragraph, Row, Table, TableState, Wrap};
use toml_edit::DocumentMut;

use super::{
    Flow, Screen, TextInput, accent, bold, centered, dim, error, hints, move_in_list, selected,
    warn,
};
use crate::agent::{AgentOptions, Kind};
use crate::config::{self, Config};
use crate::config_edit::{self, Target};
use crate::models::{self, Duty, Profile};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Column {
    Agent,
    Model,
    Effort,
}

const COLUMNS: [Column; 3] = [Column::Agent, Column::Model, Column::Effort];

/// One choice in a picker: the value to write (`None` removes the key) and its label.
#[derive(Clone, Debug)]
struct Choice {
    value: Option<String>,
    label: String,
    note: String,
    /// Opens a text field instead of writing.
    custom: bool,
}

enum Popup {
    Pick {
        title: String,
        choices: Vec<Choice>,
        index: usize,
    },
    Custom {
        title: String,
        input: TextInput,
    },
    Profile {
        index: usize,
    },
}

pub struct Editor {
    root: PathBuf,
    saved: String,
    doc: DocumentMut,
    config: Config,
    /// 0 is the defaults; role `i` is row `i + 1`.
    row: usize,
    column: usize,
    popup: Option<Popup>,
    status: Option<(String, bool)>,
    /// Quitting with unsaved changes needs a second `q`.
    quit_armed: bool,
}

impl Editor {
    pub fn new(root: &Path, text: &str) -> Result<Self, String> {
        let config = Config::parse(text, root).map_err(|errors| {
            errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n")
        })?;
        Ok(Editor {
            root: root.to_path_buf(),
            saved: text.to_string(),
            doc: config_edit::parse(text)?,
            config,
            row: 0,
            column: 0,
            popup: None,
            status: None,
            quit_armed: false,
        })
    }

    fn text(&self) -> String {
        self.doc.to_string()
    }

    fn dirty(&self) -> bool {
        self.text() != self.saved
    }

    fn target(&self) -> Target {
        match self.row {
            0 => Target::Defaults,
            i => Target::Role(i - 1),
        }
    }

    /// The agent whose options a row edits: its own kind or the default's.
    fn agent_of(&self, target: Target) -> Kind {
        match target {
            Target::Defaults => self.config.kind,
            Target::Role(i) => self.config.roles[i].kind,
        }
    }

    fn duty(&self, i: usize) -> Duty {
        let role = &self.config.roles[i];
        Duty::guess(
            &role.name,
            role.name == self.config.board.writer,
            self.config.roles.len(),
        )
    }

    /// Applies an edit if the result is a valid configuration; otherwise keeps the document
    /// and explains why.
    fn edit(&mut self, change: impl FnOnce(&mut DocumentMut)) -> bool {
        let mut doc = self.doc.clone();
        change(&mut doc);
        match Config::parse(&doc.to_string(), &self.root) {
            Ok(config) => {
                self.doc = doc;
                self.config = config;
                self.quit_armed = false;
                true
            }
            Err(errors) => {
                let first = errors.first().map(|e| e.msg.clone()).unwrap_or_default();
                self.status = Some((format!("not applied: {first}"), true));
                false
            }
        }
    }

    fn effective(&self, target: Target) -> (Option<String>, Option<String>, Vec<String>) {
        let agent = match target {
            Target::Defaults => match self.config.kind {
                Kind::Claude => AgentOptions::Claude(self.config.claude.clone()),
                Kind::Codex => AgentOptions::Codex(self.config.codex.clone()),
                Kind::Pi => AgentOptions::Pi(self.config.pi.clone()),
                Kind::Agy => AgentOptions::Agy(self.config.agy.clone()),
            },
            Target::Role(i) => self.config.roles[i].agent(),
        };
        match agent {
            AgentOptions::Claude(o) => (o.model.clone(), o.effort.clone(), o.args()),
            AgentOptions::Codex(o) => {
                let mut args = o.args();
                args.retain(|a| a != "--no-daemon");
                (o.model.clone(), o.reasoning_effort.clone(), args)
            }
            AgentOptions::Pi(o) => (o.model.clone(), o.thinking.clone(), o.args()),
            AgentOptions::Agy(o) => (o.model.clone(), o.effort.clone(), o.args()),
        }
    }

    /// A cell: the value written on this row, or the inherited one dimmed.
    fn cell(own: Option<String>, effective: Option<String>, none: &str) -> Cell<'static> {
        match (own, effective) {
            (Some(own), _) => Cell::from(own),
            (None, Some(inherited)) => Cell::from(Span::styled(format!("↳ {inherited}"), dim())),
            (None, None) => Cell::from(Span::styled(none.to_string(), dim())),
        }
    }

    fn row_cells(&self, target: Target) -> Vec<Cell<'static>> {
        let agent = self.agent_of(target);
        let (model, effort, _) = self.effective(target);
        let own_kind = config_edit::kind(&self.doc, target);
        let (name, duty) = match target {
            Target::Defaults => (
                Span::styled("defaults", bold()),
                Span::styled("all roles", dim()),
            ),
            Target::Role(i) => (
                Span::raw(self.config.roles[i].name.clone()),
                Span::styled(self.duty(i).as_str(), dim()),
            ),
        };
        let kind = match (target, own_kind) {
            (_, Some(kind)) => Cell::from(kind),
            (Target::Defaults, None) => Cell::from(Span::styled("claude", dim())),
            (Target::Role(_), None) => {
                Cell::from(Span::styled(format!("↳ {}", agent.as_str()), dim()))
            }
        };
        let own = |key: &str| config_edit::option(&self.doc, target, agent, key);
        let default_note = if target == Target::Defaults {
            "agent default"
        } else {
            "default"
        };
        vec![
            Cell::from(name),
            Cell::from(duty),
            kind,
            Self::cell(own("model"), model, default_note),
            Self::cell(own(models::effort_key(agent)), effort, default_note),
        ]
    }

    fn open_picker(&mut self) {
        let target = self.target();
        let agent = self.agent_of(target);
        let inherit = |what: &str| Choice {
            value: None,
            label: "inherit".into(),
            note: what.into(),
            custom: false,
        };
        let plain = |value: &str, note: &str| Choice {
            value: Some(value.to_string()),
            label: value.to_string(),
            note: note.to_string(),
            custom: false,
        };
        let (title, choices) = match COLUMNS[self.column] {
            Column::Agent => {
                let mut choices = Vec::new();
                if target != Target::Defaults {
                    choices.push(inherit(&format!(
                        "project default ({})",
                        self.config.kind.as_str()
                    )));
                }
                for kind in Kind::ALL {
                    choices.push(plain(kind.as_str(), ""));
                }
                ("Agent".to_string(), choices)
            }
            Column::Model => {
                let mut choices = vec![inherit(if target == Target::Defaults {
                    "the agent's own default"
                } else {
                    "the project default"
                })];
                for (model, note) in models::catalog(agent) {
                    choices.push(plain(model, note));
                }
                choices.push(Choice {
                    value: None,
                    label: "custom…".into(),
                    note: "type a model name".into(),
                    custom: true,
                });
                (format!("{} model", agent.as_str()), choices)
            }
            Column::Effort => {
                let mut choices = vec![inherit(if target == Target::Defaults {
                    "the agent's own default"
                } else {
                    "the project default"
                })];
                for effort in models::efforts(agent) {
                    choices.push(plain(effort, ""));
                }
                (
                    format!("{} {}", agent.as_str(), models::effort_key(agent)),
                    choices,
                )
            }
        };
        // The value written on this row, so that the picker starts there.
        let current = match COLUMNS[self.column] {
            Column::Agent => config_edit::kind(&self.doc, target),
            Column::Model => config_edit::option(&self.doc, target, agent, "model"),
            Column::Effort => {
                config_edit::option(&self.doc, target, agent, models::effort_key(agent))
            }
        };
        let index = choices
            .iter()
            .position(|c| !c.custom && c.value == current)
            .or_else(|| current.is_some().then(|| choices.len() - 1))
            .unwrap_or(0);
        self.popup = Some(Popup::Pick {
            title,
            choices,
            index,
        });
    }

    fn write(&mut self, value: Option<String>) {
        let target = self.target();
        let agent = self.agent_of(target);
        let column = COLUMNS[self.column];
        let applied = self.edit(|doc| match column {
            Column::Agent => {
                let kind = value.as_deref().and_then(Kind::parse);
                // Claude is the default when the project has no `kind`.
                if target == Target::Defaults
                    && kind == Some(Kind::Claude)
                    && config_edit::kind(doc, target).is_none()
                {
                    return;
                }
                config_edit::set_kind(doc, target, kind);
            }
            Column::Model => config_edit::set_option(doc, target, agent, "model", value.as_deref()),
            Column::Effort => config_edit::set_option(
                doc,
                target,
                agent,
                models::effort_key(agent),
                value.as_deref(),
            ),
        });
        if applied {
            self.status = None;
        }
    }

    /// Sets every role's model and effort for its agent from `profile`; `Inherit` removes them.
    fn apply_profile(&mut self, profile: Profile) {
        let plan: Vec<(usize, Kind, Option<String>, Option<String>)> = (0..self.config.roles.len())
            .map(|i| {
                let kind = self.config.roles[i].kind;
                let (model, effort) = match profile.options(kind, self.duty(i)) {
                    AgentOptions::Claude(o) => (o.model, o.effort),
                    AgentOptions::Codex(o) => (o.model, o.reasoning_effort),
                    AgentOptions::Pi(o) => (o.model, o.thinking),
                    AgentOptions::Agy(o) => (o.model, o.effort),
                };
                (i, kind, model, effort)
            })
            .collect();
        let applied = self.edit(|doc| {
            for (i, kind, model, effort) in plan {
                let target = Target::Role(i);
                // pi keeps its provider's model: a profile never removes one it does not set.
                if profile == Profile::Inherit || model.is_some() {
                    config_edit::set_option(doc, target, kind, "model", model.as_deref());
                }
                config_edit::set_option(
                    doc,
                    target,
                    kind,
                    models::effort_key(kind),
                    effort.as_deref(),
                );
            }
        });
        if applied {
            self.status = Some((
                format!("applied the {} profile to every role", profile.as_str()),
                false,
            ));
        }
    }

    fn save(&mut self) {
        let path = self.root.join(config::CONFIG_FILE);
        let text = self.text();
        match crate::files::write(&self.root, &path, &text) {
            Ok(()) => {
                self.saved = text;
                self.status = Some((format!("saved {}", path.display()), false));
            }
            Err(e) => self.status = Some((e, true)),
        }
    }

    fn popup_key(&mut self, key: KeyEvent) {
        let Some(popup) = &mut self.popup else { return };
        match popup {
            Popup::Pick { choices, index, .. } => match key.code {
                KeyCode::Esc | KeyCode::Char('q') => self.popup = None,
                KeyCode::Enter | KeyCode::Char(' ') => {
                    let choice = choices[*index].clone();
                    if choice.custom {
                        let current = self.current_text();
                        self.popup = Some(Popup::Custom {
                            title: "Model name".into(),
                            input: TextInput::new(&current),
                        });
                    } else {
                        self.popup = None;
                        self.write(choice.value);
                    }
                }
                _ => {
                    move_in_list(key, index, choices.len());
                }
            },
            Popup::Custom { input, .. } => match key.code {
                KeyCode::Esc => self.popup = None,
                KeyCode::Enter => {
                    let value = input.value.trim().to_string();
                    self.popup = None;
                    self.write((!value.is_empty()).then_some(value));
                }
                _ => {
                    input.key(key);
                }
            },
            Popup::Profile { index } => match key.code {
                KeyCode::Esc | KeyCode::Char('q') => self.popup = None,
                KeyCode::Enter => {
                    let profile = Profile::ALL[*index];
                    self.popup = None;
                    self.apply_profile(profile);
                }
                _ => {
                    move_in_list(key, index, Profile::ALL.len());
                }
            },
        }
    }

    fn current_text(&self) -> String {
        let target = self.target();
        config_edit::option(&self.doc, target, self.agent_of(target), "model").unwrap_or_default()
    }

    fn draw_popup(&self, frame: &mut Frame, area: Rect) {
        let Some(popup) = &self.popup else { return };
        let (title, lines, width) = match popup {
            Popup::Pick {
                title,
                choices,
                index,
            } => {
                let lines: Vec<Line> = choices
                    .iter()
                    .enumerate()
                    .map(|(i, c)| {
                        let label = format!(" {:<16}", c.label);
                        if i == *index {
                            Line::from(vec![
                                Span::styled(label, selected()),
                                Span::styled(format!(" {}", c.note), bold()),
                            ])
                        } else {
                            Line::from(vec![
                                Span::raw(label),
                                Span::styled(format!(" {}", c.note), dim()),
                            ])
                        }
                    })
                    .collect();
                (title.clone(), lines, 72)
            }
            Popup::Custom { title, input } => (
                title.clone(),
                vec![
                    input.line(),
                    Line::from(""),
                    Line::styled("Enter saves, an empty name inherits, Esc cancels.", dim()),
                ],
                60,
            ),
            Popup::Profile { index } => {
                let mut lines: Vec<Line> = Profile::ALL
                    .iter()
                    .enumerate()
                    .map(|(i, p)| {
                        let label = format!(" {:<10}", p.as_str());
                        if i == *index {
                            Line::from(vec![
                                Span::styled(label, selected()),
                                Span::styled(format!(" {}", p.description()), bold()),
                            ])
                        } else {
                            Line::from(vec![
                                Span::raw(label),
                                Span::styled(format!(" {}", p.description()), dim()),
                            ])
                        }
                    })
                    .collect();
                lines.push(Line::from(""));
                lines.push(Line::styled(
                    "Sets every role's model and effort for its agent, by its duty.",
                    dim(),
                ));
                ("Apply a model profile".into(), lines, 92)
            }
        };
        let rect = centered(area, width, lines.len() as u16 + 2);
        frame.render_widget(Clear, rect);
        frame.render_widget(
            Paragraph::new(lines).block(
                Block::bordered()
                    .border_type(BorderType::Rounded)
                    .title(Span::styled(format!(" {title} "), accent())),
            ),
            rect,
        );
    }
}

impl Screen for Editor {
    type Output = ();

    fn draw(&mut self, frame: &mut Frame) {
        let [header, table, details, status, footer] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Fill(1),
            Constraint::Length(4),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(frame.area());
        let mut title = vec![
            Span::styled(" herdr-crew config ", accent()),
            Span::styled(
                self.root.join(config::CONFIG_FILE).display().to_string(),
                dim(),
            ),
        ];
        if self.dirty() {
            title.push(Span::styled("  ● unsaved", warn()));
        }
        frame.render_widget(Paragraph::new(Line::from(title)), header);

        let mut rows = vec![Row::new(self.row_cells(Target::Defaults))];
        for i in 0..self.config.roles.len() {
            rows.push(Row::new(self.row_cells(Target::Role(i))));
        }
        let widths = [
            Constraint::Fill(3),
            Constraint::Length(11),
            Constraint::Length(9),
            Constraint::Fill(2),
            Constraint::Length(14),
        ];
        let mut state = TableState::default()
            .with_selected(Some(self.row))
            .with_selected_column(Some(self.column + 2));
        frame.render_stateful_widget(
            Table::new(rows, widths)
                .header(Row::new(["Role", "Duty", "Agent", "Model", "Effort"]).style(bold()))
                .column_spacing(2)
                .row_highlight_style(bold())
                .cell_highlight_style(selected())
                .block(Block::bordered().border_type(BorderType::Rounded)),
            table,
            &mut state,
        );

        let target = self.target();
        let agent = self.agent_of(target);
        let (_, _, args) = self.effective(target);
        let launch = if args.is_empty() {
            format!(
                "{} (no model flags: uses its own configuration)",
                agent.as_str()
            )
        } else {
            format!("{} {}", agent.as_str(), args.join(" "))
        };
        let scope = match target {
            Target::Defaults => format!(
                "Defaults for every role without its own value; options go to [{}].",
                agent.as_str()
            ),
            Target::Role(i) => format!(
                "Options of {} go to [roles.{}]; ↳ marks inherited values.",
                self.config.roles[i].name,
                agent.as_str()
            ),
        };
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(vec![Span::styled("Launch: ", bold()), Span::raw(launch)]),
                Line::styled(scope, dim()),
                Line::styled(
                    "Changes apply to new conversations; running sessions keep their model.",
                    dim(),
                ),
            ])
            .wrap(Wrap { trim: false }),
            details,
        );
        if let Some((text, is_error)) = &self.status {
            let style = if *is_error { error() } else { accent() };
            frame.render_widget(
                Paragraph::new(Line::styled(format!(" {text}"), style)),
                status,
            );
        }
        frame.render_widget(
            Paragraph::new(hints(&[
                ("↑↓←→", "move"),
                ("Enter", "change"),
                ("Del", "inherit"),
                ("p", "profile"),
                ("s", "save"),
                ("q", "quit"),
            ])),
            footer,
        );
        self.draw_popup(frame, frame.area());
    }

    fn key(&mut self, key: KeyEvent) -> Flow<()> {
        if self.popup.is_some() {
            self.popup_key(key);
            return Flow::Stay;
        }
        let rows = self.config.roles.len() + 1;
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => {
                if !self.dirty() || self.quit_armed {
                    return Flow::Done(());
                }
                self.quit_armed = true;
                self.status = Some((
                    "unsaved changes: s saves, q again quits without saving".into(),
                    true,
                ));
                return Flow::Stay;
            }
            KeyCode::Char('s') => self.save(),
            KeyCode::Char('p') => {
                self.popup = Some(Popup::Profile {
                    index: Profile::ALL
                        .iter()
                        .position(|p| *p == Profile::default())
                        .unwrap(),
                })
            }
            KeyCode::Enter | KeyCode::Char(' ') => self.open_picker(),
            KeyCode::Delete | KeyCode::Backspace => self.write(None),
            KeyCode::Left | KeyCode::Char('h') => self.column = self.column.saturating_sub(1),
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Tab => {
                self.column = (self.column + 1).min(COLUMNS.len() - 1)
            }
            KeyCode::Up | KeyCode::Char('k') => self.row = self.row.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.row = (self.row + 1).min(rows - 1),
            _ => {}
        }
        Flow::Stay
    }
}

/// Opens the editor on the project's `crew.toml`.
pub fn run(root: &Path) -> Result<(), String> {
    let path = root.join(config::CONFIG_FILE);
    crate::files::validate(root, &path, crate::files::Kind::File)?;
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let mut editor = Editor::new(root, &text)?;
    super::run(&mut editor).map_err(|e: io::Error| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::testing::{draw, send};

    const TEXT: &str = include_str!("../../tests/fixtures/crew-worktrees.toml");

    fn editor() -> Editor {
        Editor::new(Path::new("/r/globex"), TEXT).unwrap()
    }

    #[test]
    fn a_role_gets_its_own_model_and_effort() {
        let mut e = editor();
        // Row 1 is the first role; column 1 the model.
        send(&mut e, "{down}{right}{enter}");
        let screen = draw(&mut e, 120, 30);
        assert!(screen.contains("claude model"), "{screen}");
        assert!(screen.contains("sonnet"), "{screen}");
        send(&mut e, "{down}{down}{enter}");
        send(&mut e, "{right}{enter}{down}{down}{down}{enter}");
        let lead = &e.config.roles[0];
        assert_eq!(lead.claude.model.as_deref(), Some("opus"));
        assert_eq!(lead.claude.effort.as_deref(), Some("high"));
        let screen = draw(&mut e, 120, 30);
        assert!(
            screen.contains("claude --model opus --effort high"),
            "{screen}"
        );
        assert!(screen.contains("unsaved"), "{screen}");
        // Delete returns to the inherited value.
        send(&mut e, "{bs}");
        assert_eq!(e.config.roles[0].claude.effort, None);
        assert!(
            e.text().contains("[roles.claude]\nmodel = \"opus\""),
            "{}",
            e.text()
        );
    }

    #[test]
    fn defaults_change_the_agent_and_roles_inherit_its_options() {
        let mut e = editor();
        send(&mut e, "{enter}{down}{enter}");
        assert_eq!(e.config.kind, Kind::Codex);
        assert!(e.config.roles.iter().all(|r| r.kind == Kind::Codex));
        send(
            &mut e,
            "{right}{right}{enter}{down}{down}{down}{down}{down}{enter}",
        );
        assert!(
            e.text().contains("[codex]\nreasoning_effort = \"high\""),
            "{}",
            e.text()
        );
        assert_eq!(
            e.config.roles[1].codex.reasoning_effort.as_deref(),
            Some("high")
        );
        let screen = draw(&mut e, 120, 30);
        assert!(screen.contains("↳ high"), "{screen}");
    }

    #[test]
    fn a_custom_model_is_typed_and_profiles_apply_by_duty() {
        let mut e = editor();
        send(&mut e, "{down}{right}{enter}{up}{enter}");
        assert!(matches!(e.popup, Some(Popup::Custom { .. })));
        send(&mut e, "claude-opus-5-5{enter}");
        assert_eq!(
            e.config.roles[0].claude.model.as_deref(),
            Some("claude-opus-5-5")
        );
        send(&mut e, "p{down}{enter}");
        assert!(e.popup.is_none());
        for (i, role) in e.config.roles.iter().enumerate() {
            let expected = Profile::Quality.pick(e.duty(i)).unwrap();
            assert_eq!(
                role.claude.effort.as_deref(),
                Some(expected.1),
                "{}",
                role.name
            );
        }
        // The lead writes the board; reviewers judge with the frontier tier.
        assert_eq!(e.config.roles[0].claude.model.as_deref(), Some("fable"));
        send(&mut e, "p{up}{up}{enter}");
        assert!(e.config.roles.iter().all(|r| r.claude.model.is_none()));
        assert_eq!(e.text(), TEXT);
    }

    #[test]
    fn quitting_with_changes_asks_once() {
        let mut e = editor();
        send(&mut e, "{down}{enter}{down}{down}{enter}");
        assert!(e.dirty());
        assert!(matches!(send(&mut e, "q"), Some(Flow::Stay)));
        assert!(matches!(send(&mut e, "q"), Some(Flow::Done(()))));
    }
}
