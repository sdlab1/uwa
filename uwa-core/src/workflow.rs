//! # Declarative workflow types
//!
//! A workflow is a list of actions executed in order against a `Page`.
//! Actions reference **named selectors** from `ProviderCfg.selectors`
//! (`"input_box"`, `"send_btn"`, ...), not raw CSS — so per-site configs
//! stay readable and versionable.
//!
//! The types live in `uwa-core` (pure serde, no `Page` dependency) so
//! `uwa-config` can embed a `Workflow` in `ProviderCfg` without a
//! circular dependency. Execution lives in `uwa-providers`.
//!
//! ## Supported actions
//!
//! | Action | Purpose |
//! |---|---|
//! | `CLICK` | `selector.click()` |
//! | `FILL_INPUT` | JS-inject text into a named selector |
//! | `WAIT` | sleep `seconds` |
//! | `STREAM_WAIT` | poll the target until its text stabilizes |
//! | `KEY_PRESS` | dispatch a key event (e.g. `Enter`) |
//! | `IF` / `ELSE` | conditional branch |
//! | `GROUP` | labeled list (documentation only) |
//! | `CAPTURE` | store text/attr into a variable |
//!
//! ## Example
//!
//! ```toml
//! [[providers.chatgpt.workflow]]
//! action = "click"
//! target = "new_chat_btn"
//! optional = true
//!
//! [[providers.chatgpt.workflow]]
//! action = "fill_input"
//! target = "input_box"
//!
//! [[providers.chatgpt.workflow]]
//! action = "click"
//! target = "send_btn"
//!
//! [[providers.chatgpt.workflow]]
//! action = "stream_wait"
//! target = "assistant_message"
//! ```

use serde::{Deserialize, Serialize};

/// A workflow is just a list of actions. Empty = use the default
/// "fill + click + wait_response" path.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(transparent)]
pub struct Workflow {
    pub steps: Vec<Action>,
}

impl Workflow {
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }
}

/// One step. Tagged externally so TOML reads naturally:
///
/// ```toml
/// action = "click"
/// target = "send_btn"
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    /// Click a named selector.
    Click {
        target: String,
        #[serde(default)]
        optional: bool,
    },

    /// Fill a named selector. `value = None` means: use the incoming
    /// user text (the whole pipeline prompt).
    FillInput {
        target: String,
        #[serde(default)]
        value: Option<String>,
    },

    /// Fixed-duration pause. `seconds` is fractional.
    Wait { seconds: f32 },

    /// Wait for extraction of the target selector. This does **not**
    /// return the text — it just marks "response is done"; the caller
    /// extracts the text with `page.html()`.
    StreamWait {
        target: String,
        #[serde(default = "default_stream_timeout")]
        timeout_secs: u64,
    },

    /// Dispatch a key. Currently supports `Enter`, `Escape`, `Tab`.
    KeyPress { key: String },

    /// Conditional branch.
    If {
        condition: Condition,
        then: Vec<Action>,
        #[serde(default, rename = "else")]
        else_: Vec<Action>,
    },

    /// Documentation-only grouping. Executes `steps` in order.
    Group {
        #[serde(default)]
        label: String,
        steps: Vec<Action>,
    },

    /// Capture text or an attribute into the runner's variable store.
    Capture { name: String, source: CaptureSource },
}

