//! Property-based tests for `SseParser` and `json_path_str`.
//!
//! The property that matters most is chunk invariance: CDP hands us SSE in
//! arbitrary chunks, so char-by-char feeding must yield exactly the frames a
//! single bulk feed would.

use proptest::prelude::*;
use serde_json::json;
use uwa_extract::net::{json_path_str, NetDecoder, SseParser};

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

/// One line of data that can't contain `\n` (so it won't split the frame).
fn data_line() -> impl Strategy<Value = String> {
    proptest::string::string_regex("[a-zA-Z0-9 ,.!?-]{0,80}").unwrap_or_else(|_| unreachable!())
}

/// Encodes an SSE frame the way servers do.
fn encode_frame(event: Option<&str>, data: &str) -> String {
    let mut s = String::new();
    if let Some(e) = event {
        s.push_str(&format!("event: {}\n", e));
    }
    s.push_str(&format!("data: {}\n\n", data));
    s
}

/// Encodes an SSE frame with a randomly-chosen legal line terminator.
fn encode_frame_mixed(event: Option<&str>, data: &str, term: &str) -> String {
    let mut s = String::new();
    if let Some(e) = event {
        s.push_str(&format!("event: {e}{term}"));
    }
    s.push_str(&format!("data: {data}{term}{term}"));
    s
}

// ---------- guarantee 1: never panics ----------

proptest! {
    #![proptest_config(cases(256))]

    #[test]
    fn sse_never_panics_on_arbitrary_input(s in "(?s).{0,2000}") {
        let mut p = SseParser::new();
        let _ = p.feed(&s);
        let _ = p.finish();
    }

    #[test]
    fn sse_never_panics_on_arbitrary_chunks(
        chunks in proptest::collection::vec("(?s).{0,200}", 0..20)
    ) {
        let mut p = SseParser::new();
        for c in &chunks {
            let _ = p.feed(c);
        }
        let _ = p.finish();
    }

    #[test]
    fn json_path_never_panics(path in "(?s).{0,80}", text in "[a-zA-Z0-9 ]{0,32}") {
        let v = json!({
            "a": {"b": [1, 2, {"c": text}]},
            "n": 1,
            "flag": true,
            "nil": null,
            "list": [text, 2],
            "key.with.dots": text,
            "arr": [[[text]]],
        });
        let _ = json_path_str(&v, &path);
    }
}

// ---------- guarantee 2: chunk invariance ----------

