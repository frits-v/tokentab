//! Wire shape of one transcript line. Only the fields the pricing needs; everything
//! else is ignored by serde. Every field is optional because records of other types
//! share the file and the format has no schema.

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct TranscriptLine {
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub uuid: Option<String>,
    pub timestamp: Option<String>,
    #[serde(rename = "sessionId")]
    pub session_id: Option<String>,
    pub cwd: Option<String>,
    pub entrypoint: Option<String>,
    #[serde(rename = "isSidechain")]
    pub is_sidechain: Option<bool>,
    pub message: Option<Message>,
}

#[derive(Debug, Deserialize)]
pub struct Message {
    pub id: Option<String>,
    pub model: Option<String>,
    pub usage: Option<Usage>,
}

#[derive(Debug, Deserialize)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cache_read_input_tokens: Option<u64>,
    pub cache_creation_input_tokens: Option<u64>,
    pub cache_creation: Option<CacheCreation>,
}

/// Present on recent transcripts. Older lines only have the total in
/// `cache_creation_input_tokens`; those count as 5-minute writes, the cheaper band.
#[derive(Debug, Deserialize)]
pub struct CacheCreation {
    pub ephemeral_5m_input_tokens: Option<u64>,
    pub ephemeral_1h_input_tokens: Option<u64>,
}

/// Every line lands in exactly one state. `Unrecognised` is counted and never priced.
#[derive(Debug)]
pub enum LineKind<'a> {
    AssistantWithUsage(&'a TranscriptLine),
    AssistantWithoutUsage,
    OtherType(&'a str),
    Unrecognised,
}

impl TranscriptLine {
    pub fn classify(&self) -> LineKind<'_> {
        match self.kind.as_deref() {
            Some("assistant") if self.usage().is_some() => LineKind::AssistantWithUsage(self),
            Some("assistant") => LineKind::AssistantWithoutUsage,
            Some(other) if !other.is_empty() => LineKind::OtherType(other),
            Some(_) | None => LineKind::Unrecognised,
        }
    }

    pub fn usage(&self) -> Option<&Usage> {
        self.message
            .as_ref()
            .and_then(|message| message.usage.as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> Result<TranscriptLine, serde_json::Error> {
        serde_json::from_str(json)
    }

    #[test]
    fn assistant_with_usage_is_priced() -> Result<(), serde_json::Error> {
        let line =
            parse(r#"{"type":"assistant","message":{"id":"msg_1","usage":{"input_tokens":2}}}"#)?;
        assert!(matches!(line.classify(), LineKind::AssistantWithUsage(_)));
        Ok(())
    }

    #[test]
    fn assistant_without_usage_is_counted_apart() -> Result<(), serde_json::Error> {
        let api_error = parse(
            r#"{"type":"assistant","isApiErrorMessage":true,"message":{"model":"claude-opus-5-5"}}"#,
        )?;
        assert!(matches!(
            api_error.classify(),
            LineKind::AssistantWithoutUsage
        ));
        let no_message = parse(r#"{"type":"assistant"}"#)?;
        assert!(matches!(
            no_message.classify(),
            LineKind::AssistantWithoutUsage
        ));
        Ok(())
    }

    #[test]
    fn other_type_keeps_its_name() -> Result<(), serde_json::Error> {
        let line = parse(r#"{"type":"user","message":{"usage":{"input_tokens":5}}}"#)?;
        assert!(matches!(line.classify(), LineKind::OtherType("user")));
        Ok(())
    }

    #[test]
    fn missing_or_empty_type_is_unrecognised() -> Result<(), serde_json::Error> {
        assert!(matches!(
            parse(r#"{"uuid":"x"}"#)?.classify(),
            LineKind::Unrecognised
        ));
        assert!(matches!(
            parse(r#"{"type":""}"#)?.classify(),
            LineKind::Unrecognised
        ));
        Ok(())
    }
}
