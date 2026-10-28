use std::collections::{BTreeMap, BTreeSet};

use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};
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
    let mut sorted: Vec<&PricedEvent> = events.iter().collect();
    sorted.sort_by_key(|priced| priced.event.timestamp);

    let (window_5h, window_7d) = match granularity {
        Granularity::Message => (
            peak_window(&sorted, SignedDuration::from_hours(5)),
            peak_window(&sorted, SignedDuration::from_hours(7 * 24)),
        ),
        Granularity::Day => (None, None),
    };

    let active_days: BTreeSet<jiff::civil::Date> = events
        .iter()
        .map(|priced| priced.event.timestamp.to_zoned(TimeZone::UTC).date())
        .collect();
    let sessions: BTreeSet<&str> = events
        .iter()
        .map(|priced| priced.event.session_id.as_str())
        .collect();
    let requests = events.len() as u64;
    let sidechain_requests = events
        .iter()
        .filter(|priced| priced.event.sidechain)
        .count() as u64;
    let sidechain_share = if requests == 0 {
        0.0
    } else {
        sidechain_requests as f64 / requests as f64
    };

    let mut per_project = per_project(events);
    per_project.truncate(TOP_PROJECTS);

    Patterns {
        window_5h,
        window_7d,
        active_days: active_days.len() as u64,
        sessions: sessions.len() as u64,
        requests,
        sidechain_share,
        per_model: per_model(events),
        per_entrypoint: per_entrypoint(events),
        per_project,
        cache: cache_stats(events),
    }
}

const TOP_PROJECTS: usize = 10;

/// `sorted` must be ordered by timestamp. Each index enters and leaves the window once.
fn peak_window(sorted: &[&PricedEvent], length: SignedDuration) -> Option<WindowPeak> {
    let mut peak: Option<WindowPeak> = None;
    let mut end_index = 0;
    let mut window_cost = Money::ZERO;
    for (start_index, start_event) in sorted.iter().enumerate() {
        let start = start_event.event.timestamp;
        // A window that would run past Timestamp::MAX holds every remaining event.
        let bounded_end = start.checked_add(length).ok();
        let end = bounded_end.unwrap_or(Timestamp::MAX);
        while end_index < sorted.len()
            && bounded_end.is_none_or(|limit| sorted[end_index].event.timestamp < limit)
        {
            window_cost += sorted[end_index].cost.total();
            end_index += 1;
        }
        let is_new_peak = match &peak {
            None => true,
            Some(current) => window_cost > current.cost,
        };
        if is_new_peak {
            peak = Some(WindowPeak {
                start,
                end,
                cost: window_cost,
                requests: (end_index - start_index) as u64,
            });
        }
        window_cost = window_cost.saturating_sub(start_event.cost.total());
    }
    peak
}

fn per_model(events: &[PricedEvent]) -> Vec<ModelTotals> {
    let mut totals: BTreeMap<&str, ModelTotals> = BTreeMap::new();
    for priced in events {
        let entry = totals
            .entry(priced.model.as_str())
            .or_insert_with(|| ModelTotals {
                model: priced.model.clone(),
                requests: 0,
                tokens: TokenUsage::default(),
                cost: Cost::default(),
            });
        entry.requests += 1;
        entry.tokens = entry.tokens.add(&priced.event.tokens);
        entry.cost = entry.cost.add(&priced.cost);
    }
    let mut rows: Vec<ModelTotals> = totals.into_values().collect();
    rows.sort_by(|left, right| {
        right
            .cost
            .total()
            .cmp(&left.cost.total())
            .then_with(|| left.model.cmp(&right.model))
    });
    rows
}

