//! Loads the price list and plan catalog from TOML. The file schema lives here;
//! the domain never sees it. `DEFAULT_PRICING` is compiled in so the binary works
//! without any file next to it.

use thiserror::Error;

use crate::domain::plans::PlanCatalog;
use crate::domain::pricing::{PriceList, PriceListError};

pub const DEFAULT_PRICING: &str = include_str!("../../pricing/anthropic.toml");

#[derive(Debug, Error)]
pub enum PricingFileError {
    #[error("pricing file is not valid TOML: {0}")]
    Syntax(String),
    #[error("pricing file: {0}")]
    Invalid(String),
    #[error(transparent)]
    PriceList(#[from] PriceListError),
}

pub struct Loaded {
    pub prices: PriceList,
    pub plans: PlanCatalog,
}

/// Parses the TOML text. Validates at this boundary: every price finite and
/// non-negative, every plan with at least one billing option, `min_seats >= 1`,
/// `allowance_multiple >= 0`, and plan ids unique.
pub fn load(text: &str) -> Result<Loaded, PricingFileError> {
    todo!("serde structs mirroring pricing/anthropic.toml, then convert to domain types")
}
