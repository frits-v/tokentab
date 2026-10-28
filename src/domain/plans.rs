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
        YearMonth {
            year: zoned.year(),
            month: zoned.month(),
        }
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
pub fn estimate(
    catalog: &PlanCatalog,
    options: &EstimateOptions,
    months: &[MonthUsage],
) -> Result<Vec<PlanEstimate>, PlanError> {
    if catalog.plans.is_empty() {
        return Err(PlanError::EmptyCatalog);
    }
    if months.is_empty() {
        return Err(PlanError::NoMonths);
    }
    let anchor = options.allowance_anchor.unwrap_or(catalog.allowance_anchor);
    let mut estimates = Vec::with_capacity(catalog.plans.len());
    for plan in &catalog.plans {
        estimates.push(estimate_plan(plan, options, months, anchor)?);
    }
    Ok(estimates)
}

const THROTTLED_NOTE: &str = "extra usage not offered on this plan; usage above the allowance would be throttled, not billed";

/// Seat count, fee and credit settings shared by every month of one plan.
struct PlanTerms<'a> {
    plan: &'a Plan,
    credits: CreditsMode,
    seats_billed: u32,
    seat_fee: Money,
}

fn estimate_plan(
    plan: &Plan,
    options: &EstimateOptions,
    months: &[MonthUsage],
    anchor: Money,
) -> Result<PlanEstimate, PlanError> {
    let mut notes = Vec::new();
    let seats_billed = billed_seats(plan, options.seats, &mut notes);
    let seat_price = seat_price(plan, options.billing, &mut notes)?;
    if plan.kind == PlanKind::Subscription && !plan.extra_usage_at_api_rates {
        notes.push(THROTTLED_NOTE.to_string());
    }
    let terms = PlanTerms {
        plan,
        credits: options.credits,
        seats_billed,
        seat_fee: seat_price.times(u64::from(seats_billed)),
    };

    let plan_months = price_months(&terms, months, anchor);
    let total: Money = plan_months.iter().map(|month| month.total).sum();
    let peak_month = plan_months
        .iter()
        .map(|month| month.total)
        .max()
        .unwrap_or(Money::ZERO);
    let sensitivity = match plan.kind {
        PlanKind::Subscription => Some(Sensitivity {
            half_anchor_average_month: average_month(&price_months(
                &terms,
                months,
                anchor.scale(0.5),
            )),
            double_anchor_average_month: average_month(&price_months(
                &terms,
                months,
                anchor.times(2),
            )),
        }),
        PlanKind::PayAsYouGo | PlanKind::SeatPlusUsage => None,
    };

    Ok(PlanEstimate {
        plan_id: plan.id.clone(),
        plan_name: plan.name.clone(),
        kind: plan.kind,
        average_month: average_month(&plan_months),
        months: plan_months,
        peak_month,
        total,
        sensitivity,
        notes,
    })
}

fn billed_seats(plan: &Plan, requested: u32, notes: &mut Vec<String>) -> u32 {
    if requested < plan.min_seats {
        notes.push(format!(
            "seat minimum: {} seats billed (requested {requested})",
            plan.min_seats
        ));
        return plan.min_seats;
    }
    match plan.max_seats {
        Some(max_seats) if requested > max_seats => {
            notes.push(format!(
                "seat maximum: {max_seats} seats billed (requested {requested})"
            ));
            max_seats
        }
        _ => requested,
    }
}

fn seat_price(plan: &Plan, billing: Billing, notes: &mut Vec<String>) -> Result<Money, PlanError> {
    if plan.kind == PlanKind::PayAsYouGo {
        return Ok(Money::ZERO);
    }
    let (preferred, fallback, fallback_note) = match billing {
        Billing::Monthly => (
            plan.seat_monthly,
            plan.seat_annual,
            "monthly billing not offered; annual seat price used",
        ),
        Billing::Annual => (
            plan.seat_annual,
            plan.seat_monthly,
            "annual billing not offered; monthly seat price used",
        ),
    };
    match (preferred, fallback) {
        (Some(price), _) => Ok(price),
        (None, Some(price)) => {
            notes.push(fallback_note.to_string());
            Ok(price)
        }
        (None, None) => Err(PlanError::NoBilling {
            plan: plan.id.clone(),
        }),
    }
}

