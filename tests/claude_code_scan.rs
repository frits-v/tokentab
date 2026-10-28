use std::collections::BTreeMap;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use jiff::Timestamp;
use tokentab::adapters::claude_code::ClaudeCodeTranscripts;
use tokentab::domain::usage::{
    Entrypoint, Granularity, ModelName, Scan, ScanFilter, TokenUsage, UsageEvent,
};
use tokentab::ports::usage_source::{SourceError, UsageSource};

type TestResult = Result<(), Box<dyn Error>>;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("claude_code")
}

fn scan_fixtures(filter: &ScanFilter) -> Result<Scan, SourceError> {
    ClaudeCodeTranscripts::new(fixtures()).scan(filter)
}

fn event<'a>(scan: &'a Scan, message_id: &str) -> Result<&'a UsageEvent, String> {
    scan.events
        .iter()
        .find(|event| event.message_id == message_id)
        .ok_or_else(|| format!("no event for {message_id}"))
}

#[test]
fn fixture_stats_count_everything_examined() -> TestResult {
    let scan = scan_fixtures(&ScanFilter::default())?;
    let stats = &scan.stats;
    assert_eq!(scan.granularity, Granularity::Message);
    assert_eq!(stats.files_seen, 3);
    assert_eq!(stats.files_unreadable, 0);
    assert_eq!(stats.lines_read, 16);
    assert_eq!(stats.lines_unparseable, 1);
    assert_eq!(stats.assistant_lines, 12);
    assert_eq!(stats.assistant_lines_without_usage, 1);
    assert_eq!(stats.events_before_dedup, 11);
    assert_eq!(stats.duplicates_collapsed, 1);
    assert_eq!(stats.conflicting_duplicates, 0);
    assert_eq!(stats.cache_split_mismatches, 0);
    assert_eq!(stats.events_filtered_out, 0);
    assert_eq!(stats.events, 10);
    assert_eq!(scan.events.len(), 10);
    let expected_types: BTreeMap<String, u64> = [("attachment", 1), ("system", 1), ("user", 1)]
        .into_iter()
        .map(|(kind, count)| (kind.to_string(), count))
        .collect();
    assert_eq!(stats.other_line_types, expected_types);
    Ok(())
}

#[test]
fn events_are_sorted_by_timestamp() -> TestResult {
    let scan = scan_fixtures(&ScanFilter::default())?;
    assert!(
        scan.events
            .windows(2)
            .all(|pair| pair[0].timestamp <= pair[1].timestamp)
    );
    Ok(())
}

#[test]
fn streamed_message_keeps_the_final_block() -> TestResult {
    let scan = scan_fixtures(&ScanFilter::default())?;
    let streamed = event(&scan, "msg_1")?;
    assert_eq!(
        streamed.tokens,
        TokenUsage {
            input: 2,
            output: 246,
            cache_read: 11604,
            cache_write_5m: 0,
            cache_write_1h: 36476
        }
    );
    assert_eq!(streamed.model, ModelName::Named("claude-opus-5-5".into()));
    assert_eq!(streamed.entrypoint, Entrypoint::Interactive);
    assert!(!streamed.sidechain);
    assert_eq!(streamed.project, "/Users/example/src/demo");
    assert_eq!(streamed.session_id, "sess-a");
    assert_eq!(
        streamed.timestamp,
        "2026-09-30T04:12:38.100Z".parse::<Timestamp>()?
    );
    Ok(())
}

#[test]
fn fixture_events_decode_snapshot_ids_synthetic_sidechain_and_entrypoints() -> TestResult {
    let scan = scan_fixtures(&ScanFilter::default())?;
    let dated_haiku = event(&scan, "msg_6")?;
    assert_eq!(
        dated_haiku.model,
        ModelName::Named("claude-haiku-4-5".into())
    );
    assert_eq!(
        dated_haiku.tokens,
        TokenUsage {
            input: 1000,
            output: 100,
            cache_read: 0,
            cache_write_5m: 4000,
            cache_write_1h: 0
        }
    );
    assert_eq!(event(&scan, "msg_3")?.model, ModelName::Synthetic);
    assert!(event(&scan, "msg_2")?.sidechain);
    assert_eq!(event(&scan, "msg_b1")?.entrypoint, Entrypoint::Sdk);
    let desktop_event = event(&scan, "msg_c1")?;
    assert_eq!(desktop_event.entrypoint, Entrypoint::Desktop);
    assert_eq!(desktop_event.project, "/Users/example/src/other");
    Ok(())
}

#[test]
fn project_filter_keeps_one_event() -> TestResult {
    let filter = ScanFilter {
        project_substring: Some("other".into()),
        ..ScanFilter::default()
    };
    let scan = scan_fixtures(&filter)?;
    assert_eq!(scan.stats.events, 1);
    assert_eq!(scan.stats.events_filtered_out, 9);
    assert_eq!(scan.events.len(), 1);
    assert_eq!(scan.events[0].message_id, "msg_c1");
    Ok(())
}

#[test]
fn since_filter_keeps_seven_events() -> TestResult {
    let filter = ScanFilter {
        since: Some("2026-10-01T00:00:00Z".parse()?),
        ..ScanFilter::default()
    };
    let scan = scan_fixtures(&filter)?;
    assert_eq!(scan.stats.events, 7);
    assert_eq!(scan.stats.events_filtered_out, 3);
    assert_eq!(scan.events.len(), 7);
    Ok(())
}

#[test]
fn empty_directory_scans_to_zero_counts() -> TestResult {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let empty = std::env::temp_dir().join(format!("tokentab-empty-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&empty)?;
    let result = ClaudeCodeTranscripts::new(empty.clone()).scan(&ScanFilter::default());
    fs::remove_dir_all(&empty)?;
    let scan = result?;
    assert_eq!(scan.stats.files_seen, 0);
    assert_eq!(scan.stats.lines_read, 0);
    assert_eq!(scan.stats.events, 0);
    assert!(scan.events.is_empty());
    Ok(())
}

#[test]
fn missing_projects_dir_is_unreadable() {
    let missing = fixtures().join("does-not-exist");
    let result = ClaudeCodeTranscripts::new(missing).scan(&ScanFilter::default());
    assert!(
        matches!(result, Err(SourceError::Unreadable { .. })),
        "got {result:?}"
    );
}
