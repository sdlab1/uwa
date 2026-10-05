//! Property-based tests for the tool-call parser.
//!
//! These cover the four guarantees from `parser.rs`'s doc-comment:
//! 1. never panics on arbitrary input;
//! 2. a well-formed call is recognized and stripped (round-trip);
//! 3. tool names are validated against the caller's allow-list;
//! 4. only consumed ranges disappear from the visible text.

use proptest::prelude::*;
use serde_json::json;
use uwa_tools::{has_tool_marker, parse, tool_marker_index};

/// `PROPTEST_CASES` wins over the per-property default, so the nightly run
/// (`PROPTEST_CASES=10000 cargo test --test ...`) actually does more work.
fn cases(default: u32) -> ProptestConfig {
    let cases = std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default);
    ProptestConfig {
        cases,
        ..ProptestConfig::default()
    }
}

const OPEN: &str = "<tool_call>";
const CLOSE: &str = "</tool_call>";

fn known() -> Vec<String> {
    vec!["echo".into(), "get_weather".into()]
}

/// Text that cannot contain a tool-call marker.
fn plain_text() -> impl Strategy<Value = String> {
    proptest::string::string_regex("[a-zA-Z0-9 ,.!?]{0,200}").unwrap()
}

/// A value that survives the JSON round-trip inside a marker body.
fn json_scalar() -> impl Strategy<Value = serde_json::Value> {
    prop_oneof![
        proptest::string::string_regex("[a-zA-Z0-9]{0,30}")
            .unwrap()
            .prop_map(serde_json::Value::String),
        any::<i64>().prop_map(|n| json!(n)),
        any::<bool>().prop_map(|b| json!(b)),
    ]
}

// ---------- guarantee 1: never panics ----------

proptest! {
    #![proptest_config(cases(512))]

    #[test]
    fn parse_never_panics_on_arbitrary_text(s in "(?s).{0,1500}") {
        let _ = parse(&s, &known());
        let _ = parse(&s, &[]);
        let _ = has_tool_marker(&s);
        let _ = tool_marker_index(&s);
    }

    /// An unterminated, empty or oversized body must not panic either.
    #[test]
    fn parse_never_panics_on_near_xml(body in "(?s).{0,200}") {
        let text = format!("{}{}{}", OPEN, body, CLOSE);
        let _ = parse(&text, &known());
        let _ = parse(&text, &[]);
        let dangling = format!("{}{}", OPEN, body);
        let _ = parse(&dangling, &known());
    }
}

// ---------- guarantee 2: round-trip ----------

proptest! {
    #![proptest_config(cases(256))]

    /// A well-formed XML block must be recognized and stripped.
    #[test]
    fn xml_block_round_trips(
        name in prop::sample::select(vec!["echo", "get_weather"]),
        key in "[a-zA-Z]{1,10}",
        val in json_scalar(),
    ) {
        let body = json!({"name": name, "arguments": { key.clone(): val.clone() }});
        let text = format!("prefix\n{}{}{}\nsuffix", OPEN, body, CLOSE);

        let out = parse(&text, &known());

        prop_assert_eq!(out.calls.len(), 1);
        prop_assert_eq!(out.calls[0].name.as_str(), name);
        prop_assert_eq!(out.calls[0].arguments.get(&key), Some(&val));
        prop_assert!(!out.text.contains(OPEN), "marker left: {:?}", out.text);
        prop_assert!(out.text.contains("prefix"), "{:?}", out.text);
        prop_assert!(out.text.contains("suffix"), "{:?}", out.text);
    }

    /// The same block inside a ```tool_call fence.
    #[test]
    fn fenced_block_round_trips(
        key in "[a-zA-Z]{1,10}",
        val in json_scalar(),
    ) {
        let body = json!({"name": "echo", "arguments": { key.clone(): val.clone() }});
        let text = format!("```tool_call\n{}\n```", body);

        let out = parse(&text, &known());

        prop_assert_eq!(out.calls.len(), 1);
        prop_assert_eq!(out.calls[0].arguments.get(&key), Some(&val));
        prop_assert!(!out.text.contains("```tool_call"), "{:?}", out.text);
    }

    /// N blocks in one message produce N calls and leave no visible text.
    #[test]
    fn n_xml_blocks_produce_n_calls(n in 1usize..6) {
        let mut text = String::new();
        for i in 0..n {
            text.push_str(&format!(
                "{}{}{}\n",
                OPEN,
                json!({"name": "echo", "arguments": {"i": i}}),
                CLOSE
            ));
        }

        let out = parse(&text, &known());

        prop_assert_eq!(out.calls.len(), n);
        prop_assert!(out.text.trim().is_empty(), "text leftover: {:?}", out.text);
    }
}

// ---------- guarantee 3: allow-list ----------

proptest! {
    #![proptest_config(cases(256))]

    /// A name outside the allow-list is left in the text, not consumed.
    #[test]
    fn unknown_tool_name_is_not_consumed(name in "[a-z]{4,20}") {
        prop_assume!(!known().iter().any(|k| k.as_str() == name));

        let body = json!({"name": name, "arguments": {}});
        let text = format!("{}{}{}", OPEN, body, CLOSE);

        let out = parse(&text, &known());

        prop_assert!(out.calls.is_empty());
        prop_assert!(out.text.contains(&name), "block was eaten: {:?}", out.text);
    }

    /// With an empty allow-list nothing is ever a tool call.
    #[test]
    fn empty_allow_list_consumes_nothing(body in "(?s).{0,200}") {
        let text = format!("{}{}{}", OPEN, body, CLOSE);
        let out = parse(&text, &[]);
        prop_assert!(out.calls.is_empty());
    }
}

// ---------- guarantee 4: only consumed ranges disappear ----------

proptest! {
    #![proptest_config(cases(128))]

    /// Text around a block survives; the block itself yields one call.
    #[test]
    fn adjacent_markers_no_duplicates(
        prefix in plain_text(),
        suffix in plain_text(),
    ) {
        let text = format!(
            "{p}{o}{b}{c}{s}",
            p = prefix,
            o = OPEN,
            b = json!({"name": "echo", "arguments": {}}),
            c = CLOSE,
            s = suffix
        );

        let out = parse(&text, &known());

        prop_assert_eq!(out.calls.len(), 1);
        prop_assert!(out.text.contains(prefix.trim()), "{:?}", out.text);
    }

    /// Two blocks side by side are two calls, not one merged or duplicated.
    #[test]
    fn two_blocks_yield_two_calls(mid in plain_text()) {
        let one = json!({"name": "echo", "arguments": {}});
        let text = format!("{o}{b}{c}{m}{o}{b}{c}", o = OPEN, b = one, c = CLOSE, m = mid);

        let out = parse(&text, &known());

        prop_assert_eq!(out.calls.len(), 2, "{:?}", out.text);
    }

    /// Text with no markers comes back verbatim.
    #[test]
    fn pure_text_is_untouched(s in plain_text()) {
        let out = parse(&s, &known());

        prop_assert!(out.calls.is_empty());
        prop_assert_eq!(out.text, s.trim());
    }
}
