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
            // `preview` is a prefix of `sorted`, so the subtraction cannot
            // underflow — but `arithmetic_side_effects` is forbid-level and
            // cannot see that, hence saturating_sub.
            let rest = sorted.len().saturating_sub(preview.len());
            if rest > 0 {
                s.push_str(&format!(", … (+{rest} more)"));
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

/// Iterative Levenshtein DP carried in two row vectors.
///
/// Written **index-free** (iterator pairs + `Vec::last`, `saturating_add`
/// instead of raw arithmetic) to satisfy the workspace's forbid-level
/// clippy lints (`indexing_slicing`, `arithmetic_side_effects`): the DP
/// indices are provably in range, but the lints cannot prove it.
///
/// Invariant: before row `i` is computed, `prev` holds
/// `prev[j] = distance(a[..i], b[..j])`. Inside a row, `diag` trails the
/// `prev` iterator by one cell, playing `prev[j-1]` against `above = prev[j]`.
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
    for (i, ca) in a.iter().enumerate() {
        let mut cur: Vec<usize> = Vec::with_capacity(b.len().saturating_add(1));
        // Row header: distance between a[..=i] and the empty `b`.
        cur.push(i.saturating_add(1));
        let mut prev_items = prev.iter().copied();
        let mut diag = prev_items.next().unwrap_or(0);
        for cb in b.iter() {
            let above = prev_items.next().unwrap_or(0); // prev[j]
            let left = cur.last().copied().unwrap_or(0); // cur[j-1]
            let cost = usize::from(ca != cb);
            cur.push(
                above
                    .saturating_add(1)
                    .min(left.saturating_add(1))
                    .min(diag.saturating_add(cost)),
            );
            diag = above;
        }
        prev = cur;
    }
    prev.last().copied().unwrap_or(0)
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

    #[error("not supported: {0}")]
    Unsupported(String),

    #[error("upstream unavailable: {0}")]
    Unavailable(String),

    /// A human must complete a browser challenge before the upstream can be used.
    #[error("human verification required for provider `{provider}` in tab `{tab_id}` at `{url}`; complete the challenge in the logged-in browser and retry the request manually")]
    HumanVerificationRequired {
        provider: String,
        tab_id: String,
        url: String,
    },

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
            UwaError::HumanVerificationRequired { .. } => {
                (409, "server_error", "human_verification_required")
            }
            UwaError::Internal(_) => (500, "server_error", "internal_error"),
            UwaError::Unsupported(_) => (501, "server_error", "not_supported"),
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
        assert_eq!(
            UwaError::HumanVerificationRequired {
                provider: "deepseek".into(),
                tab_id: "tab_1".into(),
                url: "https://challenges.cloudflare.com/".into(),
            }
            .openai_shape(),
            (409, "server_error", "human_verification_required")
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

    // ---- index-free levenshtein / closest_match rewrite ----

    #[test]
    fn levenshtein_matches_reference_values() {
        // Locks the iterator-based rewrite against the classic textbook
        // distances, including the empty-string edges and non-ASCII input
        // that previously went through byte indexing.
        assert_eq!(levenshtein("", ""), 0);
        assert_eq!(levenshtein("abc", ""), 3);
        assert_eq!(levenshtein("", "abc"), 3);
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("flaw", "lawn"), 2);
        assert_eq!(levenshtein("gumbo", "gambol"), 2);
        // "gpt-4" is a prefix of "gpt-4o" — one deletion.
        assert_eq!(levenshtein("gpt-4o", "gpt-4"), 1);
        assert_eq!(levenshtein("изменение", "изменения"), 1);
        // Symmetry on a non-trivial pair.
        assert_eq!(
            levenshtein("claude", "gpt-4o"),
            levenshtein("gpt-4o", "claude")
        );
    }

    #[test]
    fn closest_match_suggestions_survive_the_rewrite() {
        // Prefix short-circuit still wins outright.
        let m = ModelNotFound::new("gpt-4-turbo", vec!["gpt-4o".into(), "claude".into()]);
        assert_eq!(m.suggestion.as_deref(), Some("gpt-4o"));

        // Levenshtein fallback still fires within the distance budget
        // ("cluade" -> "claude" is 2 edits; budget for 6 chars is 3).
        let m = ModelNotFound::new("cluade", vec!["claude".into(), "gpt-4o".into()]);
        assert_eq!(m.suggestion.as_deref(), Some("claude"));

        // Far-off requests still get no suggestion.
        let m = ModelNotFound::new("zzzzzzzz", vec!["claude".into(), "gpt-4o".into()]);
        assert_eq!(m.suggestion, None);
    }

    #[test]
    fn model_preview_overflow_note_is_correct() {
        // >8 candidates: the "(+N more)" tail must count the remainder.
        let available: Vec<String> = (0..12).map(|i| format!("model-{i:02}")).collect();
        let m = ModelNotFound::new("model-00", available);
        let msg = m.message();
        assert!(msg.contains("(+4 more)"), "message was: {msg}");
    }
}
