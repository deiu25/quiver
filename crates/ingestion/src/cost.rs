//! Cost computation for Claude Code session events.
//!
//! Claude Code session JSONL stores token usage in `assistant.message.usage`
//! but no monetary cost. We multiply each token category by per-million-token
//! rates published by Anthropic and sum to USD.
//!
//! Rates are hard-coded for the three current model families. When Anthropic
//! changes pricing, update [`rates_for`]. Models without a match return
//! `None` and the parser silently skips cost on those events.
//!
//! Pricing reference: <https://www.anthropic.com/pricing>
//! Last updated: 2026-05-04 (Opus 4.7, Sonnet 4.6, Haiku 4.5).

#[derive(Clone, Copy, Debug)]
pub struct ModelRates {
    /// USD per 1M input tokens.
    pub input: f64,
    /// USD per 1M output tokens.
    pub output: f64,
    /// USD per 1M cache-read input tokens.
    pub cache_read: f64,
    /// USD per 1M cache-write tokens (5-minute ephemeral, 1.25× input).
    pub cache_create_5m: f64,
    /// USD per 1M cache-write tokens (1-hour ephemeral, 2× input).
    pub cache_create_1h: f64,
}

/// Lookup the rate sheet for a Claude model id (prefix match).
///
/// Recognised: `claude-opus-4-7*`, `claude-sonnet-4-6*`, `claude-haiku-4-5*`.
/// Unknown models return `None` — caller treats this as "no cost recorded".
pub fn rates_for(model: &str) -> Option<ModelRates> {
    if model.starts_with("claude-opus-4-7") {
        return Some(ModelRates {
            input: 15.0,
            output: 75.0,
            cache_read: 1.50,
            cache_create_5m: 18.75,
            cache_create_1h: 30.0,
        });
    }
    if model.starts_with("claude-sonnet-4-6") {
        return Some(ModelRates {
            input: 3.0,
            output: 15.0,
            cache_read: 0.30,
            cache_create_5m: 3.75,
            cache_create_1h: 6.0,
        });
    }
    if model.starts_with("claude-haiku-4-5") {
        return Some(ModelRates {
            input: 0.80,
            output: 4.0,
            cache_read: 0.08,
            cache_create_5m: 1.0,
            cache_create_1h: 1.60,
        });
    }
    None
}

/// Token counts extracted from one assistant `message.usage` block.
#[derive(Clone, Copy, Debug, Default)]
pub struct UsageBreakdown {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_create_5m_tokens: u64,
    pub cache_create_1h_tokens: u64,
}

/// Parse a JSONL `usage` object into a [`UsageBreakdown`].
///
/// Defensive: missing fields → 0, never errors. The split between 5m and 1h
/// cache-creation lives in `usage.cache_creation.{ephemeral_5m,ephemeral_1h}_input_tokens`.
pub fn parse_usage(u: &serde_json::Value) -> UsageBreakdown {
    fn u64_at(v: &serde_json::Value, key: &str) -> u64 {
        v.get(key).and_then(|x| x.as_u64()).unwrap_or(0)
    }
    let cache_create = u.get("cache_creation");
    UsageBreakdown {
        input_tokens: u64_at(u, "input_tokens"),
        output_tokens: u64_at(u, "output_tokens"),
        cache_read_tokens: u64_at(u, "cache_read_input_tokens"),
        cache_create_5m_tokens: cache_create
            .map(|c| u64_at(c, "ephemeral_5m_input_tokens"))
            .unwrap_or(0),
        cache_create_1h_tokens: cache_create
            .map(|c| u64_at(c, "ephemeral_1h_input_tokens"))
            .unwrap_or(0),
    }
}

/// Cost in USD for one assistant message at the given model. Returns `None`
/// if the model is unknown (no rates published or future model not in table).
pub fn compute_cost_usd(model: &str, u: &UsageBreakdown) -> Option<f64> {
    let r = rates_for(model)?;
    let cost = (u.input_tokens as f64 * r.input
        + u.output_tokens as f64 * r.output
        + u.cache_read_tokens as f64 * r.cache_read
        + u.cache_create_5m_tokens as f64 * r.cache_create_5m
        + u.cache_create_1h_tokens as f64 * r.cache_create_1h)
        / 1_000_000.0;
    Some(cost)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn opus_47_golden() {
        let u = UsageBreakdown {
            input_tokens: 1_000_000,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_create_5m_tokens: 0,
            cache_create_1h_tokens: 0,
        };
        assert_eq!(compute_cost_usd("claude-opus-4-7", &u), Some(15.0));
    }

    #[test]
    fn sonnet_46_mixed() {
        // 100k input + 50k output + 200k cache_read + 10k 5m create + 5k 1h create
        let u = UsageBreakdown {
            input_tokens: 100_000,
            output_tokens: 50_000,
            cache_read_tokens: 200_000,
            cache_create_5m_tokens: 10_000,
            cache_create_1h_tokens: 5_000,
        };
        // 0.30 + 0.75 + 0.06 + 0.0375 + 0.030 = 1.1775
        let got = compute_cost_usd("claude-sonnet-4-6", &u).unwrap();
        assert!((got - 1.1775).abs() < 1e-9, "got {got}");
    }

    #[test]
    fn haiku_45_output_only() {
        let u = UsageBreakdown {
            output_tokens: 250_000,
            ..Default::default()
        };
        // 0.25M × $4 = $1.00
        let got = compute_cost_usd("claude-haiku-4-5-20251001", &u).unwrap();
        assert!((got - 1.0).abs() < 1e-9, "got {got}");
    }

    #[test]
    fn unknown_model_returns_none() {
        let u = UsageBreakdown {
            input_tokens: 1_000_000,
            ..Default::default()
        };
        assert_eq!(compute_cost_usd("gpt-4-turbo", &u), None);
        assert_eq!(compute_cost_usd("claude-opus-3", &u), None);
        assert_eq!(compute_cost_usd("", &u), None);
    }

    #[test]
    fn parse_usage_handles_missing_cache_creation() {
        let u = parse_usage(&json!({
            "input_tokens": 100,
            "output_tokens": 50,
            "cache_read_input_tokens": 200,
        }));
        assert_eq!(u.input_tokens, 100);
        assert_eq!(u.output_tokens, 50);
        assert_eq!(u.cache_read_tokens, 200);
        assert_eq!(u.cache_create_5m_tokens, 0);
        assert_eq!(u.cache_create_1h_tokens, 0);
    }

    #[test]
    fn parse_usage_extracts_ephemeral_split() {
        let u = parse_usage(&json!({
            "input_tokens": 6,
            "cache_creation_input_tokens": 37614,
            "cache_read_input_tokens": 29469,
            "output_tokens": 1039,
            "cache_creation": {
                "ephemeral_1h_input_tokens": 37614,
                "ephemeral_5m_input_tokens": 0
            }
        }));
        assert_eq!(u.input_tokens, 6);
        assert_eq!(u.output_tokens, 1039);
        assert_eq!(u.cache_read_tokens, 29469);
        assert_eq!(u.cache_create_5m_tokens, 0);
        assert_eq!(u.cache_create_1h_tokens, 37614);
    }
}
