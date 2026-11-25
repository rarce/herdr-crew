//! `init` in a terminal: the setup questions as steps, with live previews of the roles, their
//! models and the generated configuration. Flags given on the command line prefill answers.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Cell, Paragraph, Row, Table, TableState, Wrap};

use super::{
    Choice, Flow, Screen, TextInput, accent, bold, dim, draw_custom_popup, draw_pick_popup,
    effort_choices, error, hints, model_choices, move_in_list, selected, warn,
};
use crate::agent::Kind;
use crate::git;
use crate::init::{self, MAX_PREFIX, Options, Preset, RoleModelOverride, Selection, Topology};
use crate::models::{self, Duty, Profile};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Name,
    Workflow,
    Agent,
    Topology,
    Models,
    Worktrees,
    Base,
    Ignore,
    Review,
}

impl Step {
    fn title(self) -> &'static str {
        match self {
            Self::Name => "Project",
            Self::Workflow => "Workflow",
            Self::Agent => "Agent",
            Self::Topology => "Topology",
            Self::Models => "Models",
            Self::Worktrees => "Worktrees",
            Self::Base => "Base branch",
            Self::Ignore => "Git ignore",
            Self::Review => "Review",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ModelsFocus {
    Profile,
    Roles,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RoleColumn {
    Model,
    Effort,
}

const ROLE_COLUMNS: [RoleColumn; 2] = [RoleColumn::Model, RoleColumn::Effort];

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
}

/// The generated configuration, shown before saving.
struct Review {
    text: String,
    addition: String,
    warning: Option<String>,
    scroll: u16,
}

pub struct Wizard {
    root: PathBuf,
    step: Step,
    name: TextInput,
    preset: usize,
    agent: usize,
    topology: usize,
    profile: usize,
    overrides: BTreeMap<String, RoleModelOverride>,
    models_focus: ModelsFocus,
    role_row: usize,
    role_col: usize,
    popup: Option<Popup>,
    isolated_tabs: bool,
    isolated_tree: bool,
    explicit_tree: bool,
    explicit_tabs: bool,
    explicit_base: bool,
    base: TextInput,
    ignore: bool,
    has_remote: bool,
    /// A base that is not fetched, which a second Enter accepts.
    unfetched: Option<String>,
    message: Option<String>,
    review: Option<Review>,
}

fn index_of<T: PartialEq>(all: &[T], value: Option<T>, default: T) -> usize {
    let value = value.unwrap_or(default);
    all.iter().position(|v| *v == value).unwrap_or(0)
}

impl Wizard {
    pub fn new(root: &Path, options: &Options) -> Self {
        let suggested_base = git::default_base(root);
        let has_remote = git::has_remote(root).unwrap_or(false);
        let explicit_tree = options.topology == Some(Topology::Tree);
        let explicit_tabs = options.topology == Some(Topology::Tabs);
        let default_preset = if explicit_tree {
            Preset::Review
        } else {
            Preset::Solo
        };
        let default_agent = if explicit_tree {
            Kind::Codex
        } else {
            Kind::default()
        };
        Wizard {
            root: root.to_path_buf(),
            step: Step::Name,
            name: TextInput::new(
                &options
                    .name
                    .clone()
                    .unwrap_or_else(|| init::suggested_name(root)),
            ),
            preset: index_of(&Preset::ALL, options.preset, default_preset),
            agent: index_of(&Kind::ALL, options.agent, default_agent),
            topology: index_of(&Topology::ALL, options.topology, Topology::Tabs),
            profile: index_of(&Profile::ALL, options.models, Profile::default()),
            overrides: BTreeMap::new(),
            models_focus: ModelsFocus::Profile,
            role_row: 0,
            role_col: 0,
            popup: None,
            isolated_tabs: !options.shared_checkout
                && (options.base.is_some() || suggested_base.is_some()),
            isolated_tree: !options.shared_checkout,
            explicit_tree,
            explicit_tabs,
            explicit_base: options.base.is_some(),
            base: TextInput::new(
                options
                    .base
                    .as_deref()
                    .or(suggested_base.as_deref())
                    .unwrap_or(""),
            ),
            ignore: !options.no_ignore,
            has_remote,
            unfetched: None,
            message: None,
            review: None,
        }
    }

    fn preset(&self) -> Preset {
        Preset::ALL[self.preset]
    }

    fn agent(&self) -> Kind {
        Kind::ALL[self.agent]
    }

    fn uses_agy_tree(&self) -> bool {
        init::uses_agy_tree(self.agent(), self.preset())
    }

    fn supports_codex_tree(&self) -> bool {
        init::supports_codex_tree(self.agent(), self.preset())
    }

    fn topology(&self) -> Topology {
        init::effective_topology(self.agent(), self.preset(), Topology::ALL[self.topology])
    }

    fn is_codex_tree(&self) -> bool {
        init::is_codex_tree(self.agent(), self.preset(), self.topology())
    }

    fn is_tree(&self) -> bool {
        self.topology() == Topology::Tree
    }

    fn profile(&self) -> Profile {
        Profile::ALL[self.profile]
    }

    fn configured_roles(&self) -> &'static [(&'static str, Duty)] {
        init::configured_roles(self.agent(), self.preset(), self.topology())
    }

    fn uses_worktrees(&self) -> bool {
        if self.is_tree() {
            return false;
        }
        match self.preset() {
            Preset::Parallel => true,
            Preset::Review => self.isolated_tabs,
            _ => false,
        }
    }

    /// The steps for the current answers: topology and worktree questions only where they apply.
    fn steps(&self) -> Vec<Step> {
        let mut steps = vec![Step::Name, Step::Workflow, Step::Agent];
        if self.supports_codex_tree() {
            steps.push(Step::Topology);
        }
        steps.push(Step::Models);
        if self.preset() == Preset::Review && !self.is_codex_tree() {
            steps.push(Step::Worktrees);
        }
        if self.uses_worktrees() {
            steps.push(Step::Base);
        }
        steps.extend([Step::Ignore, Step::Review]);
        steps
    }

    fn selection(&self) -> Selection {
        let topology = self.topology();
        let tree_isolated = self.uses_agy_tree()
            && match self.preset() {
                Preset::Parallel => true,
                Preset::Review => self.isolated_tree,
                _ => false,
            };
        Selection {
            agent: self.agent(),
            name: self.name.value.clone(),
            preset: self.preset(),
            topology,
            profile: self.profile(),
            overrides: self.overrides.clone(),
            base: self
                .uses_worktrees()
                .then(|| self.base.value.trim().to_string()),
            tree_isolated,
            ignore: self.ignore,
        }
    }

