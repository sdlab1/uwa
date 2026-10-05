//! Property-based tests: the public parsers must never panic on any input.

use proptest::prelude::*;
use uwa_extract::net::{json_path_str, NetDecoder, SseParser};

proptest! {
    #[test]
    fn sse_never_panics(chunks in proptest::collection::vec(".*", 0..20)) {
        let mut p = SseParser::new();
        for c in &chunks {
            let _ = p.feed(c);
        }
        let _ = p.finish();
    }

    #[test]
    fn sse_frames_are_stable_under_chunking(data in ".{0,64}") {
        let whole = format!("event: message\ndata: {data}\n\n");
        let mut a = SseParser::new();
        let frames_a = a.feed(&whole);

        let mut b = SseParser::new();
        let mut frames_b = Vec::new();
        for ch in whole.chars() {
            frames_b.extend(b.feed(&ch.to_string()));
        }
        prop_assert_eq!(frames_a, frames_b);
    }

    #[test]
    fn json_path_never_panics(
        path in "[-.\\[\\]*a-zA-Z_0-9]{0,32}",
        text in ".{0,32}",
    ) {
        let v = serde_json::json!({
            "a": {"b": [{"c": text}]},
            "n": 1,
            "flag": true,
            "nil": null,
            "list": [text, 2],
        });
        let _ = json_path_str(&v, &path);
    }

    #[test]
    fn json_path_returns_none_for_unknown_root(
        path in "[a-z]{1,8}(\\.[a-z]{1,8}){0,4}"
    ) {
        let v = serde_json::json!({"only": "x"});
        prop_assert!(json_path_str(&v, &path).is_none());
    }
}

#[test]
fn decoder_roundtrips_through_serde() {
    let d = NetDecoder::Sse {
        json_path: "__raw__".into(),
    };
    let s = serde_json::to_string(&d).unwrap();
    let back: NetDecoder = serde_json::from_str(&s).unwrap();
    assert_eq!(d, back);
}
