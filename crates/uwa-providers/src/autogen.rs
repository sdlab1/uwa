//! Analyze a live page and propose candidate selectors.
//!
//! Strategy:
//! * **Inputs** — `textarea`, `input[type=text]`, `div[contenteditable=true]`.
//!   Score by area, visibility, `aria-label`, placeholder keywords.
//! * **Send buttons** — buttons with keywords (`send`, `submit`, ...).
//! * **Assistant containers** — elements with `role`, `data-role`,
//!   `data-testid` mentioning `assistant`/`message`/`response`/`model`.
//!
//! Every candidate carries a `score` and `evidence` string so the UI can
//! explain why it was proposed.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Instant;
use uwa_core::{Page, Result, UwaError};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Candidate {
    /// Best-effort CSS selector.
    pub selector: String,
    /// Human-readable rationale ("placeholder matches 'ask anything'").
    pub evidence: String,
    /// 0.0..=1.0, higher = more confident.
    pub score: f32,
    /// Optional tag name (for display).
    pub tag: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PageAnalysis {
    pub inputs: Vec<Candidate>,
    pub send_buttons: Vec<Candidate>,
    pub assistant_containers: Vec<Candidate>,
    /// Wall time for the analysis, ms.
    pub duration_ms: u64,
}

/// Run the analysis against a live page.
pub async fn analyze(page: &dyn Page) -> Result<PageAnalysis> {
    let start = Instant::now();
    let js = include_str!("autogen_scan.js");
    let v = page.eval(js).await?;

    let inputs = parse_candidates(v.get("inputs"))?;
    let send_buttons = parse_candidates(v.get("sendButtons"))?;
    let assistant_containers = parse_candidates(v.get("assistantContainers"))?;

    Ok(PageAnalysis {
        inputs,
        send_buttons,
        assistant_containers,
        duration_ms: start.elapsed().as_millis() as u64,
    })
}

fn parse_candidates(v: Option<&Value>) -> Result<Vec<Candidate>> {
    let Some(arr) = v.and_then(Value::as_array) else {
        return Ok(vec![]);
    };
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let selector = item
            .get("selector")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if selector.is_empty() {
            continue;
        }
        out.push(Candidate {
            selector,
            evidence: item
                .get("evidence")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            score: item.get("score").and_then(Value::as_f64).unwrap_or(0.5) as f32,
            tag: item
                .get("tag")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        });
    }
    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok(out)
}

/// Validate a single selector against a live page. Returns (matched, text).
pub async fn probe_selector(page: &dyn Page, selector: &str) -> Result<(u32, Option<String>)> {
    let js = format!(
        r#"(function() {{
            try {{
                const els = document.querySelectorAll({sel});
                const first = els[0];
                const text = first ? (first.innerText || first.textContent || '').slice(0, 500) : null;
                return {{ ok: true, n: els.length, text }};
            }} catch (e) {{
                return {{ ok: false, error: String(e) }};
            }}
        }})()"#,
        sel = serde_json::to_string(selector).unwrap(),
    );
    let v = page.eval(&js).await?;
    if v.get("ok").and_then(Value::as_bool) != Some(true) {
        let err = v.get("error").and_then(Value::as_str).unwrap_or("unknown");
        return Err(UwaError::BadRequest(format!("selector: {err}")));
    }
    let n = v.get("n").and_then(Value::as_u64).unwrap_or(0) as u32;
    let text = v.get("text").and_then(Value::as_str).map(str::to_string);
    Ok((n, text))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::ScriptedPage;
    use serde_json::json;

    #[tokio::test]
    async fn analyze_parses_candidates() {
        let scan_result = json!({
            "inputs": [
                {"selector": "input[type=text]", "evidence": "search-like", "score": 0.4, "tag": "input"},
                {"selector": "textarea#prompt", "evidence": "large textarea", "score": 0.9, "tag": "textarea"}
            ],
            "sendButtons": [
                {"selector": "button[data-testid=send]", "evidence": "near input", "score": 0.85, "tag": "button"}
            ],
            "assistantContainers": [
                {"selector": "[data-role=assistant]", "evidence": "data-role", "score": 0.95, "tag": "div"}
            ]
        });
        let p = ScriptedPage::new(vec![("candidates", scan_result)]);
        let a = analyze(&p).await.unwrap();
        assert_eq!(a.inputs.len(), 2);
        assert_eq!(a.inputs[0].selector, "textarea#prompt"); // sorted by score
        assert_eq!(a.send_buttons.len(), 1);
        assert_eq!(a.assistant_containers.len(), 1);
        assert!(a.duration_ms < 5_000);
    }

    #[tokio::test]
    async fn empty_page_returns_empty_lists() {
        // Default scripted answer is `false` — no arrays at all.
        let p = ScriptedPage::new(vec![]);
        let a = analyze(&p).await.unwrap();
        assert!(a.inputs.is_empty());
        assert!(a.send_buttons.is_empty());
        assert!(a.assistant_containers.is_empty());
    }

    #[tokio::test]
    async fn probe_reports_matched_count() {
        let p = ScriptedPage::new(vec![(
            "querySelectorAll",
            json!({"ok": true, "n": 3, "text": "hello"}),
        )]);
        let (n, t) = probe_selector(&p, "#x").await.unwrap();
        assert_eq!(n, 3);
        assert_eq!(t.as_deref(), Some("hello"));
    }

    #[tokio::test]
    async fn probe_reports_selector_error() {
        let p = ScriptedPage::new(vec![(
            "querySelectorAll",
            json!({"ok": false, "error": "SyntaxError"}),
        )]);
        assert!(probe_selector(&p, "##bad").await.is_err());
    }
}
