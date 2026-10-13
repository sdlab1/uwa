//! Profile name: the identity used to pick a browser context.
//!
//! `default` is reserved for the browser's own context — the one the
//! user logged into when they started Chromium. Every other name maps
//! to a CDP `BrowserContextId` created on demand.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProfileName(String);

impl ProfileName {
    pub const DEFAULT: &'static str = "default";

    /// Wrap a name. Empty input and the literal `"default"` (case-
    /// insensitive) both collapse to the default profile. Config
    /// validation catches the `default` case earlier; this is the
    /// defensive layer for callers that build a `ProfileName` from an
    /// HTTP header.
    pub fn new(s: impl Into<String>) -> Self {
        let s = s.into();
        if s.is_empty() || s.eq_ignore_ascii_case(Self::DEFAULT) {
            return Self(Self::DEFAULT.into());
        }
        Self(s)
    }

    pub fn default_name() -> Self {
        Self(Self::DEFAULT.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_default(&self) -> bool {
        self.0 == Self::DEFAULT
    }
}

impl Default for ProfileName {
    fn default() -> Self {
        Self::default_name()
    }
}

impl fmt::Display for ProfileName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for ProfileName {
    fn from(s: &str) -> Self {
        Self::new(s)
    }
}

impl From<String> for ProfileName {
    fn from(s: String) -> Self {
        Self::new(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_is_default() {
        assert!(ProfileName::new("").is_default());
    }

    #[test]
    fn case_insensitive_default() {
        assert!(ProfileName::new("Default").is_default());
        assert!(ProfileName::new("DEFAULT").is_default());
        assert!(ProfileName::new("default").is_default());
    }

    #[test]
    fn non_default_kept_verbatim() {
        let p = ProfileName::new("work");
        assert_eq!(p.as_str(), "work");
        assert!(!p.is_default());
    }

    #[test]
    fn round_trip_display() {
        assert_eq!(ProfileName::new("work").to_string(), "work");
        assert_eq!(ProfileName::default_name().to_string(), "default");
    }
}