fn per_entrypoint(events: &[PricedEvent]) -> Vec<EntrypointTotals> {
    let mut totals: BTreeMap<&str, (u64, Money)> = BTreeMap::new();
    for priced in events {
        let entry = totals
            .entry(priced.event.entrypoint.label())
            .or_insert((0, Money::ZERO));
        entry.0 += 1;
        entry.1 += priced.cost.total();
    }
    let mut rows: Vec<EntrypointTotals> = totals
        .into_iter()
        .map(|(label, (requests, cost))| EntrypointTotals {
            entrypoint: label.to_string(),
            requests,
            cost,
        })
        .collect();
    rows.sort_by(|left, right| {
        right
            .cost
            .cmp(&left.cost)
            .then_with(|| left.entrypoint.cmp(&right.entrypoint))
    });
    rows
}

fn per_project(events: &[PricedEvent]) -> Vec<ProjectTotals> {
    let mut totals: BTreeMap<&str, (u64, Money)> = BTreeMap::new();
    for priced in events {
        let entry = totals
            .entry(priced.event.project.as_str())
            .or_insert((0, Money::ZERO));
        entry.0 += 1;
        entry.1 += priced.cost.total();
    }
    let mut rows: Vec<ProjectTotals> = totals
        .into_iter()
        .map(|(project, (requests, cost))| ProjectTotals {
            project: project.to_string(),
            requests,
            cost,
        })
        .collect();
    rows.sort_by(|left, right| {
        right
            .cost
            .cmp(&left.cost)
            .then_with(|| left.project.cmp(&right.project))
    });
    rows
}

