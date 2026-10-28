use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read};
use std::path::Path;

use jiff::{SignedDuration, Timestamp};
use walkdir::WalkDir;

use super::record::{LineKind, TranscriptLine, Usage};
use crate::domain::usage::{
    Entrypoint, Granularity, ModelName, Scan, ScanFilter, ScanStats, TokenUsage, UsageEvent,
};
use crate::ports::usage_source::SourceError;

/// Walks `projects_dir` recursively for `*.jsonl`, following symlinks, parses each line,
/// keeps assistant lines with usage, dedupes by `message.id` keeping the line with the
/// highest `output_tokens` (the last streamed block), applies `filter`, and fills
/// `ScanStats` so the report can state what was examined. The project name is the
/// record's `cwd`, falling back to the transcript's parent directory name.
///
/// Errors only when `projects_dir` itself cannot be read; a single unreadable file,
/// or a `*.jsonl` that is not a regular file, is counted in `files_unreadable` and skipped.
pub fn scan_directory(projects_dir: &Path, filter: &ScanFilter) -> Result<Scan, SourceError> {
    std::fs::read_dir(projects_dir).map_err(|error| unreadable(projects_dir, &error))?;

    let plausible = PlausibleTimes::ending_tomorrow();
    let mut stats = ScanStats::default();
    let mut latest_by_message: BTreeMap<String, UsageEvent> = BTreeMap::new();
    // Loops through symlinked directories come back as errors and are counted below.
    for entry in WalkDir::new(projects_dir)
        .follow_links(true)
        .sort_by_file_name()
    {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) if error.depth() == 0 => return Err(unreadable(projects_dir, &error)),
            // An unreadable subdirectory hides transcripts; count it rather than skip silently.
            Err(_) => {
                stats.files_unreadable += 1;
                continue;
            }
        };
        if entry.file_type().is_dir() || entry.path().extension() != Some(OsStr::new("jsonl")) {
            continue;
        }
        stats.files_seen += 1;
        if !entry.file_type().is_file() {
            stats.files_unreadable += 1;
            continue;
        }
        scan_file(entry.path(), &plausible, &mut stats, &mut latest_by_message);
    }

    stats.duplicates_collapsed = stats.events_before_dedup - latest_by_message.len() as u64;
    let mut events = Vec::with_capacity(latest_by_message.len());
    for event in latest_by_message.into_values() {
        if filter.accepts(&event) {
            events.push(event);
        } else {
            stats.events_filtered_out += 1;
        }
    }
    events.sort_by_key(|event| event.timestamp);
    stats.events = events.len() as u64;

    Ok(Scan {
        granularity: Granularity::Message,
        events,
        stats,
    })
}

fn unreadable(path: &Path, error: &dyn std::error::Error) -> SourceError {
    SourceError::Unreadable {
        path: path.display().to_string(),
        reason: error.to_string(),
    }
}

/// Far above any real transcript line, and small enough to hold in memory.
const MAX_LINE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, PartialEq, Eq)]
enum LineRead {
    Eof,
    Line,
    /// Longer than the cap; the rest of it was skipped and `buffer` is empty.
    TooLong,
}

/// Reads one line into `buffer`, holding at most `cap` bytes of it in memory.
fn read_line_capped(
    reader: &mut impl BufRead,
    buffer: &mut Vec<u8>,
    cap: u64,
) -> io::Result<LineRead> {
    buffer.clear();
    let read = reader.by_ref().take(cap).read_until(b'\n', buffer)?;
    if read == 0 {
        return Ok(LineRead::Eof);
    }
    if buffer.ends_with(b"\n") || (read as u64) < cap {
        return Ok(LineRead::Line);
    }
    // Ends at the next newline or at the end of the file; only regular files get here.
    loop {
        buffer.clear();
        let skipped = reader.by_ref().take(cap).read_until(b'\n', buffer)?;
        if skipped == 0 || buffer.ends_with(b"\n") {
            break;
        }
    }
    buffer.clear();
    Ok(LineRead::TooLong)
}

