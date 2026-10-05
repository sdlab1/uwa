//! `FrameId` -> target attribution.
//!
//! CDP reports network events against a `frameId`. Frames can belong to a
//! different process (OOPIF), so we resolve a frame to the target that owns it
//! before publishing events on the [`crate::NetBus`].

use std::collections::HashMap;

#[derive(Debug, Clone, Default)]
pub struct FrameMap {
    /// Explicit target for frames we could not resolve (the page we pump).
    root: Option<String>,
    map: HashMap<String, String>,
}

impl FrameMap {
    /// Frame attribution falls back to the id prefix (`a.b` -> `a`).
    pub fn new() -> Self {
        Self::default()
    }

    /// Every unresolved frame belongs to `target`.
    pub fn for_target(target: impl Into<String>) -> Self {
        Self {
            root: Some(target.into()),
            map: HashMap::new(),
        }
    }

    /// Register a frame and return the target that owns it.
    pub fn attach(&mut self, frame_id: &str, parent: Option<&str>) -> String {
        let target = parent
            .and_then(|p| self.map.get(p).cloned())
            .unwrap_or_else(|| self.root.clone().unwrap_or_else(|| root_of(frame_id)));
        self.map.insert(frame_id.to_string(), target.clone());
        target
    }

    pub fn target_for(&self, frame_id: &str) -> Option<&str> {
        self.map.get(frame_id).map(String::as_str)
    }

    pub fn remove(&mut self, frame_id: &str) {
        self.map.remove(frame_id);
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// OOPIF frame ids look like `<session>.<frame>`; the leading segment identifies
/// the browser-level target that owns the tree.
fn root_of(frame_id: &str) -> String {
    frame_id.split('.').next().unwrap_or(frame_id).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn main_frame_resolves_to_prefix() {
        let mut m = FrameMap::new();
        assert_eq!(m.attach("AAAA", None), "AAAA");
        assert_eq!(m.target_for("AAAA"), Some("AAAA"));
    }

    #[test]
    fn oopif_frame_resolves_to_leading_segment() {
        let mut m = FrameMap::new();
        assert_eq!(m.attach("session1.frame9", None), "session1");
    }

    #[test]
    fn iframe_inherits_parent() {
        let mut m = FrameMap::new();
        assert_eq!(m.attach("AAAA", None), "AAAA");
        assert_eq!(m.attach("AAAA.child", Some("AAAA")), "AAAA");
        assert_eq!(m.attach("AAAA.child.deep", Some("AAAA.child")), "AAAA");
        assert_eq!(m.len(), 3);
        m.remove("AAAA.child");
        assert_eq!(m.len(), 2);
        assert!(m.target_for("AAAA.child").is_none());
        assert!(!m.is_empty());
    }

    #[test]
    fn explicit_root_overrides_prefix() {
        let mut m = FrameMap::for_target("target-1");
        assert_eq!(m.attach("whatever", None), "target-1");
        assert_eq!(m.attach("other", Some("whatever")), "target-1");
    }
}
