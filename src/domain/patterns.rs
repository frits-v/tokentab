use jiff::Timestamp;
use serde::Serialize;

use crate::domain::money::Money;
use crate::domain::pricing::Cost;
use crate::domain::usage::{Granularity, TokenUsage, UsageEvent};

/// An event the price list could price.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PricedEvent {
    pub event: UsageEvent,
    pub model: String,
    pub cost: Cost,
}

/// The most expensive window of a fixed length anywhere in the range.
/// Plan limits are enforced per rolling window, so this is where a plan would have throttled.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct WindowPeak {
    pub start: Timestamp,
    pub end: Timestamp,
    pub cost: Money,
    pub requests: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ModelTotals {
    pub model: String,
    pub requests: u64,
    pub tokens: TokenUsage,
    pub cost: Cost,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct EntrypointTotals {
    pub entrypoint: String,
    pub requests: u64,
    pub cost: Money,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ProjectTotals {
    pub project: String,
    pub requests: u64,
    pub cost: Money,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CacheStats {
    pub input_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_5m_tokens: u64,
    pub cache_write_1h_tokens: u64,
    /// cache_read / prompt tokens over all priced events.
    pub cache_read_share: f64,
    pub spend: Money,
    pub spend_without_cache: Money,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Patterns {
    /// `None` when the source's granularity cannot support a rolling window.
    pub window_5h: Option<WindowPeak>,
    pub window_7d: Option<WindowPeak>,
    pub active_days: u64,
    pub sessions: u64,
    pub requests: u64,
    pub sidechain_share: f64,
    pub per_model: Vec<ModelTotals>,
    pub per_entrypoint: Vec<EntrypointTotals>,
    /// Top ten projects by cost, descending.
    pub per_project: Vec<ProjectTotals>,
    pub cache: CacheStats,
}

/// Rolling-window peaks use a two-pointer sweep over events sorted by timestamp:
/// a window is `[start, start + length)`, and the peak is the window with the highest
/// cost total. `per_model`, `per_entrypoint` and `per_project` are sorted by cost descending.
/// Windows are `None` at `Granularity::Day`.
pub fn analyse(events: &[PricedEvent], granularity: Granularity) -> Patterns {
    todo!("see the doc comment")
}
