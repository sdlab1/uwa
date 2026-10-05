use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;
use tokio_stream::wrappers::ReceiverStream;
use uwa_core::{ExtractionStrategy, Page, Result};
use uwa_extract::dom::DomExtractor;
use uwa_extract::finisher::FinisherCfg;
use uwa_extract::net::{NetDecoder, NetDelta, NetExtractor, NetRules};
use uwa_extract::pipeline::{ExtractionPipeline, ExtractionSource, PipelineCfg};
use uwa_testkit::MockPage;

// ---- Fake NetExtractor that just emits deltas directly. ----

struct FakeNet(Vec<&'static str>);

#[async_trait]
impl NetExtractor for FakeNet {
    async fn deltas(
        &self,
        _page: &dyn Page,
        _rules: &NetRules,
    ) -> Result<ReceiverStream<NetDelta>> {
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        let parts: Vec<String> = self.0.iter().map(|s| s.to_string()).collect();
        tokio::spawn(async move {
            for p in parts {
                let _ = tx.send(NetDelta(p)).await;
            }
            // Close on drop.
        });
        Ok(ReceiverStream::new(rx))
    }
}

fn base_cfg(strategy: ExtractionStrategy) -> PipelineCfg {
    PipelineCfg {
        strategy,
        net: Some(NetRules {
            url_contains: vec!["backend".into()],
            mime_contains: vec!["event-stream".into()],
            decoder: NetDecoder::Sse {
                json_path: "__raw__".into(),
            },
            idle_timeout_ms: 200,
        }),
        dom: DomExtractor {
            assistant_message: "[data-message-author-role=assistant]".into(),
            stop_button: None,
            dom_stable_for: Duration::from_millis(20),
            max_wait: Duration::from_secs(1),
            poll_interval: Duration::from_millis(5),
        },
        finisher: FinisherCfg {
            stop_button: None,
            dom_stable_for: Duration::from_millis(20),
            poll_interval: Duration::from_millis(5),
            min_wait: Duration::from_millis(0),
            max_wait: Duration::from_secs(1),
        },
    }
}

#[tokio::test]
async fn network_first_uses_net_stream() {
    let page = MockPage::new().with_html("<html></html>");
    let pipe = ExtractionPipeline::with_net(Arc::new(FakeNet(vec!["Hel", "lo, ", "world"])));
    let out = pipe
        .run(&page, &base_cfg(ExtractionStrategy::NetworkFirst))
        .await
        .unwrap();
    assert_eq!(out.source, ExtractionSource::Network);
    assert_eq!(out.text, "Hello, world");
}

#[tokio::test]
async fn empty_network_falls_back_to_dom() {
    let html = r#"<html><body>
        <div data-message-author-role="assistant">from dom</div>
    </body></html>"#;
    let page = MockPage::new().with_html(html);
    let pipe = ExtractionPipeline::with_net(Arc::new(FakeNet(vec![])));
    let out = pipe
        .run(&page, &base_cfg(ExtractionStrategy::NetworkFirst))
        .await
        .unwrap();
    assert_eq!(out.source, ExtractionSource::Dom);
    assert_eq!(out.text, "from dom");
}

#[tokio::test]
async fn dom_only_skips_network() {
    let html = r#"<html><body>
        <div data-message-author-role="assistant">dom says hi</div>
    </body></html>"#;
    let page = MockPage::new().with_html(html);
    // FakeNet would emit, but DomOnly path never asks it.
    let pipe = ExtractionPipeline::with_net(Arc::new(FakeNet(vec!["NEVER"])));
    let out = pipe
        .run(&page, &base_cfg(ExtractionStrategy::DomOnly))
        .await
        .unwrap();
    assert_eq!(out.source, ExtractionSource::Dom);
    assert_eq!(out.text, "dom says hi");
}
