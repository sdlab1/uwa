//! Executor for `Action` lists.

use std::collections::HashMap;
use std::time::Duration;
use uwa_config::Selectors;
use uwa_core::workflow::{Action, CaptureSource, Condition};
use uwa_core::{Page, UwaError};

#[derive(Debug, thiserror::Error)]
pub enum WorkflowError {
    #[error("workflow: unknown selector target `{0}`")]
    UnknownTarget(String),
    #[error("workflow: action failed: {0}")]
    ActionFailed(String),
    #[error(transparent)]
    Core(#[from] UwaError),
}

pub type WorkflowResult<T> = std::result::Result<T, WorkflowError>;

/// Runs an action list, carrying:
/// * the named-selector map (from `ProviderCfg.selectors`),
/// * a variable store populated by `CAPTURE`,
/// * the user text for `FILL_INPUT`.
pub struct WorkflowRunner<'a> {
    selectors: &'a Selectors,
    vars: HashMap<String, String>,
    user_text: String,
    /// Timeout for `STREAM_WAIT` — how long we're willing to wait for the
    /// assistant to stop streaming.
    stream_timeout: Duration,
}

impl<'a> WorkflowRunner<'a> {
    pub fn new(selectors: &'a Selectors, user_text: impl Into<String>) -> Self {
        Self {
            selectors,
            vars: HashMap::new(),
            user_text: user_text.into(),
            stream_timeout: Duration::from_secs(120),
        }
    }

    pub fn with_stream_timeout(mut self, d: Duration) -> Self {
        self.stream_timeout = d;
        self
    }

    pub fn vars(&self) -> &HashMap<String, String> {
        &self.vars
    }

    /// Execute the whole list.
    pub async fn run(
        &mut self,
        page: &dyn Page,
        wf: &uwa_core::workflow::Workflow,
    ) -> WorkflowResult<()> {
        for step in &wf.steps {
            self.run_one(page, step).await?;
        }
        Ok(())
    }

    async fn run_one(&mut self, page: &dyn Page, action: &Action) -> WorkflowResult<()> {
        // Async recursion (If/Group branches) requires boxing.
        Box::pin(self.run_one_inner(page, action)).await
    }

