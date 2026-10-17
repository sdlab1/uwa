//! Turn a raw `Trace` into a proposed `Workflow`.
//!
//! The transformation has three stages: collapse, dedupe, convert.
//!
//! * **Collapse** — the DOM fires one `input` event per keystroke. A user
//!   typing "hello" produces five; we keep the last per element-run.
//! * **Dedupe** — a chat UI sometimes fires two clicks on the same button
//!   (icon + wrapper). Same selector within 500 ms is one intent.
//! * **Convert** — map observed selectors to named targets when they
//!   match a configured selector (loosely; the recorder's CSS path and
//!   the config's selector rarely agree byte-for-byte). Anything that
//!   doesn't map becomes a raw `Click` — the runner accepts a raw CSS
//!   selector when its name doesn't look like a named target.
//!
//! The output is a proposal, not a policy. A user is expected to read the
//! TOML and delete a step they didn't intend before saving.

use uwa_config::Selectors;
use uwa_core::workflow::{Action, Trace, TraceEvent, Workflow};

/// A workflow plus the reasons it might not be exactly right.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProposedWorkflow {
    pub workflow: Workflow,
    /// Human-readable notes. Never empty when the recorder dropped or
    /// approximated something.
    pub warnings: Vec<String>,
    /// Number of raw events that fed the proposal.
    pub event_count: usize,
}

/// Pauses longer than this become an explicit `Wait`. Shorter gaps are
/// the normal speed of a user clicking; recording them as waits would
/// make the workflow slower than the person it was copied from.
const WAIT_THRESHOLD_MS: u64 = 1500;
/// Cap on recorded waits: a user who went to lunch mid-flow shouldn't
/// produce a `Wait { seconds: 3600 }`.
const MAX_WAIT_SECS: f32 = 5.0;
/// Two clicks on the same selector within this window are one click.
const CLICK_DEDUP_MS: u64 = 500;

pub fn propose_workflow(trace: &Trace, selectors: &Selectors) -> ProposedWorkflow {
    let mut warnings = Vec::new();
    let event_count = trace.events.len();

    let collapsed = collapse_inputs(&trace.events);
    let deduped = dedupe_clicks(collapsed);

    let mut actions: Vec<Action> = Vec::new();
    let mut last_at: u64 = 0;

    for ev in &deduped {
        let at = ev.at_ms();
        let gap = at.saturating_sub(last_at);
        if gap > WAIT_THRESHOLD_MS && !actions.is_empty() {
            let secs = (gap as f32 / 1000.0).min(MAX_WAIT_SECS);
            // Round to one decimal so the TOML stays readable.
            let secs = (secs * 10.0).round() / 10.0;
            actions.push(Action::Wait { seconds: secs });
        }
        last_at = at;

        match ev {
            TraceEvent::Click { selector, .. } => {
                let named = resolve_target(selector, selectors);
                match named {
                    Some(target) => actions.push(Action::Click {
                        target,
                        optional: false,
                    }),
                    None => {
                        if looks_like_css(selector) {
                            actions.push(Action::Click {
                                target: selector.clone(),
                                optional: true,
                            });
                        } else {
                            warnings.push(format!(
                                "click on `{selector}` skipped — not a named \
                                 target and not a CSS selector"
                            ));
                        }
                    }
                }
            }
            TraceEvent::Input { selector, text, .. } => match resolve_target(selector, selectors) {
                Some(target) => {
                    // A numeric literal ("3", "2.5") is recorded as a value
                    // so "pick a number" flows stay reproducible; anything
                    // else is the pipeline's user input and left as `None`
                    // so the real prompt is substituted at run time.
                    let value = classify_input(text);
                    actions.push(Action::FillInput { target, value });
                }
                None => warnings.push(format!(
                    "input on `{selector}` skipped — not a configured selector"
                )),
            },
            TraceEvent::KeyPress { key, .. } => {
                actions.push(Action::KeyPress { key: key.clone() });
            }
            TraceEvent::Submit { selector, .. } => {
                // A native form submit means "the composer's send action";
                // map it to the configured send button when one exists.
                if selectors.send_button.is_some() {
                    actions.push(Action::Click {
                        target: "send_btn".into(),
                        optional: true,
                    });
                } else if let Some(t) = resolve_target(selector, selectors) {
                    actions.push(Action::Click {
                        target: t,
                        optional: true,
                    });
                }
            }
        }
    }

    // Append a `StreamWait` at the end when the provider has an assistant
    // selector and the trace has real actions — it's the one thing a user
    // reliably forgets. An empty trace stays empty (and warns below).
    if !actions.is_empty()
        && selectors.assistant_message.is_some()
        && !actions
            .iter()
            .any(|a| matches!(a, Action::StreamWait { .. }))
    {
        actions.push(Action::StreamWait {
            target: "assistant_message".into(),
            timeout_secs: 120,
        });
    }

    if actions.is_empty() {
        warnings.push(
            "no actions were produced — the recording captured no clicks or \
             inputs on configured selectors"
                .into(),
        );
    }

    ProposedWorkflow {
        workflow: Workflow { steps: actions },
        warnings,
        event_count,
    }
}

