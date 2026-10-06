//! # uwa-stealth
//!
//! Deterministic JS patches applied *before* any page script runs.
//!
//! ## Design
//!
//! We deliberately do **not** vendor fingerprint dictionaries. Any string
//! that could be validated against a real Chrome install (WebGL vendor,
//! plugin list, screen size, timezone) is user-provided via
//! `[stealth] user_scripts_dir` in config. Our built-ins are the *safe*
//! baseline that works on all Chromium builds and doesn't break sites.
//!
//! ## Passport (public API)
//! - [`StealthScript`] — name + JS body; `apply_before_load` flag
//! - [`StealthPack`] — ordered collection
//! - [`builtin::default_pack`] — safe baseline (`webdriver`, `chrome.runtime`,
//!   `languages`)
//! - [`builtin::full_pack`] — adds `permissions` normalization; opt-in
//! - [`apply::apply_pack`] — pushes scripts through `uwa_core::Page::eval_early`
//!
//! Everything is pure: `apply_pack` takes `&dyn Page` and returns
//! `Result<()>`; no globals, no side channels.

pub mod apply;
pub mod builtin;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StealthScript {
    pub name: String,
    /// JS source. Wrapped in an IIFE + try/catch at injection time.
    pub js: String,
    /// If `true`, applied via `Page::eval_early` so it runs on every
    /// navigation *before* any page script.
    /// If `false`, evaluated immediately in the current document.
    #[serde(default = "default_apply_before_load")]
    pub apply_before_load: bool,
}

fn default_apply_before_load() -> bool {
    true
}

impl StealthScript {
    pub fn new(name: impl Into<String>, js: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            js: js.into(),
            apply_before_load: true,
        }
    }

    /// Construct a script evaluated *now*, not on each navigation.
    pub fn current_document(name: impl Into<String>, js: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            js: js.into(),
            apply_before_load: false,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct StealthPack {
    scripts: Vec<StealthScript>,
}

impl StealthPack {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(mut self, script: StealthScript) -> Self {
        self.scripts.push(script);
        self
    }

    pub fn extend(mut self, other: StealthPack) -> Self {
        self.scripts.extend(other.scripts);
        self
    }

    pub fn scripts(&self) -> &[StealthScript] {
        &self.scripts
    }

    pub fn is_empty(&self) -> bool {
        self.scripts.is_empty()
    }

    pub fn len(&self) -> usize {
        self.scripts.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_pack() {
        let p = StealthPack::new();
        assert!(p.is_empty());
        assert_eq!(p.len(), 0);
    }

    #[test]
    fn pack_accumulates_in_order() {
        let p = StealthPack::new()
            .add(StealthScript::new("a", "1"))
            .add(StealthScript::new("b", "2"));
        assert_eq!(p.len(), 2);
        assert_eq!(p.scripts()[0].name, "a");
        assert_eq!(p.scripts()[1].name, "b");
    }

    #[test]
    fn extend_keeps_both() {
        let a = StealthPack::new().add(StealthScript::new("a", "1"));
        let b = StealthPack::new().add(StealthScript::new("b", "2"));
        let p = a.extend(b);
        assert_eq!(p.len(), 2);
    }

    #[test]
    fn current_document_flag_is_false() {
        let s = StealthScript::current_document("x", "1");
        assert!(!s.apply_before_load);
    }

    #[test]
    fn new_defaults_to_before_load() {
        let s = StealthScript::new("x", "1");
        assert!(s.apply_before_load);
    }
}