    async fn run_one_inner(&mut self, page: &dyn Page, action: &Action) -> WorkflowResult<()> {
        match action {
            Action::Click { target, optional } => {
                let sel = self.resolve(target)?;
                match crate::input::click_js(page, &sel).await {
                    Ok(()) => Ok(()),
                    Err(e) => {
                        if *optional {
                            tracing::debug!(target, "optional click skipped: {e}");
                            Ok(())
                        } else {
                            Err(WorkflowError::ActionFailed(format!(
                                "click `{target}`: {e}"
                            )))
                        }
                    }
                }
            }

            Action::FillInput { target, value } => {
                let sel = self.resolve(target)?;
                let text = match value {
                    Some(v) => v.clone(),
                    None => self.user_text.clone(),
                };
                crate::input::inject_text(page, &sel, &text).await?;
                Ok(())
            }

            Action::Wait { seconds } => {
                let d = Duration::from_secs_f32((*seconds).max(0.0));
                tokio::time::sleep(d).await;
                Ok(())
            }

            Action::StreamWait {
                target,
                timeout_secs,
            } => {
                let sel = self.resolve(target)?;
                let deadline = Duration::from_secs(*timeout_secs);
                let effective = deadline.min(self.stream_timeout);
                self.wait_for_stream_end(page, &sel, effective).await
            }

            Action::KeyPress { key } => {
                let js = format!(
                    r#"(function() {{
                        const ev = new KeyboardEvent('keydown', {{
                            key: {k}, bubbles: true, cancelable: true
                        }});
                        document.activeElement?.dispatchEvent(ev);
                        return true;
                    }})()"#,
                    k = serde_json::to_string(key).unwrap(),
                );
                page.eval(&js).await?;
                Ok(())
            }

            Action::If {
                condition,
                then,
                else_,
            } => {
                let branch = if self.eval_condition(page, condition).await? {
                    then
                } else {
                    else_
                };
                for step in branch {
                    self.run_one(page, step).await?;
                }
                Ok(())
            }

            Action::Group { label, steps } => {
                if !label.is_empty() {
                    tracing::debug!(label, "workflow group");
                }
                for step in steps {
                    self.run_one(page, step).await?;
                }
                Ok(())
            }

            Action::Capture { name, source } => {
                let value = self.eval_capture(page, source).await?;
                self.vars.insert(name.clone(), value);
                Ok(())
            }
        }
    }

    async fn eval_condition(&self, page: &dyn Page, c: &Condition) -> WorkflowResult<bool> {
        Ok(match c {
            Condition::SelectorExists { target } => {
                let sel = self.resolve(target)?;
                crate::input::exists(page, &sel).await.unwrap_or(false)
            }
            Condition::SelectorMissing { target } => {
                let sel = self.resolve(target)?;
                !crate::input::exists(page, &sel).await.unwrap_or(false)
            }
            Condition::VarEquals { name, value } => {
                self.vars.get(name).map(|v| v == value).unwrap_or(false)
            }
        })
    }

    async fn eval_capture(&self, page: &dyn Page, c: &CaptureSource) -> WorkflowResult<String> {
        match c {
            CaptureSource::Text { target } => {
                let sel = self.resolve(target)?;
                let js = format!(
                    r#"(() => {{
                        const el = document.querySelector({sel});
                        return el ? (el.innerText || el.textContent || '') : '';
                    }})()"#,
                    sel = serde_json::to_string(&sel).unwrap(),
                );
                let v = page.eval(&js).await?;
                Ok(v.as_str().unwrap_or("").to_string())
            }
            CaptureSource::Attribute { target, attr } => {
                let sel = self.resolve(target)?;
                let js = format!(
                    r#"(() => {{
                        const el = document.querySelector({sel});
                        return el ? (el.getAttribute({a}) || '') : '';
                    }})()"#,
                    sel = serde_json::to_string(&sel).unwrap(),
                    a = serde_json::to_string(attr).unwrap(),
                );
                let v = page.eval(&js).await?;
                Ok(v.as_str().unwrap_or("").to_string())
            }
        }
    }

    /// Poll until the assistant target has a stable subtree, or timeout.
    /// Uses the same "DOM stability" heuristic as `uwa-extract::finisher`.
    async fn wait_for_stream_end(
        &self,
        page: &dyn Page,
        selector: &str,
        timeout: Duration,
    ) -> WorkflowResult<()> {
        let start = std::time::Instant::now();
        let mut last_hash: Option<u64> = None;
        let mut stable_since: Option<std::time::Instant> = None;

        loop {
            if start.elapsed() >= timeout {
                return Ok(()); // best-effort
            }
            let js = format!(
                r#"(() => {{
                    const els = document.querySelectorAll({sel});
                    if (els.length === 0) return '';
                    const el = els[els.length - 1];
                    return el.innerText || el.textContent || '';
                }})()"#,
                sel = serde_json::to_string(selector).unwrap(),
            );
            let text = page
                .eval(&js)
                .await
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default();
            let h = hash_str(&text);
            match last_hash {
                Some(p) if p == h && !text.is_empty() => {
                    let since = stable_since.get_or_insert_with(std::time::Instant::now);
                    if since.elapsed() >= Duration::from_millis(700) {
                        return Ok(());
                    }
                }
                _ => {
                    last_hash = Some(h);
                    stable_since = None;
                }
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
    }

    /// Resolve a named selector against `ProviderCfg.selectors`.
    ///
    /// Unknown names fall back to the variable store — so a `CAPTURE`
    /// earlier in the workflow can provide a raw CSS selector for later
    /// steps.
    fn resolve(&self, name: &str) -> WorkflowResult<String> {
        let raw = match name {
            "input_box" | "input" => self.selectors.input.as_deref(),
            "send_btn" | "send_button" => self.selectors.send_button.as_deref(),
            "stop_btn" | "stop_button" => self.selectors.stop_button.as_deref(),
            "result_container" | "assistant_message" => self.selectors.assistant_message.as_deref(),
            "conversation_root" => self.selectors.conversation_root.as_deref(),
            other => {
                // Allow capturing custom selectors via `CAPTURE` earlier
                // in the workflow.
                return self
                    .vars
                    .get(other)
                    .cloned()
                    .ok_or_else(|| WorkflowError::UnknownTarget(other.to_string()));
            }
        };
        raw.map(|s| s.to_string())
            .ok_or_else(|| WorkflowError::UnknownTarget(name.to_string()))
    }
}