fn scan_file(
    path: &Path,
    plausible: &PlausibleTimes,
    stats: &mut ScanStats,
    latest_by_message: &mut BTreeMap<String, UsageEvent>,
) {
    let Ok(file) = File::open(path) else {
        stats.files_unreadable += 1;
        return;
    };
    let mut reader = BufReader::new(file);
    let mut buffer = Vec::new();
    let mut line_number: u64 = 0;
    loop {
        let read = match read_line_capped(&mut reader, &mut buffer, MAX_LINE_BYTES) {
            Ok(read) => read,
            // Lines already read from this file stay counted; the rest of it is lost.
            Err(_) => {
                stats.files_unreadable += 1;
                break;
            }
        };
        match read {
            LineRead::Eof => break,
            LineRead::Line => {}
            LineRead::TooLong => {
                line_number += 1;
                stats.lines_read += 1;
                stats.lines_unparseable += 1;
                continue;
            }
        }
        line_number += 1;
        stats.lines_read += 1;

        let line = buffer.trim_ascii();
        if line.is_empty() {
            continue;
        }
        let Ok(record) = serde_json::from_slice::<TranscriptLine>(line) else {
            stats.lines_unparseable += 1;
            continue;
        };
        match record.classify() {
            LineKind::AssistantWithUsage(record) => {
                stats.assistant_lines += 1;
                let Some((event, split_mismatch)) =
                    build_event(record, path, line_number, plausible)
                else {
                    stats.lines_unparseable += 1;
                    continue;
                };
                // A billed model with nothing to bill is a usage object that says nothing.
                if event.tokens.is_empty() && matches!(event.model, ModelName::Named(_)) {
                    stats.assistant_lines_without_usage += 1;
                    continue;
                }
                if split_mismatch {
                    stats.cache_split_mismatches += 1;
                }
                stats.events_before_dedup += 1;
                keep_latest(latest_by_message, event, stats);
            }
            LineKind::AssistantWithoutUsage => {
                stats.assistant_lines += 1;
                stats.assistant_lines_without_usage += 1;
            }
            LineKind::OtherType(kind) => {
                *stats.other_line_types.entry(kind.to_string()).or_insert(0) += 1;
            }
            LineKind::Unrecognised => stats.lines_unparseable += 1,
        }
    }
}

/// Message ids are globally unique, so dedup spans files.
fn keep_latest(
    latest_by_message: &mut BTreeMap<String, UsageEvent>,
    event: UsageEvent,
    stats: &mut ScanStats,
) {
    match latest_by_message.entry(event.message_id.clone()) {
        Entry::Vacant(slot) => {
            slot.insert(event);
        }
        Entry::Occupied(mut slot) => {
            let kept = &slot.get().tokens;
            if event.tokens.input != kept.input
                || event.tokens.cache_read != kept.cache_read
                || event.tokens.cache_write() != kept.cache_write()
            {
                stats.conflicting_duplicates += 1;
            }
            if event.tokens.output >= slot.get().tokens.output {
                slot.insert(event);
            }
        }
    }
}

/// Far above any real request (the largest context window is 1M tokens), and low enough
/// that no single request can overflow u64 nanodollars at any price the loader accepts.
const MAX_TOKENS_PER_FIELD: u64 = 100_000_000;

/// Transcripts cannot predate Claude Code. A timestamp outside this window would open a
/// billing month of its own, so the line counts as unparseable instead.
struct PlausibleTimes {
    earliest: Timestamp,
    latest: Timestamp,
}

impl PlausibleTimes {
    /// 2023-01-01T00:00:00Z.
    const EARLIEST: Timestamp = Timestamp::constant(1_672_531_200, 0);

    /// One day of slack after now for clock skew between machines.
    fn ending_tomorrow() -> PlausibleTimes {
        let now = Timestamp::now();
        PlausibleTimes {
            earliest: PlausibleTimes::EARLIEST,
            latest: now
                .checked_add(SignedDuration::from_hours(24))
                .unwrap_or(now),
        }
    }

    fn contains(&self, timestamp: Timestamp) -> bool {
        self.earliest <= timestamp && timestamp <= self.latest
    }
}

