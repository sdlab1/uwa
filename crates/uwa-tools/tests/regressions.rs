//! Regression tests for tool-call parsing and the streaming emit gate.
//!
//! Each test pins a bug found in the bugfix session:
//! * backticks / closing markers inside tool-call arguments truncated the
//!   block and the call was silently dropped;
//! * ```jsonc / ```json5 fences were treated as ```json;
//! * the streaming gate only knew `<tool_call>` / ```tool_call, so calls in
//!   a ```json fence or on a bare JSON line leaked to the client as text
//!   AND were reported again as `tool_calls`.

use proptest::prelude::*;
use serde_json::{json, Value};
use uwa_tools::{parse, safe_emit_end_for};

fn known() -> Vec<String> {
    vec!["write_file".into(), "echo".into()]
}

/// The streaming gate under test (kept in one place so the red/green switch
/// is a one-line change).
fn gate(buf: &str, known: &[String]) -> usize {
    safe_emit_end_for(buf, known)
}

/// Mirror of the pipeline's streaming loop: feed `chunks`, return the text a
/// client would have seen as deltas plus the final parse.
fn simulate_stream(chunks: &[&str], known: &[String]) -> (String, usize) {
    let mut acc = String::new();
    let mut emitted = 0usize;
    let mut visible = String::new();
    for c in chunks {
        acc.push_str(c);
        let end = gate(&acc, known);
        if end > emitted {
            visible.push_str(&acc[emitted..end]);
            emitted = end;
        }
    }
    let parsed = parse(&acc, known);
    if !parsed.has_calls() && emitted < acc.len() {
        visible.push_str(&acc[emitted..]);
    }
    (visible, parsed.calls.len())
}

fn call_json(name: &str, args: Value) -> String {
    json!({ "name": name, "arguments": args }).to_string()
}

// ---------- bug 2: markers inside arguments ----------

#[test]
fn tool_call_fence_with_backticks_in_arguments() {
    let args = json!({"path": "a.md", "content": "# T\n```rust\nfn x() {}\n```\n"});
    let text = format!(
        "```tool_call\n{}\n```",
        call_json("write_file", args.clone())
    );
    let out = parse(&text, &known());
    assert_eq!(out.calls.len(), 1, "call dropped: {out:?}");
    assert_eq!(out.calls.first().expect("first call").arguments, args);
    assert!(out.text.is_empty(), "leftover text: {:?}", out.text);
}

#[test]
fn json_fence_with_backticks_in_arguments() {
    let args = json!({"path": "a.md", "content": "```sh\nls\n```"});
    let text = format!(
        "Writing.\n```json\n{}\n```\nDone.",
        call_json("write_file", args.clone())
    );
    let out = parse(&text, &known());
    assert_eq!(out.calls.len(), 1, "call dropped: {out:?}");
    assert_eq!(out.calls.first().expect("first call").arguments, args);
    assert!(out.text.contains("Writing.") && out.text.contains("Done."));
    assert!(!out.text.contains("```"), "fence leftovers: {:?}", out.text);
}

#[test]
fn xml_block_with_closing_tag_in_arguments() {
    let args = json!({"path": "doc.txt", "content": "use </tool_call> to end a call"});
    let text = format!(
        "<tool_call>{}</tool_call>",
        call_json("write_file", args.clone())
    );
    let out = parse(&text, &known());
    assert_eq!(out.calls.len(), 1, "call dropped: {out:?}");
    assert_eq!(out.calls.first().expect("first call").arguments, args);
}

#[test]
fn two_fenced_calls_are_not_merged_by_candidate_search() {
    let a = call_json("echo", json!({"x": 1}));
    let b = call_json("echo", json!({"x": 2}));
    let text = format!("```tool_call\n{a}\n```\n```tool_call\n{b}\n```");
    let out = parse(&text, &known());
    assert_eq!(out.calls.len(), 2);
    assert_eq!(
        out.calls.first().expect("first call").arguments,
        json!({"x": 1})
    );
    assert_eq!(
        out.calls.get(1).expect("second call").arguments,
        json!({"x": 2})
    );
}

#[test]
fn unparseable_fence_does_not_swallow_following_valid_call() {
    let b = call_json("echo", json!({"x": 2}));
    let text = format!("```tool_call\nnot json\n```\n```tool_call\n{b}\n```");
    let out = parse(&text, &known());
    assert_eq!(out.calls.len(), 1);
    assert_eq!(
        out.calls.first().expect("first call").arguments,
        json!({"x": 2})
    );
}

