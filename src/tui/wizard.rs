//! `init` in a terminal: the setup questions as steps, with live previews of the roles, their
//! models and the generated configuration. Flags given on the command line prefill answers.

use std::io;
use std::path::{Path, PathBuf};

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Cell, Paragraph, Row, Table, Wrap};

use super::{
    Flow, Screen, TextInput, accent, bold, dim, error, hints, move_in_list, selected, warn,
};
use crate::agent::{AgentOptions, Kind};
use crate::git;
use crate::init::{self, MAX_PREFIX, Options, Preset, Selection};
use crate::models::Profile;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Name,
    Workflow,
    Agent,
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
            Self::Models => "Models",
            Self::Worktrees => "Worktrees",
            Self::Base => "Base branch",
            Self::Ignore => "Git ignore",
            Self::Review => "Review",
        }
    }
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
    profile: usize,
    isolated: bool,
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
        Wizard {
            root: root.to_path_buf(),
            step: Step::Name,
            name: TextInput::new(
                &options
                    .name
                    .clone()
                    .unwrap_or_else(|| init::suggested_name(root)),
            ),
            preset: index_of(&Preset::ALL, options.preset, Preset::Solo),
            agent: index_of(&Kind::ALL, options.agent, Kind::default()),
            profile: index_of(&Profile::ALL, options.models, Profile::default()),
            isolated: !options.shared_checkout
                && (options.base.is_some() || suggested_base.is_some()),
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

    fn profile(&self) -> Profile {
        Profile::ALL[self.profile]
    }

    fn uses_worktrees(&self) -> bool {
        match self.preset() {
            Preset::Parallel => true,
            Preset::Review => self.isolated,
            _ => false,
        }
    }

    /// The steps for the current answers: worktree questions only where they apply.
    fn steps(&self) -> Vec<Step> {
        let mut steps = vec![Step::Name, Step::Workflow, Step::Agent, Step::Models];
        if self.preset() == Preset::Review {
            steps.push(Step::Worktrees);
        }
        if self.uses_worktrees() {
            steps.push(Step::Base);
        }
        steps.extend([Step::Ignore, Step::Review]);
        steps
    }

    fn selection(&self) -> Selection {
        Selection {
            agent: self.agent(),
            name: self.name.value.clone(),
            preset: self.preset(),
            profile: self.profile(),
            base: self
                .uses_worktrees()
                .then(|| self.base.value.trim().to_string()),
            ignore: self.ignore,
        }
    }

    /// Checks the current step's answer; the message explains a refusal.
    fn accept(&mut self) -> Result<(), String> {
        match self.step {
            Step::Name if !init::valid_name(&self.name.value) => Err(format!(
                "Use 1-{MAX_PREFIX} characters: start with a-z, then a-z, 0-9, _ or -."
            )),
            Step::Workflow if self.preset() == Preset::Parallel && !self.has_remote => {
                Err(init::no_remote().lines[0].clone())
            }
            Step::Worktrees if self.isolated && !self.has_remote => {
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
                Step::Models => self.profile().as_str().into(),
                Step::Worktrees => if self.isolated { "separate" } else { "shared" }.into(),
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

    /// The roles of the chosen preset with the model and effort the profile gives them.
    fn roles_table(&self) -> Table<'static> {
        let agent = self.agent();
        let rows: Vec<Row> = self
            .preset()
            .roles()
            .iter()
            .map(|(suffix, duty)| {
                let (model, effort) = match self.profile().options(agent, *duty) {
                    AgentOptions::Claude(o) => (o.model, o.effort),
                    AgentOptions::Codex(o) => (o.model, o.reasoning_effort),
                    AgentOptions::Pi(o) => (o.model, o.thinking),
                    AgentOptions::Agy(o) => (o.model, o.effort),
                };
                let or_default = |v: Option<String>| v.unwrap_or_else(|| "agent default".into());
                Row::new(vec![
                    Cell::from(format!("{}-{suffix}", self.name.value)),
                    Cell::from(duty.as_str()),
                    Cell::from(or_default(model)),
                    Cell::from(or_default(effort)),
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
                        "Needs worktrees, which need a configured Git remote.",
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
                lines.push(Line::styled(
                    "Roles can use different agents later: herdr-crew config.",
                    dim(),
                ));
                frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
            }
            Step::Models => {
                let items: Vec<(String, String)> = Profile::ALL
                    .iter()
                    .map(|p| (p.as_str().to_string(), p.description().to_string()))
                    .collect();
                let mut lines = vec![question("Models and effort for each role"), Line::from("")];
                lines.extend(Self::choices(&items, self.profile));
                let height = lines.len() as u16 + 1;
                let [top, table, foot] = Layout::vertical([
                    Constraint::Length(height),
                    Constraint::Fill(1),
                    Constraint::Length(2),
                ])
                .areas(inner);
                frame.render_widget(Paragraph::new(lines), top);
                frame.render_widget(self.roles_table(), table);
                let foot_text = if self.agent() == Kind::Pi && self.profile() != Profile::Inherit {
                    "pi keeps your provider's model; the profile sets only the thinking level."
                } else {
                    "Change any role later with herdr-crew config. See docs/models.md."
                };
                frame.render_widget(
                    Paragraph::new(Line::styled(foot_text, dim())).wrap(Wrap { trim: false }),
                    foot,
                );
            }
            Step::Worktrees => {
                let items = vec![
                    (
                        "separate".to_string(),
                        "developer and reviewer get their own worktrees (recommended)".to_string(),
                    ),
                    (
                        "shared".to_string(),
                        "everyone works in the main checkout; no remote needed".to_string(),
                    ),
                ];
                let mut lines = vec![
                    question("Use separate worktrees for development and review?"),
                    Line::from(""),
                ];
                lines.extend(Self::choices(&items, usize::from(!self.isolated)));
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
        let keys: &[(&str, &str)] = match self.step {
            Step::Name | Step::Base => &[("Enter", "next"), ("Esc", "back"), ("Ctrl-C", "cancel")],
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
        };
        frame.render_widget(Paragraph::new(hints(keys)), footer);
    }

    fn key(&mut self, key: KeyEvent) -> Flow<Self::Output> {
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
                move_in_list(key, &mut self.preset, Preset::ALL.len());
            }
            Step::Agent => {
                move_in_list(key, &mut self.agent, Kind::ALL.len());
            }
            Step::Models => {
                move_in_list(key, &mut self.profile, Profile::ALL.len());
            }
            Step::Worktrees => {
                let mut i = usize::from(!self.isolated);
                if move_in_list(key, &mut i, 2) {
                    self.isolated = i == 0;
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
        assert_eq!(wizard.step, Step::Agent);
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
    fn parallel_without_a_remote_is_refused_at_the_workflow_step() {
        let root = git::test_repo("tui-no-remote");
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["remote", "remove", "origin"])
            .status()
            .unwrap();
        assert!(status.success());
        let mut wizard = Wizard::new(&root, &Options::default());
        send(&mut wizard, "{enter}3{enter}");
        assert_eq!(wizard.step, Step::Workflow);
        assert!(
            wizard
                .message
                .as_deref()
                .unwrap()
                .contains("configured Git remote")
        );
        assert!(matches!(send(&mut wizard, "q"), Some(Flow::Cancel)));
        std::fs::remove_dir_all(&root).unwrap();
    }
}
