use async_trait::async_trait;
use serde_json::Value;
use std::sync::Mutex;
use std::time::Duration;
use tokio::sync::broadcast;
use url::Url;
use uwa_core::{NetworkEvent, Page, Result};
use uwa_extract::finisher::{FinishSignal, Finisher, FinisherCfg};

/// A fully-scripted `Page`: HTML returns a queued sequence, `eval` returns a
/// pre-programmed value, network emits a queued sequence.
struct ScriptedPage {
    html_seq: Mutex<Vec<String>>,
    eval_answer: Mutex<Option<Value>>,
    net_tx: broadcast::Sender<NetworkEvent>,
    /// Emits the network events as soon as `wait` starts polling.
    net_send: Mutex<Option<Vec<NetworkEvent>>>,
}

impl ScriptedPage {
    fn new(html_seq: Vec<&str>) -> Self {
        let (net_tx, _) = broadcast::channel(64);
        Self {
            html_seq: Mutex::new(html_seq.into_iter().map(String::from).collect()),
            eval_answer: Mutex::new(None),
            net_tx,
            net_send: Mutex::new(None),
        }
    }
    fn with_eval(self, v: Value) -> Self {
        *self.eval_answer.lock().unwrap() = Some(v);
        self
    }
    fn with_net(self, evs: Vec<NetworkEvent>) -> Self {
        *self.net_send.lock().unwrap() = Some(evs);
        self
    }
}

#[async_trait]
impl Page for ScriptedPage {
    async fn goto(&self, _: &Url) -> Result<()> {
        Ok(())
    }
    async fn url(&self) -> Result<Url> {
        Ok("https://x/".parse().unwrap())
    }
    async fn eval(&self, _: &str) -> Result<Value> {
        Ok(self
            .eval_answer
            .lock()
            .unwrap()
            .clone()
            .unwrap_or(Value::Bool(false)))
    }
    async fn wait_for_selector(&self, _: &str, _: Duration) -> Result<()> {
        Ok(())
    }
    async fn html(&self) -> Result<String> {
        let mut q = self.html_seq.lock().unwrap();
        if q.is_empty() {
            return Ok(String::new());
        }
        if q.len() == 1 {
            Ok(q[0].clone())
        } else {
            Ok(q.remove(0))
        }
    }
    async fn click(&self, _: &str) -> Result<()> {
        Ok(())
    }
    async fn type_text(&self, _: &str, _: &str) -> Result<()> {
        Ok(())
    }
    async fn network_events(&self) -> Result<broadcast::Receiver<NetworkEvent>> {
        let rx = self.net_tx.subscribe();
        if let Some(evs) = self.net_send.lock().unwrap().take() {
            for e in evs {
                let _ = self.net_tx.send(e);
            }
        }
        Ok(rx)
    }
}

#[tokio::test]
async fn stable_dom_fires_stable_signal() {
    let p = ScriptedPage::new(vec![
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
    let p = ScriptedPage::new(vec!["<html>a</html>"; 10]).with_eval(Value::Bool(false));
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
    let p = ScriptedPage::new(vec!["<html>a</html>"; 20]).with_net(vec![NetworkEvent::Finished {
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
    let p = ScriptedPage::new(seq.iter().map(String::as_str).collect());
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
    let p = ScriptedPage::new(vec!["<html>a</html>"; 50]);
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
