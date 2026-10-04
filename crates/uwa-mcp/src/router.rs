//! Aggregates local (per-request) and remote (MCP) tool providers behind a
//! single namespace. Collisions get `<ns>__<tool>` prefix; unique names pass
//! through unchanged (better for prompt quality).

use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use uwa_core::traits::{ToolProvider, ToolSpec};
use uwa_core::{Result, UwaError};

#[derive(Default)]
pub struct ToolRouter {
    providers: HashMap<String, Arc<dyn ToolProvider>>,
}

/// A resolved tool: which provider + its original (unprefixed) name.
struct Resolved {
    provider: Arc<dyn ToolProvider>,
    tool: String,
}

impl ToolRouter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, provider: Arc<dyn ToolProvider>) {
        self.providers
            .insert(provider.namespace().to_string(), provider);
    }

    pub fn unregister(&mut self, namespace: &str) {
        self.providers.remove(namespace);
    }

    /// List all tools, computing the public name for each. If a bare tool name
    /// is unique across all providers, we keep it; otherwise prefix.
    pub async fn all_definitions(&self) -> Result<Vec<ToolSpec>> {
        // 1. Collect all (namespace, spec) pairs.
        let mut all: Vec<(String, ToolSpec)> = Vec::new();
        for (ns, provider) in self.providers.iter() {
            let provider = provider.clone();
            for spec in provider.list_tools().await? {
                all.push((ns.clone(), spec));
            }
        }
        // 2. Count bare-name collisions.
        let mut counts: std::collections::HashMap<String, usize> = Default::default();
        for (_, spec) in &all {
            *counts.entry(spec.name.clone()).or_default() += 1;
        }
        // 3. Public name.
        let out = all
            .into_iter()
            .map(|(ns, mut spec)| {
                if counts.get(&spec.name).copied().unwrap_or(0) > 1 && !ns.is_empty() {
                    spec.name = format!("{ns}__{}", spec.name);
                }
                spec
            })
            .collect();
        Ok(out)
    }

    /// Find the right provider for `public_name` and call it.
    pub async fn dispatch(&self, public_name: &str, args: Value) -> Result<String> {
        let resolved = self.resolve(public_name).await?;
        resolved.provider.call_tool(&resolved.tool, args).await
    }

    async fn resolve(&self, public_name: &str) -> Result<Resolved> {
        // Try namespaced first: `ns__tool`.
        if let Some((ns, tool)) = public_name.split_once("__") {
            if let Some(p) = self.providers.get(ns) {
                return Ok(Resolved {
                    provider: p.clone(),
                    tool: tool.into(),
                });
            }
        }
        // Then try bare name against each provider.
        for provider in self.providers.values() {
            let provider = provider.clone();
            for spec in provider.list_tools().await? {
                if spec.name == public_name {
                    return Ok(Resolved {
                        provider,
                        tool: public_name.into(),
                    });
                }
            }
        }
        Err(UwaError::BadRequest(format!(
            "unknown tool `{public_name}`"
        )))
    }

    pub fn namespaces(&self) -> Vec<String> {
        self.providers.keys().cloned().collect()
    }
}