    fn clear_overrides(&mut self) {
        self.overrides.clear();
        self.models_focus = ModelsFocus::Profile;
        self.role_row = 0;
        self.role_col = 0;
        self.popup = None;
    }

    fn is_customized(&self) -> bool {
        let agent = self.agent();
        let profile = self.profile();
        self.configured_roles().iter().any(|(suffix, duty)| {
            init::role_options(agent, profile, *duty, self.overrides.get(*suffix))
                != profile.options(agent, *duty)
        })
    }

    fn role_effective(&self, row: usize) -> (Option<String>, Option<String>) {
        let (suffix, duty) = self.configured_roles()[row];
        let options = init::role_options(
            self.agent(),
            self.profile(),
            duty,
            self.overrides.get(suffix),
        );
        init::options_model_effort(&options)
    }

    fn open_role_picker(&mut self) {
        let agent = self.agent();
        let (current_model, current_effort) = self.role_effective(self.role_row);
        let inherit_note = "the agent's own default";
        let (title, choices, current) = match ROLE_COLUMNS[self.role_col] {
            RoleColumn::Model => (
                format!("{} model", agent.as_str()),
                model_choices(agent, inherit_note),
                current_model,
            ),
            RoleColumn::Effort => (
                format!("{} {}", agent.as_str(), models::effort_key(agent)),
                effort_choices(agent, inherit_note),
                current_effort,
            ),
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

    fn apply_role_value(&mut self, value: Option<String>) {
        let agent = self.agent();
        let profile = self.profile();
        let (suffix, duty) = self.configured_roles()[self.role_row];
        let (base_model, base_effort) = init::options_model_effort(&profile.options(agent, duty));
        let mut candidate = self.overrides.get(suffix).cloned().unwrap_or_default();
        match ROLE_COLUMNS[self.role_col] {
            RoleColumn::Model => {
                candidate.model = (value != base_model).then_some(value);
            }
            RoleColumn::Effort => {
                candidate.effort = (value != base_effort).then_some(value);
            }
        }
        let effective = init::role_options(agent, profile, duty, Some(&candidate));
        match init::validate_role_options(&effective) {
            Ok(()) => {
                if candidate.model.is_none() && candidate.effort.is_none() {
                    self.overrides.remove(suffix);
                } else {
                    self.overrides.insert(suffix.to_string(), candidate);
                }
                self.message = None;
            }
            Err(problem) => {
                self.message = Some(format!("not applied: {problem}"));
            }
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
                        let (current_model, _) = self.role_effective(self.role_row);
                        self.popup = Some(Popup::Custom {
                            title: "Model name".into(),
                            input: TextInput::new(current_model.as_deref().unwrap_or("")),
                        });
                    } else {
                        self.popup = None;
                        self.apply_role_value(choice.value);
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
                    self.apply_role_value((!value.is_empty()).then_some(value));
                }
                _ => {
                    input.key(key);
                }
            },
        }
    }

    fn draw_popup(&self, frame: &mut Frame, area: Rect) {
        let Some(popup) = &self.popup else { return };
        match popup {
            Popup::Pick {
                title,
                choices,
                index,
            } => draw_pick_popup(frame, area, title, choices, *index),
            Popup::Custom { title, input } => draw_custom_popup(frame, area, title, input),
        }
    }

