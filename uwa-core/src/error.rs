//! Unified error type for the whole workspace.
//!
//! `UwaError` is the library-level error; binaries may still use `anyhow`
//! at the very top of the call stack.

use thiserror::Error;

pub type Result<T, E = UwaError> = std::result::Result<T, E>;

/// Rich payload for a 404 model miss. Rendered into the OpenAI error
/// `message` field, and (later) surfaced in the dashboard so the user
/// sees the same suggestion the client sees.
#[derive(Debug, Clone)]
pub struct ModelNotFound {
    pub requested: String,
    /// Every model name the config actually serves, sorted.
    pub available: Vec<String>,
    /// Closest candidate by Levenshtein distance, if the list is non-empty.
    pub suggestion: Option<String>,
}

impl ModelNotFound {
    pub fn new(requested: impl Into<String>, available: Vec<String>) -> Self {
        let requested = requested.into();
        let suggestion = closest_match(&requested, &available);
        Self {
            requested,
            available,
            suggestion,
        }
    }

    pub fn message(&self) -> String {
        let mut s = format!("Unknown model `{}`.", self.requested);
        if !self.available.is_empty() {
            let mut sorted = self.available.clone();
            sorted.sort();
            let preview: Vec<&str> = sorted.iter().take(8).map(String::as_str).collect();
            s.push_str(" Available: ");
            s.push_str(&preview.join(", "));
            if sorted.len() > preview.len() {
                s.push_str(&format!(", … (+{} more)", sorted.len() - preview.len()));
            }
            s.push('.');
        }
        if let Some(sug) = &self.suggestion {
            s.push_str(&format!(" Did you mean `{sug}`?"));
        }
        s
    }
}

/// Levenshtein-based closest match with an early exit: if a candidate
/// shares the first three characters (case-insensitive) or the prefix up
/// to the first `-` / `_` / `.`, it wins outright. Otherwise we take the
/// minimum edit distance, but only if it looks meaningfully close
/// (distance ≤ 5 and ≤ half the requested length).
fn closest_match(requested: &str, available: &[String]) -> Option<String> {
    if available.is_empty() {
        return None;
    }
    let req_lower = requested.to_lowercase();
    let stem = req_lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .next()
        .unwrap_or("")
        .to_string();

    // Prefix / stem short-circuit.
    if stem.len() >= 3 {
        for cand in available {
            let c = cand.to_lowercase();
            if c.starts_with(&stem)
                || c.split(|ch: char| !ch.is_ascii_alphanumeric())
                    .next()
                    .map(|x| x == stem)
                    .unwrap_or(false)
            {
                return Some(cand.clone());
            }
        }
    }

    // Levenshtein fallback.
    let max_dist = 5usize.min((req_lower.chars().count() / 2).max(2));
    let mut best: Option<(usize, &String)> = None;
    for cand in available {
        let d = levenshtein(&req_lower, &cand.to_lowercase());
        if d > max_dist {
            continue;
        }
        match best {
            Some((bd, _)) if bd <= d => {}
            _ => best = Some((d, cand)),
        }
    }
    best.map(|(_, c)| c.clone())
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            cur[j] = (cur[j - 1] + 1).min(prev[j] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

#[derive(Debug, Clone, Error)]
pub enum UwaError {
    #[error("configuration error: {0}")]
    Config(String),

    #[error("bad request: {0}")]
    BadRequest(String),

    #[error("unauthorized")]
    Unauthorized,

    #[error("{}", .0.message())]
    UnknownModel(ModelNotFound),

    #[error("no provider matched url `{0}`")]
    NoProviderForUrl(String),

    #[error("browser transport error: {0}")]
    Transport(String),

    #[error("tab `{0}` not found")]
    TabNotFound(String),

    #[error("extraction failed: {0}")]
    Extraction(String),

    #[error("upstream timeout after {0:?}")]
    Timeout(std::time::Duration),

    #[error("upstream unavailable: {0}")]
    Unavailable(String),

    #[error("internal error: {0}")]
    Internal(String),
}

impl UwaError {
    /// Map to an OpenAI-compatible `(http_status, type, code)` triple.
    /// This is the single source of truth for error normalization at the HTTP edge.
    pub fn openai_shape(&self) -> (u16, &'static str, &'static str) {
        match self {
            UwaError::Config(_) => (500, "server_error", "config_error"),
            UwaError::BadRequest(_) => (400, "invalid_request_error", "bad_request"),
            UwaError::Unauthorized => (401, "invalid_request_error", "unauthorized"),
            UwaError::UnknownModel(_) => (404, "invalid_request_error", "model_not_found"),
            UwaError::NoProviderForUrl(_) => (400, "invalid_request_error", "no_provider"),
            UwaError::Transport(_) => (502, "server_error", "transport_error"),
            UwaError::TabNotFound(_) => (503, "server_error", "no_tab"),
            UwaError::Extraction(_) => (502, "server_error", "extraction_error"),
            UwaError::Timeout(_) => (504, "server_error", "timeout"),
            UwaError::Unavailable(_) => (503, "server_error", "unavailable"),
            UwaError::Internal(_) => (500, "server_error", "internal_error"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_to_openai_shape() {
        assert_eq!(UwaError::Unauthorized.openai_shape().0, 401);
        assert_eq!(
            UwaError::UnknownModel(ModelNotFound::new("x", vec![]))
                .openai_shape()
                .2,
            "model_not_found"
        );
        assert_eq!(
            UwaError::Timeout(std::time::Duration::from_secs(1))
                .openai_shape()
                .0,
            504
        );
    }

    #[test]
    fn unknown_model_message_is_informative() {
        let e = UwaError::UnknownModel(ModelNotFound::new("gpt-4o", vec![]));
        assert!(e.to_string().contains("gpt-4o"));
    }

    #[test]
    fn model_not_found_suggests_prefix_match() {
        let e = UwaError::UnknownModel(ModelNotFound::new(
            "deepseek-flash",
            vec![
                "deepseek-chat".into(),
                "deepseek-reasoner".into(),
                "gpt-4o".into(),
            ],
        ));
        let m = e.to_string();
        assert!(m.contains("deepseek-flash"));
        assert!(m.contains("deepseek-chat"));
        assert!(m.contains("Did you mean `deepseek-chat`"));
        assert_eq!(e.openai_shape().0, 404);
    }

    #[test]
    fn model_not_found_without_candidates_omits_suggestion() {
        let e = UwaError::UnknownModel(ModelNotFound::new("nope", vec![]));
        let m = e.to_string();
        assert!(m.contains("Unknown model"));
        assert!(!m.contains("Did you mean"));
    }
}
