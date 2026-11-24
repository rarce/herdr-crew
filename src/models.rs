//! Model profiles: which model and effort each kind of role gets, per agent (docs/models.md).
//! Pure data; `init` writes the result into `crew.toml` and `config` applies it to a crew.

use crate::agent::{AgentOptions, AgyOptions, ClaudeOptions, CodexOptions, Kind, PiOptions};

/// What a role does, which decides how much capability it needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Duty {
    /// The only session: plans, implements and validates.
    Solo,
    /// Coordinates, decides, integrates and writes the board.
    Lead,
    /// Implements an assigned component in a long tool loop.
    Developer,
    /// Judges a delivered commit; missed defects are expensive.
    Reviewer,
    /// Reads and reports evidence; no edits.
    Researcher,
}

impl Duty {
    #[cfg(test)]
    pub const ALL: [Self; 5] = [
        Self::Solo,
        Self::Lead,
        Self::Developer,
        Self::Reviewer,
        Self::Researcher,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Solo => "solo",
            Self::Lead => "lead",
            Self::Developer => "developer",
            Self::Reviewer => "reviewer",
            Self::Researcher => "researcher",
        }
    }

    /// Best guess for a role of an existing crew, from the names `init` generates: the board
    /// writer leads (or works alone), then `review`, `research`, and developers otherwise.
    pub fn guess(name: &str, writer: bool, roles: usize) -> Self {
        let suffix = name.rsplit_once('-').map_or(name, |(_, s)| s);
        if writer {
            if roles == 1 { Self::Solo } else { Self::Lead }
        } else if name.contains("review") {
            Self::Reviewer
        } else if name.contains("research") {
            Self::Researcher
        } else if suffix == "lead" {
            Self::Lead
        } else {
            Self::Developer
        }
    }
}

/// A cost/capability trade-off applied to every role at once.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Profile {
    /// No model settings: each agent uses the user's own configuration.
    Inherit,
    /// Cheaper models; strong only where judgment matters.
    Economy,
    /// Strong judgment for lead and review, efficient implementation.
    #[default]
    Balanced,
    /// The most capable models everywhere; several times the cost.
    Quality,
}

impl Profile {
    pub const ALL: [Self; 4] = [Self::Inherit, Self::Economy, Self::Balanced, Self::Quality];

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.as_str() == value)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inherit => "inherit",
            Self::Economy => "economy",
            Self::Balanced => "balanced",
            Self::Quality => "quality",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Inherit => "no model settings; every role uses your agent's own default",
            Self::Economy => {
                "lowest cost: mid-tier models, light model for research, strong review"
            }
            Self::Balanced => {
                "recommended: strongest judgment for lead and review, efficient developers"
            }
            Self::Quality => "highest capability: frontier models for lead and review",
        }
    }

    /// The tier and effort of a role, or `None` to leave the agent's own default.
    pub fn pick(self, duty: Duty) -> Option<(Tier, &'static str)> {
        use Duty::*;
        use Tier::*;
        Some(match (self, duty) {
            (Self::Inherit, _) => return None,
            // Economy keeps a strong reviewer: a missed defect costs more than the review.
            (Self::Economy, Solo | Lead) => (Standard, "high"),
            (Self::Economy, Developer) => (Standard, "medium"),
            (Self::Economy, Reviewer) => (Strong, "medium"),
            (Self::Economy, Researcher) => (Light, "medium"),
            // Judgment-heavy roles get the strong tier; developers run long tool loops, where
            // the standard tier at high effort is the best cost per completed task, and
            // researchers mostly read.
            (Self::Balanced, Solo | Lead | Reviewer) => (Strong, "high"),
            (Self::Balanced, Developer) => (Standard, "high"),
            (Self::Balanced, Researcher) => (Standard, "medium"),
            // Frontier models start at high: their gains above it are uneven. The strong tier
            // at xhigh is the usual best setting for agentic coding.
            (Self::Quality, Solo | Lead | Reviewer) => (Frontier, "high"),
            (Self::Quality, Developer) => (Strong, "xhigh"),
            (Self::Quality, Researcher) => (Strong, "high"),
        })
    }

    /// The options of `agent` for a role with `duty`; empty for `Inherit`.
    pub fn options(self, agent: Kind, duty: Duty) -> AgentOptions {
        let pick = self.pick(duty);
        let model = pick
            .and_then(|(tier, _)| tier.model(agent))
            .map(str::to_string);
        let effort = pick.map(|(_, effort)| effort.to_string());
        match agent {
            Kind::Claude => AgentOptions::Claude(ClaudeOptions {
                model,
                effort,
                ..Default::default()
            }),
            Kind::Codex => AgentOptions::Codex(CodexOptions {
                model,
                reasoning_effort: effort,
                ..Default::default()
            }),
            Kind::Pi => AgentOptions::Pi(PiOptions {
                thinking: effort,
                ..Default::default()
            }),
            Kind::Agy => AgentOptions::Agy(AgyOptions {
                model,
                effort,
                ..Default::default()
            }),
        }
    }
}

/// Model capability tiers, mapped to each agent's models.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    Frontier,
    Strong,
    Standard,
    Light,
}

