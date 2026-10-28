use std::collections::BTreeMap;

use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};
use serde::Serialize;
use thiserror::Error;

use crate::domain::patterns::{self, Patterns, PricedEvent};
use crate::domain::plans::{
    self, EstimateOptions, MonthUsage, PlanCatalog, PlanError, PlanEstimate, YearMonth,
};
use crate::domain::pricing::{Cost, PriceList, Priced};
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

/// The `--since` / `--until` bounds the scan applied. `until` is exclusive.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct RequestedRange {
    pub since: Option<Timestamp>,
    pub until: Option<Timestamp>,
}

/// One hundred years of months; a longer range is a typo in `--since` or `--until`.
const MAX_MONTHS: i32 = 1_200;

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
    #[error(
        "no priced requests in range ({unpriced} unpriced, {filtered_out} filtered out, \
         {lines_unparseable} unparseable lines, \
         {assistant_lines_without_usage} assistant lines without usage)"
    )]
    NothingPriced {
        unpriced: u64,
        filtered_out: u64,
        lines_unparseable: u64,
        assistant_lines_without_usage: u64,
    },
    #[error("the range spans {months} months; at most {MAX_MONTHS} are supported")]
    RangeTooLong { months: i32 },
    #[error("usage totals do not fit in u64 nanodollars")]
    Overflow,
    #[error(transparent)]
    Plan(#[from] PlanError),
}

/// Prices every event, buckets the unpriced ones, groups priced usage by UTC month
/// split on `entrypoint.credits_eligible()`, then runs the plan estimate and the
/// pattern analysis. Fails rather than reporting a clean zero when nothing was scanned
/// or nothing could be priced.
///
/// The report range runs from `range.since` (or the first priced request's UTC day)
/// to the day before `range.until` (or the last priced request's UTC day). Every
/// calendar month it touches gets a row, idle months included, except a month holding
/// only unpriced requests.
pub fn build(
    source: String,
    scan: Scan,
    prices: &PriceList,
    catalog: &PlanCatalog,
    options: EstimateOptions,
    range: RequestedRange,
) -> Result<Report, ReportError> {
    if scan.stats.files_seen == 0 {
        return Err(ReportError::NoFiles);
    }

    let mut unpriced = Unpriced::default();
    let mut unpriced_by_month: BTreeMap<YearMonth, u64> = BTreeMap::new();
    let mut priced: Vec<PricedEvent> = Vec::with_capacity(scan.events.len());
    for event in scan.events {
        match prices.price(&event.model, &event.tokens) {
            Priced::Priced { model, cost } => {
                priced.push(PricedEvent { event, model, cost });
                continue;
            }
            Priced::Synthetic => unpriced.synthetic += 1,
            Priced::MissingModel => unpriced.missing_model += 1,
            Priced::UnknownModel(id) => *unpriced.unknown_models.entry(id).or_insert(0) += 1,
        }
        *unpriced_by_month
            .entry(YearMonth::of(event.timestamp))
            .or_insert(0) += 1;
    }

    if priced.is_empty() {
        return Err(ReportError::NothingPriced {
            unpriced: unpriced.total(),
            filtered_out: scan.stats.events_filtered_out,
            lines_unparseable: scan.stats.lines_unparseable,
            assistant_lines_without_usage: scan.stats.assistant_lines_without_usage,
        });
    }
    // Every later sum (months, plans, patterns) is a partial sum of this one.
    priced
        .iter()
        .try_fold(Cost::default(), |sum, priced_event| {
            sum.checked_add(&priced_event.cost)
        })
        .ok_or(ReportError::Overflow)?;

    let first_event = priced
        .iter()
        .map(|priced_event| priced_event.event.timestamp)
        .min();
    let last_event = priced
        .iter()
        .map(|priced_event| priced_event.event.timestamp)
        .max();
    let (first_day, last_day) = match (first_event, last_event) {
        (Some(first), Some(last)) => report_days(first, last, range),
        _ => unreachable!("priced is not empty"),
    };
    let months = group_by_month(&priced, &unpriced_by_month, first_day, last_day)?;
    let plans = plans::estimate(catalog, &options, &months)?;
    let patterns = patterns::analyse(&priced, scan.granularity);

    Ok(Report {
        source,
        pricing_as_of: prices.as_of.clone(),
        granularity: scan.granularity,
        first_event,
        last_event,
        scan: scan.stats,
        unpriced,
        options,
        allowance_anchor: options.allowance_anchor.unwrap_or(catalog.allowance_anchor),
        months,
        plans,
        patterns,
    })
}