/// Takes the anchor as a parameter so the estimate and both sensitivity runs share one path.
fn price_months(terms: &PlanTerms, months: &[MonthUsage], anchor: Money) -> Vec<PlanMonth> {
    months
        .iter()
        .map(|month| price_month(terms, month, anchor))
        .collect()
}

fn price_month(terms: &PlanTerms, month: &MonthUsage, anchor: Money) -> PlanMonth {
    let plan = terms.plan;
    let interactive = month.interactive.total();
    let headless = month.headless.total();
    let (seat_fee, allowance, extra_usage, credits_applied) = match plan.kind {
        PlanKind::PayAsYouGo => (Money::ZERO, Money::ZERO, interactive, Money::ZERO),
        PlanKind::SeatPlusUsage => (terms.seat_fee, Money::ZERO, interactive, Money::ZERO),
        PlanKind::Subscription => {
            let allowance = anchor.scale(plan.allowance_multiple);
            let extra_usage = if plan.extra_usage_at_api_rates {
                interactive.saturating_sub(allowance)
            } else {
                Money::ZERO
            };
            let credits_applied = match terms.credits {
                CreditsMode::Apply => credits_pool(plan, terms.seats_billed).min(headless),
                CreditsMode::Ignore => Money::ZERO,
            };
            (terms.seat_fee, allowance, extra_usage, credits_applied)
        }
    };
    let headless_billed = headless.saturating_sub(credits_applied);
    PlanMonth {
        month: month.month,
        seats_billed: terms.seats_billed,
        seat_fee,
        allowance,
        interactive_api_equivalent: interactive,
        extra_usage,
        headless_api_equivalent: headless,
        credits_applied,
        headless_billed,
        total: seat_fee + extra_usage + headless_billed,
    }
}

fn credits_pool(plan: &Plan, seats_billed: u32) -> Money {
    let pool = plan.api_credits_per_seat.times(u64::from(seats_billed));
    match plan.api_credits_pool_cap {
        Some(cap) => pool.min(cap),
        None => pool,
    }
}

