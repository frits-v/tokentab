#![allow(clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("claude_code")
}

fn tokentab(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tokentab"))
        .args(arguments)
        .output()
        .expect("the test binary can be started")
}

fn estimate(extra: &[&str]) -> Output {
    let projects_dir = fixtures();
    let projects_dir = projects_dir.to_str().expect("the fixture path is UTF-8");
    let mut arguments = vec!["estimate", "--projects-dir", projects_dir];
    arguments.extend_from_slice(extra);
    tokentab(&arguments)
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("tokentab writes UTF-8")
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).expect("tokentab writes UTF-8")
}

#[test]
fn estimate_on_fixtures_prints_all_five_sections() {
    let output = estimate(&[]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let text = stdout(&output);
    let headings: Vec<&str> = ["Scan", "Usage by month", "Plans", "Patterns", "Assumptions"]
        .into_iter()
        .filter(|heading| text.contains(&format!("{heading}\n{}\n", "-".repeat(heading.len()))))
        .collect();
    assert_eq!(headings.len(), 5, "{text}");
    assert!(
        text.contains("  requests in range                                  10\n"),
        "{text}"
    );
}

#[test]
fn estimate_json_on_fixtures_reports_scan_counts_and_unpriced() {
    let output = estimate(&["--format", "json"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let value: serde_json::Value =
        serde_json::from_str(&stdout(&output)).expect("JSON output parses");
    assert_eq!(value["scan"]["files_seen"], 3);
    assert_eq!(value["scan"]["events"], 10);
    assert_eq!(value["scan"]["duplicates_collapsed"], 1);
    assert_eq!(value["unpriced"]["synthetic"], 1);
    assert_eq!(value["unpriced"]["unknown_models"]["claude-unknown-9"], 1);
    assert_eq!(value["plans"].as_array().map(Vec::len), Some(7));
}

#[test]
fn estimate_with_seats_annual_and_ignored_credits_succeeds() {
    let output = estimate(&["--seats", "3", "--billing", "annual", "--credits", "ignore"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(
        text.contains("seat maximum: 1 seats billed (requested 3)"),
        "{text}"
    );
    assert!(text.contains("  seats 3, annual billing\n"), "{text}");
}

#[test]
fn seat_count_outside_the_bounds_is_rejected() {
    for seats in ["0", "10001", "4000000000"] {
        let output = estimate(&["--seats", seats]);
        assert_eq!(output.status.code(), Some(2), "--seats {seats}");
        assert!(
            stderr(&output).contains("--seats must be between 1 and 10000"),
            "{}",
            stderr(&output)
        );
    }
    assert_eq!(estimate(&["--seats", "10000"]).status.code(), Some(0));
}

#[test]
fn allowance_anchor_outside_the_bounds_is_rejected() {
    for anchor in ["-1", "1e30", "100000.5", "NaN"] {
        let argument = format!("--allowance-anchor={anchor}");
        let output = estimate(&[argument.as_str()]);
        assert_eq!(output.status.code(), Some(2), "--allowance-anchor {anchor}");
        assert!(
            stderr(&output).contains("--allowance-anchor must be between 0 and 100000 dollars"),
            "{}",
            stderr(&output)
        );
    }
    assert_eq!(
        estimate(&["--allowance-anchor", "100000"]).status.code(),
        Some(0)
    );
}

#[test]
fn missing_projects_dir_fails_with_exit_code_two() {
    let output = tokentab(&[
        "estimate",
        "--projects-dir",
        "/nonexistent/tokentab-test-projects",
    ]);
    assert_eq!(output.status.code(), Some(2));
    assert!(
        stderr(&output).contains("/nonexistent/tokentab-test-projects"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn prices_lists_every_model_and_plan() {
    let output = tokentab(&["prices"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.starts_with("price list as of 2026-10-28\n"), "{text}");
    for expected in [
        "claude-opus-5-5",
        "claude-haiku-5-5",
        "team_standard",
        "enterprise",
    ] {
        assert!(text.contains(expected), "missing {expected} in\n{text}");
    }
    assert_eq!(text.matches("claude-haiku-5-5").count(), 2, "{text}");
}
