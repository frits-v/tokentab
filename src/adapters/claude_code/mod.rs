//! Reads Claude Code's local transcripts: `~/.claude/projects/<project>/*.jsonl`,
//! one JSON record per line. Assistant records carry the API usage for one message;
//! a streamed message is written once per content block with the same `message.id`
//! and a growing `output_tokens`, so the last line per message id is the billed one.

mod record;
mod scan;

use std::path::PathBuf;

use crate::domain::usage::{Scan, ScanFilter};
use crate::ports::usage_source::{SourceError, UsageSource};

pub struct ClaudeCodeTranscripts {
    pub projects_dir: PathBuf,
}

impl ClaudeCodeTranscripts {
    pub fn new(projects_dir: PathBuf) -> ClaudeCodeTranscripts {
        ClaudeCodeTranscripts { projects_dir }
    }

    pub fn default_dir() -> Option<PathBuf> {
        std::env::home_dir().map(|home| home.join(".claude").join("projects"))
    }
}

impl UsageSource for ClaudeCodeTranscripts {
    fn name(&self) -> String {
        format!("claude-code transcripts at {}", self.projects_dir.display())
    }

    fn scan(&self, filter: &ScanFilter) -> Result<Scan, SourceError> {
        scan::scan_directory(&self.projects_dir, filter)
    }
}
