use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

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
        text.contains("seat maximum: 1 seat billed (requested 3)"),
        "{text}"
    );
    assert!(text.contains("  seats 3, annual billing\n"), "{text}");
}

#[test]
fn seat_count_outside_the_bounds_is_rejected() {
    for seats in ["0", "-1", "10001", "4000000000"] {
        let output = estimate(&["--seats", seats]);
        assert_eq!(output.status.code(), Some(2), "--seats {seats}");
        assert!(
            stderr(&output).contains(&format!("{seats} is not in 1..=10000")),
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
        for arguments in [vec![argument.as_str()], vec!["--allowance-anchor", anchor]] {
            let output = estimate(&arguments);
            assert_eq!(output.status.code(), Some(2), "{arguments:?}");
            assert!(
                stderr(&output).contains("--allowance-anchor must be between 0 and 100000 dollars"),
                "{}",
                stderr(&output)
            );
        }
    }
    assert_eq!(
        estimate(&["--allowance-anchor", "100000"]).status.code(),
        Some(0)
    );
}

#[test]
fn fixture_run_names_its_partial_months() {
    let output = estimate(&[]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(
        text.contains(
            "every month in range is partial, so avg/peak include them: 2026-09 (1 day),"
        ),
        "{text}"
    );
    assert!(text.contains("2026-10 (20 days)"), "{text}");
}

#[test]
fn since_on_or_after_until_is_rejected() {
    for (since, until) in [("2026-10-01", "2026-10-01"), ("2026-10-02", "2026-10-01")] {
        let output = estimate(&["--since", since, "--until", until]);
        assert_eq!(output.status.code(), Some(2), "{since} {until}");
        assert!(
            stderr(&output).contains("--since must be before --until"),
            "{}",
            stderr(&output)
        );
    }
}

#[test]
fn malformed_date_names_the_expected_format() {
    let output = estimate(&["--since", "2026-13-01"]);
    assert_eq!(output.status.code(), Some(2));
    let message = stderr(&output);
    assert_eq!(
        message, "tokentab: expected YYYY-MM-DD, got 2026-13-01\n",
        "{message}"
    );
}

#[test]
fn requested_range_fills_idle_months_in_json() {
    let output = estimate(&[
        "--since",
        "2026-08-01",
        "--until",
        "2026-11-01",
        "--format",
        "json",
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let value: serde_json::Value =
        serde_json::from_str(&stdout(&output)).expect("JSON output parses");
    let months: Vec<&str> = value["months"]
        .as_array()
        .expect("months is an array")
        .iter()
        .filter_map(|month| month["month"].as_str())
        .collect();
    assert_eq!(months, ["2026-08", "2026-09", "2026-10"]);
    assert_eq!(value["months"][0]["requests"], 0);
}

#[test]
fn prices_stops_quietly_when_stdout_closes() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_tokentab"))
        .arg("prices")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the test binary can be started");
    drop(child.stdout.take());
    let output = child.wait_with_output().expect("the binary exits");
    assert_eq!(output.status.code(), Some(141));
    assert_eq!(stderr(&output), "");
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
    assert!(text.contains("cache read"), "{text}");
    assert!(text.contains("allowance x"), "{text}");
    let enterprise = text
        .lines()
        .find(|line| line.starts_with("enterprise "))
        .expect("enterprise row");
    assert!(enterprise.contains(" seat_plus_usage "), "{enterprise}");
    let team = text
        .lines()
        .find(|line| line.starts_with("team_standard "))
        .expect("team_standard row");
    assert!(team.contains(" subscription "), "{team}");
    assert!(team.contains("     2   150 "), "{team}");
    let columns: Vec<&str> = enterprise.split_whitespace().collect();
    assert_eq!(
        columns[1..6],
        ["seat_plus_usage", "-", "$20.00", "20", "-"],
        "{enterprise}"
    );
}