/// None when the timestamp is missing, not RFC 3339 or outside `plausible`, or a token
/// count is above `MAX_TOKENS_PER_FIELD`; the caller counts the line as unparseable.
/// The flag is true when the cache-write split disagreed with its total.
fn build_event(
    record: &TranscriptLine,
    path: &Path,
    line_number: u64,
    plausible: &PlausibleTimes,
) -> Option<(UsageEvent, bool)> {
    let timestamp: Timestamp = record.timestamp.as_deref()?.parse().ok()?;
    if !plausible.contains(timestamp) {
        return None;
    }
    let (tokens, split_mismatch) = record.usage().map(token_usage).unwrap_or_default();
    if !within_bounds(&tokens) {
        return None;
    }
    let message = record.message.as_ref();

    let session_id = non_empty(record.session_id.as_deref())
        .map(str::to_string)
        .unwrap_or_else(|| {
            path.file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        });
    let message_id = non_empty(message.and_then(|message| message.id.as_deref()))
        .or_else(|| non_empty(record.uuid.as_deref()))
        .map(str::to_string)
        .unwrap_or_else(|| format!("{}:{}", path.display(), line_number));
    let project = non_empty(record.cwd.as_deref())
        .map(str::to_string)
        .unwrap_or_else(|| parent_directory_name(path));

    let event = UsageEvent {
        session_id,
        message_id,
        timestamp,
        model: ModelName::decode(message.and_then(|message| message.model.as_deref())),
        entrypoint: Entrypoint::decode(record.entrypoint.as_deref()),
        sidechain: record.is_sidechain.unwrap_or(false),
        project,
        tokens,
    };
    Some((event, split_mismatch))
}

fn within_bounds(tokens: &TokenUsage) -> bool {
    [
        tokens.input,
        tokens.output,
        tokens.cache_read,
        tokens.cache_write_5m,
        tokens.cache_write_1h,
    ]
    .iter()
    .all(|count| *count <= MAX_TOKENS_PER_FIELD)
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.filter(|text| !text.is_empty())
}

fn parent_directory_name(path: &Path) -> String {
    match path.parent().and_then(Path::file_name) {
        Some(name) => name.to_string_lossy().into_owned(),
        None => path.display().to_string(),
    }
}

/// A `cache_creation` split with at least one band set says how the writes divide
/// between the 5-minute and 1-hour rates. Whatever part of `cache_creation_input_tokens`
/// the split does not cover is priced as 5-minute writes, the cheaper band, as is the
/// whole total when there is no usable split. The flag is true when a split and its
/// total disagree.
fn token_usage(usage: &Usage) -> (TokenUsage, bool) {
    let total = usage.cache_creation_input_tokens.unwrap_or(0);
    let split = usage.cache_creation.as_ref().filter(|split| {
        split.ephemeral_5m_input_tokens.is_some() || split.ephemeral_1h_input_tokens.is_some()
    });
    let (cache_write_5m, cache_write_1h, mismatch) = match split {
        Some(split) => {
            let split_5m = split.ephemeral_5m_input_tokens.unwrap_or(0);
            let split_1h = split.ephemeral_1h_input_tokens.unwrap_or(0);
            let split_sum = split_5m.saturating_add(split_1h);
            let mismatch = usage.cache_creation_input_tokens.is_some() && split_sum != total;
            let remainder = total.saturating_sub(split_sum);
            (split_5m.saturating_add(remainder), split_1h, mismatch)
        }
        None => (total, 0, false),
    };
    let tokens = TokenUsage {
        input: usage.input_tokens.unwrap_or(0),
        output: usage.output_tokens.unwrap_or(0),
        cache_read: usage.cache_read_input_tokens.unwrap_or(0),
        cache_write_5m,
        cache_write_1h,
    };
    (tokens, mismatch)
}

#[cfg(test)]
mod tests {
    use std::error::Error;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::super::record::CacheCreation;
    use super::*;

    type TestResult = Result<(), Box<dyn Error>>;

    struct TempProjects {
        root: PathBuf,
    }

    impl TempProjects {
        fn new(label: &str) -> Result<TempProjects, Box<dyn Error>> {
            let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
            let root = std::env::temp_dir().join(format!(
                "tokentab-scan-{label}-{}-{nanos}",
                std::process::id()
            ));
            fs::create_dir_all(&root)?;
            Ok(TempProjects { root })
        }

        fn write(&self, relative: &str, contents: &str) -> Result<PathBuf, Box<dyn Error>> {
            let path = self.root.join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&path, contents)?;
            Ok(path)
        }

