//! Score-band policy ladder for Quiver's strict-mode hooks.
//!
//! `Policy::classify` maps a hybrid recommender score onto one of four bands:
//!
//! | Band       | Default range  | UserPromptSubmit              | PreToolUse veto                  |
//! |------------|----------------|-------------------------------|----------------------------------|
//! | Silent     | `< 0.40`       | no emit                       | no emit                          |
//! | Hint       | `0.40 – 0.59`  | `additionalContext` (legacy)  | metadata `additionalContext`     |
//! | Strong     | `0.60 – 0.74`  | `systemMessage` directive     | `permissionDecision: deny` if Δ ≥ τ_delta |
//! | Mandatory  | `≥ 0.75`       | `systemMessage` + invoke_now  | `permissionDecision: deny`; Stop block if unused |
//!
//! Defaults are overridable via `QUIVER_TAU_HINT|STRONG|MANDATORY|DELTA`.
//! `QUIVER_HOOK_SCORE_MIN` is honoured as an alias for `QUIVER_TAU_HINT`.

/// One of the four bands in the policy ladder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Policy {
    Silent,
    Hint,
    Strong,
    Mandatory,
}

impl Policy {
    /// Stable, lower-case string used in JSON / DB / hook XML.
    pub fn as_str(self) -> &'static str {
        match self {
            Policy::Silent => "silent",
            Policy::Hint => "hint",
            Policy::Strong => "strong",
            Policy::Mandatory => "mandatory",
        }
    }

    /// `true` for bands that emit a `<quiver-directive>` system-reminder.
    pub fn is_directive(self) -> bool {
        matches!(self, Policy::Strong | Policy::Mandatory)
    }
}

/// Ladder thresholds. All values are on the post-rerank hybrid score scale
/// (`0.6·cos + 0.4·BM25` with `SuccessReranker` boost).
#[derive(Debug, Clone, Copy)]
pub struct Thresholds {
    pub tau_hint: f32,
    pub tau_strong: f32,
    pub tau_mandatory: f32,
    /// Minimum gap between the recommended top-1 and the candidate the model
    /// is about to invoke for the PreToolUse veto to fire.
    pub tau_delta: f32,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            tau_hint: 0.40,
            tau_strong: 0.60,
            tau_mandatory: 0.75,
            tau_delta: 0.20,
        }
    }
}

impl Thresholds {
    /// Read overrides from env. Missing or unparseable values fall back to
    /// the default. `QUIVER_HOOK_SCORE_MIN` is treated as an alias for
    /// `QUIVER_TAU_HINT` so existing installs keep working.
    pub fn from_env() -> Self {
        let mut t = Self::default();
        if let Some(v) =
            read_env_f32("QUIVER_TAU_HINT").or_else(|| read_env_f32("QUIVER_HOOK_SCORE_MIN"))
        {
            t.tau_hint = v;
        }
        if let Some(v) = read_env_f32("QUIVER_TAU_STRONG") {
            t.tau_strong = v;
        }
        if let Some(v) = read_env_f32("QUIVER_TAU_MANDATORY") {
            t.tau_mandatory = v;
        }
        if let Some(v) = read_env_f32("QUIVER_TAU_DELTA") {
            t.tau_delta = v;
        }
        t.normalise();
        t
    }

    /// Force a sane non-decreasing ladder. If a user sets
    /// `QUIVER_TAU_STRONG` lower than `QUIVER_TAU_HINT` we promote it back
    /// up — silently bad config is worse than ignoring the override.
    fn normalise(&mut self) {
        if self.tau_strong < self.tau_hint {
            self.tau_strong = self.tau_hint;
        }
        if self.tau_mandatory < self.tau_strong {
            self.tau_mandatory = self.tau_strong;
        }
        if self.tau_delta < 0.0 {
            self.tau_delta = 0.0;
        }
    }

    /// Map a score to its band. Boundaries are inclusive on the lower edge:
    /// `score == tau_strong` → `Strong`, `score == tau_mandatory` → `Mandatory`.
    pub fn classify(&self, score: f32) -> Policy {
        if score >= self.tau_mandatory {
            Policy::Mandatory
        } else if score >= self.tau_strong {
            Policy::Strong
        } else if score >= self.tau_hint {
            Policy::Hint
        } else {
            Policy::Silent
        }
    }
}

/// Convenience wrapper: classify with default thresholds.
pub fn classify(score: f32) -> Policy {
    Thresholds::default().classify(score)
}

fn read_env_f32(key: &str) -> Option<f32> {
    std::env::var(key).ok().and_then(|s| s.trim().parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_bands() {
        let t = Thresholds::default();
        assert_eq!(t.classify(0.0), Policy::Silent);
        assert_eq!(t.classify(0.39), Policy::Silent);
        assert_eq!(t.classify(0.40), Policy::Hint);
        assert_eq!(t.classify(0.59), Policy::Hint);
        assert_eq!(t.classify(0.60), Policy::Strong);
        assert_eq!(t.classify(0.74), Policy::Strong);
        assert_eq!(t.classify(0.75), Policy::Mandatory);
        assert_eq!(t.classify(1.30), Policy::Mandatory);
    }

    #[test]
    fn is_directive_only_strong_and_mandatory() {
        assert!(!Policy::Silent.is_directive());
        assert!(!Policy::Hint.is_directive());
        assert!(Policy::Strong.is_directive());
        assert!(Policy::Mandatory.is_directive());
    }

    #[test]
    fn normalise_promotes_strong_above_hint() {
        let mut t = Thresholds {
            tau_hint: 0.5,
            tau_strong: 0.3,
            tau_mandatory: 0.4,
            tau_delta: -0.1,
        };
        t.normalise();
        assert!(t.tau_strong >= t.tau_hint);
        assert!(t.tau_mandatory >= t.tau_strong);
        assert_eq!(t.tau_delta, 0.0);
    }

    #[test]
    fn as_str_round_trip() {
        assert_eq!(Policy::Silent.as_str(), "silent");
        assert_eq!(Policy::Hint.as_str(), "hint");
        assert_eq!(Policy::Strong.as_str(), "strong");
        assert_eq!(Policy::Mandatory.as_str(), "mandatory");
    }
}
