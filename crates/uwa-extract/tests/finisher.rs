use std::time::Duration;
use uwa_core::NetworkEvent;
use uwa_extract::finisher::{FinishSignal, Finisher, FinisherCfg};
use uwa_testkit::MockPage;

#[tokio::test]
async fn stable_dom_fires_stable_signal() {
    let p = MockPage::new().with_html_seq(vec![
        "<html>a</html>",
        "<html>a</html>",
        "<html>a</html>",
        "<html>a</html>",
    ]);
    let f = Finisher::new(FinisherCfg {
        stop_button: None,
        dom_stable_for: Duration::from_millis(30),
        poll_interval: Duration::from_millis(10),
        min_wait: Duration::from_millis(0),
        max_wait: Duration::from_secs(2),
    });
    let s = f.wait(&p).await.unwrap();
    assert_eq!(s, FinishSignal::Stable);
}

#[tokio::test]
async fn stop_button_gone_fires_stop() {
    let p = MockPage::new().with_html_seq(vec!["<html>a</html>"; 10]);
    let f = Finisher::new(FinisherCfg {
        stop_button: Some("button.stop".into()),
        dom_stable_for: Duration::from_secs(10),
        poll_interval: Duration::from_millis(5),
        min_wait: Duration::from_millis(0),
        max_wait: Duration::from_secs(2),
    });
    let s = f.wait(&p).await.unwrap();
    assert_eq!(s, FinishSignal::Stop);
}

#[tokio::test]
async fn network_finished_fires_network_done() {
    let p = MockPage::new()
        .with_html_seq(vec!["<html>a</html>"; 20])
        .with_network_events(vec![NetworkEvent::Finished {
            request_id: "r1".into(),
        }]);
    let f = Finisher::new(FinisherCfg {
        stop_button: None,
        dom_stable_for: Duration::from_secs(10),
        poll_interval: Duration::from_millis(5),
        min_wait: Duration::from_millis(0),
        max_wait: Duration::from_secs(2),
    });
    let s = f.wait(&p).await.unwrap();
    assert_eq!(s, FinishSignal::NetworkDone);
}

#[tokio::test]
async fn hard_timeout_wins() {
    // HTML always different → never stable.
    let seq: Vec<String> = (0..1000).map(|i| format!("<html>{i}</html>")).collect();
    let p = MockPage::new().with_html_seq(seq.iter().map(String::as_str).collect::<Vec<&str>>());
    let f = Finisher::new(FinisherCfg {
        stop_button: None,
        dom_stable_for: Duration::from_secs(60),
        poll_interval: Duration::from_millis(5),
        min_wait: Duration::from_millis(0),
        max_wait: Duration::from_millis(50),
    });
    let s = f.wait(&p).await.unwrap();
    assert_eq!(s, FinishSignal::Timeout);
}

#[tokio::test]
async fn min_wait_blocks_early_stable() {
    let p = MockPage::new().with_html_seq(vec!["<html>a</html>"; 50]);
    let f = Finisher::new(FinisherCfg {
        stop_button: None,
        dom_stable_for: Duration::from_millis(0),
        poll_interval: Duration::from_millis(5),
        min_wait: Duration::from_millis(80),
        max_wait: Duration::from_secs(2),
    });
    let start = std::time::Instant::now();
    let _ = f.wait(&p).await.unwrap();
    assert!(start.elapsed() >= Duration::from_millis(80));
}