        fn scan(&self) -> Result<Scan, SourceError> {
            scan_directory(&self.root, &ScanFilter::default())
        }
    }

    impl Drop for TempProjects {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn assistant(id: &str, timestamp: &str, output: u64) -> String {
        format!(
            r#"{{"type":"assistant","uuid":"u-{id}-{output}","timestamp":"{timestamp}","sessionId":"s","cwd":"/work","message":{{"id":"{id}","model":"claude-opus-5-5","usage":{{"input_tokens":1,"output_tokens":{output}}}}}}}"#
        )
    }

    #[test]
    fn blank_lines_are_read_but_not_unparseable() -> TestResult {
        let projects = TempProjects::new("blank")?;
        let line = assistant("msg_a", "2026-10-01T00:00:00Z", 5);
        projects.write("p/s.jsonl", &format!("\n{line}\n   \n\r\n"))?;
        let scan = projects.scan()?;
        assert_eq!(scan.stats.lines_read, 4);
        assert_eq!(scan.stats.lines_unparseable, 0);
        assert_eq!(scan.stats.events, 1);
        Ok(())
    }

    #[test]
    fn tie_on_output_keeps_the_later_line() -> TestResult {
        let projects = TempProjects::new("tie")?;
        let first = assistant("msg_a", "2026-10-01T00:00:00Z", 7);
        let second = assistant("msg_a", "2026-10-01T00:00:05Z", 7);
        let shorter = assistant("msg_a", "2026-10-01T00:00:09Z", 3);
        projects.write("p/s.jsonl", &format!("{first}\n{second}\n{shorter}\n"))?;
        let scan = projects.scan()?;
        assert_eq!(scan.stats.events_before_dedup, 3);
        assert_eq!(scan.stats.duplicates_collapsed, 2);
        assert_eq!(scan.events.len(), 1);
        assert_eq!(
            scan.events[0].timestamp,
            "2026-10-01T00:00:05Z".parse::<Timestamp>()?
        );
        Ok(())
    }

    #[test]
    fn dedup_spans_files() -> TestResult {
        let projects = TempProjects::new("across")?;
        projects.write(
            "p/one.jsonl",
            &assistant("msg_a", "2026-10-01T00:00:00Z", 10),
        )?;
        projects.write(
            "q/two.jsonl",
            &assistant("msg_a", "2026-10-01T00:00:01Z", 40),
        )?;
        let scan = projects.scan()?;
        assert_eq!(scan.stats.files_seen, 2);
        assert_eq!(scan.stats.duplicates_collapsed, 1);
        assert_eq!(scan.events.len(), 1);
        assert_eq!(scan.events[0].tokens.output, 40);
        Ok(())
    }

    #[test]
    fn missing_ids_fall_back_to_uuid_then_file_and_line() -> TestResult {
        let projects = TempProjects::new("fallback")?;
        let with_uuid = r#"{"type":"assistant","uuid":"u-9","timestamp":"2026-10-01T00:00:00Z","message":{"usage":{"output_tokens":1}}}"#;
        let bare = r#"{"type":"assistant","timestamp":"2026-10-01T00:00:01Z","message":{"usage":{"output_tokens":2}}}"#;
        let path = projects.write(
            "-Users-x-proj/sess-42.jsonl",
            &format!("{with_uuid}\n{bare}\n"),
        )?;
        let scan = projects.scan()?;
        assert_eq!(scan.events.len(), 2);
        assert_eq!(scan.events[0].message_id, "u-9");
        assert_eq!(scan.events[1].message_id, format!("{}:2", path.display()));
        for event in &scan.events {
            assert_eq!(event.session_id, "sess-42");
            assert_eq!(event.project, "-Users-x-proj");
            assert_eq!(event.model, ModelName::Missing);
            assert_eq!(event.entrypoint, Entrypoint::Missing);
            assert!(!event.sidechain);
        }
        Ok(())
    }

    #[test]
    fn missing_or_bad_timestamp_counts_as_unparseable() -> TestResult {
        let projects = TempProjects::new("timestamp")?;
        let missing = r#"{"type":"assistant","message":{"id":"m1","usage":{"output_tokens":1}}}"#;
        let garbled = r#"{"type":"assistant","timestamp":"yesterday","message":{"id":"m2","usage":{"output_tokens":1}}}"#;
        projects.write("p/s.jsonl", &format!("{missing}\n{garbled}\n"))?;
        let scan = projects.scan()?;
        assert_eq!(scan.stats.lines_read, 2);
        assert_eq!(scan.stats.assistant_lines, 2);
        assert_eq!(scan.stats.lines_unparseable, 2);
        assert_eq!(scan.stats.events_before_dedup, 0);
        assert_eq!(scan.stats.events, 0);
        Ok(())
    }

    #[test]
    fn implausible_token_count_counts_as_unparseable() -> TestResult {
        let projects = TempProjects::new("tokens")?;
        let at_bound = assistant("msg_a", "2026-10-01T00:00:00Z", MAX_TOKENS_PER_FIELD);
        let over_bound = assistant("msg_b", "2026-10-01T00:00:01Z", 1_000_000_000_000_000);
        projects.write("p/s.jsonl", &format!("{at_bound}\n{over_bound}\n"))?;
        let scan = projects.scan()?;
        assert_eq!(scan.stats.assistant_lines, 2);
        assert_eq!(scan.stats.lines_unparseable, 1);
        assert_eq!(scan.stats.events_before_dedup, 1);
        assert_eq!(scan.events.len(), 1);
        assert_eq!(scan.events[0].message_id, "msg_a");
        Ok(())
    }

    #[test]
    fn record_without_type_counts_as_unparseable() -> TestResult {
        let projects = TempProjects::new("untyped")?;
        projects.write("p/s.jsonl", "{\"uuid\":\"x\"}\n[1,2]\nnot json\n")?;
        let scan = projects.scan()?;
        assert_eq!(scan.stats.lines_read, 3);
        assert_eq!(scan.stats.lines_unparseable, 3);
        assert!(scan.stats.other_line_types.is_empty());
        Ok(())
    }

    #[test]
    fn invalid_utf8_line_is_unparseable_and_the_file_continues() -> TestResult {
        let projects = TempProjects::new("utf8")?;
        let path = projects.root.join("p").join("s.jsonl");
        fs::create_dir_all(projects.root.join("p"))?;
        let mut bytes = vec![0xff, 0xfe, b'\n'];
        bytes.extend_from_slice(assistant("msg_a", "2026-10-01T00:00:00Z", 1).as_bytes());
        fs::write(&path, bytes)?;
        let scan = projects.scan()?;
        assert_eq!(scan.stats.lines_read, 2);
        assert_eq!(scan.stats.lines_unparseable, 1);
        assert_eq!(scan.stats.files_unreadable, 0);
        assert_eq!(scan.stats.events, 1);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_file_is_counted_and_skipped() -> TestResult {
        use std::os::unix::fs::PermissionsExt;
        let projects = TempProjects::new("unreadable")?;
        let locked = projects.write(
            "p/locked.jsonl",
            &assistant("msg_a", "2026-10-01T00:00:00Z", 1),
        )?;
        projects.write(
            "p/open.jsonl",
            &assistant("msg_b", "2026-10-01T00:00:00Z", 1),
        )?;
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000))?;
        let scan = projects.scan()?;
        assert_eq!(scan.stats.files_seen, 2);
        assert_eq!(scan.stats.files_unreadable, 1);
        assert_eq!(scan.stats.events, 1);
        Ok(())
    }

    #[test]
    fn non_jsonl_files_are_ignored() -> TestResult {
        let projects = TempProjects::new("extension")?;
        projects.write(
            "p/notes.txt",
            &assistant("msg_a", "2026-10-01T00:00:00Z", 1),
        )?;
        projects.write(
            "p/s.jsonl.bak",
            &assistant("msg_b", "2026-10-01T00:00:00Z", 1),
        )?;
        let scan = projects.scan()?;
        assert_eq!(scan.stats.files_seen, 0);
        assert_eq!(scan.stats.lines_read, 0);
        Ok(())
    }

    fn cache_usage(total: Option<u64>, split: Option<CacheCreation>) -> Usage {
        Usage {
            input_tokens: Some(3),
            output_tokens: None,
            cache_read_input_tokens: Some(9),
            cache_creation_input_tokens: total,
            cache_creation: split,
        }
    }

    #[test]
    fn partial_cache_split_prices_the_remainder_as_five_minute_writes() {
        let usage = cache_usage(
            Some(500),
            Some(CacheCreation {
                ephemeral_5m_input_tokens: Some(200),
                ephemeral_1h_input_tokens: None,
            }),
        );
        let expected = TokenUsage {
            input: 3,
            output: 0,
            cache_read: 9,
            cache_write_5m: 500,
            cache_write_1h: 0,
        };
        assert_eq!(token_usage(&usage), (expected, true));
    }

    #[test]
    fn empty_cache_split_falls_back_to_the_total() {
        let usage = cache_usage(
            Some(500),
            Some(CacheCreation {
                ephemeral_5m_input_tokens: None,
                ephemeral_1h_input_tokens: None,
            }),
        );
        let (tokens, mismatch) = token_usage(&usage);
        assert_eq!(tokens.cache_write_5m, 500);
        assert_eq!(tokens.cache_write_1h, 0);
        assert!(!mismatch);
    }

    #[test]
    fn matching_cache_split_is_used_as_is() {
        let usage = cache_usage(
            Some(500),
            Some(CacheCreation {
                ephemeral_5m_input_tokens: Some(100),
                ephemeral_1h_input_tokens: Some(400),
            }),
        );
        let (tokens, mismatch) = token_usage(&usage);
        assert_eq!((tokens.cache_write_5m, tokens.cache_write_1h), (100, 400));
        assert!(!mismatch);
    }

    #[test]
    fn advisor_line_with_a_short_split_keeps_every_cache_write() -> TestResult {
        let projects = TempProjects::new("advisor")?;
        let line = r#"{"type":"assistant","uuid":"u-adv","timestamp":"2026-10-01T00:00:00Z","sessionId":"s","cwd":"/work","entrypoint":"cli","message":{"id":"msg_adv","model":"claude-opus-5-5","usage":{"input_tokens":12,"cache_creation_input_tokens":7242,"cache_read_input_tokens":30110,"output_tokens":845,"cache_creation":{"ephemeral_5m_input_tokens":0,"ephemeral_1h_input_tokens":5972},"iterations":[{"type":"message"},{"type":"advisor_message"},{"type":"message"}]}}}"#;
        projects.write("p/s.jsonl", &format!("{line}\n"))?;
        let scan = projects.scan()?;
        assert_eq!(scan.stats.lines_unparseable, 0);
        assert_eq!(scan.stats.cache_split_mismatches, 1);
        assert_eq!(scan.events.len(), 1);
        assert_eq!(scan.events[0].tokens.cache_write_1h, 5972);
        assert_eq!(scan.events[0].tokens.cache_write_5m, 1270);
        Ok(())
    }

    #[test]
    fn timestamp_outside_the_plausible_window_is_unparseable() -> TestResult {
        let projects = TempProjects::new("plausible")?;
        let ancient = assistant("msg_a", "0001-01-01T00:00:00Z", 1);
        let before_launch = assistant("msg_b", "2022-12-31T23:59:59Z", 1);
        let future = assistant("msg_c", "2999-01-01T00:00:00Z", 1);
        let earliest = assistant("msg_d", "2023-01-01T00:00:00Z", 1);
        projects.write(
            "p/s.jsonl",
            &format!("{ancient}\n{before_launch}\n{future}\n{earliest}\n"),
        )?;
        let scan = projects.scan()?;
        assert_eq!(scan.stats.lines_unparseable, 3);
        assert_eq!(scan.events.len(), 1);
        assert_eq!(scan.events[0].message_id, "msg_d");
        Ok(())
    }

    #[test]
    fn named_model_with_all_zero_usage_is_not_a_request() -> TestResult {
        let projects = TempProjects::new("zero")?;
        let empty = r#"{"type":"assistant","timestamp":"2026-10-01T00:00:00Z","message":{"id":"m1","model":"claude-opus-5-5","usage":{}}}"#;
        let nulls = r#"{"type":"assistant","timestamp":"2026-10-01T00:00:01Z","message":{"id":"m2","model":"claude-opus-5-5","usage":{"input_tokens":null,"output_tokens":null}}}"#;
        let synthetic = r#"{"type":"assistant","timestamp":"2026-10-01T00:00:02Z","message":{"id":"m3","model":"<synthetic>","usage":{"input_tokens":0,"output_tokens":0}}}"#;
        projects.write("p/s.jsonl", &format!("{empty}\n{nulls}\n{synthetic}\n"))?;
        let scan = projects.scan()?;
        assert_eq!(scan.stats.assistant_lines, 3);
        assert_eq!(scan.stats.assistant_lines_without_usage, 2);
        assert_eq!(scan.stats.events, 1);
        assert_eq!(scan.events[0].model, ModelName::Synthetic);
        Ok(())
    }

    #[test]
    fn duplicate_with_different_input_counts_as_conflicting() -> TestResult {
        let projects = TempProjects::new("conflict")?;
        let streamed_a = assistant("msg_a", "2026-10-01T00:00:00Z", 3);
        let streamed_b = assistant("msg_a", "2026-10-01T00:00:01Z", 9);
        let other = r#"{"type":"assistant","timestamp":"2026-10-01T00:00:02Z","message":{"id":"msg_a","model":"claude-opus-5-5","usage":{"input_tokens":500,"output_tokens":9}}}"#;
        projects.write(
            "p/s.jsonl",
            &format!("{streamed_a}\n{streamed_b}\n{other}\n"),
        )?;
        let scan = projects.scan()?;
        assert_eq!(scan.stats.duplicates_collapsed, 2);
        assert_eq!(scan.stats.conflicting_duplicates, 1);
        Ok(())
    }

    #[test]
    fn capped_reader_skips_an_oversized_line() -> Result<(), io::Error> {
        let mut reader = io::Cursor::new(b"short\nthis line is far too long\nnext\ntail".to_vec());
        let mut buffer = Vec::new();
        assert_eq!(
            read_line_capped(&mut reader, &mut buffer, 8)?,
            LineRead::Line
        );
        assert_eq!(buffer, b"short\n");
        assert_eq!(
            read_line_capped(&mut reader, &mut buffer, 8)?,
            LineRead::TooLong
        );
        assert!(buffer.is_empty());
        assert_eq!(
            read_line_capped(&mut reader, &mut buffer, 8)?,
            LineRead::Line
        );
        assert_eq!(buffer, b"next\n");
        assert_eq!(
            read_line_capped(&mut reader, &mut buffer, 8)?,
            LineRead::Line
        );
        assert_eq!(buffer, b"tail");
        assert_eq!(
            read_line_capped(&mut reader, &mut buffer, 8)?,
            LineRead::Eof
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_project_directory_is_scanned() -> TestResult {
        let projects = TempProjects::new("linked")?;
        let elsewhere = TempProjects::new("linked-target")?;
        elsewhere.write("s.jsonl", &assistant("msg_a", "2026-10-01T00:00:00Z", 1))?;
        std::os::unix::fs::symlink(&elsewhere.root, projects.root.join("linked"))?;
        let scan = projects.scan()?;
        assert_eq!(scan.stats.files_seen, 1);
        assert_eq!(scan.events.len(), 1);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn jsonl_that_is_not_a_regular_file_is_unreadable() -> TestResult {
        let projects = TempProjects::new("device")?;
        projects.write(
            "p/real.jsonl",
            &assistant("msg_a", "2026-10-01T00:00:00Z", 1),
        )?;
        std::os::unix::fs::symlink("/dev/zero", projects.root.join("p").join("zero.jsonl"))?;
        let scan = projects.scan()?;
        assert_eq!(scan.stats.files_seen, 2);
        assert_eq!(scan.stats.files_unreadable, 1);
        assert_eq!(scan.stats.lines_read, 1);
        assert_eq!(scan.events.len(), 1);
        Ok(())
    }

    #[test]
    fn cache_total_without_split_is_a_five_minute_write() {
        let usage = Usage {
            input_tokens: None,
            output_tokens: Some(4),
            cache_read_input_tokens: None,
            cache_creation_input_tokens: Some(500),
            cache_creation: None,
        };
        let expected = TokenUsage {
            input: 0,
            output: 4,
            cache_read: 0,
            cache_write_5m: 500,
            cache_write_1h: 0,
        };
        assert_eq!(token_usage(&usage), (expected, false));
    }
}
