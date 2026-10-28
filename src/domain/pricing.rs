use std::collections::HashSet;

use serde::Serialize;
use thiserror::Error;

use crate::domain::money::{Money, PerToken};
use crate::domain::usage::{ModelName, TokenUsage};

/// One price band. Haiku 5.5 charges more once the prompt passes 100K tokens;
/// every other model has a single band with `prompt_tokens_over = 0`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct PriceTier {
    pub prompt_tokens_over: u64,
    pub input: PerToken,
    pub output: PerToken,
    pub cache_read: PerToken,
    pub cache_write_5m: PerToken,
    pub cache_write_1h: PerToken,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ModelPrice {
    pub id: String,
    pub display_name: String,
    /// Sorted ascending by `prompt_tokens_over`; the first tier always starts at 0.
    pub tiers: Vec<PriceTier>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PriceList {
    pub as_of: String,
    pub models: Vec<ModelPrice>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PriceListError {
    #[error("price list has no models")]
    Empty,
    #[error("model {0} appears more than once")]
    DuplicateModel(String),
    #[error("model {0} has no price tiers")]
    NoTiers(String),
    #[error("model {0}: first tier must start at prompt_tokens_over = 0")]
    FirstTierNotBase(String),
    #[error("model {0}: tiers must be strictly ascending by prompt_tokens_over")]
    TiersNotAscending(String),
}

/// Cost of one request split the way the invoice splits it. `without_cache`
/// is the counterfactual where every prompt token was billed at the base input price.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Cost {
    pub input: Money,
    pub output: Money,
    pub cache_read: Money,
    pub cache_write: Money,
    pub without_cache: Money,
}

impl Cost {
    pub fn total(&self) -> Money {
        self.input + self.output + self.cache_read + self.cache_write
    }

    /// `None` when any component or the total would not fit in u64 nanodollars.
    pub fn checked_add(&self, other: &Cost) -> Option<Cost> {
        let sum = Cost {
            input: self.input.checked_add(other.input)?,
            output: self.output.checked_add(other.output)?,
            cache_read: self.cache_read.checked_add(other.cache_read)?,
            cache_write: self.cache_write.checked_add(other.cache_write)?,
            without_cache: self.without_cache.checked_add(other.without_cache)?,
        };
        sum.input
            .checked_add(sum.output)?
            .checked_add(sum.cache_read)?
            .checked_add(sum.cache_write)?;
        Some(sum)
    }

    pub fn add(&self, other: &Cost) -> Cost {
        Cost {
            input: self.input + other.input,
            output: self.output + other.output,
            cache_read: self.cache_read + other.cache_read,
            cache_write: self.cache_write + other.cache_write,
            without_cache: self.without_cache + other.without_cache,
        }
    }
}

/// Outcome of pricing one event. Each state is reported; none is silently zero.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Priced {
    Priced { model: String, cost: Cost },
    Synthetic,
    MissingModel,
    UnknownModel(String),
}

impl PriceList {
    pub fn new(as_of: String, models: Vec<ModelPrice>) -> Result<PriceList, PriceListError> {
        if models.is_empty() {
            return Err(PriceListError::Empty);
        }
        let mut seen_ids = HashSet::with_capacity(models.len());
        for model in &models {
            if !seen_ids.insert(model.id.as_str()) {
                return Err(PriceListError::DuplicateModel(model.id.clone()));
            }
            let Some(first_tier) = model.tiers.first() else {
                return Err(PriceListError::NoTiers(model.id.clone()));
            };
            if first_tier.prompt_tokens_over != 0 {
                return Err(PriceListError::FirstTierNotBase(model.id.clone()));
            }
            for pair in model.tiers.windows(2) {
                if pair[0].prompt_tokens_over >= pair[1].prompt_tokens_over {
                    return Err(PriceListError::TiersNotAscending(model.id.clone()));
                }
            }
        }
        Ok(PriceList { as_of, models })
    }

    pub fn find(&self, id: &str) -> Option<&ModelPrice> {
        self.models.iter().find(|model| model.id == id)
    }

    pub fn price(&self, model: &ModelName, tokens: &TokenUsage) -> Priced {
        match model {
            ModelName::Named(id) => match self.find(id) {
                Some(model_price) => Priced::Priced {
                    model: id.clone(),
                    cost: model_price.cost(tokens),
                },
                None => Priced::UnknownModel(id.clone()),
            },
            ModelName::Synthetic => Priced::Synthetic,
            ModelName::Missing => Priced::MissingModel,
        }
    }
}

