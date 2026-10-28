//! Plain-text report for a terminal, at most 100 columns wide, no colour.
//!
//! Sections in order:
//! 1. Scan: source, files seen, lines read, unparseable lines, assistant lines,
//!    requests before dedup, duplicates collapsed (and how many conflicted), cache split
//!    mismatches, filtered out, priced, unpriced by reason.
//! 2. Usage by month: month, requests, interactive API-equivalent, headless API-equivalent,
//!    total, unpriced; then the unpriced requests that fall in no listed month.
//! 3. Plans: one row per plan with seats billed, average month, peak month, total over
//!    the range; a month-by-plan table of each month's cost on each plan; then per-plan
//!    notes and the sensitivity line (anchor halved / doubled).
//! 4. Patterns: peak 5-hour window and peak 7-day window (or "unavailable at day
//!    granularity"), active days, sessions, sidechain share, per-model table with
//!    tokens and cost, per-entrypoint table, top projects, cache stats with the
//!    no-cache counterfactual and the saving.
//! 5. Assumptions: pricing as-of date, allowance anchor, seats and billing, credits mode.

use std::io::{self, Write};

use jiff::Timestamp;

use crate::domain::money::{Money, group_thousands};
use crate::domain::plans::{Billing, CreditsMode};
use crate::domain::report::Report;
use crate::domain::usage::Granularity;
use crate::ports::renderer::Renderer;

pub struct TextRenderer;

const MAX_WIDTH: usize = 100;
const LABEL_WIDTH: usize = 34;
const VALUE_WIDTH: usize = 20;

impl Renderer for TextRenderer {
    fn render(&self, report: &Report, out: &mut dyn Write) -> io::Result<()> {
        render_scan(report, out)?;
        writeln!(out)?;
        render_months(report, out)?;
        writeln!(out)?;
        render_plans(report, out)?;
        writeln!(out)?;
        render_patterns(report, out)?;
        writeln!(out)?;
        render_assumptions(report, out)
    }
}

fn render_scan(report: &Report, out: &mut dyn Write) -> io::Result<()> {
    let stats = &report.scan;
    let unpriced = &report.unpriced;
    let priced: u64 = report.months.iter().map(|month| month.requests).sum();
    heading(out, "Scan")?;
    writeln!(
        out,
        "  source  {}",
        fit_head(&plain(&report.source), MAX_WIDTH - 10)
    )?;
    key_value(out, 2, "granularity", granularity_label(report.granularity))?;
    key_value(
        out,
        2,
        "first priced request",
        &optional_minute(report.first_event),
    )?;
    key_value(
        out,
        2,
        "last priced request",
        &optional_minute(report.last_event),
    )?;
    key_value(out, 2, "files seen", &thousands(stats.files_seen))?;
    key_value(
        out,
        2,
        "files unreadable",
        &thousands(stats.files_unreadable),
    )?;
    key_value(out, 2, "lines read", &thousands(stats.lines_read))?;
    key_value(
        out,
        2,
        "lines unparseable",
        &thousands(stats.lines_unparseable),
    )?;
    key_value(out, 2, "assistant lines", &thousands(stats.assistant_lines))?;
    key_value(
        out,
        2,
        "assistant lines without usage",
        &thousands(stats.assistant_lines_without_usage),
    )?;
    key_value(
        out,
        2,
        "requests before dedup",
        &thousands(stats.events_before_dedup),
    )?;
    key_value(
        out,
        2,
        "duplicates collapsed",
        &thousands(stats.duplicates_collapsed),
    )?;
    key_value(
        out,
        4,
        "with different input or cache",
        &thousands(stats.conflicting_duplicates),
    )?;
    key_value(
        out,
        2,
        "cache split mismatches",
        &thousands(stats.cache_split_mismatches),
    )?;
    key_value(
        out,
        2,
        "filtered out",
        &thousands(stats.events_filtered_out),
    )?;
    key_value(out, 2, "requests in range", &thousands(stats.events))?;
    key_value(out, 2, "priced", &thousands(priced))?;
    key_value(out, 2, "unpriced", &thousands(unpriced.total()))?;
    key_value(out, 4, "synthetic", &thousands(unpriced.synthetic))?;
    key_value(out, 4, "missing model", &thousands(unpriced.missing_model))?;
    key_value(
        out,
        4,
        "unknown model",
        &thousands(unpriced.unknown_models.values().sum()),
    )?;
    for (model, count) in &unpriced.unknown_models {
        key_value(out, 6, &plain(model), &thousands(*count))?;
    }
    key_value(
        out,
        2,
        "other line types",
        &thousands(stats.other_line_types.values().sum()),
    )?;
    for (line_type, count) in &stats.other_line_types {
        key_value(out, 4, &plain(line_type), &thousands(*count))?;
    }
    Ok(())
}