/// First and last UTC day of the report range, both inclusive. The scan filter already
/// keeps every event inside `[since, until)`, so the bounds only ever widen the range.
fn report_days(
    first_event: Timestamp,
    last_event: Timestamp,
    range: RequestedRange,
) -> (Date, Date) {
    let start = range
        .since
        .map_or(first_event, |since| since.min(first_event));
    let end = range.until.map_or(last_event, |until| {
        let last_included = until
            .checked_sub(SignedDuration::from_nanos(1))
            .unwrap_or(until);
        last_included.max(last_event)
    });
    (utc_day(start), utc_day(end))
}

fn utc_day(timestamp: Timestamp) -> Date {
    timestamp.to_zoned(TimeZone::UTC).date()
}

/// One row per calendar month from `first_day` to `last_day`, each with the days of it
/// the range covers. A month holding nothing but unpriced requests gets no row: it would
/// otherwise charge a seat fee for usage nobody can price. A month with no requests at
/// all keeps its zero row, so seat plans pay for it and averages count it.
fn group_by_month(
    priced: &[PricedEvent],
    unpriced_by_month: &BTreeMap<YearMonth, u64>,
    first_day: Date,
    last_day: Date,
) -> Result<Vec<MonthUsage>, ReportError> {
    let month_count = (i32::from(last_day.year()) - i32::from(first_day.year())) * 12
        + i32::from(last_day.month())
        - i32::from(first_day.month())
        + 1;
    if month_count > MAX_MONTHS {
        return Err(ReportError::RangeTooLong {
            months: month_count,
        });
    }

    let mut by_month: BTreeMap<YearMonth, MonthUsage> = BTreeMap::new();
    let mut month_start = first_day.first_of_month();
    for _ in 0..month_count {
        let month = YearMonth::of_date(month_start);
        let month_end = month_start.last_of_month();
        by_month.insert(
            month,
            MonthUsage {
                month,
                covered_from: month_start.max(first_day),
                covered_through: month_end.min(last_day),
                interactive: Cost::default(),
                headless: Cost::default(),
                requests: 0,
                unpriced_requests: unpriced_by_month.get(&month).copied().unwrap_or(0),
            },
        );
        match month_end.tomorrow() {
            Ok(next) => month_start = next,
            Err(_) => break,
        }
    }

    for priced_event in priced {
        let usage = by_month
            .get_mut(&YearMonth::of(priced_event.event.timestamp))
            .expect("every priced request falls inside the report range");
        if priced_event.event.entrypoint.credits_eligible() {
            usage.headless = usage.headless.add(&priced_event.cost);
        } else {
            usage.interactive = usage.interactive.add(&priced_event.cost);
        }
        usage.requests += 1;
    }
    Ok(by_month
        .into_values()
        .filter(|usage| usage.requests > 0 || usage.unpriced_requests == 0)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::money::{Money, PerToken};
    use crate::domain::plans::{Billing, CreditsMode, Plan, PlanKind};
    use crate::domain::pricing::{ModelPrice, PriceTier};
    use crate::domain::usage::{Entrypoint, ModelName, TokenUsage, UsageEvent};

    fn price_list() -> PriceList {
        let tier = PriceTier {
            prompt_tokens_over: 0,
            input: PerToken::from_usd_per_mtok(4.0),
            output: PerToken::from_usd_per_mtok(20.0),
            cache_read: PerToken::from_usd_per_mtok(0.2),
            cache_write_5m: PerToken::from_usd_per_mtok(5.0),
            cache_write_1h: PerToken::from_usd_per_mtok(8.0),
        };
        PriceList {
            as_of: "2026-10-28".into(),
            models: vec![ModelPrice {
                id: "claude-opus-5-5".into(),
                display_name: "Claude Opus 5.5".into(),
                tiers: vec![tier],
            }],
        }
    }

    fn catalog() -> PlanCatalog {
        PlanCatalog {
            allowance_anchor: Money::from_usd(60.0),
            plans: vec![Plan {
                id: "api".into(),
                name: "API pay-as-you-go".into(),
                kind: PlanKind::PayAsYouGo,
                seat_monthly: None,
                seat_annual: None,
                min_seats: 1,
                max_seats: None,
                allowance_multiple: 0.0,
                api_credits_per_seat: Money::ZERO,
                api_credits_pool_cap: None,
                extra_usage_at_api_rates: true,
            }],
        }
    }

    fn catalog_with_pro() -> PlanCatalog {
        let mut with_pro = catalog();
        with_pro.plans.push(Plan {
            id: "pro".into(),
            name: "Pro".into(),
            kind: PlanKind::Subscription,
            seat_monthly: Some(Money::from_usd(20.0)),
            seat_annual: None,
            min_seats: 1,
            max_seats: Some(1),
            allowance_multiple: 1.0,
            api_credits_per_seat: Money::ZERO,
            api_credits_pool_cap: None,
            extra_usage_at_api_rates: true,
        });
        with_pro
    }

    fn at(text: &str) -> Timestamp {
        text.parse().expect("test timestamps are valid RFC 3339")
    }

    fn months_of(report: &Report) -> Vec<String> {
        report
            .months
            .iter()
            .map(|usage| usage.month.to_string())
            .collect()
    }

    fn pro(report: &Report) -> &PlanEstimate {
        report
            .plans
            .iter()
            .find(|plan| plan.plan_id == "pro")
            .expect("pro is in the catalog")
    }

    fn options() -> EstimateOptions {
        EstimateOptions {
            seats: 1,
            billing: Billing::Monthly,
            credits: CreditsMode::Apply,
            allowance_anchor: None,
        }
    }

    fn event(timestamp: &str, model: ModelName, entrypoint: Entrypoint) -> UsageEvent {
        UsageEvent {
            session_id: "session".into(),
            message_id: format!("message-{timestamp}"),
            timestamp: timestamp
                .parse()
                .expect("test timestamps are valid RFC 3339"),
            model,
            entrypoint,
            sidechain: false,
            project: "/Users/example/src/demo".into(),
            tokens: TokenUsage {
                input: 1_000,
                output: 100,
                ..TokenUsage::default()
            },
        }
    }

    fn opus() -> ModelName {
        ModelName::Named("claude-opus-5-5".into())
    }

    fn scan(files_seen: u64, events: Vec<UsageEvent>) -> Scan {
        let stats = ScanStats {
            files_seen,
            events: events.len() as u64,
            events_filtered_out: 4,
            lines_unparseable: 5,
            assistant_lines_without_usage: 6,
            ..ScanStats::default()
        };
        Scan {
            granularity: Granularity::Message,
            events,
            stats,
        }
    }

    #[test]
    fn report_fails_when_no_files_were_seen() {
        let result = build(
            "test".into(),
            scan(0, Vec::new()),
            &price_list(),
            &catalog(),
            options(),
            RequestedRange::default(),
        );
        assert_eq!(result, Err(ReportError::NoFiles));
    }

    #[test]
    fn report_fails_when_nothing_could_be_priced() {
        let events = vec![
            event(
                "2026-09-01T00:00:00Z",
                ModelName::Synthetic,
                Entrypoint::Interactive,
            ),
            event(
                "2026-09-01T01:00:00Z",
                ModelName::Missing,
                Entrypoint::Interactive,
            ),
            event(
                "2026-09-01T02:00:00Z",
                ModelName::Named("claude-unreleased".into()),
                Entrypoint::Sdk,
            ),
        ];
        let result = build(
            "test".into(),
            scan(1, events),
            &price_list(),
            &catalog(),
            options(),
            RequestedRange::default(),
        );
        assert_eq!(
            result,
            Err(ReportError::NothingPriced {
                unpriced: 3,
                filtered_out: 4,
                lines_unparseable: 5,
                assistant_lines_without_usage: 6,
            })
        );
    }

    #[test]
    fn report_buckets_unpriced_and_splits_months_by_entrypoint() {
        let events = vec![
            event("2026-10-02T09:00:00Z", opus(), Entrypoint::Sdk),
            event("2026-09-30T04:12:00Z", opus(), Entrypoint::Interactive),
            event(
                "2026-09-30T05:00:00Z",
                opus(),
                Entrypoint::Unrecognised("vscode-2030".into()),
            ),
            event(
                "2026-09-30T06:00:00Z",
                ModelName::Synthetic,
                Entrypoint::Interactive,
            ),
            event(
                "2026-10-03T06:00:00Z",
                ModelName::Named("claude-unreleased".into()),
                Entrypoint::Sdk,
            ),
            event(
                "2026-10-04T06:00:00Z",
                ModelName::Named("claude-unreleased".into()),
                Entrypoint::Sdk,
            ),
            event(
                "2026-11-01T00:00:00Z",
                ModelName::Missing,
                Entrypoint::Missing,
            ),
        ];
        let report = build(
            "test".into(),
            scan(2, events),
            &price_list(),
            &catalog(),
            options(),
            RequestedRange::default(),
        )
        .expect("two months are priceable");

        assert_eq!(report.unpriced.synthetic, 1);
        assert_eq!(report.unpriced.missing_model, 1);
        assert_eq!(
            report.unpriced.unknown_models.get("claude-unreleased"),
            Some(&2)
        );
        assert_eq!(report.unpriced.total(), 4);

        let one_request = Money::from_nanos(6_000_000);
        assert_eq!(report.months.len(), 2);
        let september = &report.months[0];
        assert_eq!(
            september.month,
            YearMonth {
                year: 2026,
                month: 9
            }
        );
        assert_eq!(september.requests, 2);
        assert_eq!(september.unpriced_requests, 1);
        assert_eq!(september.interactive.total(), one_request.times(2));
        assert_eq!(september.headless.total(), Money::ZERO);
        let october = &report.months[1];
        assert_eq!(
            october.month,
            YearMonth {
                year: 2026,
                month: 10
            }
        );
        assert_eq!(october.requests, 1);
        assert_eq!(october.unpriced_requests, 2);
        assert_eq!(october.interactive.total(), Money::ZERO);
        assert_eq!(october.headless.total(), one_request);

        assert_eq!(
            report.first_event,
            Some("2026-09-30T04:12:00Z".parse().expect("valid timestamp"))
        );
        assert_eq!(
            report.last_event,
            Some("2026-10-02T09:00:00Z".parse().expect("valid timestamp"))
        );
        assert_eq!(report.allowance_anchor, Money::from_usd(60.0));
        assert_eq!(report.pricing_as_of, "2026-10-28");
        assert_eq!(report.scan.files_seen, 2);
    }

    #[test]
    fn gap_month_gets_a_zero_row_and_a_seat_fee() {
        let events = vec![
            event("2026-09-10T10:00:00Z", opus(), Entrypoint::Interactive),
            event("2026-11-10T10:00:00Z", opus(), Entrypoint::Interactive),
        ];
        let report = build(
            "test".into(),
            scan(1, events),
            &price_list(),
            &catalog_with_pro(),
            options(),
            RequestedRange::default(),
        )
        .expect("two priced events");
        assert_eq!(months_of(&report), ["2026-09", "2026-10", "2026-11"]);
        let october = &report.months[1];
        assert_eq!(october.requests, 0);
        assert_eq!(october.unpriced_requests, 0);
        assert_eq!(october.interactive.total(), Money::ZERO);
        assert_eq!(october.headless.total(), Money::ZERO);
        assert!(october.is_complete());
        let pro = pro(&report);
        assert_eq!(pro.months.len(), 3);
        assert_eq!(pro.months[1].seat_fee, Money::from_usd(20.0));
        assert_eq!(pro.total, Money::from_usd(60.0));
        assert_eq!(pro.average_month, Money::from_usd(20.0));
    }

    #[test]
    fn requested_range_bills_every_month_it_covers() {
        let events = vec![
            event("2026-08-10T10:00:00Z", opus(), Entrypoint::Interactive),
            event("2026-10-31T23:00:00Z", opus(), Entrypoint::Interactive),
        ];
        let range = RequestedRange {
            since: Some(at("2026-08-01T00:00:00Z")),
            until: Some(at("2026-11-01T00:00:00Z")),
        };
        let report = build(
            "test".into(),
            scan(1, events),
            &price_list(),
            &catalog_with_pro(),
            options(),
            range,
        )
        .expect("two priced events");
        assert_eq!(months_of(&report), ["2026-08", "2026-09", "2026-10"]);
        assert!(report.months.iter().all(MonthUsage::is_complete));
        let pro = pro(&report);
        assert_eq!(pro.total, Money::from_usd(20.0).times(3));
        assert!(pro.notes.is_empty(), "{:?}", pro.notes);
    }

    #[test]
    fn range_ends_are_partial_without_bounds() {
        let events = vec![
            event("2026-08-10T10:00:00Z", opus(), Entrypoint::Interactive),
            event("2026-09-15T10:00:00Z", opus(), Entrypoint::Interactive),
            event("2026-10-20T23:59:59Z", opus(), Entrypoint::Interactive),
        ];
        let report = build(
            "test".into(),
            scan(1, events),
            &price_list(),
            &catalog_with_pro(),
            options(),
            RequestedRange::default(),
        )
        .expect("three priced events");
        let spans: Vec<(String, i8, bool)> = report
            .months
            .iter()
            .map(|usage| {
                (
                    usage.month.to_string(),
                    usage.days_covered(),
                    usage.is_complete(),
                )
            })
            .collect();
        assert_eq!(
            spans,
            [
                ("2026-08".to_string(), 22, false),
                ("2026-09".to_string(), 30, true),
                ("2026-10".to_string(), 20, false),
            ]
        );
        assert_eq!(
            pro(&report).notes,
            vec![
                "partial months excluded from avg/peak: 2026-08 (22 days), 2026-10 (20 days)"
                    .to_string()
            ]
        );
    }

    #[test]
    fn unpriced_only_month_between_priced_months_adds_no_seat_fee() {
        let events = vec![
            event("2026-08-10T10:00:00Z", opus(), Entrypoint::Interactive),
            event(
                "2026-09-10T10:00:00Z",
                ModelName::Synthetic,
                Entrypoint::Interactive,
            ),
            event("2026-10-10T10:00:00Z", opus(), Entrypoint::Interactive),
        ];
        let report = build(
            "test".into(),
            scan(1, events),
            &price_list(),
            &catalog_with_pro(),
            options(),
            RequestedRange::default(),
        )
        .expect("two priced events");
        assert_eq!(months_of(&report), ["2026-08", "2026-10"]);
        assert_eq!(report.unpriced.synthetic, 1);
        assert_eq!(pro(&report).total, Money::from_usd(40.0));
    }

    #[test]
    fn range_longer_than_the_month_cap_is_rejected() {
        let events = vec![event(
            "2026-08-10T10:00:00Z",
            opus(),
            Entrypoint::Interactive,
        )];
        let range = RequestedRange {
            since: Some(at("1900-01-01T00:00:00Z")),
            until: None,
        };
        let result = build(
            "test".into(),
            scan(1, events),
            &price_list(),
            &catalog(),
            options(),
            range,
        );
        assert_eq!(result, Err(ReportError::RangeTooLong { months: 1520 }));
    }

    #[test]
    fn usage_totals_beyond_u64_nanodollars_are_an_error() {
        let mut prices = price_list();
        prices.models[0].tiers[0].input = PerToken::from_usd_per_mtok(1_000_000.0);
        let mut events = vec![
            event("2026-08-10T10:00:00Z", opus(), Entrypoint::Interactive),
            event("2026-08-11T10:00:00Z", opus(), Entrypoint::Interactive),
        ];
        for heavy in &mut events {
            heavy.tokens.input = 10_000_000_000;
        }
        let result = build(
            "test".into(),
            scan(1, events),
            &prices,
            &catalog(),
            options(),
            RequestedRange::default(),
        );
        assert_eq!(result, Err(ReportError::Overflow));
    }

    #[test]
    fn report_anchor_override_wins_over_catalog() {
        let events = vec![event(
            "2026-09-30T04:12:00Z",
            opus(),
            Entrypoint::Interactive,
        )];
        let overridden = EstimateOptions {
            allowance_anchor: Some(Money::from_usd(90.0)),
            ..options()
        };
        let report = build(
            "test".into(),
            scan(1, events),
            &price_list(),
            &catalog(),
            overridden,
            RequestedRange::default(),
        )
        .expect("one priced event");
        assert_eq!(report.allowance_anchor, Money::from_usd(90.0));
    }
}

/// Hand-built reports for the renderer tests, independent of the pricing, plan and pattern arithmetic.
#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;
    use crate::domain::money::Money;
    use crate::domain::patterns::{
        CacheStats, EntrypointTotals, ModelTotals, ProjectTotals, WindowPeak,
    };
    use crate::domain::plans::{Billing, CreditsMode, PlanKind, PlanMonth, Sensitivity};
    use crate::domain::usage::TokenUsage;
    use jiff::civil::date;

    fn timestamp(text: &str) -> Timestamp {
        text.parse().expect("fixture timestamps are valid RFC 3339")
    }

    fn cost(total_usd: f64) -> Cost {
        Cost {
            input: Money::from_usd(total_usd),
            without_cache: Money::from_usd(total_usd * 3.0),
            ..Cost::default()
        }
    }

    fn plan_month(month: YearMonth, total_usd: f64) -> PlanMonth {
        PlanMonth {
            month,
            seats_billed: 1,
            seat_fee: Money::ZERO,
            allowance: Money::ZERO,
            interactive_api_equivalent: Money::from_usd(total_usd),
            extra_usage: Money::ZERO,
            headless_api_equivalent: Money::ZERO,
            credits_applied: Money::ZERO,
            headless_billed: Money::ZERO,
            total: Money::from_usd(total_usd),
        }
    }

    pub(crate) fn sample_report(granularity: Granularity) -> Report {
        let september = YearMonth {
            year: 2026,
            month: 9,
        };
        let october = YearMonth {
            year: 2026,
            month: 10,
        };
        let window = |start: &str, end: &str, usd: f64, requests: u64| WindowPeak {
            start: timestamp(start),
            end: timestamp(end),
            cost: Money::from_usd(usd),
            requests,
        };
        let (window_5h, window_7d) = match granularity {
            Granularity::Message => (
                Some(window(
                    "2026-09-30T04:12:37Z",
                    "2026-09-30T09:12:37Z",
                    12.5,
                    14,
                )),
                Some(window(
                    "2026-09-28T00:00:00Z",
                    "2026-10-05T00:00:00Z",
                    1234.56,
                    3210,
                )),
            ),
            Granularity::Day => (None, None),
        };
        let mut other_line_types = BTreeMap::new();
        other_line_types.insert("attachment".to_string(), 4);
        other_line_types.insert("summary".to_string(), 1);
        let mut unknown_models = BTreeMap::new();
        unknown_models.insert(
            "claude-unreleased-model-with-a-very-long-identifier-that-keeps-going".to_string(),
            3,
        );

        Report {
            source: "claude-code (/Users/example/.claude/projects)".into(),
            pricing_as_of: "2026-10-28".into(),
            granularity,
            first_event: Some(timestamp("2026-09-30T04:12:37Z")),
            last_event: Some(timestamp("2026-10-02T09:01:00Z")),
            scan: ScanStats {
                files_seen: 3,
                files_unreadable: 1,
                lines_read: 16,
                lines_unparseable: 1,
                assistant_lines: 11,
                assistant_lines_without_usage: 1,
                events_before_dedup: 10,
                duplicates_collapsed: 2,
                conflicting_duplicates: 1,
                cache_split_mismatches: 3,
                events_filtered_out: 1,
                events: 7,
                other_line_types,
            },
            unpriced: Unpriced { synthetic: 1, missing_model: 0, unknown_models },
            options: EstimateOptions { seats: 1, billing: Billing::Monthly, credits: CreditsMode::Apply, allowance_anchor: None },
            allowance_anchor: Money::from_usd(60.0),
            months: vec![
                MonthUsage { month: september, covered_from: date(2026, 9, 30), covered_through: date(2026, 9, 30), interactive: cost(10.0), headless: Cost::default(), requests: 2, unpriced_requests: 1 },
                MonthUsage { month: october, covered_from: date(2026, 10, 1), covered_through: date(2026, 10, 2), interactive: cost(2.5), headless: cost(1.25), requests: 1_234_567, unpriced_requests: 3 },
            ],
            plans: vec![
                PlanEstimate {
                    plan_id: "api".into(),
                    plan_name: "API pay-as-you-go".into(),
                    kind: PlanKind::PayAsYouGo,
                    seats_billed: 1,
                    months: vec![plan_month(september, 10.0), plan_month(october, 3.75)],
                    average_month: Money::from_usd(6.875),
                    peak_month: Money::from_usd(10.0),
                    total: Money::from_usd(13.75),
                    sensitivity: None,
                    notes: Vec::new(),
                },
                PlanEstimate {
                    plan_id: "pro".into(),
                    plan_name: "Pro".into(),
                    kind: PlanKind::Subscription,
                    seats_billed: 1,
                    months: vec![plan_month(september, 20.0), plan_month(october, 20.0)],
                    average_month: Money::from_usd(20.0),
                    peak_month: Money::from_usd(20.0),
                    total: Money::from_usd(40.0),
                    sensitivity: Some(Sensitivity {
                        half_anchor_average_month: Money::from_usd(20.0),
                        double_anchor_average_month: Money::from_usd(20.0),
                    }),
                    notes: vec![
                        "seats raised to the plan minimum of 1 because the plan does not offer fewer seats than that, which is a long note that has to wrap across more than one line of output".into(),
                    ],
                },
            ],
            patterns: Patterns {
                window_5h,
                window_7d,
                active_days: 2,
                sessions: 3,
                requests: 1_234_569,
                sidechain_share: 0.125,
                per_model: vec![ModelTotals {
                    model: "claude-opus-5-5".into(),
                    requests: 1_234_569,
                    tokens: TokenUsage { input: 1_234_567_890, output: 98_765, cache_read: 9_876_543_210, cache_write_5m: 1_000, cache_write_1h: 36_476 },
                    cost: cost(13.75),
                }],
                per_entrypoint: vec![
                    EntrypointTotals { entrypoint: "interactive".into(), requests: 2, cost: Money::from_usd(12.5) },
                    EntrypointTotals { entrypoint: "sdk".into(), requests: 1, cost: Money::from_usd(1.25) },
                ],
                per_project: vec![ProjectTotals {
                    project: "/Users/example/src/a-project-with-an-extremely-long-directory-name/that/nests/deeply/into/the/tree/demo".into(),
                    requests: 3,
                    cost: Money::from_usd(13.75),
                }],
                cache: CacheStats {
                    input_tokens: 1_234_567_890,
                    cache_read_tokens: 9_876_543_210,
                    cache_write_5m_tokens: 1_000,
                    cache_write_1h_tokens: 36_476,
                    cache_read_share: 0.8889,
                    spend: Money::from_usd(13.75),
                    spend_without_cache: Money::from_usd(41.25),
                },
            },
        }
    }
}
