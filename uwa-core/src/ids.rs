//! Strongly-typed identifiers used across the whole system.
//!
//! All IDs are newtypes over `String`/`Uuid` to prevent mixing them up.
//! They are cheap to clone and serialize to plain strings.

use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

macro_rules! string_id {
    ($name:ident, $prefix:literal) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Generate a new random id with the given prefix.
            pub fn new() -> Self {
                Self(format!("{}_{}", $prefix, Uuid::new_v4().simple()))
            }
            /// Wrap an existing string (e.g. from an incoming header).
            pub fn from_raw(s: impl Into<String>) -> Self {
                Self(s.into())
            }
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
        impl From<$name> for String {
            fn from(v: $name) -> String {
                v.0
            }
        }
    };
}

string_id!(RequestId, "req");
string_id!(SessionId, "sess");
string_id!(TabId, "tab");
string_id!(ConversationId, "conv");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_ids_are_unique_and_prefixed() {
        let a = TabId::new();
        let b = TabId::new();
        assert_ne!(a, b);
        assert!(a.as_str().starts_with("tab_"));
    }

    #[test]
    fn ids_serialize_transparently() {
        let id = RequestId::from_raw("req_abc");
        let j = serde_json::to_string(&id).unwrap();
        assert_eq!(j, "\"req_abc\"");
        let back: RequestId = serde_json::from_str(&j).unwrap();
        assert_eq!(back, id);
    }
}
