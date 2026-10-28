use std::fmt;

use jiff::Timestamp;
use serde::Serialize;
use thiserror::Error;

use crate::domain::money::Money;
use crate::domain::pricing::Cost;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanKind {
    /// No fee; every token at API rates. The Console / Enterprise API shape.
    PayAsYouGo,
    /// Seat fee buys an allowance; usage beyond it is extra usage at API rates.
    Subscription,
    /// Seat fee plus every token at API rates, no allowance (Claude Enterprise).
    SeatPlusUsage,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Billing {
    Monthly,
    Annual,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CreditsMode {
    /// Bundled API credits offset SDK / headless usage up to the pool.
    Apply,
    Ignore,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Plan {
    pub id: String,
    pub name: String,
    pub kind: PlanKind,
    /// Per seat per month on monthly billing; `None` when the plan is annual-only.
    pub seat_monthly: Option<Money>,
    /// Per seat per month on annual billing; `None` when the plan is monthly-only.
    pub seat_annual: Option<Money>,
    pub min_seats: u32,
    pub max_seats: Option<u32>,
    /// Included usage per seat as a multiple of the catalog's allowance anchor.
    pub allowance_multiple: f64,
    pub api_credits_per_seat: Money,
    pub api_credits_pool_cap: Option<Money>,
    pub extra_usage_at_api_rates: bool,
}

/// Anthropic publishes only ratios between plans (Max is 5x or 20x Pro, a Premium
/// seat is 5x a Standard seat). The anchor is the one assumption: the API-equivalent
/// usage one Pro seat gets per month. Every allowance is `anchor * allowance_multiple`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PlanCatalog {
    pub allowance_anchor: Money,
    pub plans: Vec<Plan>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct EstimateOptions {
    pub seats: u32,
    pub billing: Billing,
    pub credits: CreditsMode,
    /// Overrides the catalog anchor when set.
    pub allowance_anchor: Option<Money>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct YearMonth {
    pub year: i16,
    pub month: i8,
}

impl YearMonth {
    pub fn of(timestamp: Timestamp) -> YearMonth {
        let zoned = timestamp.to_zoned(jiff::tz::TimeZone::UTC);
        YearMonth { year: zoned.year(), month: zoned.month() }
    }
}

impl fmt::Display for YearMonth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}-{:02}", self.year, self.month)
    }
}

/// One seat's priced usage in one calendar month, split by whether bundled API credits could cover it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MonthUsage {
    pub month: YearMonth,
    pub interactive: Cost,
    pub headless: Cost,
    pub requests: u64,
    pub unpriced_requests: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PlanMonth {
    pub month: YearMonth,
    pub seats_billed: u32,
    pub seat_fee: Money,
    pub allowance: Money,
    pub interactive_api_equivalent: Money,
    pub extra_usage: Money,
    pub headless_api_equivalent: Money,
    pub credits_applied: Money,
    pub headless_billed: Money,
    pub total: Money,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Sensitivity {
    pub half_anchor_average_month: Money,
    pub double_anchor_average_month: Money,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PlanEstimate {
    pub plan_id: String,
    pub plan_name: String,
    pub kind: PlanKind,
    pub months: Vec<PlanMonth>,
    pub average_month: Money,
    pub peak_month: Money,
    pub total: Money,
    /// Present only for plans whose number depends on the allowance anchor.
    pub sensitivity: Option<Sensitivity>,
    /// Assumptions the reader must see next to the number (seat minimum applied, billing fallback, ...).
    pub notes: Vec<String>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PlanError {
    #[error("no months of usage to estimate")]
    NoMonths,
    #[error("plan {plan} offers neither monthly nor annual billing")]
    NoBilling { plan: String },
    #[error("plan catalog has no plans")]
    EmptyCatalog,
}

/// Estimates every plan in the catalog against the same months of one seat's usage.
///
/// Rules:
/// - seats below `min_seats` are raised to the minimum and a note says so; seats above
///   `max_seats` are capped with a note.
/// - the usage belongs to one seat; other seats carry their fee and nothing else.
/// - `PayAsYouGo`: total = interactive + headless at API rates, no fee, no credits.
/// - `Subscription`: fee = seats x seat price for the chosen billing (fall back to the
///   other billing with a note when not offered); allowance = anchor x multiple for the
///   one active seat; extra usage = max(0, interactive - allowance) when
///   `extra_usage_at_api_rates`, otherwise a note says the usage would be throttled;
///   credits pool = min(credits_per_seat x seats, pool cap); headless billed = headless
///   minus pool when `CreditsMode::Apply`, else headless in full.
/// - `SeatPlusUsage`: fee = seats x seat price; interactive + headless at API rates; no credits.
/// - `sensitivity` recomputes the average month with the anchor halved and doubled,
///   only for `Subscription` plans.
pub fn estimate(catalog: &PlanCatalog, options: &EstimateOptions, months: &[MonthUsage]) -> Result<Vec<PlanEstimate>, PlanError> {
    todo!("see the rules in the doc comment")
}
