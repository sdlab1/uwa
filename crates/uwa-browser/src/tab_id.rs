//! `TabId` <-> CDP `TargetId` mapping.
//!
//! Every CDP target id gets a namespaced `TabId` so ids from different
//! transports can never collide.

use uwa_core::TabId;

const PREFIX: &str = "tab_";

pub fn tab_id_from_target(target_id: &str) -> TabId {
    TabId::from_raw(format!("{PREFIX}{target_id}"))
}

pub fn target_id_from_tab(tab: &TabId) -> Option<&str> {
    tab.as_str().strip_prefix(PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let target = "0A1B2C3D";
        let tab = tab_id_from_target(target);
        assert_eq!(tab.as_str(), "tab_0A1B2C3D");
        assert_eq!(target_id_from_tab(&tab), Some(target));
    }

    #[test]
    fn unrelated_tab_id_has_no_target() {
        let tab = TabId::from_raw("something_else");
        assert_eq!(target_id_from_tab(&tab), None);
    }
}
