//! What a `/v1/messages` call cost, in dollars, when dollars are the right unit.
//!
//! # Why only the HTTP path needs this
//!
//! `/v1/messages` reports tokens, not money, so the only way to a dollar figure
//! is a published rate. The other way Mach reaches a model — Claude Code, and
//! the default — does not need a table at all: `total_cost_usd` comes back on
//! the result document, and [`super::cli::one_shot_cost`] reads it. A number the
//! program that made the call arrived at beats a number this file remembers.
//!
//! # When dollars are not the right unit
//!
//! On a subscription bearer token the tokens draw down a quota rather than a
//! balance. There is no invoice, and pricing them from a list would put a figure
//! on screen that nothing will ever agree with. So [`cost_usd`] answers `None`
//! for a bearer credential, the ledger stores NULL, and the count limit in
//! [`crate::suggest::budget`] is what protects the owner.
//!
//! # The table will go stale, and that is survivable
//!
//! Prices move and models are added, so a model this does not recognise costs
//! `None` rather than a guess. That is the same "absent, not zero" rule the
//! column is built on: an unpriced generation still counts against the count
//! limit, which is the limit that matters.

use super::complete::Usage;
use super::config::{AgentConfig, Credential};

/// Dollars per million tokens, as published.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Price {
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
}

/// A cache read is a tenth of an input token.
const CACHE_READ_MULTIPLIER: f64 = 0.1;

/// Writing the cache costs a quarter more than the tokens would have.
const CACHE_WRITE_MULTIPLIER: f64 = 1.25;

/// List prices, keyed by the family in the model id.
///
/// Matched on substring rather than on the exact id because the id is free text
/// — [`crate::suggest::MODEL_KEY`] takes whatever he types, and
/// `claude-sonnet-5`, `claude-sonnet-4-6` and a dated snapshot of either should
/// all price as Sonnet. Longest match wins, so `opus` cannot swallow a future id
/// that happens to contain it.
///
/// Sonnet 5 carries an introductory rate below the figure here. The standard
/// price is used on purpose: a cap calibrated against a promotional rate is a
/// cap that silently loosens the day the promotion ends.
const PRICES: &[(&str, Price)] = &[
    (
        "opus",
        Price {
            input_per_mtok: 5.0,
            output_per_mtok: 25.0,
        },
    ),
    (
        "sonnet",
        Price {
            input_per_mtok: 3.0,
            output_per_mtok: 15.0,
        },
    ),
    (
        "haiku",
        Price {
            input_per_mtok: 1.0,
            output_per_mtok: 5.0,
        },
    ),
    (
        "fable",
        Price {
            input_per_mtok: 10.0,
            output_per_mtok: 50.0,
        },
    ),
];

/// The price list for a model id, or `None` for one this build has not heard of.
pub fn for_model(model: &str) -> Option<Price> {
    let model = model.trim().to_ascii_lowercase();
    PRICES
        .iter()
        .filter(|(family, _)| model.contains(family))
        .max_by_key(|(family, _)| family.len())
        .map(|(_, price)| *price)
}

/// What this call cost, or `None` when that is not a question with an answer.
///
/// Three ways to get `None`, and they are all honest rather than convenient: the
/// credential is a subscription bearer, so the spend is quota; the response
/// carried no `usage`, so nothing is known; or the model is not in the table, so
/// the rate is not known. Each of them is a NULL in the ledger and a generation
/// that still counts against the count limit.
pub fn cost_usd(config: &AgentConfig, model: &str, usage: &Usage) -> Option<f64> {
    if !matches!(config.credential, Credential::ApiKey(_)) {
        return None;
    }
    if !usage.is_known() {
        return None;
    }
    let price = for_model(model)?;

    let plain = usage.input_tokens.unwrap_or(0) as f64;
    let cache_write = usage.cache_creation_input_tokens.unwrap_or(0) as f64;
    let cache_read = usage.cache_read_input_tokens.unwrap_or(0) as f64;
    let output = usage.output_tokens.unwrap_or(0) as f64;

    let input_cost = (plain
        + cache_write * CACHE_WRITE_MULTIPLIER
        + cache_read * CACHE_READ_MULTIPLIER)
        * price.input_per_mtok
        / 1_000_000.0;
    let output_cost = output * price.output_per_mtok / 1_000_000.0;
    Some(input_cost + output_cost)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(input: i64, output: i64) -> Usage {
        Usage {
            input_tokens: Some(input),
            output_tokens: Some(output),
            ..Default::default()
        }
    }

    fn with_key() -> AgentConfig {
        AgentConfig {
            credential: Credential::ApiKey("k".into()),
            model: "claude-opus-5".into(),
            effort: "medium".into(),
            max_tokens: 32_000,
            base_url: "https://api.anthropic.test".into(),
            fallbacks: true,
        }
    }

    fn with_subscription() -> AgentConfig {
        AgentConfig {
            credential: Credential::BearerToken("oauth".into()),
            ..with_key()
        }
    }

    #[test]
    fn a_sonnet_generation_costs_about_two_cents() {
        // The figure the whole default cap is sized against: ~2k in, ~400 out.
        let cost = cost_usd(&with_key(), "claude-sonnet-5", &usage(2_000, 400)).unwrap();
        assert!((cost - 0.012).abs() < 1e-9, "{cost}");
        assert!(cost < 0.05, "a generation should be cents, not dollars");
    }

    #[test]
    fn the_family_is_matched_out_of_a_free_text_model_id() {
        for id in ["claude-sonnet-5", "CLAUDE-SONNET-4-6", " claude-sonnet-5-20260101 "] {
            assert_eq!(for_model(id).map(|p| p.input_per_mtok), Some(3.0), "{id}");
        }
        assert_eq!(for_model("claude-opus-5").map(|p| p.input_per_mtok), Some(5.0));
        assert_eq!(for_model("claude-haiku-4-5").map(|p| p.input_per_mtok), Some(1.0));
    }

    #[test]
    fn a_model_nobody_has_heard_of_has_no_price_rather_than_a_guess() {
        assert_eq!(for_model("some-other-model"), None);
        assert_eq!(cost_usd(&with_key(), "some-other-model", &usage(2_000, 400)), None);
    }

    #[test]
    fn a_subscription_generation_has_no_dollar_figure() {
        // Real tokens, real quota, and no invoice a price list would agree with.
        // The CLI is the other half of this answer: it reports a figure of its
        // own, so the subscription path is only priceless over HTTP.
        assert_eq!(
            cost_usd(&with_subscription(), "claude-sonnet-5", &usage(2_000, 400)),
            None
        );
    }

    #[test]
    fn a_response_that_reported_nothing_costs_nothing_knowable() {
        assert_eq!(
            cost_usd(&with_key(), "claude-sonnet-5", &Usage::default()),
            None
        );
    }

    #[test]
    fn cached_input_is_cheaper_than_fresh_input() {
        let fresh = cost_usd(&with_key(), "claude-sonnet-5", &usage(10_000, 0)).unwrap();
        let cached = cost_usd(
            &with_key(),
            "claude-sonnet-5",
            &Usage {
                input_tokens: Some(0),
                output_tokens: Some(0),
                cache_read_input_tokens: Some(10_000),
                cache_creation_input_tokens: None,
            },
        )
        .unwrap();
        assert!(cached < fresh, "{cached} should be a tenth of {fresh}");
        assert!((cached * 10.0 - fresh).abs() < 1e-9);
    }
}
