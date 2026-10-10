//! Verify that `NetDecoder::Site` routes frames to the right parser end
//! to end: CDP events → SSE frames → site parser → incremental deltas.
//!
//! Order matters: `deltas()` must subscribe to the page's network channel
//! before the test sends events, so the stream is created first.

use uwa_core::{NetDecoder, NetRules, NetworkEvent};
use uwa_extract::net::{collect_deltas, DefaultNetExtractor, NetExtractor};
use uwa_testkit::MockPage;

fn rules_for(name: &str) -> NetRules {
    NetRules {
        // Empty = any URL matches; keep the mime filter.
        url_contains: vec![],
        mime_contains: vec!["event-stream".into()],
        decoder: NetDecoder::Site { name: name.into() },
        idle_timeout_ms: 500,
    }
}

#[tokio::test]
async fn chatgpt_cumulative_becomes_incremental() {
    let page = MockPage::new();
    let extractor = DefaultNetExtractor;
    let stream = extractor
        .deltas(&page, &rules_for("chatgpt"))
        .await
        .unwrap();

    let tx = page.network_sender();
    tx.send(NetworkEvent::ResponseBody {
        url: "https://chatgpt.com/backend-api/f/conversation".into(),
        body: concat!(
            "data: {\"v\":{\"message\":{\"content\":{\"parts\":[\"Hel\"]}}}}\n\n",
            "data: {\"v\":{\"message\":{\"content\":{\"parts\":[\"Hello\"]}}}}\n\n",
            "data: {\"v\":{\"message\":{\"content\":{\"parts\":[\"Hello world\"]}}}}\n\n",
            "data: [DONE]\n\n",
        )
        .into(),
        mime: "text/event-stream".into(),
    })
    .unwrap();
    tx.send(NetworkEvent::Finished {
        request_id: "r".into(),
    })
    .unwrap();

    let text = collect_deltas(stream).await;
    // Cumulative input → incremental output, concatenated = full text.
    assert_eq!(text, "Hello world");
}

#[tokio::test]
async fn claude_text_deltas_pass_through() {
    let page = MockPage::new();
    let extractor = DefaultNetExtractor;
    let stream = extractor.deltas(&page, &rules_for("claude")).await.unwrap();

    let tx = page.network_sender();
    tx.send(NetworkEvent::ResponseBody {
        url: "https://claude.ai/api/xxx".into(),
        body: concat!(
            "data: {\"type\":\"message_start\"}\n\n",
            "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"Hel\"}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"lo\"}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{}\"}}\n\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        )
        .into(),
        mime: "text/event-stream".into(),
    })
    .unwrap();
    tx.send(NetworkEvent::Finished {
        request_id: "r".into(),
    })
    .unwrap();

    let text = collect_deltas(stream).await;
    assert_eq!(text, "Hello");
}

#[tokio::test]
async fn gemini_batchexecute_cumulative_diffed() {
    let page = MockPage::new();
    let extractor = DefaultNetExtractor;
    let stream = extractor.deltas(&page, &rules_for("gemini")).await.unwrap();

    let tx = page.network_sender();
    let f1 = "[[\"wrb.fr\", null, \"{\\\"candidates\\\":[{\\\"content\\\":{\\\"parts\\\":[{\\\"text\\\":\\\"Hel\\\"}]}}]}\"]]";
    let f2 = "[[\"wrb.fr\", null, \"{\\\"candidates\\\":[{\\\"content\\\":{\\\"parts\\\":[{\\\"text\\\":\\\"Hello\\\"}]}}]}\"]]";
    tx.send(NetworkEvent::ResponseBody {
        url: "https://gemini.google.com/batchexecute".into(),
        body: format!("data: {f1}\n\ndata: {f2}\n\ndata: [DONE]\n\n"),
        mime: "text/event-stream".into(),
    })
    .unwrap();
    tx.send(NetworkEvent::Finished {
        request_id: "r".into(),
    })
    .unwrap();

    let text = collect_deltas(stream).await;
    assert_eq!(text, "Hello");
}

#[tokio::test]
async fn unknown_site_name_yields_nothing() {
    let page = MockPage::new();
    let extractor = DefaultNetExtractor;
    let stream = extractor
        .deltas(&page, &rules_for("no-such-site"))
        .await
        .unwrap();

    let tx = page.network_sender();
    tx.send(NetworkEvent::ResponseBody {
        url: "https://x.com/api/stream".into(),
        body: "data: {}\n\n".into(),
        mime: "text/event-stream".into(),
    })
    .unwrap();
    tx.send(NetworkEvent::Finished {
        request_id: "r".into(),
    })
    .unwrap();

    let text = collect_deltas(stream).await;
    assert_eq!(text, "");
}