// ---------------------------------------------------------------------------
// Collapse / dedupe
// ---------------------------------------------------------------------------

fn collapse_inputs(events: &[TraceEvent]) -> Vec<TraceEvent> {
    let mut out: Vec<TraceEvent> = Vec::with_capacity(events.len());
    let mut pending: Option<TraceEvent> = None;

    for ev in events {
        match ev {
            TraceEvent::Input { selector, .. } => {
                if let Some(TraceEvent::Input { selector: prev, .. }) = pending.as_ref() {
                    if prev == selector {
                        // Same element, later value: keep the later one.
                        pending = Some(ev.clone());
                        continue;
                    }
                }
                if let Some(p) = pending.take() {
                    out.push(p);
                }
                pending = Some(ev.clone());
            }
            _ => {
                if let Some(p) = pending.take() {
                    out.push(p);
                }
                out.push(ev.clone());
            }
        }
    }
    if let Some(p) = pending.take() {
        out.push(p);
    }
    out
}

fn dedupe_clicks(events: Vec<TraceEvent>) -> Vec<TraceEvent> {
    let mut out: Vec<TraceEvent> = Vec::with_capacity(events.len());
    let mut last: Option<(String, u64)> = None;

    for ev in events {
        match &ev {
            TraceEvent::Click {
                selector, at_ms, ..
            } => {
                if let Some((prev, t)) = last.as_ref() {
                    if prev == selector && at_ms.saturating_sub(*t) < CLICK_DEDUP_MS {
                        last = Some((selector.clone(), *at_ms));
                        continue;
                    }
                }
                last = Some((selector.clone(), *at_ms));
            }
            _ => last = None,
        }
        out.push(ev);
    }
    out
}

// ---------------------------------------------------------------------------
// Selector matching
// ---------------------------------------------------------------------------

/// Whether a user-configured selector and an observed CSS path plausibly
/// refer to the same element.
///
/// Byte-for-byte comparison fails on almost every real case:
/// `[data-testid=send-button]` vs `button[data-testid="send-button"]`,
/// `#prompt-textarea` vs `#prompt-textarea`, `div.ql-editor` vs
/// `div.ql-editor`. This normaliser strips quoting and whitespace and
/// then accepts substring containment when the shorter side is long
/// enough to be meaningful.
fn matches_loosely(observed: &str, configured: &str) -> bool {
    if observed == configured {
        return true;
    }
    let norm = |s: &str| -> String {
        s.chars()
            .filter(|c| !c.is_whitespace() && *c != '"' && *c != '\'')
            .collect()
    };
    let (o, c) = (norm(observed), norm(configured));
    if o.is_empty() || c.is_empty() {
        return false;
    }
    if o == c {
        return true;
    }
    if o.len() < c.len() {
        c.contains(&o) && o.len() >= 5
    } else {
        o.contains(&c) && c.len() >= 5
    }
}

