use tokentab::adapters::render_json::JsonRenderer;
use tokentab::adapters::render_text::TextRenderer;
use tokentab::domain::money::{Money, PerToken};
use tokentab::domain::plans::{
    Billing, CreditsMode, EstimateOptions, Plan, PlanCatalog, PlanKind, YearMonth,
};
use tokentab::domain::pricing::{ModelPrice, PriceList, PriceTier};
use tokentab::domain::report::{self, Report, ReportError, RequestedRange};
use tokentab::domain::usage::{
    Entrypoint, Granularity, ModelName, Scan, ScanStats, TokenUsage, UsageEvent,
};
use tokentab::ports::renderer::Renderer;

fn single_tier(id: &str, input: f64, output: f64) -> ModelPrice {
    ModelPrice {
        id: id.into(),
        display_name: id.into(),
        tiers: vec![PriceTier {
            prompt_tokens_over: 0,
            input: PerToken::from_usd_per_mtok(input),
            output: PerToken::from_usd_per_mtok(output),
            cache_read: PerToken::from_usd_per_mtok(input / 10.0),
            cache_write_5m: PerToken::from_usd_per_mtok(input * 1.25),
            cache_write_1h: PerToken::from_usd_per_mtok(input * 2.0),
        }],
    }
}

fn price_list() -> PriceList {
    PriceList {
        as_of: "2026-10-28".into(),
        models: vec![
            single_tier("claude-opus-5-5", 4.0, 20.0),
            single_tier("claude-sonnet-5-5", 2.0, 10.0),
        ],
    }
}

fn plan(
    id: &str,
    name: &str,
    kind: PlanKind,
    seat_usd: Option<f64>,
    allowance_multiple: f64,
) -> Plan {
    Plan {
        id: id.into(),
        name: name.into(),
        kind,
        seat_monthly: seat_usd.map(Money::from_usd),
        seat_annual: None,
        min_seats: 1,
        max_seats: Some(1),
        allowance_multiple,
        api_credits_per_seat: Money::ZERO,
        api_credits_pool_cap: None,
        extra_usage_at_api_rates: true,
    }
}