proptest! {
    #![proptest_config(cases(128))]

    /// Feeding the same bytes one char at a time yields the same frames as
    /// feeding them all at once.
    #[test]
    fn sse_char_by_char_equals_bulk(
        events in proptest::collection::vec(
            (proptest::option::of("[a-z]{1,10}"), data_line()),
            0..8,
        ),
    ) {
        let full: String = events
            .iter()
            .map(|(e, d)| encode_frame(e.as_deref(), d))
            .collect();

        let mut bulk = SseParser::new();
        let bulk_frames = bulk.feed(&full);

        let mut one_by_one = SseParser::new();
        let mut one_frames = Vec::new();
        for ch in full.chars() {
            one_frames.extend(one_by_one.feed(&ch.to_string()));
        }

        prop_assert_eq!(bulk_frames.len(), events.len());
        prop_assert_eq!(bulk_frames, one_frames);
    }

    /// Chunk invariance with mixed line terminators.
    #[test]
    fn sse_char_by_char_equals_bulk_mixed_terms(
        events in proptest::collection::vec(
            (proptest::option::of("[a-z]{1,10}"), data_line(),
             proptest::sample::select(vec!["\n", "\r\n", "\r"])),
            0..8,
        ),
    ) {
        let full: String = events
            .iter()
            .map(|(e, d, t)| encode_frame_mixed(e.as_deref(), d, t))
            .collect();
        let mut bulk = SseParser::new();
        let mut bulk_frames = bulk.feed(&full);
        if let Some(last) = bulk.finish() {
            bulk_frames.push(last);
        }
        let mut one_by_one = SseParser::new();
        let mut one_frames = Vec::new();
        for ch in full.chars() {
            one_frames.extend(one_by_one.feed(&ch.to_string()));
        }
        if let Some(last) = one_by_one.finish() {
            one_frames.push(last);
        }
        prop_assert_eq!(bulk_frames.len(), events.len());
        prop_assert_eq!(bulk_frames, one_frames);
    }

    /// Splitting at arbitrary char boundaries also produces identical frames.
    #[test]
    fn sse_random_chunk_boundaries(
        events in proptest::collection::vec(data_line(), 0..6),
        cuts in proptest::collection::vec(1usize..20, 0..5),
    ) {
        let full: String = events.iter().map(|d| encode_frame(None, d)).collect();

        let mut bulk = SseParser::new();
        let expected = bulk.feed(&full);

        let chars: Vec<char> = full.chars().collect();
        let mut pieces: Vec<String> = Vec::new();
        let mut prev = 0usize;
        for &c in &cuts {
            let c = c.min(chars.len());
            if c > prev {
                pieces.push(chars.get(prev..c).unwrap_or(&[]).iter().collect());
                prev = c;
            }
        }
        if prev < chars.len() {
            pieces.push(chars.get(prev..).unwrap_or(&[]).iter().collect());
        }

        let mut chunked = SseParser::new();
        let mut actual = Vec::new();
        for p in pieces {
            actual.extend(chunked.feed(&p));
        }
        if let Some(last) = chunked.finish() {
            actual.push(last);
        }

        prop_assert_eq!(expected, actual);
    }

    /// A frame left unterminated at end-of-stream still comes out of `finish`.
    #[test]
    fn finish_flushes_a_partial_frame(data in data_line()) {
        let whole = format!("data: {data}");
        let mut bulk = SseParser::new();
        prop_assert!(bulk.feed(&whole).is_empty());

        let mut charwise = SseParser::new();
        for ch in whole.chars() {
            assert!(charwise.feed(&ch.to_string()).is_empty());
        }

        prop_assert_eq!(bulk.finish(), charwise.finish());
    }
}

// ---------- mixed line terminator handling (bugfix regression) ----------

/// A buffer with `\n` before `\r\n` must split at the earlier `\n`,
/// not merge both lines into one.
#[test]
fn feed_splits_at_earliest_lf_before_crlf() {
    let mut p = SseParser::new();
    // "data: first\n" then "data: second\r\n" then blank line.
    let frames = p.feed("data: first\ndata: second\r\n\r\n");
    assert_eq!(frames.len(), 1);
    let frame = frames.first().unwrap_or_else(|| unreachable!());
    assert_eq!(&frame.data, "first\nsecond");
}

/// A lone `\r` before a `\n` (not adjacent) must split at the `\r`.
/// Per SSE spec, \r IS a line terminator, so "data: a\rb\n" yields
/// lines "data: a" and "b". Since "b" lacks "data:" prefix, only "a"
/// becomes frame data. To get multi-line data, each line needs "data:".
#[test]
fn feed_splits_at_lone_cr_before_lf() {
    let mut p = SseParser::new();
    // \r terminates first line; "b" is a separate line without data: prefix.
    let frames = p.feed("data: a\rb\n\n");
    assert_eq!(frames.len(), 1);
    let frame = frames.first().unwrap_or_else(|| unreachable!());
    assert_eq!(&frame.data, "a");
}

/// Two data lines separated by \r produce joined data.
#[test]
fn feed_joins_data_lines_separated_by_cr() {
    let mut p = SseParser::new();
    // Each line has data: prefix; \r separates them.
    let frames = p.feed("data: a\rdata: b\n\n");
    assert_eq!(frames.len(), 1);
    let frame = frames.first().unwrap_or_else(|| unreachable!());
    assert_eq!(&frame.data, "a\nb");
}

/// A `\r` at the very end of a chunk must be held back until the next
/// chunk disambiguates CRLF vs lone CR.
#[test]
fn feed_holds_trailing_cr_until_next_chunk() {
    let mut p = SseParser::new();
    // First chunk ends with \r — must NOT emit a frame yet.
    let frames1 = p.feed("data: hello\r");
    assert!(
        frames1.is_empty(),
        "trailing \\r must not terminate a line yet"
    );
    // Second chunk completes the CRLF and the frame.
    let frames2 = p.feed("\n\n");
    assert_eq!(frames2.len(), 1);
    let frame = frames2.first().unwrap_or_else(|| unreachable!());
    assert_eq!(&frame.data, "hello");
}