    /// Checks the current step's answer; the message explains a refusal.
    fn accept(&mut self) -> Result<(), String> {
        match self.step {
            Step::Name if !init::valid_name(&self.name.value) => Err(format!(
                "Use 1-{MAX_PREFIX} characters: start with a-z, then a-z, 0-9, _ or -."
            )),
            Step::Workflow if self.explicit_tree && self.preset() == Preset::Solo => {
                Err(init::TREE_PRESET_ERROR.into())
            }
            Step::Workflow
                if self.explicit_base
                    && !matches!(self.preset(), Preset::Review | Preset::Parallel) =>
            {
                Err("--base requires a review or parallel preset using worktrees".into())
            }
            Step::Agent
                if self.explicit_tree && !matches!(self.agent(), Kind::Codex | Kind::Agy) =>
            {
                Err(init::TREE_AGENT_ERROR.into())
            }
            Step::Agent if self.explicit_tabs && self.uses_agy_tree() => {
                Err(init::AGY_TABS_ERROR.into())
            }
            Step::Agent if self.explicit_base && self.uses_agy_tree() => {
                Err(init::AGY_TREE_BASE_ERROR.into())
            }
            Step::Agent
                if self.preset() == Preset::Parallel
                    && !matches!(self.agent(), Kind::Codex | Kind::Agy)
                    && !self.has_remote =>
            {
                Err(init::no_remote().lines[0].clone())
            }
            Step::Topology if self.is_codex_tree() && self.explicit_base => {
                Err(init::TREE_BASE_INTERACTIVE_ERROR.into())
            }
            Step::Topology
                if self.preset() == Preset::Parallel && !self.is_tree() && !self.has_remote =>
            {
                Err(init::no_remote().lines[0].clone())
            }
            Step::Worktrees if self.explicit_base && !self.is_tree() && !self.isolated_tabs => {
                Err("--base requires separate worktrees; choose separate or omit --base".into())
            }
            Step::Worktrees if !self.is_tree() && self.isolated_tabs && !self.has_remote => {
                Err(init::no_remote().lines[0].clone())
            }
            Step::Base => {
                let base = self.base.value.trim().to_string();
                git::validate_base(&self.root, &base)?;
                if !git::knows_base(&self.root, &base) && self.unfetched.as_deref() != Some(&base) {
                    self.unfetched = Some(base.clone());
                    return Err(format!(
                        "{} Press Enter again to keep it.",
                        init::unfetched(&base)
                    ));
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn go(&mut self, forward: bool) {
        let steps = self.steps();
        let i = steps.iter().position(|s| *s == self.step).unwrap_or(0);
        let next = if forward {
            steps.get(i + 1)
        } else {
            i.checked_sub(1).and_then(|i| steps.get(i))
        };
        if let Some(step) = next {
            self.step = *step;
        }
        if self.step == Step::Review {
            self.prepare_review();
        }
    }

    fn prepare_review(&mut self) {
        let selection = self.selection();
        let result = init::render(&selection, &self.root).and_then(|text| {
            let check = init::ignore_check(&self.root, selection.base.is_some(), selection.ignore)?;
            Ok(Review {
                text,
                addition: check.addition,
                warning: check.warning,
                scroll: 0,
            })
        });
        match result {
            Ok(review) => self.review = Some(review),
            Err(fail) => {
                self.review = None;
                self.message = Some(fail.lines.join(" "));
            }
        }
    }

    fn draw_steps(&self, frame: &mut Frame, area: Rect) {
        let answer = |step: Step| -> String {
            match step {
                Step::Name => self.name.value.clone(),
                Step::Workflow => self.preset().id().into(),
                Step::Agent => self.agent().as_str().into(),
                Step::Topology => self.topology().as_str().into(),
                Step::Models => {
                    if self.is_customized() {
                        format!("{}*", self.profile().as_str())
                    } else {
                        self.profile().as_str().into()
                    }
                }
                Step::Worktrees => {
                    let isolated = if self.is_tree() {
                        self.isolated_tree
                    } else {
                        self.isolated_tabs
                    };
                    if isolated { "separate" } else { "shared" }.into()
                }
                Step::Base => self.base.value.clone(),
                Step::Ignore => if self.ignore { "yes" } else { "no" }.into(),
                Step::Review => String::new(),
            }
        };
        let steps = self.steps();
        let current = steps.iter().position(|s| *s == self.step).unwrap_or(0);
        let lines: Vec<Line> = steps
            .iter()
            .enumerate()
            .map(|(i, step)| {
                let (mark, style) = if i == current {
                    ("▶ ", accent())
                } else if i < current {
                    ("✓ ", bold())
                } else {
                    ("  ", dim())
                };
                let mut spans = vec![Span::styled(format!("{mark}{:<12}", step.title()), style)];
                if i < current {
                    spans.push(Span::styled(answer(*step), dim()));
                }
                Line::from(spans)
            })
            .collect();
        frame.render_widget(
            Paragraph::new(lines).block(
                Block::bordered()
                    .border_type(BorderType::Rounded)
                    .title(" Steps "),
            ),
            area,
        );
    }

    /// A selectable list with the chosen item highlighted and a note per item.
    fn choices(items: &[(String, String)], chosen: usize) -> Vec<Line<'static>> {
        items
            .iter()
            .enumerate()
            .map(|(i, (label, note))| {
                let marker = format!(" {} {label:<10}", i + 1);
                if i == chosen {
                    Line::from(vec![
                        Span::styled(marker, selected()),
                        Span::styled(format!(" {note}"), bold()),
                    ])
                } else {
                    Line::from(vec![
                        Span::raw(marker),
                        Span::styled(format!(" {note}"), dim()),
                    ])
                }
            })
            .collect()
    }

    /// The configured roles with the model and effort the profile (plus overrides) gives them.
    fn roles_table(&self) -> Table<'static> {
        let agent = self.agent();
        let profile = self.profile();
        let rows: Vec<Row> = self
            .configured_roles()
            .iter()
            .map(|(suffix, duty)| {
                let (base_model, base_effort) =
                    init::options_model_effort(&profile.options(agent, *duty));
                let options =
                    init::role_options(agent, profile, *duty, self.overrides.get(*suffix));
                let (model, effort) = init::options_model_effort(&options);
                let cell = |value: Option<String>, base: Option<String>| {
                    let changed = value != base;
                    match value {
                        Some(v) if changed => Cell::from(Span::styled(v, accent())),
                        Some(v) => Cell::from(v),
                        None if changed => Cell::from(Span::styled("agent default", accent())),
                        None => Cell::from(Span::styled("agent default", dim())),
                    }
                };
                Row::new(vec![
                    Cell::from(format!("{}-{suffix}", self.name.value)),
                    Cell::from(duty.as_str()),
                    cell(model, base_model),
                    cell(effort, base_effort),
                ])
            })
            .collect();
        Table::new(
            rows,
            [
                Constraint::Fill(3),
                Constraint::Length(11),
                Constraint::Fill(2),
                Constraint::Length(13),
            ],
        )
        .header(Row::new(["Role", "Duty", "Model", "Effort"]).style(bold()))
        .column_spacing(1)
        .row_highlight_style(bold())
        .cell_highlight_style(selected())
    }

    fn draw_main(&mut self, frame: &mut Frame, area: Rect) {
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .title(Span::styled(format!(" {} ", self.step.title()), accent()));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let question = |text: &str| Line::styled(text.to_string(), bold());
        match self.step {
            Step::Name => {
                let lines = vec![
                    question("Project name / role prefix"),
                    Line::from(""),
                    self.name.line(),
                    Line::from(""),
                    Line::styled(
                        "Names the herdr workspace and prefixes every role, e.g. NAME-lead.",
                        dim(),
                    ),
                    Line::styled(
                        format!("1-{MAX_PREFIX} characters: a-z first, then a-z, 0-9, _ or -."),
                        dim(),
                    ),
                ];
                frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
            }
            Step::Workflow => {
                let items: Vec<(String, String)> = Preset::ALL
                    .iter()
                    .map(|p| {
                        let roles: Vec<&str> = p.roles().iter().map(|(s, _)| *s).collect();
                        (p.id().to_string(), roles.join(", "))
                    })
                    .collect();
                let mut lines = vec![
                    question("Choose a workflow; add sessions only when the task benefits"),
                    Line::from(""),
                ];
                lines.extend(Self::choices(&items, self.preset));
                lines.push(Line::from(""));
                lines.push(Line::from(self.preset().description()));
                if self.preset() == Preset::Parallel && !self.has_remote {
                    lines.push(Line::styled(
                        "Separate tabs need a configured Git remote; agy or Codex tree mode works without one.",
                        warn(),
                    ));
                }
                frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
            }
            Step::Agent => {
                let note = |k: Kind| match k {
                    Kind::Claude => "Claude Code; native SendMessage between sessions",
                    Kind::Codex => "OpenAI Codex CLI; run herdr-crew codex-install after setup",
                    Kind::Pi => "pi; run herdr-crew pi-install after setup",
                    Kind::Agy => "Antigravity CLI (agy); run herdr-crew agy-install after setup",
                };
                let items: Vec<(String, String)> = Kind::ALL
                    .iter()
                    .map(|k| (k.as_str().to_string(), note(*k).to_string()))
                    .collect();
                let mut lines = vec![
                    question("Which coding agent runs the roles?"),
                    Line::from(""),
                ];
                lines.extend(Self::choices(&items, self.agent));
                lines.push(Line::from(""));
                if self.explicit_base && self.uses_agy_tree() {
                    lines.push(Line::styled(init::AGY_TREE_BASE_ERROR, warn()));
                } else if self.explicit_tabs && self.uses_agy_tree() {
                    lines.push(Line::styled(init::AGY_TABS_ERROR, warn()));
                } else if self.preset() == Preset::Parallel
                    && !matches!(self.agent(), Kind::Codex | Kind::Agy)
                    && !self.has_remote
                {
                    lines.push(Line::styled(
                        "Parallel tabs need a configured Git remote (or choose codex tree mode or agy).",
                        warn(),
                    ));
                } else if self.uses_agy_tree() {
                    lines.push(Line::styled(
                        "Multi-role agy uses native tree mode (1 coordinator tab + subagents, no remote needed).",
                        dim(),
                    ));
                } else if self.supports_codex_tree() {
                    lines.push(Line::styled(
                        "Multi-role Codex can run as independent tabs (default) or native tree mode.",
                        dim(),
                    ));
                } else {
                    lines.push(Line::styled(
                        "Roles can use different agents later: herdr-crew config.",
                        dim(),
                    ));
                }
                frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
            }
            Step::Topology => {
                let items: Vec<(String, String)> = Topology::ALL
                    .iter()
                    .map(|t| (t.as_str().to_string(), t.description().to_string()))
                    .collect();
                let mut lines = vec![question("How should Codex roles run?"), Line::from("")];
                lines.extend(Self::choices(&items, self.topology));
                lines.push(Line::from(""));
                if self.explicit_base && self.is_codex_tree() {
                    lines.push(Line::styled(init::TREE_BASE_INTERACTIVE_ERROR, warn()));
                } else if self.explicit_base {
                    lines.push(Line::styled(
                        "--base was given, so tabs topology is preselected to keep worktrees.",
                        warn(),
                    ));
                } else if self.preset() == Preset::Parallel && !self.is_tree() && !self.has_remote {
                    lines.push(Line::styled(
                        "Parallel tabs need worktrees, which need a configured Git remote (or choose tree mode).",
                        warn(),
                    ));
                } else {
                    lines.push(Line::styled(
                        "tree mode uses one Codex coordinator tab and native subagents in the shared checkout (requires features.multi_agent_v2 = true in Codex).",
                        dim(),
                    ));
                }
                frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
            }
            Step::Models => {
                let items: Vec<(String, String)> = Profile::ALL
                    .iter()
                    .map(|p| (p.as_str().to_string(), p.description().to_string()))
                    .collect();
                let prompt_text = if self.is_tree() {
                    "Model and effort for the coordinator role"
                } else {
                    "Models and effort for each role"
                };
                let mut lines = vec![question(prompt_text), Line::from("")];
                lines.extend(Self::choices(&items, self.profile));
                let height = lines.len() as u16 + 1;
                let [top, table, foot] = Layout::vertical([
                    Constraint::Length(height),
                    Constraint::Fill(1),
                    Constraint::Length(2),
                ])
                .areas(inner);
                frame.render_widget(Paragraph::new(lines), top);
                if self.models_focus == ModelsFocus::Roles {
                    let mut state = TableState::default()
                        .with_selected(Some(self.role_row))
                        .with_selected_column(Some(self.role_col + 2));
                    frame.render_stateful_widget(self.roles_table(), table, &mut state);
                } else {
                    frame.render_widget(self.roles_table(), table);
                }
                let foot_text = if self.models_focus == ModelsFocus::Roles {
                    "Editing roles: Enter changes a cell, Del uses agent default, r resets to profile, Esc/Tab finishes."
                } else if self.agent() == Kind::Pi && self.profile() != Profile::Inherit {
                    "pi keeps your provider's model; press e or Tab to customize roles."
                } else if self.is_tree() {
                    "Press e or Tab to customize the coordinator's model or effort, or Enter to continue."
                } else {
                    "Press e or Tab to customize any role's model or effort, or Enter to continue."
                };
                frame.render_widget(
                    Paragraph::new(Line::styled(foot_text, dim())).wrap(Wrap { trim: false }),
                    foot,
                );
            }
            Step::Worktrees => {
                let (title, items, chosen) = if self.is_tree() {
                    (
                        "Isolate implementation workers in native worktrees?",
                        vec![
                            (
                                "separate".to_string(),
                                "isolated native worktrees (Workspace: \"share\", recommended)"
                                    .to_string(),
                            ),
                            (
                                "shared".to_string(),
                                "shared main checkout (Workspace: \"inherit\")".to_string(),
                            ),
                        ],
                        usize::from(!self.isolated_tree),
                    )
                } else {
                    (
                        "Use separate worktrees for development and review?",
                        vec![
                            (
                                "separate".to_string(),
                                "developer and reviewer get their own worktrees (recommended)"
                                    .to_string(),
                            ),
                            (
                                "shared".to_string(),
                                "everyone works in the main checkout; no remote needed".to_string(),
                            ),
                        ],
                        usize::from(!self.isolated_tabs),
                    )
                };
                let mut lines = vec![question(title), Line::from("")];
                lines.extend(Self::choices(&items, chosen));
                frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
            }
            Step::Base => {
                let lines = vec![
                    question("Worktree base (configured remote/branch)"),
                    Line::from(""),
                    self.base.line(),
                    Line::from(""),
                    Line::styled(
                        "Worktrees start from a remote branch, not uncommitted local changes. No fetch runs during setup.",
                        dim(),
                    ),
                ];
                frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
            }
            Step::Ignore => {
                let items = vec![
                    (
                        "yes".to_string(),
                        "append rules for board, prompts and runtime files to .gitignore"
                            .to_string(),
                    ),
                    ("no".to_string(), "manage those rules yourself".to_string()),
                ];
                let mut lines = vec![
                    question("Ignore generated crew files in .gitignore?"),
                    Line::from(""),
                ];
                lines.extend(Self::choices(&items, usize::from(!self.ignore)));
                lines.push(Line::from(""));
                lines.push(Line::styled(
                    ".herdr/crew.toml itself stays versionable.",
                    dim(),
                ));
                frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
            }
            Step::Review => {
                let summary = init::summary(&self.selection());
                let Some(review) = &mut self.review else {
                    frame.render_widget(
                        Paragraph::new(Line::styled(
                            "The configuration could not be generated.",
                            error(),
                        )),
                        inner,
                    );
                    return;
                };
                let mut text = Text::default();
                for line in summary.lines() {
                    text.push_line(Line::styled(line.to_string(), bold()));
                }
                text.push_line(Line::from(""));
                if let Some(warning) = &review.warning {
                    text.push_line(Line::styled(warning.clone(), warn()));
                    text.push_line(Line::from(""));
                }
                if !review.addition.trim().is_empty() {
                    text.push_line(Line::styled(".gitignore additions:", bold()));
                    for line in review.addition.trim().lines() {
                        text.push_line(Line::styled(line.to_string(), dim()));
                    }
                    text.push_line(Line::from(""));
                }
                text.push_line(Line::styled(".herdr/crew.toml:", bold()));
                for line in review.text.lines() {
                    let style = if line.starts_with('#') {
                        dim()
                    } else if line.starts_with('[') {
                        accent()
                    } else {
                        Default::default()
                    };
                    text.push_line(Line::styled(line.to_string(), style));
                }
                let max = (text.lines.len() as u16).saturating_sub(inner.height);
                review.scroll = review.scroll.min(max);
                frame.render_widget(
                    Paragraph::new(text)
                        .wrap(Wrap { trim: false })
                        .scroll((review.scroll, 0)),
                    inner,
                );
            }
        }
    }
}

impl Screen for Wizard {
    type Output = (Selection, String);

    fn draw(&mut self, frame: &mut Frame) {
        let [header, body, message, footer] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Fill(1),
            Constraint::Length(2),
            Constraint::Length(1),
        ])
        .areas(frame.area());
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" herdr-crew setup ", accent()),
                Span::styled(self.root.display().to_string(), dim()),
            ])),
            header,
        );
        let [steps, main] =
            Layout::horizontal([Constraint::Length(30), Constraint::Fill(1)]).areas(body);
        self.draw_steps(frame, steps);
        self.draw_main(frame, main);
        if let Some(text) = &self.message {
            let style = if self.unfetched.is_some() && self.step == Step::Base {
                warn()
            } else {
                error()
            };
            frame.render_widget(
                Paragraph::new(Line::styled(format!(" {text}"), style)).wrap(Wrap { trim: false }),
                message,
            );
        }
        let keys: &[(&str, &str)] = match &self.popup {
            Some(Popup::Custom { .. }) => &[("Enter", "apply"), ("Esc", "cancel")],
            Some(Popup::Pick { .. }) => {
                &[("↑↓ 1-9", "choose"), ("Enter", "apply"), ("Esc", "cancel")]
            }
            None => match self.step {
                Step::Name | Step::Base => {
                    &[("Enter", "next"), ("Esc", "back"), ("Ctrl-C", "cancel")]
                }
                Step::Models if self.models_focus == ModelsFocus::Roles => &[
                    ("↑↓←→", "move"),
                    ("Enter", "change"),
                    ("Del", "inherit"),
                    ("r", "reset"),
                    ("Esc", "done"),
                ],
                Step::Models => &[
                    ("↑↓ 1-4", "profile"),
                    ("e / Tab", "customize"),
                    ("Enter", "next"),
                    ("Esc", "back"),
                    ("q", "cancel"),
                ],
                Step::Review => &[
                    ("Enter", "create"),
                    ("↑↓ PgUp PgDn", "scroll"),
                    ("Esc", "back"),
                    ("q", "cancel"),
                ],
                _ => &[
                    ("↑↓ 1-9", "choose"),
                    ("Enter", "next"),
                    ("Esc", "back"),
                    ("q", "cancel"),
                ],
            },
        };
        frame.render_widget(Paragraph::new(hints(keys)), footer);
        self.draw_popup(frame, frame.area());
    }

    fn key(&mut self, key: KeyEvent) -> Flow<Self::Output> {
        if self.popup.is_some() {
            self.popup_key(key);
            return Flow::Stay;
        }
        if self.step == Step::Models && self.models_focus == ModelsFocus::Roles {
            let roles = self.configured_roles().len();
            match key.code {
                KeyCode::Esc | KeyCode::Tab => {
                    self.models_focus = ModelsFocus::Profile;
                    self.message = None;
                }
                KeyCode::Char('q') => return Flow::Cancel,
                KeyCode::Enter | KeyCode::Char(' ') => self.open_role_picker(),
                KeyCode::Delete | KeyCode::Backspace => self.apply_role_value(None),
                KeyCode::Char('r') => {
                    self.overrides.clear();
                    self.message = None;
                }
                KeyCode::Up | KeyCode::Char('k') => self.role_row = self.role_row.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => {
                    self.role_row = (self.role_row + 1).min(roles.saturating_sub(1));
                }
                KeyCode::Left | KeyCode::Char('h') => {
                    self.role_col = self.role_col.saturating_sub(1);
                }
                KeyCode::Right | KeyCode::Char('l') => {
                    self.role_col = (self.role_col + 1).min(ROLE_COLUMNS.len() - 1);
                }
                _ => {}
            }
            return Flow::Stay;
        }
        let typing = matches!(self.step, Step::Name | Step::Base);
        if key.code != KeyCode::Enter {
            self.message = None;
        }
        match key.code {
            KeyCode::Esc if self.step == Step::Name => return Flow::Cancel,
            KeyCode::Esc => {
                self.go(false);
                return Flow::Stay;
            }
            KeyCode::Char('q') if !typing => return Flow::Cancel,
            KeyCode::Enter if self.step == Step::Review => {
                return match self.review.take() {
                    Some(review) => Flow::Done((self.selection(), review.text)),
                    None => {
                        self.prepare_review();
                        Flow::Stay
                    }
                };
            }
            KeyCode::Enter => {
                match self.accept() {
                    Ok(()) => {
                        self.message = None;
                        self.go(true);
                    }
                    Err(why) => self.message = Some(why),
                }
                return Flow::Stay;
            }
            _ => {}
        }
        match self.step {
            Step::Name => {
                self.name.key(key);
            }
            Step::Base => {
                if self.base.key(key) {
                    self.unfetched = None;
                }
            }
            Step::Workflow => {
                let prev = self.preset;
                if move_in_list(key, &mut self.preset, Preset::ALL.len()) && self.preset != prev {
                    self.clear_overrides();
                }
            }
            Step::Agent => {
                let prev = self.agent;
                if move_in_list(key, &mut self.agent, Kind::ALL.len()) && self.agent != prev {
                    self.clear_overrides();
                }
            }
            Step::Topology => {
                let prev = self.topology;
                if move_in_list(key, &mut self.topology, Topology::ALL.len())
                    && self.topology != prev
                {
                    self.clear_overrides();
                }
            }
            Step::Models => match key.code {
                KeyCode::Char('e') | KeyCode::Char('c') | KeyCode::Tab => {
                    self.models_focus = ModelsFocus::Roles;
                    self.role_row = self
                        .role_row
                        .min(self.configured_roles().len().saturating_sub(1));
                }
                _ => {
                    let prev = self.profile;
                    if move_in_list(key, &mut self.profile, Profile::ALL.len())
                        && self.profile != prev
                    {
                        self.overrides.clear();
                    }
                }
            },
            Step::Worktrees => {
                let isolated = if self.is_tree() {
                    &mut self.isolated_tree
                } else {
                    &mut self.isolated_tabs
                };
                let mut i = usize::from(!*isolated);
                if move_in_list(key, &mut i, 2) {
                    *isolated = i == 0;
                }
            }
            Step::Ignore => {
                let mut i = usize::from(!self.ignore);
                match key.code {
                    KeyCode::Char('y') => self.ignore = true,
                    KeyCode::Char('n') => self.ignore = false,
                    _ if move_in_list(key, &mut i, 2) => self.ignore = i == 0,
                    _ => {}
                }
            }
            Step::Review => {
                if let Some(review) = &mut self.review {
                    review.scroll = match key.code {
                        KeyCode::Up | KeyCode::Char('k') => review.scroll.saturating_sub(1),
                        KeyCode::Down | KeyCode::Char('j') => review.scroll.saturating_add(1),
                        KeyCode::PageUp => review.scroll.saturating_sub(10),
                        KeyCode::PageDown | KeyCode::Char(' ') => review.scroll.saturating_add(10),
                        KeyCode::Home => 0,
                        _ => review.scroll,
                    };
                }
            }
        }
        Flow::Stay
    }
}

