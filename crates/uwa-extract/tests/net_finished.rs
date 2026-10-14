//! Regression: a `Finished` event for a request other than the one whose
//! body we are decoding must not end the stream.
//!
//! Real-world failure: opening a provider page fires a dozen unrelated
//! requests (favicon, telemetry); the first `loadingFinished` for any of
//! them killed the answer stream mid-flight.

use std::time::Duration;
use tokio_stream::StreamExt;
use uwa_core::NetworkEvent;
use uwa_extract::net::{DefaultNetExtractor, NetDecoder, NetExtractor, NetRules};
use uwa_testkit::MockPage;

fn rules() -> NetRules {
    NetRules {
        url_contains: vec!["backend".into()],
        mime_contains: vec!["event-stream".into()],
        decoder: NetDecoder::Sse {
            json_path: "choices[0].delta.content".into(),
        },
        idle_timeout_ms: 2_000,
    }
}

fn sse(content: &str) -> String {
    format!("data: {{\"choices\":[{{\"delta\":{{\"content\":\"{content}\"}}}}]}}\n\n")
}

#[tokio::test]
async fn unrelated_finished_does_not_end_the_stream() {
    let page = MockPage::new().with_network_events(vec![
        NetworkEvent::ResponseBody {
            request_id: "ours".into(),
            url: "https://site/backend/stream".into(),
            body: sse("a"),
            mime: "text/event-stream".into(),
            cf_mitigated_challenge: false,
        },
        // An unrelated request finishing used to `return` out of the loop
        // here, so "b" never arrived.
        NetworkEvent::Finished {
            request_id: "favicon".into(),
        },
        NetworkEvent::ResponseBody {
            request_id: "ours".into(),
            url: "https://site/backend/stream".into(),
            body: sse("b"),
            mime: "text/event-stream".into(),
            cf_mitigated_challenge: false,
        },
        NetworkEvent::Finished {
            request_id: "ours".into(),
        },
    ]);

    let net = DefaultNetExtractor;
    let mut stream = net.deltas(&page, &rules()).await.expect("deltas");

    let mut got: Vec<String> = Vec::new();
    while let Some(d) = tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .expect("stream must not stall")
    {
        // The empty delta is the end-of-stream marker emitted on `Finished`.
        if d.0.is_empty() {
            break;
        }
        got.push(d.0);
    }
    // "b" arriving after the unrelated Finished is the regression signal;
    // the loop then ends on the empty end-of-stream marker.
    assert_eq!(got, vec!["a".to_string(), "b".to_string()]);
}