/// A `\r` at end-of-stream (in `finish`) IS treated as a terminator,
/// because no more data can arrive.
#[test]
fn finish_treats_trailing_cr_as_terminator() {
    let mut p = SseParser::new();
    p.feed("data: hello\r");
    let frame = p.finish();
    assert!(frame.is_some());
    assert_eq!(frame.map(|f| f.data), Some("hello".to_string()));
}

/// `finish()` must split `\r`-terminated lines in the residual buffer.
#[test]
fn finish_splits_cr_terminated_lines() {
    let mut p = SseParser::new();
    // Feed data with \r separators; each line has data: prefix.
    p.feed("data: a\rdata: b\rdata: c");
    let frame = p.finish();
    assert!(frame.is_some());
    assert_eq!(frame.map(|f| f.data), Some("a\nb\nc".to_string()));
}

/// Mixed terminators across chunk boundaries produce the same result
/// as a single bulk feed (chunk invariance).
#[test]
fn mixed_terminators_chunk_invariance() {
    let full = "event: e1\ndata: one\r\n\ndata: two\n\ndata: three\r\r\n";
    let mut bulk = SseParser::new();
    let mut bulk_frames = bulk.feed(full);

    let mut chunked = SseParser::new();
    let mut chunked_frames = Vec::new();
    // Split at every character boundary.
    for ch in full.chars() {
        chunked_frames.extend(chunked.feed(&ch.to_string()));
    }
    if let Some(last) = chunked.finish() {
        chunked_frames.push(last);
    }
    if let Some(last) = bulk.finish() {
        bulk_frames.push(last);
    }
    assert_eq!(bulk_frames, chunked_frames);
}

/// CRLF split across chunk boundaries is handled correctly.
#[test]
fn crlf_split_across_chunks() {
    let mut p = SseParser::new();
    let f1 = p.feed("data: split\r");
    assert!(f1.is_empty());
    let f2 = p.feed("\n\r");
    assert!(f2.is_empty());
    let f3 = p.feed("\n");
    assert_eq!(f3.len(), 1);
    let frame = f3.first().unwrap_or_else(|| unreachable!());
    assert_eq!(&frame.data, "split");
}

// ---------- guarantee 3: json_path finds what exists ----------

proptest! {
    #![proptest_config(cases(128))]

    /// If a key exists, the plain key path returns its value as text.
    #[test]
    fn json_path_finds_present_key(
        key in "[a-z]{1,8}",
        val in "[a-zA-Z0-9]{0,40}",
    ) {
        let v = json!({ key.clone(): val.clone() });
        let got = json_path_str(&v, &key);
        prop_assert_eq!(got.as_deref(), Some(val.as_str()));
    }

    /// Wildcard over an object yields all its values joined.
    #[test]
    fn json_path_wildcard_on_object(
        vals in proptest::collection::vec("[a-z]{1,8}", 1..5),
    ) {
        let obj: serde_json::Map<String, serde_json::Value> = vals
            .iter()
            .enumerate()
            .map(|(i, v)| (format!("k{i}"), json!(v)))
            .collect();
        let root = serde_json::Value::Object(obj);

        let got = json_path_str(&root, "*").unwrap_or_else(|| unreachable!());

        for v in &vals {
            prop_assert!(got.contains(v.as_str()), "{v:?} missing from {got:?}");
        }
    }

    /// A path the document cannot satisfy returns nothing. The lone key holds a
    /// digit, which `[a-z]` cannot generate.
    #[test]
    fn json_path_returns_none_for_unknown_root(
        path in r"[a-z]{1,8}(\.[a-z]{1,8}){0,4}"
    ) {
        let v = json!({"only1": "x"});
        prop_assert!(json_path_str(&v, &path).is_none());
    }
}

#[test]
fn decoder_roundtrips_through_serde() {
    let d = NetDecoder::Sse {
        json_path: "__raw__".into(),
    };
    let s = serde_json::to_string(&d).unwrap_or_else(|_| unreachable!());
    let back: NetDecoder = serde_json::from_str(&s).unwrap_or_else(|_| unreachable!());
    assert_eq!(d, back);
}
