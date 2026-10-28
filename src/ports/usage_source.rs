use thiserror::Error;

use crate::domain::usage::{Scan, ScanFilter};

#[derive(Debug, Error)]
pub enum SourceError {
    #[error("{path}: {reason}")]
    Unreadable { path: String, reason: String },
    #[error("{0}")]
    Other(String),
}

/// Inbound port. A source turns whatever it reads (local transcripts today, an
/// analytics API export later) into `UsageEvent`s plus the counts of what it looked at.
pub trait UsageSource {
    fn name(&self) -> String;
    fn scan(&self, filter: &ScanFilter) -> Result<Scan, SourceError>;
}
