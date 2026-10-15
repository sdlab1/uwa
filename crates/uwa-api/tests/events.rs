use uwa_core::NetworkEvent;

/// Seed the log with deterministic events for testing.
pub fn seed(log: &mut Vec<String>) -> Vec<NetworkEvent> {
    log.clear();
    vec![
        NetworkEvent::ResponseBody {
            request_id: "r1".into(),
            url: "https://example.com/".into(),
            body: "hello world".into(),
            mime: "text/plain".into(),
            cf_mitigated_challenge: false,
        },
        NetworkEvent::ResponseBody {
            request_id: "r2".into(),
            url: "https://example.com/api".into(),
            body: "{\"ok\": true}".into(),
            mime: "application/json".into(),
            cf_mitigated_challenge: false,
        },
        NetworkEvent::ResponseBody {
            request_id: "r3".into(),
            url: "https://example.com/stream".into(),
            body: "data: {\"msg\": \"hello\"}\n\n".into(),
            mime: "text/event-stream".into(),
            cf_mitigated_challenge: false,
        },
    ]
}