/// Runs the wizard; `None` when the user cancels.
pub fn run(root: &Path, options: &Options) -> io::Result<Option<(Selection, String)>> {
    super::run(&mut Wizard::new(root, options))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::tui::testing::{draw, send};

    fn finish(wizard: &mut Wizard, script: &str) -> (Selection, String) {
        match send(wizard, script) {
            Some(Flow::Done(result)) => result,
            _ => panic!("the wizard did not finish:\n{}", draw(wizard, 100, 30)),
        }
    }

    #[test]
    fn defaults_create_a_balanced_solo_crew() {
        let root = git::test_repo("tui-solo");
        let mut wizard = Wizard::new(&root, &Options::default());
        let screen = draw(&mut wizard, 100, 24);
        assert!(screen.contains("Project name / role prefix"), "{screen}");
        // Name, workflow, agent, models, ignore, review.
        let (selection, text) = finish(&mut wizard, "{enter}{enter}{enter}{enter}{enter}{enter}");
        assert_eq!(selection.preset, Preset::Solo);
        assert_eq!(selection.profile, Profile::Balanced);
        let config = Config::parse(&text, &root).unwrap();
        assert_eq!(config.roles[0].claude.model.as_deref(), Some("opus"));
        assert_eq!(config.roles[0].claude.effort.as_deref(), Some("high"));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn choices_show_role_models_and_reach_the_configuration() {
        let root = git::test_repo("tui-research");
        let mut wizard = Wizard::new(&root, &Options::default());
        // A bad name is refused, then corrected.
        send(
            &mut wizard,
            "{bs}{bs}{bs}{bs}{bs}{bs}{bs}{bs}{bs}{bs}{bs}{bs}{bs}{bs}{bs}{bs}{bs}{bs}{bs}{bs}{bs}{bs}9x{enter}",
        );
        assert!(wizard.message.is_some());
        assert_eq!(wizard.step, Step::Name);
        send(&mut wizard, "{bs}{bs}acme{enter}4{enter}2{enter}");
        assert_eq!(wizard.step, Step::Topology);
        // Codex research defaults to tabs (1).
        send(&mut wizard, "{enter}");
        assert_eq!(wizard.step, Step::Models);
        send(&mut wizard, "1");
        let screen = draw(&mut wizard, 110, 28);
        assert!(screen.contains("acme-research-a"), "{screen}");
        assert!(screen.contains("agent default"), "{screen}");
        send(&mut wizard, "4");
        let screen = draw(&mut wizard, 110, 28);
        assert!(screen.contains("gpt-6.2-astra"), "{screen}");
        // Going back keeps answers.
        send(&mut wizard, "{esc}");
        assert_eq!(wizard.step, Step::Topology);
        let (selection, text) = finish(&mut wizard, "{enter}{enter}n{enter}{enter}");
        assert_eq!(selection.agent, Kind::Codex);
        assert!(!selection.ignore);
        let config = Config::parse(&text, &root).unwrap();
        assert_eq!(config.kind, Kind::Codex);
        let lead = config.role("acme-lead").unwrap();
        assert_eq!(lead.codex.model.as_deref(), Some("gpt-6.2-astra"));
        let researcher = config.role("acme-research-a").unwrap();
        assert_eq!(researcher.codex.reasoning_effort.as_deref(), Some("high"));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn worktree_steps_validate_the_base_and_confirm_an_unfetched_one() {
        let root = git::test_repo("tui-review");
        git::test_remote_branch(&root, "origin/main");
        let options = Options {
            name: Some("shop".into()),
            preset: Some(Preset::Review),
            ..Options::default()
        };
        let mut wizard = Wizard::new(&root, &options);
        send(&mut wizard, "{enter}{enter}{enter}{enter}");
        assert_eq!(wizard.step, Step::Worktrees);
        send(&mut wizard, "{enter}");
        assert_eq!(wizard.step, Step::Base);
        assert_eq!(wizard.base.value, "origin/main");
        send(&mut wizard, "{bs}{bs}{bs}{bs}next{enter}");
        assert_eq!(wizard.step, Step::Base);
        assert!(
            wizard
                .message
                .as_deref()
                .unwrap()
                .contains("not among the fetched")
        );
        send(&mut wizard, "{enter}");
        assert_eq!(wizard.step, Step::Ignore);
        let screen = {
            send(&mut wizard, "{enter}");
            draw(&mut wizard, 100, 60)
        };
        assert!(screen.contains("Worktree base: origin/next"), "{screen}");
        assert!(screen.contains("/.worktrees/"), "{screen}");
        let (selection, text) = finish(&mut wizard, "{enter}");
        assert_eq!(selection.base.as_deref(), Some("origin/next"));
        let config = Config::parse(&text, &root).unwrap();
        assert!(config.role("shop-dev").unwrap().worktree);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn parallel_without_a_remote_refuses_tabs_but_allows_agy_tree() {
        let root = git::test_repo("tui-no-remote");
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["remote", "remove", "origin"])
            .status()
            .unwrap();
        assert!(status.success());
        let mut wizard = Wizard::new(&root, &Options::default());
        // Name -> Workflow (3 = parallel) -> Agent (1 = claude, refused without remote)
        send(&mut wizard, "{enter}3{enter}{enter}");
        assert_eq!(wizard.step, Step::Agent);
        assert!(
            wizard
                .message
                .as_deref()
                .unwrap()
                .contains("configured Git remote")
        );
        // Switching to agy (4) goes straight to Models (no Topology step) without needing a remote.
        send(&mut wizard, "4{enter}");
        assert_eq!(wizard.step, Step::Models);
        let screen = draw(&mut wizard, 110, 28);
        assert!(screen.contains("-lead"), "{screen}");
        assert!(!screen.contains("-dev-a"), "{screen}");
        assert!(!screen.contains("-reviewer"), "{screen}");
        let (selection, text) = finish(&mut wizard, "{enter}{enter}{enter}");
        assert!(selection.is_tree());
        assert!(selection.tree_isolated);
        assert!(selection.base.is_none());
        let config = Config::parse(&text, &root).unwrap();
        assert!(config.agy_tree);
        assert!(config.worktrees.is_none());
        assert_eq!(config.roles.len(), 1);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn agy_review_tree_wizard_skips_topology_and_asks_native_isolation_without_base() {
        let root = git::test_repo("tui-agy-review-tree");
        let options = Options {
            name: Some("shop".into()),
            preset: Some(Preset::Review),
            agent: Some(Kind::Agy),
            ..Options::default()
        };
        let mut wizard = Wizard::new(&root, &options);
        // Name -> Workflow -> Agent -> Models (no Topology step; shows only shop-lead)
        send(&mut wizard, "{enter}{enter}{enter}");
        assert_eq!(wizard.step, Step::Models);
        let models_screen = draw(&mut wizard, 110, 28);
        assert!(!models_screen.contains("Topology"), "{models_screen}");
        assert!(models_screen.contains("shop-lead"), "{models_screen}");
        assert!(!models_screen.contains("shop-dev"), "{models_screen}");
        assert!(!models_screen.contains("shop-reviewer"), "{models_screen}");
        // Models -> Worktrees (native isolation, defaults to separate / Workspace: "share")
        send(&mut wizard, "{enter}");
        assert_eq!(wizard.step, Step::Worktrees);
        let wt_screen = draw(&mut wizard, 110, 28);
        assert!(wt_screen.contains("Workspace: \"share\""), "{wt_screen}");
        assert!(wt_screen.contains("Workspace: \"inherit\""), "{wt_screen}");
        // Pick shared (2) -> skips Base straight to Ignore -> Review -> Finish
        send(&mut wizard, "2{enter}");
        assert_eq!(wizard.step, Step::Ignore);
        let (selection, text) = finish(&mut wizard, "{enter}{enter}");
        assert!(selection.is_tree());
        assert!(!selection.tree_isolated);
        assert!(selection.base.is_none());
        let config = Config::parse(&text, &root).unwrap();
        assert!(config.agy_tree);
        assert_eq!(config.roles.len(), 1);
        assert!(
            config.roles[0]
                .prompt
                .contains("- dev (TypeName: \"self\", Workspace: \"inherit\")")
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn customizing_role_model_and_effort_updates_preview_and_config() {
        let root = git::test_repo("tui-custom-models");
        let options = Options {
            name: Some("shop".into()),
            preset: Some(Preset::Review),
            shared_checkout: true,
            ..Options::default()
        };
        let mut wizard = Wizard::new(&root, &options);
        // Name -> Workflow -> Agent (claude) -> Models (balanced)
        send(&mut wizard, "{enter}{enter}{enter}");
        assert_eq!(wizard.step, Step::Models);
        // Enter role customization on lead (row 0, col 0 = Model):
        // Picker choices for Claude model: 0=inherit, 1=fable, 2=opus, 3=sonnet, 4=haiku, 5=custom…
        // Select fable (key '2' -> index 1), then move right to Effort and select xhigh ('5' -> index 4).
        send(&mut wizard, "e{enter}2{enter}{right}{enter}5{enter}");
        // Move down to dev (row 1), left to Model, open custom… ('6' -> index 5), type custom model.
        send(
            &mut wizard,
            "{down}{left}{enter}6{enter}{bs}{bs}{bs}{bs}{bs}{bs}claude-custom-1{enter}",
        );
        // Move down to reviewer (row 2), set Model to inherit with Backspace.
        send(&mut wizard, "{down}{bs}");
        let screen = draw(&mut wizard, 110, 28);
        assert!(screen.contains("fable"), "{screen}");
        assert!(screen.contains("xhigh"), "{screen}");
        assert!(screen.contains("claude-custom-1"), "{screen}");
        assert!(screen.contains("agent default"), "{screen}");
        // Exit roles table, advance past Models -> Worktrees (shared) -> Ignore -> Review -> Create.
        send(&mut wizard, "{esc}{enter}");
        assert_eq!(wizard.step, Step::Worktrees);
        let sidebar = draw(&mut wizard, 110, 28);
        assert!(sidebar.contains("balanced*"), "{sidebar}");
        let (selection, text) = finish(&mut wizard, "{enter}{enter}{enter}");
        assert!(selection.is_customized());
        assert!(text.contains("balanced customized models"), "{text}");
        let config = Config::parse(&text, &root).unwrap();
        let lead = config.role("shop-lead").unwrap();
        assert_eq!(lead.claude.model.as_deref(), Some("fable"));
        assert_eq!(lead.claude.effort.as_deref(), Some("xhigh"));
        let dev = config.role("shop-dev").unwrap();
        assert_eq!(dev.claude.model.as_deref(), Some("claude-custom-1"));
        assert_eq!(dev.claude.effort.as_deref(), Some("high"));
        let reviewer = config.role("shop-reviewer").unwrap();
        assert_eq!(reviewer.claude.model, None);
        assert_eq!(reviewer.claude.effort.as_deref(), Some("high"));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn invalid_agy_model_effort_combination_is_refused_inline() {
        let root = git::test_repo("tui-agy-validate");
        let options = Options {
            name: Some("app".into()),
            agent: Some(Kind::Agy),
            ..Options::default()
        };
        let mut wizard = Wizard::new(&root, &options);
        send(&mut wizard, "{enter}{enter}{enter}");
        assert_eq!(wizard.step, Step::Models);
        // Enter roles table on solo dev (gemini-4-neon + high).
        // Agy model choices: 0=inherit, 1..6=catalog (6 items), 7=custom… (key '8').
        send(&mut wizard, "e{enter}8{enter}-high{enter}");
        assert!(
            wizard
                .message
                .as_deref()
                .unwrap_or_default()
                .contains("conflicts with a model slug"),
            "{:?}",
            wizard.message
        );
        assert!(!wizard.is_customized());
        // Clear effort to inherit first, then suffixed slug is allowed.
        send(&mut wizard, "{right}{bs}{left}{enter}8{enter}-high{enter}");
        assert!(wizard.message.is_none(), "{:?}", wizard.message);
        let (selection, text) = finish(&mut wizard, "{esc}{enter}{enter}{enter}");
        assert!(selection.is_customized());
        let config = Config::parse(&text, &root).unwrap();
        assert_eq!(
            config.roles[0].agy.model.as_deref(),
            Some("gemini-4-neon-high")
        );
        assert_eq!(config.roles[0].agy.effort, None);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn changing_profile_agent_or_workflow_clears_stale_overrides() {
        let root = git::test_repo("tui-reset-overrides");
        let mut wizard = Wizard::new(&root, &Options::default());
        send(&mut wizard, "{enter}{enter}{enter}");
        assert_eq!(wizard.step, Step::Models);
        // Customize solo model to haiku (Claude choice 5 -> index 4).
        send(&mut wizard, "e{enter}5{enter}{esc}");
        assert!(wizard.is_customized());
        // Switching profile clears overrides.
        send(&mut wizard, "4");
        assert!(!wizard.is_customized());
        // Customize again, then go back to Agent and switch to Codex.
        send(&mut wizard, "e{enter}5{enter}{esc}{esc}2{enter}");
        assert_eq!(wizard.step, Step::Models);
        assert!(!wizard.is_customized());
        // Customize again, then go back to Workflow and switch to Research (Codex -> Topology -> Models).
        send(
            &mut wizard,
            "e{enter}1{enter}{esc}{esc}{esc}4{enter}{enter}{enter}",
        );
        assert_eq!(wizard.step, Step::Models);
        assert!(!wizard.is_customized());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn codex_wizard_offers_topology_defaulting_to_tabs_and_skips_worktrees_for_tree() {
        let root = git::test_repo("tui-codex-topology");
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["remote", "remove", "origin"])
            .status()
            .unwrap();
        assert!(status.success());

        // Parallel without remote: Codex reaches Topology; tabs (1) is refused, tree (2) succeeds.
        let mut wizard = Wizard::new(&root, &Options::default());
        send(&mut wizard, "{enter}3{enter}2{enter}");
        assert_eq!(wizard.step, Step::Topology);
        let screen = draw(&mut wizard, 110, 28);
        assert!(screen.contains("tabs"), "{screen}");
        assert!(screen.contains("tree"), "{screen}");
        assert!(screen.contains("features.multi_agent_v2"), "{screen}");
        send(&mut wizard, "1{enter}");
        assert_eq!(wizard.step, Step::Topology);
        assert!(
            wizard
                .message
                .as_deref()
                .unwrap()
                .contains("configured Git remote")
        );
        send(&mut wizard, "2{enter}");
        assert_eq!(wizard.step, Step::Models);
        let models_screen = draw(&mut wizard, 110, 28);
        assert!(models_screen.contains("-lead"), "{models_screen}");
        assert!(!models_screen.contains("-dev-a"), "{models_screen}");
        let (selection, text) = finish(&mut wizard, "{enter}{enter}{enter}");
        assert!(selection.is_codex_tree());
        assert!(selection.base.is_none());
        let config = Config::parse(&text, &root).unwrap();
        assert!(config.codex_tree);
        assert!(config.worktrees.is_none());
        assert_eq!(config.roles.len(), 1);

        // Codex review + tree skips Worktrees and Base straight to Ignore.
        let review_opts = Options {
            name: Some("shop".into()),
            preset: Some(Preset::Review),
            agent: Some(Kind::Codex),
            ..Options::default()
        };
        let mut review_wizard = Wizard::new(&root, &review_opts);
        send(&mut review_wizard, "{enter}{enter}{enter}");
        assert_eq!(review_wizard.step, Step::Topology);
        assert_eq!(review_wizard.topology(), Topology::Tabs);
        send(&mut review_wizard, "2{enter}{enter}");
        assert_eq!(review_wizard.step, Step::Ignore);
        let (selection, text) = finish(&mut review_wizard, "{enter}{enter}");
        assert!(selection.is_codex_tree());
        assert!(Config::parse(&text, &root).unwrap().codex_tree);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn explicit_base_refuses_codex_tree_and_multi_role_agy_and_shared_checkout() {
        let root = git::test_repo("tui-explicit-base");
        git::test_remote_branch(&root, "origin/main");
        let options = Options {
            name: Some("shop".into()),
            base: Some("origin/main".into()),
            ..Options::default()
        };
        let mut wizard = Wizard::new(&root, &options);
        // Solo is refused with --base.
        send(&mut wizard, "{enter}{enter}");
        assert_eq!(wizard.step, Step::Workflow);
        assert!(
            wizard
                .message
                .as_deref()
                .unwrap()
                .contains("--base requires a review or parallel preset")
        );
        // Review -> Agy is refused at Agent because multi-role agy uses native tree mode.
        send(&mut wizard, "2{enter}4{enter}");
        assert_eq!(wizard.step, Step::Agent);
        assert_eq!(wizard.message.as_deref(), Some(init::AGY_TREE_BASE_ERROR));
        // Review -> Codex reaches Topology (default tabs); selecting tree (2) is refused.
        send(&mut wizard, "2{enter}");
        assert_eq!(wizard.step, Step::Topology);
        assert_eq!(wizard.topology(), Topology::Tabs);
        send(&mut wizard, "2{enter}");
        assert_eq!(wizard.step, Step::Topology);
        assert_eq!(
            wizard.message.as_deref(),
            Some(init::TREE_BASE_INTERACTIVE_ERROR)
        );
        // Pick tabs (1) -> Models -> Worktrees.
        send(&mut wizard, "1{enter}{enter}");
        assert_eq!(wizard.step, Step::Worktrees);
        // Choosing shared (2) is refused so --base is not silently discarded.
        send(&mut wizard, "2{enter}");
        assert_eq!(wizard.step, Step::Worktrees);
        assert!(
            wizard
                .message
                .as_deref()
                .unwrap()
                .contains("--base requires separate worktrees")
        );
        // Pick separate (1) -> Base (origin/main) -> Ignore -> Review -> Finish.
        let (selection, text) = finish(&mut wizard, "1{enter}{enter}{enter}{enter}");
        assert!(!selection.is_tree());
        assert_eq!(selection.agent, Kind::Codex);
        assert_eq!(selection.base.as_deref(), Some("origin/main"));
        let config = Config::parse(&text, &root).unwrap();
        assert!(!config.codex_tree);
        assert_eq!(config.worktrees.as_ref().unwrap().branch, "main");
        std::fs::remove_dir_all(&root).unwrap();
    }
}