impl ModelPrice {
    /// The band whose threshold the prompt exceeds; the base band when none does.
    pub fn tier_for(&self, prompt_tokens: u64) -> &PriceTier {
        self.tiers
            .iter()
            .rev()
            .find(|tier| tier.prompt_tokens_over < prompt_tokens)
            .or(self.tiers.first())
            .expect("PriceList::new rejects a model without tiers")
    }

    pub fn cost(&self, tokens: &TokenUsage) -> Cost {
        let prompt_tokens = tokens.prompt_tokens();
        let tier = self.tier_for(prompt_tokens);
        let output = tier.output.cost(tokens.output);
        Cost {
            input: tier.input.cost(tokens.input),
            output,
            cache_read: tier.cache_read.cost(tokens.cache_read),
            cache_write: tier.cache_write_5m.cost(tokens.cache_write_5m)
                + tier.cache_write_1h.cost(tokens.cache_write_1h),
            without_cache: tier.input.cost(prompt_tokens) + output,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tier(
        prompt_tokens_over: u64,
        input: f64,
        output: f64,
        cache_read: f64,
        cache_write_5m: f64,
        cache_write_1h: f64,
    ) -> PriceTier {
        PriceTier {
            prompt_tokens_over,
            input: PerToken::from_usd_per_mtok(input),
            output: PerToken::from_usd_per_mtok(output),
            cache_read: PerToken::from_usd_per_mtok(cache_read),
            cache_write_5m: PerToken::from_usd_per_mtok(cache_write_5m),
            cache_write_1h: PerToken::from_usd_per_mtok(cache_write_1h),
        }
    }

    fn model(id: &str, tiers: Vec<PriceTier>) -> ModelPrice {
        ModelPrice {
            id: id.to_string(),
            display_name: id.to_string(),
            tiers,
        }
    }

    fn opus_5_5() -> ModelPrice {
        model("claude-opus-5-5", vec![tier(0, 4.0, 20.0, 0.20, 5.0, 8.0)])
    }

    fn fable_5_1() -> ModelPrice {
        model(
            "claude-fable-5-1",
            vec![tier(0, 10.0, 50.0, 0.25, 12.5, 20.0)],
        )
    }

    fn haiku_5_5() -> ModelPrice {
        model(
            "claude-haiku-5-5",
            vec![
                tier(0, 0.10, 0.50, 0.01, 0.125, 0.20),
                tier(100_000, 0.50, 2.50, 0.05, 0.625, 1.0),
            ],
        )
    }

    fn price_list() -> PriceList {
        PriceList::new(
            "2026-10-28".to_string(),
            vec![opus_5_5(), fable_5_1(), haiku_5_5()],
        )
        .expect("test price list is valid")
    }

    #[test]
    fn new_accepts_a_valid_list() {
        let list = price_list();
        assert_eq!(list.models.len(), 3);
        assert_eq!(list.as_of, "2026-10-28");
        assert_eq!(
            list.find("claude-haiku-5-5").map(|found| found.tiers.len()),
            Some(2)
        );
        assert_eq!(list.find("claude-unknown"), None);
    }

    #[test]
    fn new_rejects_an_empty_list() {
        assert_eq!(
            PriceList::new("x".into(), vec![]),
            Err(PriceListError::Empty)
        );
    }

    #[test]
    fn new_rejects_a_duplicate_model() {
        let result = PriceList::new("x".into(), vec![opus_5_5(), fable_5_1(), opus_5_5()]);
        assert_eq!(
            result,
            Err(PriceListError::DuplicateModel("claude-opus-5-5".into()))
        );
    }

    #[test]
    fn new_rejects_a_model_without_tiers() {
        let result = PriceList::new("x".into(), vec![opus_5_5(), model("bare", vec![])]);
        assert_eq!(result, Err(PriceListError::NoTiers("bare".into())));
    }

    #[test]
    fn new_rejects_a_first_tier_above_zero() {
        let result = PriceList::new(
            "x".into(),
            vec![model("late", vec![tier(1, 1.0, 1.0, 1.0, 1.0, 1.0)])],
        );
        assert_eq!(result, Err(PriceListError::FirstTierNotBase("late".into())));
    }

    #[test]
    fn new_rejects_tiers_out_of_order_or_repeated() {
        let descending = model(
            "down",
            vec![
                tier(0, 1.0, 1.0, 1.0, 1.0, 1.0),
                tier(200, 1.0, 1.0, 1.0, 1.0, 1.0),
                tier(100, 1.0, 1.0, 1.0, 1.0, 1.0),
            ],
        );
        assert_eq!(
            PriceList::new("x".into(), vec![descending]),
            Err(PriceListError::TiersNotAscending("down".into()))
        );
        let repeated = model(
            "flat",
            vec![
                tier(0, 1.0, 1.0, 1.0, 1.0, 1.0),
                tier(0, 2.0, 2.0, 2.0, 2.0, 2.0),
            ],
        );
        assert_eq!(
            PriceList::new("x".into(), vec![repeated]),
            Err(PriceListError::TiersNotAscending("flat".into()))
        );
    }

    #[test]
    fn tier_boundary_is_exclusive() {
        let haiku = haiku_5_5();
        assert_eq!(haiku.tier_for(0).prompt_tokens_over, 0);
        assert_eq!(haiku.tier_for(100_000).prompt_tokens_over, 0);
        assert_eq!(haiku.tier_for(100_001).prompt_tokens_over, 100_000);
        assert_eq!(haiku.tier_for(1_000_000).prompt_tokens_over, 100_000);
    }

    #[test]
    fn opus_cost_splits_like_the_invoice() {
        let tokens = TokenUsage {
            input: 2,
            output: 246,
            cache_read: 11604,
            cache_write_5m: 0,
            cache_write_1h: 36476,
        };
        let cost = opus_5_5().cost(&tokens);
        assert_eq!(cost.input, Money::from_usd(0.000008));
        assert_eq!(cost.output, Money::from_usd(0.00492));
        assert_eq!(cost.cache_read, Money::from_usd(0.0023208));
        assert_eq!(cost.cache_write, Money::from_usd(0.291808));
        assert_eq!(cost.total(), Money::from_usd(0.2990568));
        assert_eq!(cost.without_cache, Money::from_usd(0.197248));
    }

    #[test]
    fn cache_write_sums_both_lifetimes() {
        let tokens = TokenUsage {
            input: 0,
            output: 0,
            cache_read: 0,
            cache_write_5m: 1_000_000,
            cache_write_1h: 1_000_000,
        };
        let cost = opus_5_5().cost(&tokens);
        assert_eq!(cost.cache_write, Money::from_usd(13.0));
        assert_eq!(cost.without_cache, Money::from_usd(8.0));
    }

    #[test]
    fn fable_cache_read_is_a_fortieth_of_input() {
        let tokens = TokenUsage {
            cache_read: 1_000_000,
            ..TokenUsage::default()
        };
        let cost = fable_5_1().cost(&tokens);
        assert_eq!(cost.total(), Money::from_usd(0.25));
        assert_eq!(cost.without_cache, Money::from_usd(10.0));
    }

    #[test]
    fn haiku_long_prompt_pays_the_second_tier_for_the_whole_request() {
        let long = haiku_5_5().cost(&TokenUsage {
            input: 120_000,
            output: 1000,
            ..TokenUsage::default()
        });
        assert_eq!(long.input, Money::from_usd(0.06));
        assert_eq!(long.output, Money::from_usd(0.0025));
        assert_eq!(long.without_cache, Money::from_usd(0.0625));
        let short = haiku_5_5().cost(&TokenUsage {
            input: 1000,
            output: 1000,
            ..TokenUsage::default()
        });
        assert_eq!(short.input, Money::from_usd(0.0001));
        assert_eq!(short.output, Money::from_usd(0.0005));
    }

    #[test]
    fn haiku_tier_counts_cached_prompt_tokens() {
        let tokens = TokenUsage {
            input: 1,
            cache_read: 100_000,
            ..TokenUsage::default()
        };
        let cost = haiku_5_5().cost(&tokens);
        assert_eq!(cost.cache_read, Money::from_usd(0.005));
    }

    #[test]
    fn price_maps_every_model_state() {
        let list = price_list();
        let tokens = TokenUsage {
            input: 1000,
            output: 1000,
            ..TokenUsage::default()
        };
        let named = list.price(&ModelName::Named("claude-haiku-5-5".into()), &tokens);
        assert_eq!(
            named,
            Priced::Priced {
                model: "claude-haiku-5-5".into(),
                cost: Cost {
                    input: Money::from_usd(0.0001),
                    output: Money::from_usd(0.0005),
                    cache_read: Money::ZERO,
                    cache_write: Money::ZERO,
                    without_cache: Money::from_usd(0.0006),
                }
            }
        );
        assert_eq!(
            list.price(&ModelName::Named("claude-opus-9".into()), &tokens),
            Priced::UnknownModel("claude-opus-9".into())
        );
        assert_eq!(
            list.price(&ModelName::Synthetic, &tokens),
            Priced::Synthetic
        );
        assert_eq!(
            list.price(&ModelName::Missing, &tokens),
            Priced::MissingModel
        );
    }
}