fn hash_str(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::ScriptedPage;
    use serde_json::json;
    use uwa_config::Selectors;
    use uwa_core::workflow::Workflow;

    fn selectors() -> Selectors {
        Selectors {
            input: Some("#prompt".into()),
            send_button: Some("button.send".into()),
            stop_button: Some("button.stop".into()),
            assistant_message: Some("[data-role=assistant]".into()),
            conversation_root: None,
        }
    }

    #[tokio::test]
    async fn click_resolves_named_selector() {
        let p = ScriptedPage::new(vec![("el.click()", json!(true))]);
        let sels = selectors();
        let mut runner = WorkflowRunner::new(&sels, "hi");
        let wf = Workflow {
            steps: vec![Action::Click {
                target: "send_btn".into(),
                optional: false,
            }],
        };
        runner.run(&p, &wf).await.unwrap();
        assert!(p.log().iter().any(|s| s.contains("button.send")));
    }

    #[tokio::test]
    async fn optional_click_skips_missing() {
        // default eval returns `false` → click_js fails → optional skips.
        let p = ScriptedPage::new(vec![]);
        let sels = selectors();
        let mut runner = WorkflowRunner::new(&sels, "hi");
        let wf = Workflow {
            steps: vec![Action::Click {
                target: "send_btn".into(),
                optional: true,
            }],
        };
        runner.run(&p, &wf).await.unwrap();
    }

    #[tokio::test]
    async fn mandatory_click_fails_when_missing() {
        let p = ScriptedPage::new(vec![]);
        let sels = selectors();
        let mut runner = WorkflowRunner::new(&sels, "hi");
        let wf = Workflow {
            steps: vec![Action::Click {
                target: "send_btn".into(),
                optional: false,
            }],
        };
        assert!(runner.run(&p, &wf).await.is_err());
    }

    #[tokio::test]
    async fn fill_input_uses_user_text_when_value_missing() {
        let p = ScriptedPage::new(vec![("setter.call(el", json!({"ok": true}))]);
        let sels = selectors();
        let mut runner = WorkflowRunner::new(&sels, "the prompt");
        let wf = Workflow {
            steps: vec![Action::FillInput {
                target: "input_box".into(),
                value: None,
            }],
        };
        runner.run(&p, &wf).await.unwrap();
        let logs = p.log();
        let inject = logs
            .iter()
            .find(|s| s.contains("setter.call"))
            .expect("inject ran");
        assert!(inject.contains("the prompt"));
    }

    #[tokio::test]
    async fn fill_input_literal_value() {
        let p = ScriptedPage::new(vec![("setter.call(el", json!({"ok": true}))]);
        let sels = selectors();
        let mut runner = WorkflowRunner::new(&sels, "ignored");
        let wf = Workflow {
            steps: vec![Action::FillInput {
                target: "input_box".into(),
                value: Some("literal value".into()),
            }],
        };
        runner.run(&p, &wf).await.unwrap();
        let logs = p.log();
        let inject = logs.iter().find(|s| s.contains("setter.call")).unwrap();
        assert!(inject.contains("literal value"));
        assert!(!inject.contains("ignored"));
    }

    #[tokio::test]
    async fn wait_sleeps() {
        let p = ScriptedPage::new(vec![]);
        let sels = selectors();
        let mut runner = WorkflowRunner::new(&sels, "");
        let wf = Workflow {
            steps: vec![Action::Wait { seconds: 0.05 }],
        };
        let start = std::time::Instant::now();
        runner.run(&p, &wf).await.unwrap();
        assert!(start.elapsed() >= Duration::from_millis(40));
    }

    #[tokio::test]
    async fn if_branch_takes_else_when_missing() {
        let p = ScriptedPage::new(vec![
            ("querySelector(\"button.stop\")", json!(false)),
            ("el.click()", json!(true)),
        ]);
        let sels = selectors();
        let mut runner = WorkflowRunner::new(&sels, "");
        let wf = Workflow {
            steps: vec![Action::If {
                condition: Condition::SelectorExists {
                    target: "stop_btn".into(),
                },
                then: vec![Action::Click {
                    target: "send_btn".into(),
                    optional: false,
                }],
                else_: vec![Action::Wait { seconds: 0.01 }],
            }],
        };
        runner.run(&p, &wf).await.unwrap();
        // then-branch click should NOT have run
        assert!(!p.log().iter().any(|s| s.contains("button.send")));
    }

    #[tokio::test]
    async fn if_branch_takes_then_when_present() {
        let p = ScriptedPage::new(vec![
            ("querySelector(\"button.stop\")", json!(true)),
            ("el.click()", json!(true)),
        ]);
        let sels = selectors();
        let mut runner = WorkflowRunner::new(&sels, "");
        let wf = Workflow {
            steps: vec![Action::If {
                condition: Condition::SelectorExists {
                    target: "stop_btn".into(),
                },
                then: vec![Action::Click {
                    target: "send_btn".into(),
                    optional: false,
                }],
                else_: vec![],
            }],
        };
        runner.run(&p, &wf).await.unwrap();
        assert!(p.log().iter().any(|s| s.contains("button.send")));
    }

    #[tokio::test]
    async fn capture_populates_vars() {
        let p = ScriptedPage::new(vec![("innerText", json!("captured value"))]);
        let sels = selectors();
        let mut runner = WorkflowRunner::new(&sels, "");
        let wf = Workflow {
            steps: vec![Action::Capture {
                name: "greeting".into(),
                source: CaptureSource::Text {
                    target: "assistant_message".into(),
                },
            }],
        };
        runner.run(&p, &wf).await.unwrap();
        assert_eq!(
            runner.vars().get("greeting").map(String::as_str),
            Some("captured value")
        );
    }

    #[tokio::test]
    async fn captured_var_can_drive_condition_and_custom_selector() {
        // Capture a raw selector, then use it as the target of a click.
        let p = ScriptedPage::new(vec![
            ("innerText", json!("button.custom")),
            ("el.click()", json!(true)),
        ]);
        let sels = selectors();
        let mut runner = WorkflowRunner::new(&sels, "");
        let wf = Workflow {
            steps: vec![
                Action::Capture {
                    name: "my_btn".into(),
                    source: CaptureSource::Text {
                        target: "assistant_message".into(),
                    },
                },
                Action::If {
                    condition: Condition::VarEquals {
                        name: "my_btn".into(),
                        value: "button.custom".into(),
                    },
                    then: vec![Action::Click {
                        target: "my_btn".into(),
                        optional: false,
                    }],
                    else_: vec![],
                },
            ],
        };
        runner.run(&p, &wf).await.unwrap();
        assert!(p.log().iter().any(|s| s.contains("button.custom")));
    }

    #[tokio::test]
    async fn group_executes_children() {
        let p = ScriptedPage::new(vec![
            ("setter.call(el", json!({"ok": true})),
            ("el.click()", json!(true)),
        ]);
        let sels = selectors();
        let mut runner = WorkflowRunner::new(&sels, "hi");
        let wf = Workflow {
            steps: vec![Action::Group {
                label: "send flow".into(),
                steps: vec![
                    Action::FillInput {
                        target: "input_box".into(),
                        value: None,
                    },
                    Action::Click {
                        target: "send_btn".into(),
                        optional: false,
                    },
                ],
            }],
        };
        runner.run(&p, &wf).await.unwrap();
    }

    #[tokio::test]
    async fn unknown_target_errors() {
        let p = ScriptedPage::new(vec![]);
        let sels = selectors();
        let mut runner = WorkflowRunner::new(&sels, "");
        let wf = Workflow {
            steps: vec![Action::Click {
                target: "definitely_not_a_selector".into(),
                optional: false,
            }],
        };
        let e = runner.run(&p, &wf).await.unwrap_err();
        assert!(matches!(e, WorkflowError::UnknownTarget(_)));
    }

    #[tokio::test]
    async fn stream_wait_returns_on_stable_text() {
        // The scripted page returns the same text for every eval after the
        // first; stability requires 700ms of identical hashes — the test
        // page answers instantly, so the wait completes quickly.
        let p = ScriptedPage::new(vec![
            ("innerText", json!("steady answer")),
            ("innerText", json!("steady answer")),
            ("innerText", json!("steady answer")),
            ("innerText", json!("steady answer")),
            ("innerText", json!("steady answer")),
            ("innerText", json!("steady answer")),
        ]);
        let sels = selectors();
        let mut runner = WorkflowRunner::new(&sels, "");
        let wf = Workflow {
            steps: vec![Action::StreamWait {
                target: "assistant_message".into(),
                timeout_secs: 5,
            }],
        };
        runner.run(&p, &wf).await.unwrap();
    }
}