fn catalog() -> PlanCatalog {
    PlanCatalog {
        allowance_anchor: Money::from_usd(60.0),
        plans: vec![
            plan("api", "API pay-as-you-go", PlanKind::PayAsYouGo, None, 0.0),
            plan("pro", "Pro", PlanKind::Subscription, Some(20.0), 1.0),
        ],
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
        session_id: "session-a".into(),
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

fn named(id: &str) -> ModelName {
    ModelName::Named(id.into())
}

fn five_event_scan(files_seen: u64) -> Scan {
    let events = vec![
        event(
            "2026-09-30T04:12:00Z",
            named("claude-opus-5-5"),
            Entrypoint::Interactive,
        ),
        event(
            "2026-09-30T05:00:00Z",
            named("claude-sonnet-5-5"),
            Entrypoint::Interactive,
        ),
        event(
            "2026-09-30T06:00:00Z",
            named("claude-unreleased"),
            Entrypoint::Sdk,
        ),
        event(
            "2026-10-02T09:00:00Z",
            named("claude-opus-5-5"),
            Entrypoint::Sdk,
        ),
        event(
            "2026-10-02T10:00:00Z",
            ModelName::Synthetic,
            Entrypoint::Interactive,
        ),
    ];
    let stats = ScanStats {
        files_seen,
        lines_read: 12,
        assistant_lines: 6,
        events_before_dedup: 6,
        duplicates_collapsed: 1,
        events: events.len() as u64,
        ..ScanStats::default()
    };
    Scan {
        granularity: Granularity::Message,
        events,
        stats,
    }
}

fn built() -> Report {
    report::build(
        "claude-code (fixture)".into(),
        five_event_scan(2),
        &price_list(),
        &catalog(),
        options(),
        RequestedRange::default(),
    )
    .expect("three events are priceable")
}

fn render(renderer: &dyn Renderer, report: &Report) -> String {
    let mut buffer = Vec::new();
    renderer
        .render(report, &mut buffer)
        .expect("rendering into a Vec cannot fail");
    String::from_utf8(buffer).expect("renderers write UTF-8")
}

#[test]
fn unpriced_requests_are_bucketed_by_reason() {
    let report = built();
    assert_eq!(report.unpriced.synthetic, 1);
    assert_eq!(report.unpriced.missing_model, 0);
    assert_eq!(report.unpriced.unknown_models.len(), 1);
    assert_eq!(
        report.unpriced.unknown_models.get("claude-unreleased"),
        Some(&1)
    );
    assert_eq!(report.unpriced.total(), 2);
}

#[test]
fn months_split_interactive_from_headless() {
    let report = built();
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
    assert_eq!(september.interactive.total(), Money::from_nanos(9_000_000));
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
    assert_eq!(october.unpriced_requests, 1);
    assert_eq!(october.interactive.total(), Money::ZERO);
    assert_eq!(october.headless.total(), Money::from_nanos(6_000_000));
}

#[test]
fn built_report_estimates_every_catalog_plan() {
    let report = built();
    let plan_ids: Vec<&str> = report
        .plans
        .iter()
        .map(|plan| plan.plan_id.as_str())
        .collect();
    assert_eq!(plan_ids, vec!["api", "pro"]);
    assert_eq!(report.plans[0].total, Money::from_nanos(15_000_000));
    assert_eq!(report.patterns.requests, 3);
}

#[test]
fn no_files_is_an_error() {
    let result = report::build(
        "claude-code (fixture)".into(),
        five_event_scan(0),
        &price_list(),
        &catalog(),
        options(),
        RequestedRange::default(),
    );
    assert_eq!(result.err(), Some(ReportError::NoFiles));
}

#[test]
fn nothing_priced_is_an_error() {
    let mut scan = five_event_scan(2);
    scan.events.retain(|event| !matches!(&event.model, ModelName::Named(id) if id.starts_with("claude-opus") || id.starts_with("claude-sonnet")));
    scan.stats.events_filtered_out = 7;
    assert_eq!(scan.events.len(), 2);
    let result = report::build(
        "claude-code (fixture)".into(),
        scan,
        &price_list(),
        &catalog(),
        options(),
        RequestedRange::default(),
    );
    assert_eq!(
        result.err(),
        Some(ReportError::NothingPriced {
            unpriced: 2,
            filtered_out: 7,
            lines_unparseable: 0,
            assistant_lines_without_usage: 0,
        })
    );
}

#[test]
fn json_output_has_the_top_level_sections() {
    let text = render(&JsonRenderer, &built());
    let value: serde_json::Value =
        serde_json::from_str(&text).expect("JSON renderer writes valid JSON");
    let object = value.as_object().expect("report serialises as an object");
    for key in ["scan", "unpriced", "months", "plans", "patterns"] {
        assert!(object.contains_key(key), "missing key {key}");
    }
    assert_eq!(value["scan"]["files_seen"], 2);
    assert_eq!(value["unpriced"]["unknown_models"]["claude-unreleased"], 1);
    assert_eq!(value["months"].as_array().map(Vec::len), Some(2));
    assert_eq!(value["months"][0]["month"], "2026-09");
    assert_eq!(value["months"][1]["month"], "2026-10");
    assert_eq!(value["months"][0]["covered_from"], "2026-09-30");
    assert_eq!(value["plans"][1]["months"][0]["month"], "2026-09");
    assert_eq!(value["plans"].as_array().map(Vec::len), Some(2));
}

#[test]
fn text_output_has_every_section_and_key_phrase() {
    let text = render(&TextRenderer, &built());
    for expected in [
        "Scan\n",
        "Usage by month\n",
        "Plans\n",
        "Patterns\n",
        "Assumptions\n",
        "duplicates collapsed",
        "unknown model",
        "claude-unreleased",
        "no-cache counterfactual",
        "Allowances are an assumption",
    ] {
        assert!(text.contains(expected), "missing {expected:?} in\n{text}");
    }
    let mut lines_checked = 0;
    for line in text.lines() {
        assert!(line.len() <= 100, "line is {} columns: {line}", line.len());
        lines_checked += 1;
    }
    assert!(lines_checked > 40, "only {lines_checked} lines rendered");
}