fn average_month(plan_months: &[PlanMonth]) -> Money {
    let month_count = plan_months.len() as u64;
    if month_count == 0 {
        return Money::ZERO;
    }
    let total: Money = plan_months.iter().map(|month| month.total).sum();
    Money::from_nanos(total.nanos() / month_count)
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    fn usd(amount: f64) -> Money {
        Money::from_usd(amount)
    }

    fn input_cost(amount: f64) -> Cost {
        Cost {
            input: usd(amount),
            without_cache: usd(amount),
            ..Cost::default()
        }
    }

    fn month(year: i16, month: i8, interactive: f64, headless: f64) -> MonthUsage {
        MonthUsage {
            month: YearMonth { year, month },
            interactive: input_cost(interactive),
            headless: input_cost(headless),
            requests: 10,
            unpriced_requests: 0,
        }
    }

    fn base_plan(id: &str, kind: PlanKind) -> Plan {
        Plan {
            id: id.to_string(),
            name: id.to_string(),
            kind,
            seat_monthly: None,
            seat_annual: None,
            min_seats: 1,
            max_seats: None,
            allowance_multiple: 0.0,
            api_credits_per_seat: Money::ZERO,
            api_credits_pool_cap: None,
            extra_usage_at_api_rates: true,
        }
    }

    fn catalog() -> PlanCatalog {
        let api = base_plan("api", PlanKind::PayAsYouGo);
        let pro = Plan {
            seat_monthly: Some(usd(20.0)),
            seat_annual: Some(usd(17.0)),
            max_seats: Some(1),
            allowance_multiple: 1.0,
            ..base_plan("pro", PlanKind::Subscription)
        };
        let max5 = Plan {
            seat_monthly: Some(usd(100.0)),
            max_seats: Some(1),
            allowance_multiple: 5.0,
            api_credits_per_seat: usd(100.0),
            ..base_plan("max5", PlanKind::Subscription)
        };
        let team_standard = Plan {
            seat_monthly: Some(usd(25.0)),
            seat_annual: Some(usd(20.0)),
            min_seats: 2,
            max_seats: Some(150),
            allowance_multiple: 1.0,
            api_credits_per_seat: usd(20.0),
            api_credits_pool_cap: Some(usd(500.0)),
            ..base_plan("team_standard", PlanKind::Subscription)
        };
        let enterprise = Plan {
            seat_annual: Some(usd(20.0)),
            min_seats: 20,
            ..base_plan("enterprise", PlanKind::SeatPlusUsage)
        };
        PlanCatalog {
            allowance_anchor: usd(60.0),
            plans: vec![api, pro, max5, team_standard, enterprise],
        }
    }

    fn options(seats: u32, billing: Billing, credits: CreditsMode) -> EstimateOptions {
        EstimateOptions {
            seats,
            billing,
            credits,
            allowance_anchor: None,
        }
    }

    fn one_month() -> Vec<MonthUsage> {
        vec![month(2026, 9, 100.0, 30.0)]
    }

    fn find<'a>(estimates: &'a [PlanEstimate], id: &str) -> &'a PlanEstimate {
        estimates
            .iter()
            .find(|estimate| estimate.plan_id == id)
            .expect("plan present in test catalog")
    }

    fn run(seats: u32, billing: Billing, credits: CreditsMode) -> Vec<PlanEstimate> {
        let estimates = estimate(&catalog(), &options(seats, billing, credits), &one_month())
            .expect("estimate succeeds");
        assert_eq!(estimates.len(), 5);
        estimates
    }

    #[test]
    fn pay_as_you_go_bills_everything_at_api_rates() {
        let estimates = run(1, Billing::Monthly, CreditsMode::Apply);
        let api = find(&estimates, "api");
        assert_eq!(api.months.len(), 1);
        let only = &api.months[0];
        assert_eq!(only.seat_fee, Money::ZERO);
        assert_eq!(only.allowance, Money::ZERO);
        assert_eq!(only.extra_usage, usd(100.0));
        assert_eq!(only.credits_applied, Money::ZERO);
        assert_eq!(only.headless_billed, usd(30.0));
        assert_eq!(only.total, usd(130.0));
        assert_eq!(api.total, usd(130.0));
        assert_eq!(api.sensitivity, None);
        assert!(api.notes.is_empty());
    }

    #[test]
    fn pro_monthly_charges_extra_usage_above_the_allowance() {
        let estimates = run(1, Billing::Monthly, CreditsMode::Apply);
        let pro = find(&estimates, "pro");
        let only = &pro.months[0];
        assert_eq!(only.seats_billed, 1);
        assert_eq!(only.seat_fee, usd(20.0));
        assert_eq!(only.allowance, usd(60.0));
        assert_eq!(only.interactive_api_equivalent, usd(100.0));
        assert_eq!(only.extra_usage, usd(40.0));
        assert_eq!(only.headless_api_equivalent, usd(30.0));
        assert_eq!(only.credits_applied, Money::ZERO);
        assert_eq!(only.headless_billed, usd(30.0));
        assert_eq!(only.total, usd(90.0));
        assert_eq!(pro.average_month, usd(90.0));
        assert_eq!(
            pro.sensitivity,
            Some(Sensitivity {
                half_anchor_average_month: usd(120.0),
                double_anchor_average_month: usd(50.0)
            })
        );
        assert!(pro.notes.is_empty());
    }

    #[test]
    fn pro_annual_uses_the_annual_seat_price() {
        let estimates = run(1, Billing::Annual, CreditsMode::Apply);
        let pro = find(&estimates, "pro");
        assert_eq!(pro.months[0].seat_fee, usd(17.0));
        assert_eq!(pro.total, usd(87.0));
        assert!(pro.notes.is_empty());
    }

    #[test]
    fn max5_credits_cover_headless_usage() {
        let estimates = run(1, Billing::Monthly, CreditsMode::Apply);
        let max5 = find(&estimates, "max5");
        let only = &max5.months[0];
        assert_eq!(only.seat_fee, usd(100.0));
        assert_eq!(only.allowance, usd(300.0));
        assert_eq!(only.extra_usage, Money::ZERO);
        assert_eq!(only.credits_applied, usd(30.0));
        assert_eq!(only.headless_billed, Money::ZERO);
        assert_eq!(only.total, usd(100.0));
    }

    #[test]
    fn max5_ignoring_credits_bills_headless_in_full() {
        let estimates = run(1, Billing::Monthly, CreditsMode::Ignore);
        let max5 = find(&estimates, "max5");
        assert_eq!(max5.months[0].credits_applied, Money::ZERO);
        assert_eq!(max5.months[0].headless_billed, usd(30.0));
        assert_eq!(max5.total, usd(130.0));
    }

    #[test]
    fn max5_annual_falls_back_to_monthly_price_with_note() {
        let estimates = run(1, Billing::Annual, CreditsMode::Apply);
        let max5 = find(&estimates, "max5");
        assert_eq!(max5.months[0].seat_fee, usd(100.0));
        assert_eq!(
            max5.notes,
            vec!["annual billing not offered; monthly seat price used".to_string()]
        );
    }

    #[test]
    fn team_standard_raises_seats_to_the_minimum() {
        let estimates = run(1, Billing::Monthly, CreditsMode::Apply);
        let team = find(&estimates, "team_standard");
        let only = &team.months[0];
        assert_eq!(only.seats_billed, 2);
        assert_eq!(only.seat_fee, usd(50.0));
        assert_eq!(only.allowance, usd(60.0));
        assert_eq!(only.extra_usage, usd(40.0));
        assert_eq!(only.credits_applied, usd(30.0));
        assert_eq!(only.headless_billed, Money::ZERO);
        assert_eq!(only.total, usd(90.0));
        assert_eq!(
            team.notes,
            vec!["seat minimum: 2 seats billed (requested 1)".to_string()]
        );
    }

    #[test]
    fn team_standard_caps_seats_and_the_credit_pool() {
        let months = vec![month(2026, 9, 100.0, 800.0)];
        let estimates = estimate(
            &catalog(),
            &options(200, Billing::Monthly, CreditsMode::Apply),
            &months,
        )
        .expect("estimate succeeds");
        let team = find(&estimates, "team_standard");
        let only = &team.months[0];
        assert_eq!(only.seats_billed, 150);
        assert_eq!(only.seat_fee, usd(3750.0));
        assert_eq!(only.credits_applied, usd(500.0));
        assert_eq!(only.headless_billed, usd(300.0));
        assert_eq!(only.total, usd(3750.0 + 40.0 + 300.0));
        assert_eq!(
            team.notes,
            vec!["seat maximum: 150 seats billed (requested 200)".to_string()]
        );
    }

    #[test]
    fn enterprise_bills_seat_minimum_plus_all_usage() {
        let estimates = run(1, Billing::Monthly, CreditsMode::Apply);
        let enterprise = find(&estimates, "enterprise");
        let only = &enterprise.months[0];
        assert_eq!(only.seats_billed, 20);
        assert_eq!(only.seat_fee, usd(400.0));
        assert_eq!(only.allowance, Money::ZERO);
        assert_eq!(only.credits_applied, Money::ZERO);
        assert_eq!(only.total, usd(530.0));
        assert_eq!(enterprise.sensitivity, None);
        assert_eq!(
            enterprise.notes,
            vec![
                "seat minimum: 20 seats billed (requested 1)".to_string(),
                "monthly billing not offered; annual seat price used".to_string(),
            ]
        );
    }

    #[test]
    fn notes_are_pushed_once_per_plan_not_per_month() {
        let months = vec![
            month(2026, 8, 100.0, 30.0),
            month(2026, 9, 100.0, 30.0),
            month(2026, 10, 100.0, 30.0),
        ];
        let estimates = estimate(
            &catalog(),
            &options(1, Billing::Monthly, CreditsMode::Apply),
            &months,
        )
        .expect("estimate succeeds");
        let enterprise = find(&estimates, "enterprise");
        assert_eq!(enterprise.months.len(), 3);
        assert_eq!(enterprise.notes.len(), 2);
    }

    #[test]
    fn average_and_peak_cover_every_month() {
        let months = vec![month(2026, 8, 100.0, 30.0), month(2026, 9, 200.0, 0.0)];
        let estimates = estimate(
            &catalog(),
            &options(1, Billing::Monthly, CreditsMode::Apply),
            &months,
        )
        .expect("estimate succeeds");
        let pro = find(&estimates, "pro");
        assert_eq!(pro.months.len(), 2);
        assert_eq!(pro.months[0].total, usd(90.0));
        assert_eq!(pro.months[1].total, usd(160.0));
        assert_eq!(pro.total, usd(250.0));
        assert_eq!(pro.average_month, usd(125.0));
        assert_eq!(pro.peak_month, usd(160.0));
        let api = find(&estimates, "api");
        assert_eq!(api.total, usd(330.0));
        assert_eq!(api.average_month, usd(165.0));
        assert_eq!(api.peak_month, usd(200.0));
    }

    #[test]
    fn anchor_override_replaces_the_catalog_anchor() {
        let overridden = EstimateOptions {
            allowance_anchor: Some(usd(120.0)),
            ..options(1, Billing::Monthly, CreditsMode::Apply)
        };
        let estimates = estimate(&catalog(), &overridden, &one_month()).expect("estimate succeeds");
        let pro = find(&estimates, "pro");
        assert_eq!(pro.months[0].allowance, usd(120.0));
        assert_eq!(pro.total, usd(50.0));
        assert_eq!(
            pro.sensitivity,
            Some(Sensitivity {
                half_anchor_average_month: usd(90.0),
                double_anchor_average_month: usd(50.0)
            })
        );
    }

    #[test]
    fn subscription_without_extra_usage_is_throttled_with_one_note() {
        let throttled = Plan {
            seat_monthly: Some(usd(20.0)),
            allowance_multiple: 1.0,
            extra_usage_at_api_rates: false,
            ..base_plan("throttled", PlanKind::Subscription)
        };
        let single = PlanCatalog {
            allowance_anchor: usd(60.0),
            plans: vec![throttled],
        };
        let months = vec![month(2026, 8, 100.0, 30.0), month(2026, 9, 100.0, 30.0)];
        let estimates = estimate(
            &single,
            &options(1, Billing::Monthly, CreditsMode::Apply),
            &months,
        )
        .expect("estimate succeeds");
        assert_eq!(estimates.len(), 1);
        assert_eq!(estimates[0].months[0].extra_usage, Money::ZERO);
        assert_eq!(estimates[0].months[0].total, usd(50.0));
        assert_eq!(estimates[0].notes, vec![THROTTLED_NOTE.to_string()]);
    }

    #[test]
    fn zero_multiple_subscription_has_zero_allowance() {
        let bare = Plan {
            seat_monthly: Some(usd(10.0)),
            ..base_plan("bare", PlanKind::Subscription)
        };
        let single = PlanCatalog {
            allowance_anchor: usd(60.0),
            plans: vec![bare],
        };
        let estimates = estimate(
            &single,
            &options(1, Billing::Monthly, CreditsMode::Apply),
            &one_month(),
        )
        .expect("estimate succeeds");
        assert_eq!(estimates[0].kind, PlanKind::Subscription);
        assert_eq!(estimates[0].months[0].allowance, Money::ZERO);
        assert_eq!(estimates[0].months[0].extra_usage, usd(100.0));
        assert_eq!(estimates[0].total, usd(140.0));
        assert!(estimates[0].sensitivity.is_some());
    }

    #[test]
    fn plan_without_any_seat_price_is_rejected() {
        let unpriced = base_plan("unpriced", PlanKind::Subscription);
        let single = PlanCatalog {
            allowance_anchor: usd(60.0),
            plans: vec![unpriced],
        };
        let result = estimate(
            &single,
            &options(1, Billing::Monthly, CreditsMode::Apply),
            &one_month(),
        );
        assert_eq!(
            result,
            Err(PlanError::NoBilling {
                plan: "unpriced".to_string()
            })
        );
    }

    #[test]
    fn empty_months_are_rejected() {
        let result = estimate(
            &catalog(),
            &options(1, Billing::Monthly, CreditsMode::Apply),
            &[],
        );
        assert_eq!(result, Err(PlanError::NoMonths));
    }

    #[test]
    fn empty_catalog_is_rejected() {
        let empty = PlanCatalog {
            allowance_anchor: usd(60.0),
            plans: Vec::new(),
        };
        let result = estimate(
            &empty,
            &options(1, Billing::Monthly, CreditsMode::Apply),
            &one_month(),
        );
        assert_eq!(result, Err(PlanError::EmptyCatalog));
    }
}
