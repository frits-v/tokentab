use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::ffi::OsStr;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use jiff::Timestamp;
use walkdir::WalkDir;

use super::record::{LineKind, TranscriptLine, Usage};
use crate::domain::usage::{
    Entrypoint, Granularity, ModelName, Scan, ScanFilter, ScanStats, TokenUsage, UsageEvent,
};
use crate::ports::usage_source::SourceError;

/// Walks `projects_dir` recursively for `*.jsonl`, parses each line, keeps assistant
/// lines with usage, dedupes by `message.id` keeping the line with the highest
/// `output_tokens` (the last streamed block), applies `filter`, and fills `ScanStats`
/// so the report can state what was examined. The project name is the record's `cwd`,
/// falling back to the transcript's parent directory name.
///
/// Errors only when `projects_dir` itself cannot be read; a single unreadable file
/// is counted in `files_unreadable` and skipped.
pub fn scan_directory(projects_dir: &Path, filter: &ScanFilter) -> Result<Scan, SourceError> {
    std::fs::read_dir(projects_dir).map_err(|error| unreadable(projects_dir, &error))?;

    let mut stats = ScanStats::default();
    let mut latest_by_message: BTreeMap<String, UsageEvent> = BTreeMap::new();
    for entry in WalkDir::new(projects_dir).sort_by_file_name() {
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
        scan_file(entry.path(), &mut stats, &mut latest_by_message);
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

fn scan_file(
    path: &Path,
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
        buffer.clear();
        match reader.read_until(b'\n', &mut buffer) {
            Ok(0) => break,
            Ok(_) => {}
            // Lines already read from this file stay counted; the rest of it is lost.
            Err(_) => {
                stats.files_unreadable += 1;
                break;
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
                let Some(event) = build_event(record, path, line_number) else {
                    stats.lines_unparseable += 1;
                    continue;
                };
                stats.events_before_dedup += 1;
                keep_latest(latest_by_message, event);
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
fn keep_latest(latest_by_message: &mut BTreeMap<String, UsageEvent>, event: UsageEvent) {
    match latest_by_message.entry(event.message_id.clone()) {
        Entry::Vacant(slot) => {
            slot.insert(event);
        }
        Entry::Occupied(mut slot) => {
            if event.tokens.output >= slot.get().tokens.output {
                slot.insert(event);
            }
        }
    }
}

/// Far above any real request (the largest context window is 1M tokens), and low enough
/// that no single request can overflow u64 nanodollars at any price the loader accepts.
const MAX_TOKENS_PER_FIELD: u64 = 100_000_000;

/// None when the timestamp is missing or not RFC 3339, or a token count is above
/// `MAX_TOKENS_PER_FIELD`; the caller counts the line as unparseable.
fn build_event(record: &TranscriptLine, path: &Path, line_number: u64) -> Option<UsageEvent> {
    let timestamp: Timestamp = record.timestamp.as_deref()?.parse().ok()?;
    let tokens = record.usage().map(token_usage).unwrap_or_default();
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

    Some(UsageEvent {
        session_id,
        message_id,
        timestamp,
        model: ModelName::decode(message.and_then(|message| message.model.as_deref())),
        entrypoint: Entrypoint::decode(record.entrypoint.as_deref()),
        sidechain: record.is_sidechain.unwrap_or(false),
        project,
        tokens,
    })
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

fn token_usage(usage: &Usage) -> TokenUsage {
    let (cache_write_5m, cache_write_1h) = match &usage.cache_creation {
        Some(split) => (
            split.ephemeral_5m_input_tokens.unwrap_or(0),
            split.ephemeral_1h_input_tokens.unwrap_or(0),
        ),
        None => (usage.cache_creation_input_tokens.unwrap_or(0), 0),
    };
    TokenUsage {
        input: usage.input_tokens.unwrap_or(0),
        output: usage.output_tokens.unwrap_or(0),
        cache_read: usage.cache_read_input_tokens.unwrap_or(0),
        cache_write_5m,
        cache_write_1h,
    }
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

    #[test]
    fn cache_creation_split_wins_over_total() {
        let usage = Usage {
            input_tokens: Some(3),
            output_tokens: None,
            cache_read_input_tokens: Some(9),
            cache_creation_input_tokens: Some(500),
            cache_creation: Some(CacheCreation {
                ephemeral_5m_input_tokens: Some(200),
                ephemeral_1h_input_tokens: None,
            }),
        };
        let expected = TokenUsage {
            input: 3,
            output: 0,
            cache_read: 9,
            cache_write_5m: 200,
            cache_write_1h: 0,
        };
        assert_eq!(token_usage(&usage), expected);
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
        assert_eq!(token_usage(&usage), expected);
    }
}
