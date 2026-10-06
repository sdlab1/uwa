//! Built-in stealth packs.
//!
//! Everything here is **deterministic**: same input, same output, no
//! randomness. That's on purpose — random fingerprints are more suspicious
//! than uniform ones, because they can't be correlated against real Chrome
//! distributions.

use crate::{StealthPack, StealthScript};

/// Safe baseline. Only patches things that are:
/// 1. obviously headless (`navigator.webdriver`);
/// 2. missing in headless (`window.chrome.runtime`);
/// 3. typed wrong in some Chromium builds (`navigator.languages`);
///
/// We do **not** spoof WebGL, plugins, screen, or UA here.
pub fn default_pack() -> StealthPack {
    StealthPack::new()
        .add(StealthScript::new(
            "webdriver",
            r#"Object.defineProperty(navigator, 'webdriver', { get: () => undefined });"#,
        ))
        .add(StealthScript::new(
            "chrome-runtime",
            r#"
            if (!window.chrome) { window.chrome = {}; }
            if (!window.chrome.runtime) { window.chrome.runtime = {}; }
            "#,
        ))
        .add(StealthScript::new(
            "languages",
            r#"
            Object.defineProperty(navigator, 'languages', {
                get: () => ['en-US', 'en']
            });
            "#,
        ))
}

/// Same as [`default_pack`] plus a permissions normalization that some
/// fingerprinters check. Never enable by default — it can break sites that
/// legitimately query `Notification.permission` and expect `'default'`.
pub fn full_pack() -> StealthPack {
    default_pack().add(StealthScript::new(
        "permissions-normalize",
        r#"
        const orig = navigator.permissions.query.bind(navigator.permissions);
        navigator.permissions.query = (p) => {
            if (p && p.name === 'notifications') {
                return Promise.resolve({ state: Notification.permission });
            }
            return orig(p);
        };
        "#,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_pack_has_three_scripts() {
        assert_eq!(default_pack().len(), 3);
    }

    #[test]
    fn full_pack_is_superset() {
        assert!(full_pack().len() > default_pack().len());
        assert_eq!(full_pack().len(), default_pack().len() + 1);
    }

    #[test]
    fn all_scripts_apply_before_load() {
        for s in default_pack().scripts() {
            assert!(
                s.apply_before_load,
                "script `{}` must be before_load",
                s.name
            );
        }
    }

    #[test]
    fn scripts_are_non_empty_and_named() {
        for s in full_pack().scripts() {
            assert!(!s.name.is_empty());
            assert!(!s.js.trim().is_empty());
        }
    }

    #[test]
    fn webdriver_patch_is_present() {
        let names: Vec<_> = default_pack()
            .scripts()
            .iter()
            .map(|s| s.name.clone())
            .collect();
        assert!(names.iter().any(|n| n == "webdriver"));
    }
}