fn render_months(report: &Report, out: &mut dyn Write) -> io::Result<()> {
    heading(out, "Usage by month")?;
    writeln!(
        out,
        "  {:<8} {:>12} {:>16} {:>16} {:>16} {:>10}",
        "month", "requests", "interactive", "headless", "total", "unpriced"
    )?;
    let mut requests = 0;
    let mut unpriced = 0;
    let mut interactive = Money::ZERO;
    let mut headless = Money::ZERO;
    for month in &report.months {
        let month_interactive = month.interactive.total();
        let month_headless = month.headless.total();
        writeln!(
            out,
            "  {:<8} {:>12} {:>16} {:>16} {:>16} {:>10}",
            month.month.to_string(),
            thousands(month.requests),
            month_interactive.to_string(),
            month_headless.to_string(),
            (month_interactive + month_headless).to_string(),
            thousands(month.unpriced_requests)
        )?;
        requests += month.requests;
        unpriced += month.unpriced_requests;
        interactive += month_interactive;
        headless += month_headless;
    }
    writeln!(
        out,
        "  {:<8} {:>12} {:>16} {:>16} {:>16} {:>10}",
        "all",
        thousands(requests),
        interactive.to_string(),
        headless.to_string(),
        (interactive + headless).to_string(),
        thousands(unpriced)
    )?;
    let outside = report.unpriced.total().saturating_sub(unpriced);
    if outside > 0 {
        writeln!(
            out,
            "  {} unpriced requests fall in months without priced usage and are not listed",
            thousands(outside)
        )?;
    }
    Ok(())
}

fn render_plans(report: &Report, out: &mut dyn Write) -> io::Result<()> {
    heading(out, "Plans")?;
    writeln!(
        out,
        "  {:<28} {:>6} {:>16} {:>16} {:>16}",
        "plan", "seats", "avg/month", "peak month", "total"
    )?;
    if report.plans.is_empty() {
        writeln!(out, "  no plans in the catalog")?;
    }
    for plan in &report.plans {
        writeln!(
            out,
            "  {:<28} {:>6} {:>16} {:>16} {:>16}",
            fit_head(&plain(&plan.plan_name), 28),
            thousands(u64::from(plan.seats_billed)),
            plan.average_month.to_string(),
            plan.peak_month.to_string(),
            plan.total.to_string()
        )?;
    }
    render_plan_months(report, out)?;
    for plan in &report.plans {
        if plan.notes.is_empty() && plan.sensitivity.is_none() {
            continue;
        }
        writeln!(out)?;
        writeln!(
            out,
            "  {}",
            fit_head(&plain(&plan.plan_name), MAX_WIDTH - 2)
        )?;
        for note in &plan.notes {
            wrap(out, 4, &plain(note))?;
        }
        if let Some(sensitivity) = &plan.sensitivity {
            writeln!(
                out,
                "    allowance anchor halved: {}/month, doubled: {}/month",
                sensitivity.half_anchor_average_month, sensitivity.double_anchor_average_month
            )?;
        }
    }
    Ok(())
}

/// Months down, plans across: the variation the average and peak columns flatten.
fn render_plan_months(report: &Report, out: &mut dyn Write) -> io::Result<()> {
    const MONTH_WIDTH: usize = 7;
    const COLUMN_WIDTH: usize = 12;
    let plans = &report.plans;
    if plans.is_empty() {
        return Ok(());
    }
    if 2 + MONTH_WIDTH + (COLUMN_WIDTH + 1) * plans.len() > MAX_WIDTH {
        writeln!(
            out,
            "  per-month plan costs omitted: {} plans do not fit in {} columns; use --format json",
            plans.len(),
            MAX_WIDTH
        )?;
        return Ok(());
    }
    writeln!(out)?;
    write!(out, "  {:<MONTH_WIDTH$}", "month")?;
    for plan in plans {
        let header: String = plain(&plan.plan_id).chars().take(COLUMN_WIDTH).collect();
        write!(out, " {header:>COLUMN_WIDTH$}")?;
    }
    writeln!(out)?;
    let month_count = plans
        .iter()
        .map(|plan| plan.months.len())
        .max()
        .unwrap_or(0);
    for index in 0..month_count {
        let Some(month) = plans.iter().find_map(|plan| plan.months.get(index)) else {
            break;
        };
        write!(out, "  {:<MONTH_WIDTH$}", month.month.to_string())?;
        for plan in plans {
            let cell = match plan.months.get(index) {
                Some(plan_month) => plan_month.total.to_string(),
                None => "-".to_string(),
            };
            write!(out, " {:>COLUMN_WIDTH$}", cell)?;
        }
        writeln!(out)?;
    }
    Ok(())
}

