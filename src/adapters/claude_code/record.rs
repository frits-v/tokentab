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
    #[serde(rename = "requestId")]
    pub request_id: Option<String>,
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
        todo!("assistant + message.usage -> AssistantWithUsage; assistant without -> AssistantWithoutUsage; other known type string -> OtherType; missing type -> Unrecognised")
    }
}