fn cache_stats(events: &[PricedEvent]) -> CacheStats {
    let mut tokens = TokenUsage::default();
    let mut prompt_tokens: u64 = 0;
    let mut spend = Money::ZERO;
    let mut spend_without_cache = Money::ZERO;
    for priced in events {
        tokens = tokens.add(&priced.event.tokens);
        prompt_tokens += priced.event.tokens.prompt_tokens();
        spend += priced.cost.total();
        spend_without_cache += priced.cost.without_cache;
    }
    let cache_read_share = if prompt_tokens == 0 {
        0.0
    } else {
        tokens.cache_read as f64 / prompt_tokens as f64
    };
    CacheStats {
        input_tokens: tokens.input,
        cache_read_tokens: tokens.cache_read,
        cache_write_5m_tokens: tokens.cache_write_5m,
        cache_write_1h_tokens: tokens.cache_write_1h,
        cache_read_share,
        spend,
        spend_without_cache,
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use crate::domain::usage::{Entrypoint, ModelName};

    fn at(text: &str) -> Timestamp {
        text.parse().expect("test timestamps are valid RFC 3339")
    }

    fn priced(timestamp: &str, usd: f64) -> PricedEvent {
        PricedEvent {
            event: UsageEvent {
                session_id: "session-a".to_string(),
                message_id: format!("message-{timestamp}"),
                timestamp: at(timestamp),
                model: ModelName::Named("claude-opus-5-5".to_string()),
                entrypoint: Entrypoint::Interactive,
                sidechain: false,
                project: "demo".to_string(),
                tokens: TokenUsage::default(),
            },
            model: "claude-opus-5-5".to_string(),
            cost: Cost {
                output: Money::from_usd(usd),
                ..Cost::default()
            },
        }
    }

    #[test]
    fn five_hour_peak_is_the_lone_expensive_event() {
        let events = vec![
            priced("2026-03-02T16:30:00Z", 4.0),
            priced("2026-03-02T10:00:00Z", 1.0),
            priced("2026-03-02T11:00:00Z", 2.0),
        ];
        assert_eq!(
            analyse(&events, Granularity::Message).window_5h,
            Some(WindowPeak {
                start: at("2026-03-02T16:30:00Z"),
                end: at("2026-03-02T21:30:00Z"),
                cost: Money::from_usd(4.0),
                requests: 1
            })
        );
    }

    #[test]
    fn five_hour_peak_spans_two_events() {
        let events = vec![
            priced("2026-03-02T10:00:00Z", 3.0),
            priced("2026-03-02T11:00:00Z", 3.0),
            priced("2026-03-02T16:30:00Z", 4.0),
        ];
        assert_eq!(
            analyse(&events, Granularity::Message).window_5h,
            Some(WindowPeak {
                start: at("2026-03-02T10:00:00Z"),
                end: at("2026-03-02T15:00:00Z"),
                cost: Money::from_usd(6.0),
                requests: 2
            })
        );
    }

    #[test]
    fn window_end_is_exclusive_and_ties_keep_the_earlier_window() {
        let events = vec![
            priced("2026-03-02T10:00:00Z", 2.0),
            priced("2026-03-02T15:00:00Z", 2.0),
        ];
        assert_eq!(
            analyse(&events, Granularity::Message).window_5h,
            Some(WindowPeak {
                start: at("2026-03-02T10:00:00Z"),
                end: at("2026-03-02T15:00:00Z"),
                cost: Money::from_usd(2.0),
                requests: 1
            })
        );
    }

    #[test]
    fn seven_day_peak_finds_the_densest_span() {
        let events = vec![
            priced("2026-03-01T12:00:00Z", 5.0),
            priced("2026-03-04T12:00:00Z", 1.0),
            priced("2026-03-05T12:00:00Z", 2.0),
            priced("2026-03-08T12:00:00Z", 3.0),
            priced("2026-03-10T12:00:00Z", 4.0),
        ];
        assert_eq!(
            analyse(&events, Granularity::Message).window_7d,
            Some(WindowPeak {
                start: at("2026-03-04T12:00:00Z"),
                end: at("2026-03-11T12:00:00Z"),
                cost: Money::from_usd(10.0),
                requests: 4
            })
        );
    }

    #[test]
    fn window_at_the_end_of_time_still_counts_its_start_event() {
        let mut events = vec![
            priced("2026-03-02T10:00:00Z", 1.0),
            priced("2026-03-02T11:00:00Z", 1.0),
            priced("2026-03-02T12:00:00Z", 1.0),
        ];
        events[1].event.timestamp = Timestamp::MAX;
        events[2].event.timestamp = Timestamp::MAX;
        assert_eq!(
            analyse(&events, Granularity::Message).window_5h,
            Some(WindowPeak {
                start: Timestamp::MAX,
                end: Timestamp::MAX,
                cost: Money::from_usd(2.0),
                requests: 2
            })
        );
    }

    #[test]
    fn day_granularity_has_no_windows() {
        let events = vec![priced("2026-03-02T10:00:00Z", 1.0)];
        let patterns = analyse(&events, Granularity::Day);
        assert_eq!(patterns.window_5h, None);
        assert_eq!(patterns.window_7d, None);
        assert_eq!(patterns.requests, 1);
    }

    #[test]
    fn empty_input_has_no_windows_zero_shares_and_empty_tables() {
        let patterns = analyse(&[], Granularity::Message);
        assert_eq!(patterns.window_5h, None);
        assert_eq!(patterns.window_7d, None);
        assert_eq!(patterns.active_days, 0);
        assert_eq!(patterns.sessions, 0);
        assert_eq!(patterns.requests, 0);
        assert_eq!(patterns.sidechain_share, 0.0);
        assert_eq!(patterns.cache.cache_read_share, 0.0);
        assert_eq!(patterns.cache.spend, Money::ZERO);
        assert!(patterns.per_model.is_empty());
        assert!(patterns.per_entrypoint.is_empty());
        assert!(patterns.per_project.is_empty());
    }

    #[test]
    fn per_model_sums_and_orders_by_cost_then_name() {
        let mut events = Vec::new();
        for (model, usd) in [
            ("claude-haiku-5-5", 2.0),
            ("claude-opus-5-5", 5.0),
            ("claude-sonnet-5-5", 2.0),
            ("claude-haiku-5-5", 1.0),
        ] {
            let mut event = priced("2026-03-02T10:00:00Z", usd);
            event.model = model.to_string();
            event.event.tokens = TokenUsage {
                input: 10,
                output: 1,
                ..TokenUsage::default()
            };
            events.push(event);
        }
        let per_model = analyse(&events, Granularity::Message).per_model;
        let names: Vec<&str> = per_model.iter().map(|row| row.model.as_str()).collect();
        assert_eq!(
            names,
            ["claude-opus-5-5", "claude-haiku-5-5", "claude-sonnet-5-5"]
        );
        assert_eq!(per_model[1].requests, 2);
        assert_eq!(per_model[1].cost.total(), Money::from_usd(3.0));
        assert_eq!(
            per_model[1].tokens,
            TokenUsage {
                input: 20,
                output: 2,
                ..TokenUsage::default()
            }
        );
    }

    #[test]
    fn per_entrypoint_groups_by_label() {
        let mut events = vec![
            priced("2026-03-02T10:00:00Z", 1.0),
            priced("2026-03-02T11:00:00Z", 2.0),
            priced("2026-03-02T12:00:00Z", 4.0),
        ];
        events[0].event.entrypoint = Entrypoint::Sdk;
        events[1].event.entrypoint = Entrypoint::Sdk;
        events[2].event.entrypoint = Entrypoint::Unrecognised("vscode-2030".to_string());
        let rows = analyse(&events, Granularity::Message).per_entrypoint;
        assert_eq!(
            rows,
            vec![
                EntrypointTotals {
                    entrypoint: "vscode-2030".to_string(),
                    requests: 1,
                    cost: Money::from_usd(4.0)
                },
                EntrypointTotals {
                    entrypoint: "sdk".to_string(),
                    requests: 2,
                    cost: Money::from_usd(3.0)
                },
            ]
        );
    }

    #[test]
    fn per_project_keeps_the_top_ten_by_cost() {
        let events: Vec<PricedEvent> = (1..=12)
            .map(|index| {
                let mut event = priced("2026-03-02T10:00:00Z", index as f64);
                event.event.project = format!("project-{index:02}");
                event
            })
            .collect();
        let rows = analyse(&events, Granularity::Message).per_project;
        assert_eq!(rows.len(), 10);
        assert_eq!(rows[0].project, "project-12");
        assert_eq!(rows[0].cost, Money::from_usd(12.0));
        assert_eq!(rows[9].project, "project-03");
    }

    #[test]
    fn cache_read_share_is_reads_over_prompt_tokens() {
        let mut event = priced("2026-03-02T10:00:00Z", 1.0);
        event.event.tokens = TokenUsage {
            input: 100,
            output: 50,
            cache_read: 300,
            ..TokenUsage::default()
        };
        event.cost.without_cache = Money::from_usd(2.5);
        let cache = analyse(&[event], Granularity::Message).cache;
        assert_eq!(cache.input_tokens, 100);
        assert_eq!(cache.cache_read_tokens, 300);
        assert_eq!(cache.cache_read_share, 0.75);
        assert_eq!(cache.spend, Money::from_usd(1.0));
        assert_eq!(cache.spend_without_cache, Money::from_usd(2.5));
    }

    #[test]
    fn counts_days_sessions_and_sidechain_share() {
        let mut events = vec![
            priced("2026-03-02T23:30:00Z", 1.0),
            priced("2026-03-03T00:30:00Z", 1.0),
            priced("2026-03-03T01:00:00Z", 1.0),
            priced("2026-03-03T02:00:00Z", 1.0),
        ];
        events[1].event.session_id = "session-b".to_string();
        events[3].event.sidechain = true;
        let patterns = analyse(&events, Granularity::Message);
        assert_eq!(patterns.active_days, 2);
        assert_eq!(patterns.sessions, 2);
        assert_eq!(patterns.requests, 4);
        assert_eq!(patterns.sidechain_share, 0.25);
    }
}