#[test]
fn jsonc_and_json5_fences_are_not_json_fences() {
    let c = call_json("echo", json!({"x": 1}));
    for tag in ["jsonc", "json5", "jsonl"] {
        let text = format!("```{tag}\n{c}\n```");
        let out = parse(&text, &known());
        assert_eq!(out.calls.len(), 0, "```{tag} must not be parsed as a call");
        assert_eq!(out.text, text.trim());
    }
}

// ---------- bug 1: streaming gate leaks non-xml formats ----------

#[test]
fn stream_json_fence_call_is_not_leaked_as_text() {
    let c = call_json("echo", json!({"msg": "hi"}));
    let full = format!("Sure.\n```json\n{c}\n```");
    let chunks: Vec<&str> = split_every(&full, 7);
    let (visible, ncalls) = simulate_stream(&chunks, &known());
    assert_eq!(ncalls, 1);
    assert!(!visible.contains("\"name\""), "leaked: {visible:?}");
    assert!(!visible.contains("```json"), "leaked: {visible:?}");
    assert!(visible.starts_with("Sure."));
}

#[test]
fn stream_bare_json_call_is_not_leaked_as_text() {
    let c = call_json("echo", json!({"msg": "hi"}));
    let full = format!("Sure.\n{c}\n");
    let chunks: Vec<&str> = split_every(&full, 5);
    let (visible, ncalls) = simulate_stream(&chunks, &known());
    assert_eq!(ncalls, 1);
    assert!(!visible.contains("\"name\""), "leaked: {visible:?}");
}

#[test]
fn stream_ordinary_json_code_block_is_still_delivered_in_full() {
    let full = "Config:\n```json\n{\"a\": 1}\n```\nBye.";
    let chunks: Vec<&str> = split_every(full, 4);
    let (visible, ncalls) = simulate_stream(&chunks, &known());
    assert_eq!(ncalls, 0);
    assert_eq!(visible, full);
}

#[test]
fn stream_plain_text_with_braces_is_delivered_in_full() {
    let full = "{not a call}\nand {\"name\": \"unknown_tool\", \"arguments\": {}}\nend";
    let chunks: Vec<&str> = split_every(full, 3);
    let (visible, ncalls) = simulate_stream(&chunks, &known());
    assert_eq!(ncalls, 0);
    assert_eq!(visible, full);
}

fn split_every(s: &str, n: usize) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut count = 0;
    for (i, _) in s.char_indices() {
        if count == n {
            out.push(&s[start..i]);
            start = i;
            count = 0;
        }
        count = count.saturating_add(1);
    }
    out.push(&s[start..]);
    out
}

// ---------- property: no loss, no duplication ----------

#[derive(Debug, Clone, Copy)]
enum Style {
    Xml,
    ToolFence,
    JsonFence,
    Bare,
}

fn render(style: Style, body: &str) -> String {
    match style {
        Style::Xml => format!("<tool_call>{body}</tool_call>"),
        Style::ToolFence => format!("```tool_call\n{body}\n```"),
        Style::JsonFence => format!("```json\n{body}\n```"),
        Style::Bare => format!("\n{body}\n"),
    }
}

fn style() -> impl Strategy<Value = Style> {
    prop_oneof![
        Just(Style::Xml),
        Just(Style::ToolFence),
        Just(Style::JsonFence),
        Just(Style::Bare),
    ]
}

/// Argument strings that deliberately contain the protocol's own markers.
fn nasty_string() -> impl Strategy<Value = String> {
    proptest::collection::vec(
        prop_oneof![
            Just("```".to_string()),
            Just("```json\n".to_string()),
            Just("</tool_call>".to_string()),
            Just("<tool_call>".to_string()),
            Just("\n".to_string()),
            Just("{".to_string()),
            Just("}".to_string()),
            proptest::string::string_regex("[a-z ]{0,6}").expect("regex"),
        ],
        0..8,
    )
    .prop_map(|v| v.concat())
}

fn cfg() -> ProptestConfig {
    let cases = std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(500);
    ProptestConfig {
        cases,
        ..ProptestConfig::default()
    }
}

