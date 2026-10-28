use std::collections::BTreeMap;

use jiff::Timestamp;
use serde::Serialize;
use thiserror::Error;

use crate::domain::patterns::{self, Patterns, PricedEvent};
use crate::domain::plans::{self, EstimateOptions, MonthUsage, PlanCatalog, PlanError, PlanEstimate, YearMonth};
use crate::domain::pricing::{PriceList, Priced};
use crate::domain::usage::{Granularity, Scan, ScanStats};

/// Requests the price list could not price, by reason. Always reported; never folded into $0.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Unpriced {
    pub synthetic: u64,
    pub missing_model: u64,
    pub unknown_models: BTreeMap<String, u64>,
}

impl Unpriced {
    pub fn total(&self) -> u64 {
        self.synthetic + self.missing_model + self.unknown_models.values().sum::<u64>()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Report {
    pub source: String,
    pub pricing_as_of: String,
    pub granularity: Granularity,
    pub first_event: Option<Timestamp>,
    pub last_event: Option<Timestamp>,
    pub scan: ScanStats,
    pub unpriced: Unpriced,
    pub options: EstimateOptions,
    pub allowance_anchor: crate::domain::money::Money,
    pub months: Vec<MonthUsage>,
    pub plans: Vec<PlanEstimate>,
    pub patterns: Patterns,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ReportError {
    #[error("no transcript files were found")]
    NoFiles,
    #[error("no priced requests in range ({unpriced} unpriced, {filtered_out} filtered out)")]
    NothingPriced { unpriced: u64, filtered_out: u64 },
    #[error(transparent)]
    Plan(#[from] PlanError),
}

/// Prices every event, buckets the unpriced ones, groups priced usage by UTC month
/// split on `entrypoint.credits_eligible()`, then runs the plan estimate and the
/// pattern analysis. Fails rather than reporting a clean zero when nothing was scanned
/// or nothing could be priced.
pub fn build(source: String, scan: Scan, prices: &PriceList, catalog: &PlanCatalog, options: EstimateOptions) -> Result<Report, ReportError> {
    todo!("see the doc comment")
}