fn resolve_target(observed: &str, s: &Selectors) -> Option<String> {
    if let Some(v) = s.input.as_deref() {
        if matches_loosely(observed, v) {
            return Some("input_box".into());
        }
    }
    if let Some(v) = s.send_button.as_deref() {
        if matches_loosely(observed, v) {
            return Some("send_btn".into());
        }
    }
    if let Some(v) = s.stop_button.as_deref() {
        if matches_loosely(observed, v) {
            return Some("stop_btn".into());
        }
    }
    if let Some(v) = s.assistant_message.as_deref() {
        if matches_loosely(observed, v) {
            return Some("result_container".into());
        }
    }
    if let Some(v) = s.conversation_root.as_deref() {
        if matches_loosely(observed, v) {
            return Some("conversation_root".into());
        }
    }
    None
}

fn looks_like_css(s: &str) -> bool {
    s.contains('#')
        || s.contains('.')
        || s.contains('[')
        || s.contains('>')
        || s.contains(':')
        || s.contains(' ')
        || s.starts_with('*')
}

// ---------------------------------------------------------------------------
// Input classification
// ---------------------------------------------------------------------------

/// A numeric literal (`"3"`, `"2.5"`, `"-7"`) is recorded as a value so
/// "pick a number" flows stay reproducible. Anything else — including a
/// short word like `"hello"` — is the pipeline's user input and left as
/// `None` so the real prompt is substituted at run time. Baking free
/// text into the workflow would replay the recording's prompt on every
/// run, which is never what a chat flow wants.
fn classify_input(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.parse::<f64>().is_ok() {
        return Some(trimmed.to_string());
    }
    None
}

// ---------------------------------------------------------------------------
// TOML rendering
// ---------------------------------------------------------------------------

/// Render a workflow as a TOML fragment suitable for pasting into
/// `uwa.toml` under `[providers.<name>]`.
///
/// We emit explicit `[[providers.X.workflow]]` tables rather than
/// serialising the `Workflow` struct, because the struct is a
/// transparent `Vec<Action>` and TOML has no way to represent "an array
/// of tables at this dotted path" through serde's default walker.
pub fn workflow_to_toml_snippet(provider: &str, wf: &Workflow) -> String {
    if wf.steps.is_empty() {
        return format!("# no workflow recorded for provider `{provider}`\n");
    }
    let mut s = String::new();
    s.push_str(&format!(
        "# Recorded workflow for `{provider}`. Paste below the provider's \
         existing tables, or replace an existing `[[providers.{provider}.workflow]]` block.\n\n"
    ));
    for step in &wf.steps {
        s.push_str(&format!("[[providers.{provider}.workflow]]\n"));
        match step {
            Action::Click { target, optional } => {
                s.push_str("action = \"click\"\n");
                s.push_str(&format!("target = {}\n", toml_str(target)));
                s.push_str(&format!("optional = {optional}\n"));
            }
            Action::FillInput { target, value } => {
                s.push_str("action = \"fill_input\"\n");
                s.push_str(&format!("target = {}\n", toml_str(target)));
                if let Some(v) = value {
                    s.push_str(&format!("value = {}\n", toml_str(v)));
                }
            }
            Action::Wait { seconds } => {
                s.push_str("action = \"wait\"\n");
                s.push_str(&format!("seconds = {seconds}\n"));
            }
            Action::StreamWait {
                target,
                timeout_secs,
            } => {
                s.push_str("action = \"stream_wait\"\n");
                s.push_str(&format!("target = {}\n", toml_str(target)));
                s.push_str(&format!("timeout_secs = {timeout_secs}\n"));
            }
            Action::KeyPress { key } => {
                s.push_str("action = \"key_press\"\n");
                s.push_str(&format!("key = {}\n", toml_str(key)));
            }
            Action::If { .. } | Action::Group { .. } | Action::Capture { .. } => {
                s.push_str("# (control-flow actions are not produced by the recorder)\n");
            }
        }
        s.push('\n');
    }
    s
}