impl Tier {
    /// Claude Code aliases follow the latest model of each family; Codex and agy use their current
    /// catalogs (`codex debug models`, `agy models`, 2026-11). pi's model depends on the user's
    /// provider, so it keeps its own and only the thinking level is set.
    pub fn model(self, agent: Kind) -> Option<&'static str> {
        match agent {
            Kind::Claude => Some(match self {
                Self::Frontier => "fable",
                Self::Strong => "opus",
                Self::Standard => "sonnet",
                Self::Light => "haiku",
            }),
            Kind::Codex => Some(match self {
                Self::Frontier | Self::Strong => "gpt-6.2-astra",
                Self::Standard => "gpt-6.2-sol",
                Self::Light => "gpt-6-luna",
            }),
            Kind::Pi => None,
            Kind::Agy => Some(match self {
                Self::Frontier => "gemini-4-argon",
                Self::Strong => "gemini-4-neon",
                Self::Standard => "gemini-4-hydrogen",
                Self::Light => "gemini-3.8-flash",
            }),
        }
    }
}

/// Models offered by the configuration editor, with a short note. Any other name can be typed.
pub fn catalog(agent: Kind) -> &'static [(&'static str, &'static str)] {
    match agent {
        Kind::Claude => &[
            ("fable", "most capable; long, hard reasoning; highest cost"),
            ("opus", "strong coding and judgment"),
            ("sonnet", "fast everyday coding; best cost per task"),
            ("haiku", "fastest and cheapest; reading and simple tasks"),
        ],
        Kind::Codex => &[
            ("gpt-6.2-astra", "most capable for complex coding"),
            ("gpt-6.2-sol", "fast go-to model for coding"),
            ("gpt-6.1-terra", "efficient; visual understanding"),
            ("gpt-6-luna", "fast and affordable for easier tasks"),
        ],
        Kind::Pi => &[],
        Kind::Agy => &[
            (
                "gemini-4-argon",
                "frontier reasoning; architecture and deep review",
            ),
            ("gemini-4-neon", "strong coding and multi-step judgment"),
            (
                "gemini-4-hydrogen",
                "fast everyday coding; efficient tool loops",
            ),
            (
                "gemini-3.8-flash",
                "fastest and lightest; reading and simple tasks",
            ),
            ("claude-opus-5-5", "Claude Opus 5.5 via agy"),
            ("claude-sonnet-5-5", "Claude Sonnet 5.5 via agy"),
        ],
    }
}

/// Effort levels each agent accepts, lowest first.
pub fn efforts(agent: Kind) -> &'static [&'static str] {
    match agent {
        Kind::Claude => crate::agent::CLAUDE_EFFORTS,
        Kind::Codex => crate::agent::CODEX_EFFORTS,
        Kind::Pi => crate::agent::PI_THINKING,
        Kind::Agy => crate::agent::AGY_EFFORTS,
    }
}

/// The config key of the effort option of each agent.
pub fn effort_key(agent: Kind) -> &'static str {
    match agent {
        Kind::Claude => "effort",
        Kind::Codex => "reasoning_effort",
        Kind::Pi => "thinking",
        Kind::Agy => "effort",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_pick_is_valid_for_every_agent() {
        for profile in Profile::ALL {
            assert_eq!(Profile::parse(profile.as_str()), Some(profile));
            for duty in Duty::ALL {
                for agent in Kind::ALL {
                    let problems = match profile.options(agent, duty) {
                        AgentOptions::Claude(o) => o.problems(),
                        AgentOptions::Codex(o) => o.problems(),
                        AgentOptions::Pi(o) => o.problems(),
                        AgentOptions::Agy(o) => {
                            let mut p = o.problems();
                            if let Some(lp) = o.launch_problem() {
                                p.push(lp.into());
                            }
                            p
                        }
                    };
                    assert!(
                        problems.is_empty(),
                        "{profile:?} {duty:?} {agent:?}: {problems:?}"
                    );
                    if let Some((_, effort)) = profile.pick(duty) {
                        assert!(efforts(agent).contains(&effort));
                    }
                }
            }
        }
    }

    #[test]
    fn inherit_sets_nothing_and_balanced_favors_judgment() {
        for agent in Kind::ALL {
            let empty = match Profile::Inherit.options(agent, Duty::Lead) {
                AgentOptions::Claude(o) => o == ClaudeOptions::default(),
                AgentOptions::Codex(o) => o == CodexOptions::default(),
                AgentOptions::Pi(o) => o == PiOptions::default(),
                AgentOptions::Agy(o) => o == AgyOptions::default(),
            };
            assert!(empty);
        }
        assert_eq!(
            Profile::Balanced.pick(Duty::Reviewer),
            Some((Tier::Strong, "high"))
        );
        assert_eq!(
            Profile::Balanced.pick(Duty::Developer),
            Some((Tier::Standard, "high"))
        );
    }

    #[test]
    fn duties_are_guessed_from_generated_names() {
        assert_eq!(Duty::guess("p-dev", true, 1), Duty::Solo);
        assert_eq!(Duty::guess("p-lead", true, 3), Duty::Lead);
        assert_eq!(Duty::guess("p-reviewer", false, 3), Duty::Reviewer);
        assert_eq!(Duty::guess("p-research-a", false, 3), Duty::Researcher);
        assert_eq!(Duty::guess("p-dev-b", false, 4), Duty::Developer);
        assert_eq!(Duty::guess("p-lead", false, 4), Duty::Lead);
    }
}
