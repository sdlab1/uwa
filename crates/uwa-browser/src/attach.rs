//! Inject stealth scripts before any page JavaScript runs.

use chromiumoxide::cdp::browser_protocol::page::AddScriptToEvaluateOnNewDocumentParams;
use chromiumoxide::Page as CdpPage;
use uwa_core::{Result, UwaError};
use uwa_stealth::StealthPack;

/// Register every `apply_before_load` script on `page`.
///
/// Scripts are wrapped in an IIFE with a `try/catch` so one broken payload
/// cannot break navigation.
pub async fn attach_stealth(page: &CdpPage, pack: &StealthPack) -> Result<()> {
    for script in pack.scripts() {
        if !script.apply_before_load {
            continue;
        }
        let params = AddScriptToEvaluateOnNewDocumentParams::new(wrap(&script.js));
        page.execute(params).await.map_err(|e| {
            UwaError::Transport(format!("add_init_script `{}`: {e}", script.name))
        })?;
    }
    Ok(())
}

/// Wrap a payload in an IIFE guarded by `try/catch`.
fn wrap(js: &str) -> String {
    format!("(function(){{try{{{js}}}catch(_e){{}}}})();")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_shields_every_payload() {
        let pack = uwa_stealth::builtin::default_pack();
        assert!(!pack.is_empty());
        for s in pack.scripts() {
            let wrapped = wrap(&s.js);
            assert!(wrapped.starts_with("(function(){try{"));
            assert!(wrapped.ends_with("catch(_e){}})();"));
            assert!(wrapped.contains(&s.js));
        }
    }

    #[test]
    fn wrap_contains_no_newlines_that_break_init_scripts() {
        let wrapped = wrap("throw new Error('x');");
        assert_eq!(
            wrapped,
            "(function(){try{throw new Error('x');}catch(_e){}})();"
        );
    }
}
