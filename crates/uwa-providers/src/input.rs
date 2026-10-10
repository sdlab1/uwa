//! Robust DOM input helpers shared by every [`uwa_core::SiteProvider`].
//!
//! Each helper is one self-contained JS snippet evaluated through
//! [`Page::eval`](uwa_core::Page::eval), so a scripted test page can answer
//! them without a real browser.

use std::time::Duration;
use uwa_core::{Page, Result, UwaError};

fn quoted(s: &str) -> String {
    serde_json::to_string(s).expect("serializing a &str never fails")
}

/// Type `text` into `selector`, firing the events frameworks listen for.
///
/// Works for `<textarea>`/`<input>` (native value setter + `input`/`change`)
/// and for contenteditable containers (innerText + `insertText`).
pub async fn inject_text(page: &dyn Page, selector: &str, text: &str) -> Result<()> {
    let js = format!(
        r#"
        (() => {{
            const el = document.querySelector({sel});
            if (!el) return {{ ok: false, reason: "no-element" }};
            el.focus();
            const tag = el.tagName;
            const isField = tag === "TEXTAREA" || tag === "INPUT";
            if (isField) {{
                const proto = tag === "TEXTAREA" ? window.HTMLTextAreaElement.prototype : window.HTMLInputElement.prototype;
                const setter = Object.getOwnPropertyDescriptor(proto, "value").set;
                setter.call(el, ""); setter.call(el, {txt});
                el.dispatchEvent(new Event("input", {{ bubbles: true }}));
                el.dispatchEvent(new Event("change", {{ bubbles: true }}));
            }} else {{
                el.innerText = ""; el.innerText = {txt};
                el.dispatchEvent(new InputEvent("input", {{ bubbles: true, inputType: "insertText", data: {txt} }}));
            }}
            return {{ ok: true }};
        }})()
    "#,
        sel = quoted(selector),
        txt = quoted(text)
    );
    let v = page.eval(&js).await?;
    if v.get("ok").and_then(|b| b.as_bool()) != Some(true) {
        return Err(UwaError::Transport(format!("inject `{selector}`: {v:?}")));
    }
    Ok(())
}

/// Does `selector` match at least one element?
pub async fn exists(page: &dyn Page, selector: &str) -> Result<bool> {
    let js = format!(r#"!!document.querySelector({sel})"#, sel = quoted(selector));
    Ok(page.eval(&js).await?.as_bool().unwrap_or(false))
}

/// Is `selector` present *and* not `disabled`?
pub async fn is_enabled(page: &dyn Page, selector: &str) -> Result<bool> {
    let js = format!(
        r#"(() => {{ const el = document.querySelector({sel}); return !!el && !el.disabled; }})()"#,
        sel = quoted(selector)
    );
    Ok(page.eval(&js).await?.as_bool().unwrap_or(false))
}

/// Native `el.click()` — bypasses overlay/input quirks of synthetic clicks.
pub async fn click_js(page: &dyn Page, selector: &str) -> Result<()> {
    let js = format!(
        r#"(() => {{ const el = document.querySelector({sel}); if (!el) return false; el.click(); return true; }})()"#,
        sel = quoted(selector)
    );
    if page.eval(&js).await?.as_bool() == Some(true) {
        Ok(())
    } else {
        Err(UwaError::Transport(format!(
            "click `{selector}`: not found"
        )))
    }
}

/// Is `selector` an empty input/textarea (or has no text)?
pub async fn is_empty(page: &dyn Page, selector: &str) -> Result<bool> {
    let js = format!(
        r#"(() => {{ const el = document.querySelector({sel}); if (!el) return true;
        const v = el.value !== undefined ? el.value : el.innerText;
        return !v || v.trim().length === 0; }})()"#,
        sel = quoted(selector)
    );
    Ok(page.eval(&js).await?.as_bool().unwrap_or(true))
}

/// How many elements match `selector`?
pub async fn count(page: &dyn Page, selector: &str) -> Result<u32> {
    let js = format!(
        r#"document.querySelectorAll({sel}).length"#,
        sel = quoted(selector)
    );
    Ok(page.eval(&js).await?.as_u64().unwrap_or(0) as u32)
}

/// Wait until `selector` exists, retrying transient eval failures.
///
/// Returns [`UwaError::Timeout`] once `timeout` elapses.
pub async fn wait_exists(page: &dyn Page, selector: &str, timeout: Duration) -> Result<()> {
    let start = std::time::Instant::now();
    let mut probe = uwa_resilience::RetryCfg::new(2);
    probe.base = Duration::from_millis(50);
    probe.max = Duration::from_millis(100);
    loop {
        let hit = uwa_resilience::retry(&probe, || exists(page, selector))
            .await
            .unwrap_or(false);
        if hit {
            return Ok(());
        }
        if start.elapsed() >= timeout {
            return Err(UwaError::Timeout(timeout));
        }
        tokio::time::sleep(Duration::from_millis(80)).await;
    }
}
