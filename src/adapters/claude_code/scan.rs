use std::path::Path;

use crate::domain::usage::{Scan, ScanFilter};
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
    todo!("see the doc comment")
}