fn render_patterns(report: &Report, out: &mut dyn Write) -> io::Result<()> {
    let patterns = &report.patterns;
    heading(out, "Patterns")?;
    for (label, window) in [
        ("peak 5-hour window", &patterns.window_5h),
        ("peak 7-day window", &patterns.window_7d),
    ] {
        match window {
            Some(peak) => writeln!(
                out,
                "  {:<20} {}  {:>14}  {:>12} requests",
                label,
                minute(peak.start),
                peak.cost.to_string(),
                thousands(peak.requests)
            )?,
            None => writeln!(out, "  {:<20} unavailable at day granularity", label)?,
        }
    }
    key_value(out, 2, "active days", &thousands(patterns.active_days))?;
    key_value(out, 2, "sessions", &thousands(patterns.sessions))?;
    key_value(out, 2, "requests", &thousands(patterns.requests))?;
    key_value(
        out,
        2,
        "sidechain share",
        &percent(patterns.sidechain_share),
    )?;

    writeln!(out)?;
    writeln!(
        out,
        "  {:<18} {:>9} {:>13} {:>13} {:>14} {:>13} {:>12}",
        "model", "requests", "input", "output", "cache read", "cache write", "cost"
    )?;
    if patterns.per_model.is_empty() {
        writeln!(out, "  none")?;
    }
    for model in &patterns.per_model {
        writeln!(
            out,
            "  {:<18} {:>9} {:>13} {:>13} {:>14} {:>13} {:>12}",
            fit_head(&plain(&model.model), 18),
            thousands(model.requests),
            thousands(model.tokens.input),
            thousands(model.tokens.output),
            thousands(model.tokens.cache_read),
            thousands(model.tokens.cache_write()),
            model.cost.total().to_string()
        )?;
    }

    writeln!(out)?;
    writeln!(
        out,
        "  {:<20} {:>12} {:>16}",
        "entrypoint", "requests", "cost"
    )?;
    if patterns.per_entrypoint.is_empty() {
        writeln!(out, "  none")?;
    }
    for entrypoint in &patterns.per_entrypoint {
        writeln!(
            out,
            "  {:<20} {:>12} {:>16}",
            fit_head(&plain(&entrypoint.entrypoint), 20),
            thousands(entrypoint.requests),
            entrypoint.cost.to_string()
        )?;
    }

    writeln!(out)?;
    writeln!(out, "  {:<64} {:>12} {:>16}", "project", "requests", "cost")?;
    if patterns.per_project.is_empty() {
        writeln!(out, "  none")?;
    }
    for project in &patterns.per_project {
        writeln!(
            out,
            "  {:<64} {:>12} {:>16}",
            fit_tail(&plain(&project.project), 64),
            thousands(project.requests),
            project.cost.to_string()
        )?;
    }

    let cache = &patterns.cache;
    writeln!(out)?;
    writeln!(out, "  cache")?;
    key_value(out, 4, "input tokens", &thousands(cache.input_tokens))?;
    key_value(
        out,
        4,
        "cache read tokens",
        &thousands(cache.cache_read_tokens),
    )?;
    key_value(
        out,
        4,
        "cache write 5m tokens",
        &thousands(cache.cache_write_5m_tokens),
    )?;
    key_value(
        out,
        4,
        "cache write 1h tokens",
        &thousands(cache.cache_write_1h_tokens),
    )?;
    key_value(out, 4, "cache read share", &percent(cache.cache_read_share))?;
    key_value(out, 4, "spend", &cache.spend.to_string())?;
    writeln!(
        out,
        "    {}",
        counterfactual_line(cache.spend, cache.spend_without_cache)
    )
}

