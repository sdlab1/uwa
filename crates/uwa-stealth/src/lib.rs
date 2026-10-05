//! # uwa-stealth
//!
//! Anti-detection scripts injected into every page before site JS runs.
//!
//! ## Passport (public API)
//! - [`StealthPack`], [`StealthScript`]
//! - [`builtin::default_pack`], [`builtin::full_pack`]
//! - [`apply::apply_pack`]

pub mod apply;
pub mod builtin;

pub use apply::apply_pack;
pub use builtin::{default_pack, full_pack};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StealthScript {
    pub name: String,
    pub js: String,
    #[serde(default = "yes")]
    pub apply_before_load: bool,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Default)]
pub struct StealthPack {
    scripts: Vec<StealthScript>,
}

impl StealthPack {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_script(mut self, s: StealthScript) -> Self {
        self.scripts.push(s);
        self
    }

    pub fn scripts(&self) -> &[StealthScript] {
        &self.scripts
    }

    pub fn len(&self) -> usize {
        self.scripts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.scripts.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn script(name: &str, before: bool) -> StealthScript {
        StealthScript {
            name: name.into(),
            js: format!("/* {name} */"),
            apply_before_load: before,
        }
    }

    #[test]
    fn pack_accumulates() {
        let p = StealthPack::new()
            .with_script(script("a", true))
            .with_script(script("b", false));
        assert_eq!(p.len(), 2);
        assert!(!p.is_empty());
        assert_eq!(p.scripts()[0].name, "a");
    }

    #[test]
    fn default_pack_has_core_scripts() {
        let p = default_pack();
        assert!(!p.is_empty());
        assert!(p.scripts().iter().all(|s| s.apply_before_load));
    }

    #[test]
    fn full_pack_is_superset_of_default() {
        let d = default_pack();
        let f = full_pack();
        assert!(f.len() > d.len());
        for s in d.scripts() {
            assert!(
                f.scripts().iter().any(|x| x.name == s.name),
                "missing {}",
                s.name
            );
        }
    }
}
