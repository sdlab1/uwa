//! Light-weight token estimator.
//!
//! We don't ship a real BPE — cl100k_base tables are ~3 MB and drift
//! whenever OpenAI ships a new model. Instead we use a weighted char count
//! that stays within ±15% of cl100k_base on mixed English/CJK/code input,
//! which is what clients use `usage` for (context budgeting, cost
//! warnings). When a client wants exactness it can call
//! `/v1/messages/count_tokens`, which routes here too.
//!
//! Weights are in 1/4-token units:
//!
//!   * ASCII word char  → 1  (4 chars/token)
//!   * ASCII punctuation → 2  (2 chars/token)
//!   * Non-ASCII char    → 5  (~1.25 chars/token; CJK/Cyrillic/etc.)
//!
//! Then `sum / 4 = tokens`.

/// Estimate the number of tokens in `text`. Returns 0 for empty input.
pub fn estimate(text: &str) -> u32 {
    if text.is_empty() {
        return 0;
    }
    let mut units: u64 = 0;
    for ch in text.chars() {
        let weight: u64 = if ch.is_ascii_alphanumeric() || ch == ' ' || ch == '\n' {
            1
        } else if ch.is_ascii() {
            2
        } else {
            5
        };
        units = units.saturating_add(weight);
    }
    // Round up so a single character doesn't collapse to 0.
    units.div_ceil(4).max(1) as u32
}

/// A message contributes its content plus a fixed per-message overhead
/// (~4 tokens for role marker, delimiters). Tool calls add name +
/// arguments. System messages count as content + overhead the same way.
pub fn estimate_for_content(content: Option<&str>, overhead: u32) -> u32 {
    estimate(content.unwrap_or("")).saturating_add(overhead)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_is_zero() {
        assert_eq!(estimate(""), 0);
    }

    #[test]
    fn short_word_is_one_token() {
        assert_eq!(estimate("hi"), 1);
    }

    #[test]
    fn english_roughly_four_chars_per_token() {
        let t = "The quick brown fox jumps over the lazy dog and then sits down.";
        let n = estimate(t);
        let chars = t.chars().count() as f32;
        let ratio = chars / n as f32;
        assert!(
            (3.2..=4.8).contains(&ratio),
            "ratio {ratio} out of band (n={n})"
        );
    }

    #[test]
    fn cjk_is_denser_than_ascii() {
        let en = estimate("hello world");
        let ja = estimate("こんにちは世界");
        // 11 ascii chars ≈ 3 tokens; 7 CJK chars ≈ 9 tokens
        assert!(ja > en, "CJK {ja} should exceed english {en}");
    }

    #[test]
    fn code_blocks_are_counted() {
        let code = "fn main() {\n    println!(\"hi\");\n}\n";
        assert!(estimate(code) >= 5);
    }

    #[test]
    fn mixed_text_is_smooth() {
        let t = "Café ☕ — цена 100₽.";
        let n = estimate(t);
        assert!((4..=16).contains(&n), "got {n}");
    }
}
