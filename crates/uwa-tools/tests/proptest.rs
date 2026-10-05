use proptest::prelude::*;
use uwa_tools::parse;

proptest! {
    #[test]
    fn tool_call_parser_never_panics(text in ".*") {
        let _ = parse(&text, &["a".into(), "b".into()]);
    }

    #[test]
    fn tool_call_parser_chunks_never_panic(
        chunks in proptest::collection::vec(".*", 0..8)
    ) {
        let _ = parse(&chunks.join("\n"), &["a".into(), "b".into()]);
    }
}
