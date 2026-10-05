//! Push a [`StealthPack`] into a [`Page`].

use uwa_core::{Page, Result};

use crate::StealthPack;

/// Wrap a script so a syntax error in one payload can't break navigation.
pub fn wrap(js: &str) -> String {
    format!("(function(){{try{{{}}}catch(_e){{}}}})();", js)
}

/// Register every `apply_before_load` script via `Page::eval_early`, then run
/// the remaining ones immediately via `Page::eval`.
pub async fn apply_pack(page: &dyn Page, pack: &StealthPack) -> Result<()> {
    for script in pack.scripts() {
        let wrapped = wrap(&script.js);
        if script.apply_before_load {
            page.eval_early(&wrapped).await?;
        } else {
            page.eval(&wrapped).await?;
        }
    }
    Ok(())
}

/// Helper for callers that want the raw wrapped form.
pub fn wrapped_scripts(pack: &StealthPack) -> Vec<String> {
    pack.scripts().iter().map(|s| wrap(&s.js)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use uwa_testkit::MockPage;

    #[tokio::test]
    async fn apply_uses_eval_early_for_before_load_scripts() {
        let page = MockPage::new();
        let pack = crate::StealthPack::new()
            .with_script(crate::StealthScript {
                name: "early".into(),
                js: "a".into(),
                apply_before_load: true,
            })
            .with_script(crate::StealthScript {
                name: "late".into(),
                js: "b".into(),
                apply_before_load: false,
            });
        apply_pack(&page, &pack).await.unwrap();
        let early: Vec<String> = page
            .log()
            .into_iter()
            .filter(|e| e.starts_with("early:"))
            .collect();
        let live: Vec<String> = page
            .log()
            .into_iter()
            .filter(|e| !e.starts_with("early:"))
            .collect();
        assert_eq!(early.len(), 1, "{early:?}");
        assert_eq!(live.len(), 1, "{live:?}");
        assert!(early[0].contains("try{a}"), "{:?}", early[0]);
    }
}
