use std::collections::BTreeMap;

use jiff::Timestamp;
use serde::Serialize;

/// Decoded model identifier. Every transcript value maps to exactly one state;
/// pricing decides later whether a `Named` model has a price.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(tag = "kind", content = "name", rename_all = "snake_case")]
pub enum ModelName {
    /// Canonical id with any trailing `-YYYYMMDD` snapshot suffix removed.
    Named(String),
    /// Claude Code writes `<synthetic>` for locally generated assistant turns; nothing was billed.
    Synthetic,
    Missing,
}

impl ModelName {
    pub fn decode(raw: Option<&str>) -> ModelName {
        match raw.map(str::trim) {
            None | Some("") => ModelName::Missing,
            Some("<synthetic>") => ModelName::Synthetic,
            Some(name) => ModelName::Named(strip_snapshot_suffix(name).to_string()),
        }
    }
}

fn strip_snapshot_suffix(name: &str) -> &str {
    match name.rsplit_once('-') {
        Some((stem, suffix)) if suffix.len() == 8 && suffix.bytes().all(|b| b.is_ascii_digit()) => {
            stem
        }
        _ => name,
    }
}

/// How the request reached the API. Only `Sdk` usage is eligible for the API
/// credits bundled with Max and Team plans; interactive Claude Code is not.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(tag = "kind", content = "raw", rename_all = "snake_case")]
pub enum Entrypoint {
    Interactive,
    Sdk,
    Desktop,
    Missing,
    Unrecognised(String),
}

impl Entrypoint {
    pub fn decode(raw: Option<&str>) -> Entrypoint {
        match raw.map(str::trim) {
            None | Some("") => Entrypoint::Missing,
            Some("cli") => Entrypoint::Interactive,
            Some("sdk-cli") | Some("sdk-py") | Some("sdk-ts") => Entrypoint::Sdk,
            Some("claude-desktop") => Entrypoint::Desktop,
            Some(other) => Entrypoint::Unrecognised(other.to_string()),
        }
    }

    pub fn credits_eligible(&self) -> bool {
        matches!(self, Entrypoint::Sdk)
    }

    pub fn label(&self) -> &str {
        match self {
            Entrypoint::Interactive => "interactive",
            Entrypoint::Sdk => "sdk",
            Entrypoint::Desktop => "desktop",
            Entrypoint::Missing => "missing",
            Entrypoint::Unrecognised(raw) => raw,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct TokenUsage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write_5m: u64,
    pub cache_write_1h: u64,
}

impl TokenUsage {
    /// Prompt length as the pricing page defines it: every input token, cached or not.
    pub fn prompt_tokens(&self) -> u64 {
        self.input + self.cache_read + self.cache_write()
    }

    pub fn cache_write(&self) -> u64 {
        self.cache_write_5m + self.cache_write_1h
    }

    pub fn is_empty(&self) -> bool {
        *self == TokenUsage::default()
    }

    pub fn add(&self, other: &TokenUsage) -> TokenUsage {
        TokenUsage {
            input: self.input + other.input,
            output: self.output + other.output,
            cache_read: self.cache_read + other.cache_read,
            cache_write_5m: self.cache_write_5m + other.cache_write_5m,
            cache_write_1h: self.cache_write_1h + other.cache_write_1h,
        }
    }
}

/// One billed request, whatever source it came from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct UsageEvent {
    pub session_id: String,
    pub message_id: String,
    pub timestamp: Timestamp,
    pub model: ModelName,
    pub entrypoint: Entrypoint,
    pub sidechain: bool,
    pub project: String,
    pub tokens: TokenUsage,
}

/// Finest time resolution the source offers. Windowed peaks need `Message`;
/// a daily analytics export can only say "unavailable", never pretend.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Granularity {
    Message,
    Day,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ScanStats {
    pub files_seen: u64,
    pub files_unreadable: u64,
    pub lines_read: u64,
    pub lines_unparseable: u64,
    pub assistant_lines: u64,
    pub assistant_lines_without_usage: u64,
    pub events_before_dedup: u64,
    pub duplicates_collapsed: u64,
    pub events_filtered_out: u64,
    pub events: u64,
    /// Other record types seen, by `type` value, so a changed format shows up as a count rather than silence.
    pub other_line_types: BTreeMap<String, u64>,
}

#[derive(Clone, Debug, Default)]
pub struct ScanFilter {
    pub since: Option<Timestamp>,
    pub until: Option<Timestamp>,
    pub project_substring: Option<String>,
}

impl ScanFilter {
    pub fn accepts(&self, event: &UsageEvent) -> bool {
        if let Some(since) = self.since
            && event.timestamp < since
        {
            return false;
        }
        if let Some(until) = self.until
            && event.timestamp >= until
        {
            return false;
        }
        if let Some(needle) = &self.project_substring
            && !event.project.contains(needle.as_str())
        {
            return false;
        }
        true
    }
}

#[derive(Clone, Debug)]
pub struct Scan {
    pub granularity: Granularity,
    pub events: Vec<UsageEvent>,
    pub stats: ScanStats,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_decoder_covers_every_state() {
        assert_eq!(ModelName::decode(None), ModelName::Missing);
        assert_eq!(ModelName::decode(Some("")), ModelName::Missing);
        assert_eq!(ModelName::decode(Some("<synthetic>")), ModelName::Synthetic);
        assert_eq!(
            ModelName::decode(Some("claude-opus-5-5")),
            ModelName::Named("claude-opus-5-5".into())
        );
        assert_eq!(
            ModelName::decode(Some("claude-haiku-4-5-20251001")),
            ModelName::Named("claude-haiku-4-5".into())
        );
        assert_eq!(
            ModelName::decode(Some("claude-opus-4-5-20251101")),
            ModelName::Named("claude-opus-4-5".into())
        );
    }

    #[test]
    fn snapshot_suffix_needs_exactly_eight_digits() {
        assert_eq!(
            strip_snapshot_suffix("claude-sonnet-5-5"),
            "claude-sonnet-5-5"
        );
        assert_eq!(strip_snapshot_suffix("model-1234567"), "model-1234567");
        assert_eq!(strip_snapshot_suffix("model-123456789"), "model-123456789");
    }

    #[test]
    fn entrypoint_decoder_covers_every_state() {
        assert_eq!(Entrypoint::decode(Some("cli")), Entrypoint::Interactive);
        assert_eq!(Entrypoint::decode(Some("sdk-cli")), Entrypoint::Sdk);
        assert_eq!(Entrypoint::decode(Some("sdk-py")), Entrypoint::Sdk);
        assert_eq!(
            Entrypoint::decode(Some("claude-desktop")),
            Entrypoint::Desktop
        );
        assert_eq!(Entrypoint::decode(None), Entrypoint::Missing);
        assert_eq!(
            Entrypoint::decode(Some("vscode-2030")),
            Entrypoint::Unrecognised("vscode-2030".into())
        );
        assert!(Entrypoint::Sdk.credits_eligible());
        assert!(!Entrypoint::Unrecognised("x".into()).credits_eligible());
    }

    #[test]
    fn prompt_tokens_count_every_input_token() {
        let tokens = TokenUsage {
            input: 2,
            output: 9,
            cache_read: 100,
            cache_write_5m: 10,
            cache_write_1h: 20,
        };
        assert_eq!(tokens.prompt_tokens(), 132);
        assert_eq!(tokens.cache_write(), 30);
    }
}