fn render_assumptions(report: &Report, out: &mut dyn Write) -> io::Result<()> {
    let options = &report.options;
    heading(out, "Assumptions")?;
    writeln!(
        out,
        "  pricing as of {}",
        fit_head(&plain(&report.pricing_as_of), 40)
    )?;
    let anchor_origin = if options.allowance_anchor.is_some() {
        "set on the command line"
    } else {
        "price list default"
    };
    writeln!(
        out,
        "  allowance anchor {} of API-equivalent usage per Pro seat per month ({anchor_origin})",
        report.allowance_anchor
    )?;
    let billing = match options.billing {
        Billing::Monthly => "monthly",
        Billing::Annual => "annual",
    };
    writeln!(out, "  seats {}, {billing} billing", options.seats)?;
    let credits = match options.credits {
        CreditsMode::Apply => "bundled API credits applied to SDK and headless usage only",
        CreditsMode::Ignore => "bundled API credits ignored",
    };
    writeln!(out, "  credits: {credits}")?;
    writeln!(
        out,
        "  Allowances are an assumption: Anthropic publishes plan ratios, not dollar limits."
    )
}

fn heading(out: &mut dyn Write, title: &str) -> io::Result<()> {
    writeln!(out, "{title}")?;
    writeln!(out, "{}", "-".repeat(title.len()))
}

/// Values line up in one column whatever the indent.
fn key_value(out: &mut dyn Write, indent: usize, label: &str, value: &str) -> io::Result<()> {
    let label_width = LABEL_WIDTH - indent;
    writeln!(
        out,
        "{:indent$}{:<label_width$} {:>VALUE_WIDTH$}",
        "",
        fit_head(label, label_width),
        value
    )
}

fn counterfactual_line(spend: Money, without_cache: Money) -> String {
    let base = without_cache.usd();
    let share = |amount: Money| if base > 0.0 { amount.usd() / base } else { 0.0 };
    if spend <= without_cache {
        let saving = without_cache.saturating_sub(spend);
        format!(
            "no-cache counterfactual {without_cache}, saving {saving} ({})",
            percent(share(saving))
        )
    } else {
        let extra = spend.saturating_sub(without_cache);
        format!(
            "no-cache counterfactual {without_cache}, saving -{extra} (-{})",
            percent(share(extra))
        )
    }
}

fn granularity_label(granularity: Granularity) -> &'static str {
    match granularity {
        Granularity::Message => "message",
        Granularity::Day => "day",
    }
}

fn thousands(count: u64) -> String {
    group_thousands(&count.to_string())
}

fn percent(fraction: f64) -> String {
    if fraction.is_finite() {
        format!("{:.1}%", fraction * 100.0)
    } else {
        "n/a".to_string()
    }
}

fn minute(timestamp: Timestamp) -> String {
    format!(
        "{} UTC",
        timestamp
            .to_zoned(jiff::tz::TimeZone::UTC)
            .strftime("%Y-%m-%d %H:%M")
    )
}

fn optional_minute(timestamp: Option<Timestamp>) -> String {
    timestamp.map(minute).unwrap_or_else(|| "none".to_string())
}

/// Transcript strings are untrusted: anything outside printable ASCII could move the cursor or recolour the terminal.
fn plain(text: &str) -> String {
    text.chars()
        .map(|character| {
            if (' '..='~').contains(&character) {
                character
            } else {
                '?'
            }
        })
        .collect()
}

/// Expects `plain` text, so byte offsets are character offsets.
fn fit_head(text: &str, width: usize) -> String {
    if text.len() <= width {
        return text.to_string();
    }
    format!("{}...", &text[..width.saturating_sub(3)])
}

/// Keeps the end, where a project path is most distinctive. Expects `plain` text.
fn fit_tail(text: &str, width: usize) -> String {
    if text.len() <= width {
        return text.to_string();
    }
    let keep = width.saturating_sub(3);
    format!("...{}", &text[text.len() - keep..])
}