fn toml_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len().saturating_add(2));
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sels() -> Selectors {
        Selectors {
            input: Some("#prompt-textarea".into()),
            send_button: Some("[data-testid=send-button]".into()),
            stop_button: Some("[data-testid=stop-button]".into()),
            assistant_message: Some("[data-message-author-role=assistant]".into()),
            conversation_root: None,
        }
    }

    fn trace(events: Vec<TraceEvent>) -> Trace {
        Trace {
            started_at_ms: 0,
            ended_at_ms: 10_000,
            events,
        }
    }

    #[test]
    fn simple_flow_produces_input_send_streamwait() {
        let t = trace(vec![
            TraceEvent::Input {
                selector: "#prompt-textarea".into(),
                text: "hello".into(),
                seq: 1,
                at_ms: 100,
                src: "".into(),
            },
            TraceEvent::Click {
                selector: "[data-testid=send-button]".into(),
                seq: 2,
                at_ms: 500,
                src: "".into(),
            },
        ]);
        let p = propose_workflow(&t, &sels());
        assert_eq!(p.workflow.steps.len(), 3);
        let step = p.workflow.steps.first().unwrap_or_else(|| unreachable!());
        assert!(matches!(step, Action::FillInput { target, value }
            if target == "input_box" && value.is_none()));
        assert!(
            matches!(p.workflow.steps.get(1).unwrap_or_else(|| unreachable!()), Action::Click { target, .. }
            if target == "send_btn")
        );
        assert!(
            matches!(p.workflow.steps.get(2).unwrap_or_else(|| unreachable!()), Action::StreamWait { target, .. }
            if target == "assistant_message")
        );
        assert!(p.warnings.is_empty(), "{:?}", p.warnings);
    }

    #[test]
    fn keystroke_run_is_collapsed_to_one_input() {
        let mut events = Vec::new();
        for (i, v) in ["h", "he", "hel", "hell", "hello"].iter().enumerate() {
            events.push(TraceEvent::Input {
                selector: "#prompt-textarea".into(),
                text: v.to_string(),
                seq: (i + 1) as u64,
                at_ms: (i as u64 + 1) * 50,
                src: "".into(),
            });
        }
        let p = propose_workflow(&trace(events), &sels());
        // One FillInput + the appended StreamWait.
        assert_eq!(p.workflow.steps.len(), 2);
        let step = p.workflow.steps.first().unwrap_or_else(|| unreachable!());
        assert!(matches!(step, Action::FillInput { .. }));
    }

    #[test]
    fn duplicate_clicks_are_deduped() {
        let t = trace(vec![
            TraceEvent::Click {
                selector: "[data-testid=send-button]".into(),
                seq: 1,
                at_ms: 100,
                src: "".into(),
            },
            TraceEvent::Click {
                selector: "[data-testid=send-button]".into(),
                seq: 2,
                at_ms: 150,
                src: "".into(),
            },
        ]);
        let p = propose_workflow(&t, &sels());
        let clicks: Vec<&Action> = p
            .workflow
            .steps
            .iter()
            .filter(|a| matches!(a, Action::Click { .. }))
            .collect();
        assert_eq!(clicks.len(), 1);
    }

    #[test]
    fn unknown_click_becomes_raw_css_optional() {
        let t = trace(vec![TraceEvent::Click {
            selector: "#new-chat-button".into(),
            seq: 1,
            at_ms: 100,
            src: "".into(),
        }]);
        let p = propose_workflow(&t, &sels());
        let step = p.workflow.steps.first().unwrap_or_else(|| unreachable!());
        assert!(matches!(step, Action::Click { target, optional }
            if target == "#new-chat-button" && *optional));
    }

    #[test]
    fn loose_selector_match_accepts_quoted_attribute() {
        let t = trace(vec![TraceEvent::Click {
            selector: "button[data-testid=\"send-button\"]".into(),
            seq: 1,
            at_ms: 100,
            src: "".into(),
        }]);
        let p = propose_workflow(&t, &sels());
        let step = p.workflow.steps.first().unwrap_or_else(|| unreachable!());
        assert!(matches!(step, Action::Click { target, optional }
            if target == "send_btn" && !*optional));
    }

    #[test]
    fn long_text_is_left_as_pipeline_input() {
        let t = trace(vec![TraceEvent::Input {
            selector: "#prompt-textarea".into(),
            text: "Explain the Rust borrow checker in one paragraph.".into(),
            seq: 1,
            at_ms: 100,
            src: "".into(),
        }]);
        let p = propose_workflow(&t, &sels());
        let step = p.workflow.steps.first().unwrap_or_else(|| unreachable!());
        assert!(matches!(step, Action::FillInput { value, .. }
            if value.is_none()));
    }

    #[test]
    fn short_numeric_input_is_recorded_as_literal() {
        let t = trace(vec![TraceEvent::Input {
            selector: "#prompt-textarea".into(),
            text: "3".into(),
            seq: 1,
            at_ms: 100,
            src: "".into(),
        }]);
        let p = propose_workflow(&t, &sels());
        let step = p.workflow.steps.first().unwrap_or_else(|| unreachable!());
        assert!(matches!(step, Action::FillInput { value, .. }
            if value.as_deref() == Some("3")));
    }

    #[test]
    fn large_gap_becomes_wait() {
        let t = trace(vec![
            TraceEvent::Click {
                selector: "#a".into(),
                seq: 1,
                at_ms: 0,
                src: "".into(),
            },
            TraceEvent::Click {
                selector: "#b".into(),
                seq: 2,
                at_ms: 4_000,
                src: "".into(),
            },
        ]);
        let p = propose_workflow(&t, &sels());
        // Two clicks + one Wait between them + StreamWait.
        assert!(p
            .workflow
            .steps
            .iter()
            .any(|a| matches!(a, Action::Wait { .. })));
    }

    #[test]
    fn long_gap_wait_is_capped() {
        let t = trace(vec![
            TraceEvent::Click {
                selector: "#a".into(),
                seq: 1,
                at_ms: 0,
                src: "".into(),
            },
            TraceEvent::Click {
                selector: "#b".into(),
                seq: 2,
                at_ms: 3_600_000,
                src: "".into(),
            },
        ]);
        let p = propose_workflow(&t, &sels());
        let wait = p
            .workflow
            .steps
            .iter()
            .find_map(|a| {
                if let Action::Wait { seconds } = a {
                    Some(*seconds)
                } else {
                    None
                }
            })
            .unwrap_or_else(|| unreachable!("wait must exist"));
        assert!(wait <= MAX_WAIT_SECS);
    }

    #[test]
    fn empty_trace_warns() {
        let p = propose_workflow(&trace(vec![]), &sels());
        assert!(!p.warnings.is_empty());
    }

    #[test]
    fn toml_snippet_renders_actions() {
        let t = trace(vec![
            TraceEvent::Input {
                selector: "#prompt-textarea".into(),
                text: "hi".into(),
                seq: 1,
                at_ms: 0,
                src: "".into(),
            },
            TraceEvent::Click {
                selector: "[data-testid=send-button]".into(),
                seq: 2,
                at_ms: 100,
                src: "".into(),
            },
        ]);
        let p = propose_workflow(&t, &sels());
        let toml = workflow_to_toml_snippet("chatgpt", &p.workflow);
        assert!(toml.contains("[[providers.chatgpt.workflow]]"));
        assert!(toml.contains("action = \"fill_input\""));
        assert!(toml.contains("action = \"click\""));
        assert!(toml.contains("action = \"stream_wait\""));
    }

    #[test]
    fn toml_str_escapes_quotes_and_newlines() {
        assert_eq!(toml_str("a\"b"), "\"a\\\"b\"");
        assert_eq!(toml_str("a\nb"), "\"a\\nb\"");
    }
}