proptest! {
    #![proptest_config(cfg())]

    /// Every well-formed call survives exactly once, whatever the framing
    /// and whatever marker-like junk sits in its arguments.
    #[test]
    fn calls_are_neither_lost_nor_duplicated(
        items in proptest::collection::vec((style(), nasty_string()), 1..4),
        lead in "[a-zA-Z ]{0,20}",
    ) {
        let mut text = lead.clone();
        let mut expected = Vec::new();
        for (st, s) in &items {
            let args = json!({ "content": s });
            expected.push(args.clone());
            // Always separate blocks with a blank line.
            text.push_str("\n\n");
            text.push_str(&render(*st, &call_json("write_file", args)));
        }
        let out = parse(&text, &known());
        let got: Vec<Value> = out.calls.iter().map(|c| c.arguments.clone()).collect();
        prop_assert_eq!(got, expected);
        let marker_free = !out.text.contains("\"name\":\"write_file\"");
        prop_assert!(marker_free, "call JSON left in visible text: {:?}", out.text);
    }

    /// The streaming gate never lets call JSON reach the client, regardless
    /// of how the response is chunked.
    #[test]
    fn stream_never_leaks_calls(
        st in style(),
        s in nasty_string(),
        chunk in 1usize..12,
    ) {
        let args = json!({ "content": s });
        let full = format!("Hi.\n{}\n", render(st, &call_json("write_file", args)));
        let chunks = split_every(&full, chunk);
        let (visible, ncalls) = simulate_stream(&chunks, &known());
        prop_assert_eq!(ncalls, 1);
        prop_assert!(!visible.contains("\"name\":\"write_file\""), "leaked: {:?}", visible);
    }
}

// ---------- bug: partial ```json opener leaked before the rest arrived ----------

/// `safe_emit_end` held back partial `<tool_call>` / ```tool_call openers but
/// not partial ```json ones. A chunk boundary after "```js" released those
/// five bytes to the client; the next chunk then completed a fence that
/// `parse` turned into a call, leaving stray "```js" in the visible text.
#[test]
fn stream_json_fence_call_never_leaks_a_partial_opener() {
    let c = call_json("echo", json!({"msg": "hi"}));
    let full = format!("Sure.\n```json\n{c}\n```");
    for size in 1..=full.len() {
        let chunks: Vec<&str> = split_every(&full, size);
        let (visible, ncalls) = simulate_stream(&chunks, &known());
        assert_eq!(ncalls, 1, "chunk size {size}");
        assert_eq!(visible, "Sure.\n", "chunk size {size} leaked {visible:?}");
    }
}

#[test]
fn gate_holds_every_prefix_of_every_opener() {
    let k = known();
    for opener in ["<tool_call>", "```tool_call", "```json"] {
        for len in 1..opener.len() {
            let tail = &opener[..len];
            let buf = format!("abc {tail}");
            assert_eq!(gate(&buf, &k), 4, "prefix {tail:?} must be held back");
        }
    }
}

#[test]
fn gate_hold_respects_multibyte_boundaries() {
    let k = known();
    // 'é' is 2 bytes; the hold must still land on a char boundary.
    let buf = "héllo ```jso";
    let end = gate(buf, &k);
    assert!(buf.is_char_boundary(end));
    assert_eq!(&buf[..end], "héllo ");
    let buf = "日本語 <tool_";
    let end = gate(buf, &k);
    assert!(buf.is_char_boundary(end));
    assert_eq!(&buf[..end], "日本語 ");
}

/// A held-back prefix is flushed when the stream ends and the text turns
/// out to be ordinary prose: nothing may be swallowed.
#[test]
fn held_partial_opener_is_delivered_when_it_was_only_prose() {
    for full in ["Use ```jso", "Use ```js", "Use ```", "Use <tool_", "Use ``"] {
        for size in 1..=full.len() {
            let chunks: Vec<&str> = split_every(full, size);
            let (visible, ncalls) = simulate_stream(&chunks, &known());
            assert_eq!(ncalls, 0);
            assert_eq!(visible, full, "chunk size {size}");
        }
    }
}

#[test]
fn stream_tool_call_xml_never_leaks_at_any_chunk_size() {
    let c = call_json("write_file", json!({"path": "a", "content": "x```y</b>"}));
    let full = format!("Ok, writing.\n<tool_call>{c}</tool_call>");
    for size in 1..=full.len() {
        let chunks: Vec<&str> = split_every(&full, size);
        let (visible, ncalls) = simulate_stream(&chunks, &known());
        assert_eq!(ncalls, 1, "chunk size {size}");
        assert_eq!(visible, "Ok, writing.\n", "chunk size {size}: {visible:?}");
    }
}
