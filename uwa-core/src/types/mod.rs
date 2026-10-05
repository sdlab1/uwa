pub mod anthropic;
pub mod openai;

/// A message role used by both OpenAI and Anthropic adapters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// Finish reason normalized across providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    Length,
    ToolCalls,
    ContentFilter,
}

/// Characters per pseudo-streaming delta, shared by the OpenAI and Anthropic
/// streams.
pub const STREAM_CHUNK_CHARS: usize = 24;

/// Split `s` into pseudo-streaming chunks of at most [`STREAM_CHUNK_CHARS`].
pub fn stream_chunks(s: &str) -> Vec<String> {
    s.chars()
        .collect::<Vec<_>>()
        .chunks(STREAM_CHUNK_CHARS)
        .map(|c| c.iter().collect())
        .collect()
}