fn default_stream_timeout() -> u64 {
    120
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Condition {
    /// True if the named selector exists.
    SelectorExists { target: String },
    /// True if the named selector is absent.
    SelectorMissing { target: String },
    /// True if the captured variable equals `value`.
    VarEquals { name: String, value: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CaptureSource {
    /// `element.innerText`.
    Text { target: String },
    /// `element.getAttribute(attr)`.
    Attribute { target: String, attr: String },
}

// ---------------------------------------------------------------------------
// Recording
// ---------------------------------------------------------------------------
//
// A `Trace` is a raw sequence of DOM interactions a user performed during
// a recording session. It's produced by `uwa-browser::recorder` from a JS
// observer, carried over the HTTP layer as JSON, and consumed by
// `uwa-providers::workflow::propose_workflow` to synthesise a `Workflow`.
//
// The types live here so the producer (`uwa-browser`) and the consumer
// (`uwa-providers`) share one definition without a dependency edge
// between them.

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Trace {
    /// Wall-clock start of the recording, milliseconds since Unix epoch.
    #[serde(default)]
    pub started_at_ms: u64,
    /// Wall-clock end, milliseconds since Unix epoch.
    #[serde(default)]
    pub ended_at_ms: u64,
    pub events: Vec<TraceEvent>,
}

impl Trace {
    /// Events observed between two wall-clock timestamps, inclusive of
    /// the lower bound. Used by tests and by the dashboard's timeline
    /// scrubber.
    pub fn events_between(&self, from_ms: u64, to_ms: u64) -> Vec<&TraceEvent> {
        self.events
            .iter()
            .filter(|e| {
                let at = e.at_ms();
                at >= from_ms && at <= to_ms
            })
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TraceEvent {
    Click {
        selector: String,
        #[serde(default)]
        seq: u64,
        #[serde(default)]
        at_ms: u64,
        #[serde(default)]
        src: String,
    },
    Input {
        selector: String,
        text: String,
        #[serde(default)]
        seq: u64,
        #[serde(default)]
        at_ms: u64,
        #[serde(default)]
        src: String,
    },
    /// Explicit Enter; the recorder emits these so a flow that submits by
    /// pressing Enter rather than clicking a button is captured.
    #[serde(rename = "keypress")]
    KeyPress {
        key: String,
        #[serde(default)]
        selector: String,
        #[serde(default)]
        seq: u64,
        #[serde(default)]
        at_ms: u64,
        #[serde(default)]
        src: String,
    },
    Submit {
        #[serde(default)]
        selector: String,
        #[serde(default)]
        seq: u64,
        #[serde(default)]
        at_ms: u64,
        #[serde(default)]
        src: String,
    },
}

impl TraceEvent {
    pub fn at_ms(&self) -> u64 {
        match self {
            Self::Click { at_ms, .. }
            | Self::Input { at_ms, .. }
            | Self::KeyPress { at_ms, .. }
            | Self::Submit { at_ms, .. } => *at_ms,
        }
    }

    pub fn seq(&self) -> u64 {
        match self {
            Self::Click { seq, .. }
            | Self::Input { seq, .. }
            | Self::KeyPress { seq, .. }
            | Self::Submit { seq, .. } => *seq,
        }
    }

    pub fn selector(&self) -> &str {
        match self {
            Self::Click { selector, .. }
            | Self::Input { selector, .. }
            | Self::KeyPress { selector, .. }
            | Self::Submit { selector, .. } => selector,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn click_parses() -> Result<(), Box<dyn std::error::Error>> {
        let toml = r#"
            action = "click"
            target = "send_btn"
            optional = true
        "#;
        let a: Action = toml_edit::de::from_str(toml)?;
        assert_eq!(
            a,
            Action::Click {
                target: "send_btn".into(),
                optional: true
            }
        );
        Ok(())
    }

    #[test]
    fn fill_input_defaults_value_to_none() -> Result<(), Box<dyn std::error::Error>> {
        let toml = r#"
            action = "fill_input"
            target = "input_box"
        "#;
        let a: Action = toml_edit::de::from_str(toml)?;
        assert_eq!(
            a,
            Action::FillInput {
                target: "input_box".into(),
                value: None
            }
        );
        Ok(())
    }

    #[test]
    fn if_parses_with_else() -> Result<(), Box<dyn std::error::Error>> {
        let toml = r#"
            action = "if"
            [condition]
            kind = "selector_exists"
            target = "new_chat_btn"
            [[then]]
            action = "click"
            target = "new_chat_btn"
            [[else]]
            action = "wait"
            seconds = 0.5
        "#;
        let a: Action = toml_edit::de::from_str(toml)?;
        match a {
            Action::If {
                condition,
                then,
                else_,
            } => {
                assert_eq!(
                    condition,
                    Condition::SelectorExists {
                        target: "new_chat_btn".into()
                    }
                );
                assert_eq!(then.len(), 1);
                assert_eq!(else_.len(), 1);
                Ok(())
            }
            _ => Err("expected If".into()),
        }
    }

    #[test]
    fn wait_fractional_seconds() -> Result<(), Box<dyn std::error::Error>> {
        let a: Action = toml_edit::de::from_str(
            r#"action = "wait"
seconds = 0.25"#,
        )?;
        match a {
            Action::Wait { seconds } => assert!((seconds - 0.25).abs() < 0.001),
            _ => return Err("expected Wait".into()),
        }
        Ok(())
    }

    #[test]
    fn capture_parses() -> Result<(), Box<dyn std::error::Error>> {
        let toml = r#"
            action = "capture"
            name = "greeting"
            [source]
            kind = "text"
            target = "assistant_message"
        "#;
        let a: Action = toml_edit::de::from_str(toml)?;
        assert_eq!(
            a,
            Action::Capture {
                name: "greeting".into(),
                source: CaptureSource::Text {
                    target: "assistant_message".into()
                }
            }
        );
        Ok(())
    }

    #[test]
    fn workflow_is_transparent_list() -> Result<(), Box<dyn std::error::Error>> {
        let toml = r#"
            [[x]]
            action = "wait"
            seconds = 0.1
            [[x]]
            action = "click"
            target = "send_btn"
        "#;
        // A workflow is `#[serde(transparent)]`: any table name works.
        #[derive(serde::Deserialize)]
        struct Wrap {
            x: Workflow,
        }
        let w: Wrap = toml_edit::de::from_str(toml)?;
        assert_eq!(w.x.steps.len(), 2);
        assert!(!w.x.is_empty());
        Ok(())
    }

    #[test]
    fn empty_workflow_is_default() -> Result<(), Box<dyn std::error::Error>> {
        let w = Workflow::default();
        assert!(w.is_empty());
        let s = serde_json::to_string(&w)?;
        assert_eq!(s, "[]");
        Ok(())
    }

    #[test]
    fn trace_event_at_ms_is_uniform() {
        let click = TraceEvent::Click {
            selector: "#x".into(),
            seq: 1,
            at_ms: 100,
            src: String::new(),
        };
        let input = TraceEvent::Input {
            selector: "#y".into(),
            text: "hi".into(),
            seq: 2,
            at_ms: 200,
            src: String::new(),
        };
        assert_eq!(click.at_ms(), 100);
        assert_eq!(input.at_ms(), 200);
        assert_eq!(click.seq(), 1);
        assert_eq!(input.seq(), 2);
    }

    #[test]
    fn trace_roundtrips_through_json() -> Result<(), Box<dyn std::error::Error>> {
        let trace = Trace {
            started_at_ms: 1,
            ended_at_ms: 2,
            events: vec![
                TraceEvent::Click {
                    selector: "#a".into(),
                    seq: 1,
                    at_ms: 10,
                    src: "https://x/".into(),
                },
                TraceEvent::Input {
                    selector: "#b".into(),
                    text: "hi".into(),
                    seq: 2,
                    at_ms: 20,
                    src: "https://x/".into(),
                },
            ],
        };
        let j = serde_json::to_string(&trace)?;
        let back: Trace = serde_json::from_str(&j)?;
        assert_eq!(back.events.len(), 2);
        let first = back.events.first();
        assert!(matches!(first, Some(TraceEvent::Click { selector, .. }) if selector == "#a"));
        Ok(())
    }
}
