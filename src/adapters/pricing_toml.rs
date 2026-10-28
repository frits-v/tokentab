//! Loads the price list and plan catalog from TOML. The file schema lives here;
//! the domain never sees it. `DEFAULT_PRICING` is compiled in so the binary works
//! without any file next to it.

use std::collections::HashSet;

use serde::Deserialize;
use thiserror::Error;

use crate::domain::money::{Money, PerToken};
use crate::domain::plans::{Plan, PlanCatalog, PlanKind};
use crate::domain::pricing::{ModelPrice, PriceList, PriceListError, PriceTier};

pub const DEFAULT_PRICING: &str = include_str!("../../pricing/anthropic.toml");

/// Bounds that keep every converted amount far inside u64 nanodollars, even times `MAX_SEATS`.
const MAX_USD_PER_MTOK: f64 = 1_000_000.0;
pub const MAX_USD_AMOUNT: f64 = 100_000.0;
pub const MAX_SEATS: u32 = 10_000;

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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PricingFile {
    as_of: String,
    models: Vec<ModelEntry>,
    plans: PlansSection,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelEntry {
    id: String,
    display_name: String,
    tiers: Vec<TierEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TierEntry {
    prompt_tokens_over: u64,
    input: f64,
    output: f64,
    cache_read: f64,
    cache_write_5m: f64,
    cache_write_1h: f64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlansSection {
    allowance_anchor_usd: f64,
    plan: Vec<PlanEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlanEntry {
    id: String,
    name: String,
    kind: PlanKindEntry,
    seat_monthly_usd: Option<f64>,
    seat_annual_usd: Option<f64>,
    min_seats: u32,
    max_seats: Option<u32>,
    allowance_multiple: f64,
    api_credits_per_seat_usd: f64,
    api_credits_pool_cap_usd: Option<f64>,
    extra_usage_at_api_rates: bool,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PlanKindEntry {
    PayAsYouGo,
    Subscription,
    SeatPlusUsage,
}

impl PlanKindEntry {
    fn to_domain(self) -> PlanKind {
        match self {
            PlanKindEntry::PayAsYouGo => PlanKind::PayAsYouGo,
            PlanKindEntry::Subscription => PlanKind::Subscription,
            PlanKindEntry::SeatPlusUsage => PlanKind::SeatPlusUsage,
        }
    }
}

/// Parses the TOML text. Validates at this boundary: every price finite and
/// non-negative, every plan with at least one billing option, `min_seats >= 1`,
/// `allowance_multiple >= 0`, and plan ids unique.
pub fn load(text: &str) -> Result<Loaded, PricingFileError> {
    // Parsed as a plain table first so a syntax error and a schema error are reported as different kinds.
    text.parse::<toml::Table>()
        .map_err(|error| PricingFileError::Syntax(error.to_string()))?;
    let file: PricingFile =
        toml::from_str(text).map_err(|error| PricingFileError::Invalid(error.to_string()))?;

    let mut models = Vec::with_capacity(file.models.len());
    for entry in &file.models {
        models.push(convert_model(entry)?);
    }
    let prices = PriceList::new(file.as_of, models)?;
    let plans = convert_plans(&file.plans)?;
    Ok(Loaded { prices, plans })
}

fn convert_model(entry: &ModelEntry) -> Result<ModelPrice, PricingFileError> {
    let mut tiers = Vec::with_capacity(entry.tiers.len());
    for (index, tier) in entry.tiers.iter().enumerate() {
        let place = format!("model {} tier {index}", entry.id);
        tiers.push(PriceTier {
            prompt_tokens_over: tier.prompt_tokens_over,
            input: per_token(tier.input, &place, "input")?,
            output: per_token(tier.output, &place, "output")?,
            cache_read: per_token(tier.cache_read, &place, "cache_read")?,
            cache_write_5m: per_token(tier.cache_write_5m, &place, "cache_write_5m")?,
            cache_write_1h: per_token(tier.cache_write_1h, &place, "cache_write_1h")?,
        });
    }
    Ok(ModelPrice {
        id: entry.id.clone(),
        display_name: entry.display_name.clone(),
        tiers,
    })
}

fn convert_plans(section: &PlansSection) -> Result<PlanCatalog, PricingFileError> {
    let allowance_anchor = dollars(
        section.allowance_anchor_usd,
        "plans",
        "allowance_anchor_usd",
    )?;
    if section.plan.is_empty() {
        return Err(PricingFileError::Invalid(
            "plans: no plans listed".to_string(),
        ));
    }
    let mut seen_ids = HashSet::with_capacity(section.plan.len());
    let mut plans = Vec::with_capacity(section.plan.len());
    for entry in &section.plan {
        if !seen_ids.insert(entry.id.as_str()) {
            return Err(PricingFileError::Invalid(format!(
                "plan {} appears more than once",
                entry.id
            )));
        }
        plans.push(convert_plan(entry)?);
    }
    Ok(PlanCatalog {
        allowance_anchor,
        plans,
    })
}

fn convert_plan(entry: &PlanEntry) -> Result<Plan, PricingFileError> {
    let place = format!("plan {}", entry.id);
    let kind = entry.kind.to_domain();
    let seat_monthly = optional_dollars(entry.seat_monthly_usd, &place, "seat_monthly_usd")?;
    let seat_annual = optional_dollars(entry.seat_annual_usd, &place, "seat_annual_usd")?;
    if kind != PlanKind::PayAsYouGo && seat_monthly.is_none() && seat_annual.is_none() {
        return Err(invalid(
            &place,
            "seat_monthly_usd",
            "needs seat_monthly_usd or seat_annual_usd unless kind is pay_as_you_go",
        ));
    }
    if entry.min_seats < 1 {
        return Err(invalid(&place, "min_seats", "must be at least 1"));
    }
    if let Some(max_seats) = entry.max_seats
        && max_seats < entry.min_seats
    {
        return Err(invalid(
            &place,
            "max_seats",
            &format!("{max_seats} is below min_seats {}", entry.min_seats),
        ));
    }
    if !entry.allowance_multiple.is_finite() || entry.allowance_multiple < 0.0 {
        return Err(invalid(
            &place,
            "allowance_multiple",
            &format!(
                "must be finite and non-negative, got {}",
                entry.allowance_multiple
            ),
        ));
    }
    Ok(Plan {
        id: entry.id.clone(),
        name: entry.name.clone(),
        kind,
        seat_monthly,
        seat_annual,
        min_seats: entry.min_seats,
        max_seats: entry.max_seats,
        allowance_multiple: entry.allowance_multiple,
        api_credits_per_seat: dollars(
            entry.api_credits_per_seat_usd,
            &place,
            "api_credits_per_seat_usd",
        )?,
        api_credits_pool_cap: optional_dollars(
            entry.api_credits_pool_cap_usd,
            &place,
            "api_credits_pool_cap_usd",
        )?,
        extra_usage_at_api_rates: entry.extra_usage_at_api_rates,
    })
}

fn per_token(usd_per_mtok: f64, place: &str, field: &str) -> Result<PerToken, PricingFileError> {
    if !usd_per_mtok.is_finite() || !(0.0..=MAX_USD_PER_MTOK).contains(&usd_per_mtok) {
        return Err(invalid(
            place,
            field,
            &format!(
                "must be between 0 and {MAX_USD_PER_MTOK} dollars per MTok, got {usd_per_mtok}"
            ),
        ));
    }
    let nanos_per_token = usd_per_mtok * 1_000.0;
    if (nanos_per_token - nanos_per_token.round()).abs() >= 1e-6 {
        return Err(invalid(
            place,
            field,
            &format!("{usd_per_mtok} per MTok is not a whole number of nanodollars per token"),
        ));
    }
    Ok(PerToken::from_usd_per_mtok(usd_per_mtok))
}

fn dollars(usd: f64, place: &str, field: &str) -> Result<Money, PricingFileError> {
    if !usd.is_finite() || !(0.0..=MAX_USD_AMOUNT).contains(&usd) {
        return Err(invalid(
            place,
            field,
            &format!("must be between 0 and {MAX_USD_AMOUNT} dollars, got {usd}"),
        ));
    }
    Ok(Money::from_usd(usd))
}

fn optional_dollars(
    usd: Option<f64>,
    place: &str,
    field: &str,
) -> Result<Option<Money>, PricingFileError> {
    usd.map(|amount| dollars(amount, place, field)).transpose()
}

fn invalid(place: &str, field: &str, reason: &str) -> PricingFileError {
    PricingFileError::Invalid(format!("{place} {field}: {reason}"))
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use crate::domain::pricing::Priced;
    use crate::domain::usage::{ModelName, TokenUsage};

    const MINIMAL: &str = r#"
as_of = "2026-10-28"

[[models]]
id = "claude-opus-5-5"
display_name = "Claude Opus 5.5"
tiers = [{ prompt_tokens_over = 0, input = 4.0, output = 20.0, cache_read = 0.20, cache_write_5m = 5.0, cache_write_1h = 8.0 }]

[plans]
allowance_anchor_usd = 60.0

[[plans.plan]]
id = "pro"
name = "Pro"
kind = "subscription"
seat_monthly_usd = 20.0
min_seats = 1
max_seats = 1
allowance_multiple = 1.0
api_credits_per_seat_usd = 0.0
extra_usage_at_api_rates = true
"#;

    fn load_error(text: &str) -> PricingFileError {
        match load(text) {
            Ok(_) => panic!("expected the pricing file to be rejected"),
            Err(error) => error,
        }
    }

    fn invalid_message(text: &str) -> String {
        match load_error(text) {
            PricingFileError::Invalid(message) => message,
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    fn default_loaded() -> Loaded {
        load(DEFAULT_PRICING).expect("the compiled-in price list loads")
    }

    #[test]
    fn default_pricing_loads_every_model() {
        let loaded = default_loaded();
        assert_eq!(loaded.prices.as_of, "2026-10-28");
        assert_eq!(loaded.prices.models.len(), 20);
        assert_eq!(loaded.plans.allowance_anchor, Money::from_usd(60.0));
    }

    #[test]
    fn default_haiku_5_5_has_a_second_tier_at_100k() {
        let loaded = default_loaded();
        let haiku = loaded
            .prices
            .find("claude-haiku-5-5")
            .expect("haiku 5.5 is listed");
        assert_eq!(haiku.tiers.len(), 2);
        assert_eq!(haiku.tiers[0].prompt_tokens_over, 0);
        assert_eq!(haiku.tiers[1].prompt_tokens_over, 100_000);
        assert_eq!(haiku.tiers[1].input, PerToken::from_usd_per_mtok(0.50));
    }

    #[test]
    fn default_plans_are_listed_in_file_order() {
        let loaded = default_loaded();
        let ids: Vec<&str> = loaded
            .plans
            .plans
            .iter()
            .map(|plan| plan.id.as_str())
            .collect();
        assert_eq!(
            ids,
            [
                "api",
                "pro",
                "max5",
                "max20",
                "team_standard",
                "team_premium",
                "enterprise"
            ]
        );
    }

    #[test]
    fn default_enterprise_is_annual_only_with_twenty_seats() {
        let loaded = default_loaded();
        let enterprise = loaded
            .plans
            .plans
            .iter()
            .find(|plan| plan.id == "enterprise")
            .expect("enterprise is listed");
        assert_eq!(enterprise.kind, PlanKind::SeatPlusUsage);
        assert_eq!(enterprise.seat_monthly, None);
        assert_eq!(enterprise.seat_annual, Some(Money::from_usd(20.0)));
        assert_eq!(enterprise.min_seats, 20);
        assert_eq!(enterprise.max_seats, None);
    }

    #[test]
    fn default_team_standard_pool_is_capped() {
        let loaded = default_loaded();
        let team = loaded
            .plans
            .plans
            .iter()
            .find(|plan| plan.id == "team_standard")
            .expect("team_standard is listed");
        assert_eq!(team.api_credits_pool_cap, Some(Money::from_usd(500.0)));
        assert_eq!(team.api_credits_per_seat, Money::from_usd(20.0));
        assert_eq!(team.min_seats, 2);
        assert_eq!(team.max_seats, Some(150));
    }

    #[test]
    fn default_api_plan_needs_no_seat_price() {
        let loaded = default_loaded();
        let api = loaded
            .plans
            .plans
            .iter()
            .find(|plan| plan.id == "api")
            .expect("api is listed");
        assert_eq!(api.kind, PlanKind::PayAsYouGo);
        assert_eq!(api.seat_monthly, None);
        assert_eq!(api.seat_annual, None);
    }

    #[test]
    fn default_opus_5_5_prices_the_fixture_request() {
        let loaded = default_loaded();
        let tokens = TokenUsage {
            input: 2,
            output: 246,
            cache_read: 11604,
            cache_write_5m: 0,
            cache_write_1h: 36476,
        };
        match loaded
            .prices
            .price(&ModelName::Named("claude-opus-5-5".into()), &tokens)
        {
            Priced::Priced { model, cost } => {
                assert_eq!(model, "claude-opus-5-5");
                assert_eq!(cost.total(), Money::from_usd(0.2990568));
                assert_eq!(cost.without_cache, Money::from_usd(0.197248));
            }
            other => panic!("expected a priced request, got {other:?}"),
        }
    }

    #[test]
    fn minimal_file_loads() {
        let loaded = load(MINIMAL).expect("minimal file is valid");
        assert_eq!(loaded.prices.models.len(), 1);
        assert_eq!(loaded.plans.plans.len(), 1);
    }

    #[test]
    fn integer_prices_are_accepted() {
        let loaded =
            load(&MINIMAL.replace("input = 4.0", "input = 4")).expect("integer price is valid");
        assert_eq!(
            loaded.prices.models[0].tiers[0].input,
            PerToken::from_usd_per_mtok(4.0)
        );
    }

    #[test]
    fn unknown_field_is_rejected() {
        let message =
            invalid_message(&MINIMAL.replace("min_seats = 1", "min_seats = 1\nmin_seat = 1"));
        assert!(message.contains("min_seat"), "{message}");
        invalid_message(&MINIMAL.replace("input = 4.0", "input = 4.0, inptu = 4.0"));
        invalid_message(&MINIMAL.replace("as_of", "currency = \"usd\"\nas_of"));
    }

    #[test]
    fn missing_field_is_rejected() {
        invalid_message(&MINIMAL.replace("extra_usage_at_api_rates = true", ""));
    }

    #[test]
    fn unknown_plan_kind_is_rejected() {
        let message =
            invalid_message(&MINIMAL.replace("kind = \"subscription\"", "kind = \"freemium\""));
        assert!(message.contains("freemium"), "{message}");
    }

    #[test]
    fn negative_price_is_rejected_naming_model_and_field() {
        let message = invalid_message(&MINIMAL.replace("cache_read = 0.20", "cache_read = -0.20"));
        assert!(
            message.contains("claude-opus-5-5") && message.contains("cache_read"),
            "{message}"
        );
    }

    #[test]
    fn fractional_nanodollar_price_is_rejected() {
        let message = invalid_message(&MINIMAL.replace("output = 20.0", "output = 20.0001"));
        assert!(
            message.contains("claude-opus-5-5") && message.contains("output"),
            "{message}"
        );
    }

    #[test]
    fn non_finite_price_is_rejected() {
        invalid_message(&MINIMAL.replace("input = 4.0", "input = inf"));
        invalid_message(&MINIMAL.replace("input = 4.0", "input = nan"));
    }

    #[test]
    fn negative_dollar_amounts_are_rejected() {
        let anchor = invalid_message(
            &MINIMAL.replace("allowance_anchor_usd = 60.0", "allowance_anchor_usd = -1.0"),
        );
        assert!(anchor.contains("allowance_anchor_usd"), "{anchor}");
        let seat = invalid_message(
            &MINIMAL.replace("seat_monthly_usd = 20.0", "seat_monthly_usd = -20.0"),
        );
        assert!(
            seat.contains("plan pro") && seat.contains("seat_monthly_usd"),
            "{seat}"
        );
        let credits = invalid_message(&MINIMAL.replace(
            "api_credits_per_seat_usd = 0.0",
            "api_credits_per_seat_usd = -5.0",
        ));
        assert!(credits.contains("api_credits_per_seat_usd"), "{credits}");
    }

    #[test]
    fn duplicate_plan_id_is_rejected() {
        let plan_start = MINIMAL
            .find("[[plans.plan]]")
            .expect("minimal file has a plan");
        let doubled = format!("{MINIMAL}\n{}", &MINIMAL[plan_start..]);
        let message = invalid_message(&doubled);
        assert!(
            message.contains("plan pro appears more than once"),
            "{message}"
        );
    }

    #[test]
    fn plan_without_any_seat_price_is_rejected() {
        let message = invalid_message(&MINIMAL.replace("seat_monthly_usd = 20.0\n", ""));
        assert!(message.contains("plan pro"), "{message}");
    }

    #[test]
    fn pay_as_you_go_without_seat_price_is_accepted() {
        let text = MINIMAL
            .replace("seat_monthly_usd = 20.0\n", "")
            .replace("kind = \"subscription\"", "kind = \"pay_as_you_go\"");
        let loaded = load(&text).expect("pay-as-you-go needs no seat price");
        assert_eq!(loaded.plans.plans[0].kind, PlanKind::PayAsYouGo);
    }

    #[test]
    fn seat_bounds_are_checked() {
        let zero = invalid_message(&MINIMAL.replace("min_seats = 1", "min_seats = 0"));
        assert!(zero.contains("min_seats"), "{zero}");
        let inverted = invalid_message(&MINIMAL.replace("min_seats = 1", "min_seats = 3"));
        assert!(inverted.contains("max_seats"), "{inverted}");
    }

    #[test]
    fn dollar_amount_above_the_cap_is_rejected() {
        let message = invalid_message(
            &MINIMAL.replace("seat_monthly_usd = 20.0", "seat_monthly_usd = 100000.01"),
        );
        assert!(
            message.contains("plan pro") && message.contains("seat_monthly_usd"),
            "{message}"
        );
        let loaded =
            load(&MINIMAL.replace("seat_monthly_usd = 20.0", "seat_monthly_usd = 100000.0"))
                .expect("the cap itself is allowed");
        assert_eq!(
            loaded.plans.plans[0].seat_monthly,
            Some(Money::from_usd(100_000.0))
        );
    }

    #[test]
    fn negative_allowance_multiple_is_rejected() {
        let message = invalid_message(
            &MINIMAL.replace("allowance_multiple = 1.0", "allowance_multiple = -1.0"),
        );
        assert!(message.contains("allowance_multiple"), "{message}");
    }

    #[test]
    fn empty_plan_list_is_rejected() {
        let plan_start = MINIMAL
            .find("[[plans.plan]]")
            .expect("minimal file has a plan");
        let text = format!("{}plan = []\n", &MINIMAL[..plan_start]);
        let message = invalid_message(&text);
        assert!(message.contains("no plans"), "{message}");
    }

    #[test]
    fn price_list_errors_surface() {
        let error =
            load_error(&MINIMAL.replace("prompt_tokens_over = 0", "prompt_tokens_over = 5"));
        assert!(
            matches!(error, PricingFileError::PriceList(PriceListError::FirstTierNotBase(ref id)) if id == "claude-opus-5-5"),
            "{error:?}"
        );
        let error = load_error(&MINIMAL.replace("tiers = [{ prompt_tokens_over = 0, input = 4.0, output = 20.0, cache_read = 0.20, cache_write_5m = 5.0, cache_write_1h = 8.0 }]", "tiers = []"));
        assert!(
            matches!(
                error,
                PricingFileError::PriceList(PriceListError::NoTiers(_))
            ),
            "{error:?}"
        );
    }

    #[test]
    fn toml_syntax_error_is_reported_as_syntax() {
        let error = load_error("as_of = \"2026-10-28\"\n[[models]\nid = ");
        assert!(matches!(error, PricingFileError::Syntax(_)), "{error:?}");
    }
}