fn wrap(out: &mut dyn Write, indent: usize, text: &str) -> io::Result<()> {
    let width = MAX_WIDTH - indent;
    let mut line = String::new();
    for word in text.split_whitespace() {
        let word = fit_head(word, width);
        if !line.is_empty() && line.len() + 1 + word.len() > width {
            writeln!(out, "{:indent$}{line}", "")?;
            line.clear();
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(&word);
    }
    if !line.is_empty() {
        writeln!(out, "{:indent$}{line}", "")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::plans::EstimateOptions;
    use crate::domain::report::fixtures::sample_report;

    fn rendered(report: &Report) -> String {
        let mut buffer = Vec::new();
        TextRenderer
            .render(report, &mut buffer)
            .expect("rendering into a Vec cannot fail");
        String::from_utf8(buffer).expect("the renderer writes ASCII")
    }

    fn sections(text: &str) -> Vec<&str> {
        let lines: Vec<&str> = text.lines().collect();
        lines
            .windows(2)
            .filter(|pair| {
                !pair[0].is_empty()
                    && pair[1].chars().all(|character| character == '-')
                    && !pair[1].is_empty()
            })
            .map(|pair| pair[0])
            .collect()
    }

    #[test]
    fn text_report_has_five_sections_in_order() {
        let text = rendered(&sample_report(Granularity::Message));
        assert_eq!(
            sections(&text),
            vec!["Scan", "Usage by month", "Plans", "Patterns", "Assumptions"]
        );
        assert!(text.starts_with("Scan\n"));
    }

    #[test]
    fn text_report_fits_100_columns_in_plain_ascii() {
        for granularity in [Granularity::Message, Granularity::Day] {
            let mut report = sample_report(granularity);
            report.source = "claude-code \u{1b}[31m/very/long/path/".repeat(10);
            let text = rendered(&report);
            let mut lines_checked = 0;
            for line in text.lines() {
                assert!(
                    line.len() <= MAX_WIDTH,
                    "line is {} columns: {line}",
                    line.len()
                );
                assert!(
                    line.bytes().all(|byte| (b' '..=b'~').contains(&byte)),
                    "line is not printable ASCII: {line:?}"
                );
                lines_checked += 1;
            }
            assert!(lines_checked > 60, "only {lines_checked} lines rendered");
        }
    }

    #[test]
    fn text_report_lists_every_scan_count_and_unpriced_bucket() {
        let text = rendered(&sample_report(Granularity::Message));
        for expected in [
            "  files seen                                          3",
            "  files unreadable                                    1",
            "  lines read                                         16",
            "  lines unparseable                                   1",
            "  assistant lines                                    11",
            "  assistant lines without usage                       1",
            "  requests before dedup                              10",
            "  duplicates collapsed                                2",
            "    with different input or cache                     1",
            "  cache split mismatches                              3",
            "  filtered out                                        1",
            "  requests in range                                   7",
            "  priced                                      1,234,569",
            "  unpriced                                            4",
            "    synthetic                                         1",
            "    missing model                                     0",
            "    unknown model                                     3",
            "      claude-unreleased-model-w...                    3",
            "  other line types                                    5",
            "    attachment                                        4",
            "    summary                                           1",
            "  first priced request             2026-09-30 04:12 UTC",
        ] {
            assert!(
                text.contains(&format!("{expected}\n")),
                "missing line {expected:?} in\n{text}"
            );
        }
    }

    #[test]
    fn text_report_month_table_right_aligns_money_and_counts() {
        let text = rendered(&sample_report(Granularity::Message));
        assert!(text.contains("  2026-09             2           $10.00            $0.00           $10.00          1\n"), "{text}");
        assert!(text.contains("  2026-10     1,234,567            $2.50            $1.25            $3.75          3\n"), "{text}");
        assert!(text.contains("  all         1,234,569           $12.50            $1.25           $13.75          4\n"), "{text}");
        assert!(
            !text.contains("fall in months without priced usage"),
            "{text}"
        );
    }

    #[test]
    fn text_report_counts_unpriced_requests_outside_the_month_table() {
        let mut report = sample_report(Granularity::Message);
        report.unpriced.synthetic += 2;
        let text = rendered(&report);
        assert!(
            text.contains(
                "  2 unpriced requests fall in months without priced usage and are not listed\n"
            ),
            "{text}"
        );
    }

    #[test]
    fn text_report_plans_show_notes_and_sensitivity() {
        let text = rendered(&sample_report(Granularity::Message));
        assert!(text.contains("  plan                          seats        avg/month       peak month            total\n"), "{text}");
        assert!(text.contains("  API pay-as-you-go                 1            $6.88           $10.00           $13.75\n"), "{text}");
        assert!(
            text.contains("    seats raised to the plan minimum of 1"),
            "{text}"
        );
        assert!(
            text.contains("    allowance anchor halved: $20.00/month, doubled: $20.00/month\n"),
            "{text}"
        );
        assert_eq!(text.matches("allowance anchor halved").count(), 1);
    }

    #[test]
    fn text_report_plans_list_every_month_per_plan() {
        let report = sample_report(Granularity::Message);
        let text = rendered(&report);
        let header = format!(
            "  {:<7}{}\n",
            "month",
            report
                .plans
                .iter()
                .map(|plan| format!(" {:>12}", plan.plan_id.chars().take(12).collect::<String>()))
                .collect::<String>()
        );
        let matrix = text
            .split_once(&header)
            .map(|(_, after)| after)
            .unwrap_or_else(|| panic!("no plan matrix header in {text}"));
        for plan in &report.plans {
            for month in &plan.months {
                let row = matrix
                    .lines()
                    .find(|line| line.starts_with(&format!("  {}", month.month)))
                    .unwrap_or_else(|| panic!("no row for {}", month.month));
                assert!(row.contains(&month.total.to_string()), "{row}");
            }
        }
    }

    #[test]
    fn text_report_windows_print_start_cost_and_requests() {
        let text = rendered(&sample_report(Granularity::Message));
        assert!(text.contains("  peak 5-hour window   2026-09-30 04:12 UTC          $12.50            14 requests\n"), "{text}");
        assert!(text.contains("  peak 7-day window    2026-09-28 00:00 UTC       $1,234.56         3,210 requests\n"), "{text}");
        let daily = rendered(&sample_report(Granularity::Day));
        assert_eq!(daily.matches("unavailable at day granularity").count(), 2);
    }

    #[test]
    fn text_report_patterns_tables_and_cache_line() {
        let text = rendered(&sample_report(Granularity::Message));
        assert!(text.contains("  claude-opus-5-5    1,234,569 1,234,567,890        98,765  9,876,543,210        37,476       $13.75\n"), "{text}");
        assert!(
            text.contains("  sidechain share                                 12.5%\n"),
            "{text}"
        );
        assert!(
            text.contains("    cache read share                              88.9%\n"),
            "{text}"
        );
        assert!(text.contains("  ...mely-long-directory-name/that/nests/deeply/into/the/tree/demo            3           $13.75\n"), "{text}");
        assert!(
            text.contains("    no-cache counterfactual $41.25, saving $27.50 (66.7%)\n"),
            "{text}"
        );
    }

    #[test]
    fn text_report_assumptions_state_anchor_seats_and_credits() {
        let text = rendered(&sample_report(Granularity::Message));
        assert!(text.contains("  pricing as of 2026-10-28\n"));
        assert!(text.contains("  allowance anchor $60.00 of API-equivalent usage per Pro seat per month (price list default)\n"));
        assert!(text.contains("  seats 1, monthly billing\n"));
        assert!(
            text.contains(
                "  credits: bundled API credits applied to SDK and headless usage only\n"
            )
        );
        assert!(text.ends_with(
            "  Allowances are an assumption: Anthropic publishes plan ratios, not dollar limits.\n"
        ));
    }

    #[test]
    fn text_report_assumptions_follow_annual_ignore_and_anchor_override() {
        let mut report = sample_report(Granularity::Message);
        report.options = EstimateOptions {
            seats: 3,
            billing: Billing::Annual,
            credits: CreditsMode::Ignore,
            allowance_anchor: Some(Money::from_usd(90.0)),
        };
        report.allowance_anchor = Money::from_usd(90.0);
        let text = rendered(&report);
        assert!(text.contains("  allowance anchor $90.00 of API-equivalent usage per Pro seat per month (set on the command line)\n"), "{text}");
        assert!(text.contains("  seats 3, annual billing\n"), "{text}");
        assert!(
            text.contains("  credits: bundled API credits ignored\n"),
            "{text}"
        );
    }

    #[test]
    fn report_counterfactual_reports_negative_saving_and_zero_base() {
        assert_eq!(
            counterfactual_line(Money::from_usd(15.0), Money::from_usd(10.0)),
            "no-cache counterfactual $10.00, saving -$5.00 (-50.0%)"
        );
        assert_eq!(
            counterfactual_line(Money::ZERO, Money::ZERO),
            "no-cache counterfactual $0.00, saving $0.00 (0.0%)"
        );
    }

    #[test]
    fn report_helpers_group_thousands_and_sanitise() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000), "1,000");
        assert_eq!(thousands(9_876_543_210), "9,876,543,210");
        assert_eq!(percent(f64::NAN), "n/a");
        assert_eq!(percent(0.0005), "0.1%");
        assert_eq!(plain("a\u{1b}[0mé"), "a?[0m?");
        assert_eq!(fit_head("abcdefghij", 6), "abc...");
        assert_eq!(fit_tail("abcdefghij", 6), "...hij");
        assert_eq!(fit_head("abc", 6), "abc");
    }
}
