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
        todo!("validate: non-empty, unique ids, each model has tiers, first tier at 0, strictly ascending")
    }

    pub fn find(&self, id: &str) -> Option<&ModelPrice> {
        self.models.iter().find(|model| model.id == id)
    }

    pub fn price(&self, model: &ModelName, tokens: &TokenUsage) -> Priced {
        todo!("map ModelName to Priced; Named looks up by canonical id")
    }
}

impl ModelPrice {
    /// The band whose threshold the prompt exceeds; the base band when none does.
    pub fn tier_for(&self, prompt_tokens: u64) -> &PriceTier {
        todo!("last tier with prompt_tokens_over < prompt_tokens, else tiers[0]")
    }

    pub fn cost(&self, tokens: &TokenUsage) -> Cost {
        todo!("pick tier by tokens.prompt_tokens(); input/output/cache_read/cache_write at their rates; without_cache = prompt_tokens at input rate + output")
    }
}
