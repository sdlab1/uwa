//! Routing groups: a logical name that fans out to a set of
//! `(provider, preset)` pairs. Requests to `/group/{id}/v1/...` get routed
//! round-robin (or failover) within the group.

use serde::{Deserialize, Serialize};

use uwa_core::UwaError;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GroupStrategy {
    /// Pick by round-robin. Good for load distribution.
    #[default]
    RoundRobin,
    /// Pick the first healthy member. Good for failover.
    Failover,
    /// Hash the conversation id to a member. Sticky.
    HashConversation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupMember {
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupCfg {
    #[serde(default)]
    pub strategy: GroupStrategy,
    pub members: Vec<GroupMember>,
}

impl GroupCfg {
    pub fn validate(
        &self,
        providers: &std::collections::HashMap<String, super::ProviderCfg>,
    ) -> Result<(), UwaError> {
        if self.members.is_empty() {
            return Err(UwaError::Config("group has no members".into()));
        }
        for m in &self.members {
            let Some(p) = providers.get(&m.provider) else {
                return Err(UwaError::Config(format!(
                    "group member provider `{}` is not defined",
                    m.provider
                )));
            };
            if let Some(preset) = &m.preset {
                if !p.has_preset(preset) {
                    return Err(UwaError::Config(format!(
                        "group member provider `{}` has no preset `{preset}`",
                        m.provider
                    )));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn group_validates_missing_provider() {
        let g = GroupCfg {
            strategy: GroupStrategy::RoundRobin,
            members: vec![GroupMember {
                provider: "missing".into(),
                preset: None,
            }],
        };
        assert!(g.validate(&HashMap::new()).is_err());
    }

    #[test]
    fn empty_group_errors() {
        let g = GroupCfg {
            strategy: GroupStrategy::Failover,
            members: vec![],
        };
        assert!(g.validate(&HashMap::new()).is_err());
    }
}
