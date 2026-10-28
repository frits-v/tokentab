use std::collections::BTreeMap;

use jiff::Timestamp;
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
pub fn build(
    source: String,
    scan: Scan,
    prices: &PriceList,
    catalog: &PlanCatalog,
    options: EstimateOptions,
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
        });
    }

    let first_event = priced
        .iter()
        .map(|priced_event| priced_event.event.timestamp)
        .min();
    let last_event = priced
        .iter()
        .map(|priced_event| priced_event.event.timestamp)
        .max();
    let months = group_by_month(&priced, &unpriced_by_month);
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

/// Months come from priced events only: a month holding nothing but unpriced
/// requests would otherwise charge a seat fee for usage nobody can price.
fn group_by_month(
    priced: &[PricedEvent],
    unpriced_by_month: &BTreeMap<YearMonth, u64>,
) -> Vec<MonthUsage> {
    let mut by_month: BTreeMap<YearMonth, MonthUsage> = BTreeMap::new();
    for priced_event in priced {
        let month = YearMonth::of(priced_event.event.timestamp);
        let usage = by_month.entry(month).or_insert_with(|| MonthUsage {
            month,
            interactive: Cost::default(),
            headless: Cost::default(),
            requests: 0,
            unpriced_requests: unpriced_by_month.get(&month).copied().unwrap_or(0),
        });
        if priced_event.event.entrypoint.credits_eligible() {
            usage.headless = usage.headless.add(&priced_event.cost);
        } else {
            usage.interactive = usage.interactive.add(&priced_event.cost);
        }
        usage.requests += 1;
    }
    by_month.into_values().collect()
}

#[cfg(test)]
#[allow(clippy::expect_used)]
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
        );
        assert_eq!(
            result,
            Err(ReportError::NothingPriced {
                unpriced: 3,
                filtered_out: 4
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
        )
        .expect("one priced event");
        assert_eq!(report.allowance_anchor, Money::from_usd(90.0));
    }
}

/// Hand-built reports for the renderer tests, independent of the pricing, plan and pattern arithmetic.
#[cfg(test)]
#[allow(clippy::expect_used)]
pub(crate) mod fixtures {
    use super::*;
    use crate::domain::money::Money;
    use crate::domain::patterns::{
        CacheStats, EntrypointTotals, ModelTotals, ProjectTotals, WindowPeak,
    };
    use crate::domain::plans::{Billing, CreditsMode, PlanKind, PlanMonth, Sensitivity};
    use crate::domain::usage::TokenUsage;

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
                events_filtered_out: 1,
                events: 7,
                other_line_types,
            },
            unpriced: Unpriced { synthetic: 1, missing_model: 0, unknown_models },
            options: EstimateOptions { seats: 1, billing: Billing::Monthly, credits: CreditsMode::Apply, allowance_anchor: None },
            allowance_anchor: Money::from_usd(60.0),
            months: vec![
                MonthUsage { month: september, interactive: cost(10.0), headless: Cost::default(), requests: 2, unpriced_requests: 1 },
                MonthUsage { month: october, interactive: cost(2.5), headless: cost(1.25), requests: 1_234_567, unpriced_requests: 3 },
            ],
            plans: vec![
                PlanEstimate {
                    plan_id: "api".into(),
                    plan_name: "API pay-as-you-go".into(),
                    kind: PlanKind::PayAsYouGo,
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
