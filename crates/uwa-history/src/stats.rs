//! Aggregated statistics over a window of records.

use crate::record::{RequestRecord, RequestStatus};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::SystemTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatsWindow {
    Last50,
    Last200,
    All,
}

impl StatsWindow {
    pub fn take(&self) -> usize {
        match self {
            Self::Last50 => 50,
            Self::Last200 => 200,
            Self::All => usize::MAX,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProviderStats {
    pub provider: String,
    pub total: u64,
    pub success: u64,
    pub error: u64,
    pub pending: u64,
    /// Failure rate 0.0..=1.0.
    pub error_rate: f32,
    pub avg_total_ms: u64,
    pub p50_total_ms: u64,
    pub p95_total_ms: u64,
    pub avg_send_ms: u64,
    pub avg_wait_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avg_ttft_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p50_ttft_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p95_ttft_ms: Option<u64>,
    /// Number of distinct tabs used.
    pub tabs_used: u64,
    pub last_seen: Option<SystemTime>,
    /// Extraction source distribution.
    pub network_count: u64,
    pub dom_count: u64,
    /// Estimated token usage rolled up per provider.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub tokens_in: u64,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub tokens_out: u64,
}

fn is_zero_u64(n: &u64) -> bool {
    *n == 0
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Stats {
    pub window: String,
    pub total: u64,
    pub success: u64,
    pub error: u64,
    pub pending: u64,
    pub avg_total_ms: u64,
    pub p50_total_ms: u64,
    pub p95_total_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avg_ttft_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p50_ttft_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p95_ttft_ms: Option<u64>,
    /// Distribution per finish_reason.
    pub finish_reasons: HashMap<String, u64>,
    /// Per-provider breakdown.
    pub providers: Vec<ProviderStats>,
    /// Global tab utilization (tab_id → count).
    pub tab_utilization: HashMap<String, u64>,
    /// Requests per minute over the last hour (buckets of 1 min).
    pub requests_per_minute: Vec<u64>,
    /// Estimated token usage totals for the window.
    #[serde(default)]
    pub tokens_in: u64,
    #[serde(default)]
    pub tokens_out: u64,
    /// Token sums per minute, most recent last (60 slots). Same shape
    /// as `requests_per_minute`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tokens_in_per_minute: Vec<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tokens_out_per_minute: Vec<u64>,
    /// Requests and tokens rolled up by browser profile.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub profiles: Vec<ProfileStats>,
}

/// Tokens and requests rolled up by browser profile. The profile name
/// is the `X-UWA-Session` value; `"default"` covers requests without
/// the header.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProfileStats {
    pub profile: String,
    pub total: u64,
    pub success: u64,
    pub error: u64,
    pub tokens_in: u64,
    pub tokens_out: u64,
}

pub fn compute(records: &[RequestRecord], window: StatsWindow) -> Stats {
    let take = window.take();
    // `get` instead of slicing: forbid-level lint, and `take` may be
    // usize::MAX for the All window.
    let slice = records.get(..take).unwrap_or(records);

    let mut s = Stats {
        window: format!("{window:?}"),
        ..Default::default()
    };
    s.total = slice.len() as u64;

    let mut durations: Vec<u64> = Vec::with_capacity(slice.len());
    let mut per_provider: HashMap<String, ProviderStats> = HashMap::new();
    let mut per_provider_durations: HashMap<String, Vec<u64>> = HashMap::new();
    let mut per_profile: HashMap<String, ProfileStats> = HashMap::new();
    let now = SystemTime::now();
    let mut tok_in_buckets = vec![0u64; 60];
    let mut tok_out_buckets = vec![0u64; 60];

    for r in slice {
        match r.status {
            RequestStatus::Success => s.success = s.success.saturating_add(1),
            RequestStatus::Error => s.error = s.error.saturating_add(1),
            RequestStatus::Pending => s.pending = s.pending.saturating_add(1),
        }

        if r.timing.total_ms > 0 {
            durations.push(r.timing.total_ms);
        }

        if let Some(resp) = &r.response {
            let cell = s
                .finish_reasons
                .entry(resp.finish_reason.clone())
                .or_insert(0);
            *cell = cell.saturating_add(1);
        }

        if let Some(tab) = &r.tab_id {
            let cell = s.tab_utilization.entry(tab.clone()).or_insert(0);
            *cell = cell.saturating_add(1);
        }

        let usage = r.response.as_ref().and_then(|x| x.usage.as_ref());

        // Window totals and per-minute buckets (newest last).
        if let Some(u) = usage {
            s.tokens_in = s.tokens_in.saturating_add(u.prompt_tokens as u64);
            s.tokens_out = s.tokens_out.saturating_add(u.completion_tokens as u64);
            if let Ok(age) = now.duration_since(r.started_at) {
                let mins = age.as_secs() / 60; // constant divisor: lint-safe
                if mins < 60 {
                    let idx = 59usize.saturating_sub(mins as usize);
                    if let (Some(cell_in), Some(cell_out)) =
                        (tok_in_buckets.get_mut(idx), tok_out_buckets.get_mut(idx))
                    {
                        *cell_in = cell_in.saturating_add(u.prompt_tokens as u64);
                        *cell_out = cell_out.saturating_add(u.completion_tokens as u64);
                    }
                }
            }
        }

        // Per-provider tokens.
        let entry = per_provider
            .entry(r.provider.clone())
            .or_insert_with(|| ProviderStats {
                provider: r.provider.clone(),
                ..Default::default()
            });

        if let Some(u) = usage {
            entry.tokens_in = entry.tokens_in.saturating_add(u.prompt_tokens as u64);
            entry.tokens_out = entry.tokens_out.saturating_add(u.completion_tokens as u64);
        }

        // Per-profile rollup (`X-UWA-Session`, "default" when absent).
        let profile_name = r.profile.clone().unwrap_or_else(|| "default".into());
        let profile_entry =
            per_profile
                .entry(profile_name.clone())
                .or_insert_with(|| ProfileStats {
                    profile: profile_name,
                    ..Default::default()
                });
        profile_entry.total = profile_entry.total.saturating_add(1);
        match r.status {
            RequestStatus::Success => {
                profile_entry.success = profile_entry.success.saturating_add(1)
            }
            RequestStatus::Error => profile_entry.error = profile_entry.error.saturating_add(1),
            RequestStatus::Pending => {}
        }
        if let Some(u) = usage {
            profile_entry.tokens_in = profile_entry
                .tokens_in
                .saturating_add(u.prompt_tokens as u64);
            profile_entry.tokens_out = profile_entry
                .tokens_out
                .saturating_add(u.completion_tokens as u64);
        }

        entry.total = entry.total.saturating_add(1);
        match r.status {
            RequestStatus::Success => entry.success = entry.success.saturating_add(1),
            RequestStatus::Error => entry.error = entry.error.saturating_add(1),
            RequestStatus::Pending => entry.pending = entry.pending.saturating_add(1),
        }
        if let Some(resp) = &r.response {
            match resp.extraction_source.as_str() {
                "network" => entry.network_count = entry.network_count.saturating_add(1),
                "dom" => entry.dom_count = entry.dom_count.saturating_add(1),
                _ => {}
            }
        }
        entry.last_seen = Some(match entry.last_seen {
            Some(prev) if prev >= r.started_at => prev,
            _ => r.started_at,
        });
        if r.timing.total_ms > 0 {
            per_provider_durations
                .entry(r.provider.clone())
                .or_default()
                .push(r.timing.total_ms);
        }
    }

    s.avg_total_ms = avg(&durations);
    s.p50_total_ms = percentile(&durations, 50);
    s.p95_total_ms = percentile(&durations, 95);

    let mut ttfts: Vec<u64> = Vec::new();
    let mut per_provider_ttfts: HashMap<String, Vec<u64>> = HashMap::new();

    for r in slice {
        if let Some(t) = r.timing.ttft_ms {
            ttfts.push(t);
            per_provider_ttfts
                .entry(r.provider.clone())
                .or_default()
                .push(t);
        }
    }
    if !ttfts.is_empty() {
        s.avg_ttft_ms = Some(avg(&ttfts));
        s.p50_ttft_ms = percentile_opt(&mut ttfts.clone(), 50);
        s.p95_ttft_ms = percentile_opt(&mut ttfts, 95);
    }

    for (name, mut stats) in per_provider {
        let total = stats.total.max(1);
        // total >= 1, so the division is finite and sensible. UFCS call:
        // `arithmetic_side_effects` flags the `/` operator, not the
        // trait-method form (f32 has no checked_div).
        stats.error_rate = std::ops::Div::div(stats.error as f32, total as f32);
        let durs = per_provider_durations
            .get(&name)
            .cloned()
            .unwrap_or_default();
        stats.avg_total_ms = avg(&durs);
        stats.p50_total_ms = percentile(&durs, 50);
        stats.p95_total_ms = percentile(&durs, 95);
        if let Some(mut v) = per_provider_ttfts.remove(&name) {
            if !v.is_empty() {
                stats.avg_ttft_ms = Some(avg(&v));
                stats.p50_ttft_ms = percentile_opt(&mut v.clone(), 50);
                stats.p95_ttft_ms = percentile_opt(&mut v, 95);
            }
        }
        stats.tabs_used = s
            .tab_utilization
            .keys()
            .filter(|k| {
                slice
                    .iter()
                    .any(|r| r.provider == name && r.tab_id.as_deref() == Some(k.as_str()))
            })
            .count() as u64;
        s.providers.push(stats);
    }
    s.providers.sort_by_key(|p| std::cmp::Reverse(p.total));

    // Requests per minute over last hour.
    let mut buckets = vec![0u64; 60];
    for r in slice {
        if let Ok(age) = now.duration_since(r.started_at) {
            let mins = age.as_secs() / 60; // constant divisor: lint-safe
            if mins < 60 {
                // Newest first: bucket 0 = the current minute.
                let idx = 59usize.saturating_sub(mins as usize);
                if let Some(cell) = buckets.get_mut(idx) {
                    *cell = cell.saturating_add(1);
                }
            }
        }
    }
    s.requests_per_minute = buckets;
    s.tokens_in_per_minute = tok_in_buckets;
    s.tokens_out_per_minute = tok_out_buckets;

    let mut profiles: Vec<ProfileStats> = per_profile.into_values().collect();
    profiles.sort_by_key(|p| std::cmp::Reverse(p.total));
    s.profiles = profiles;

    s
}

fn avg(v: &[u64]) -> u64 {
    if v.is_empty() {
        0
    } else {
        // Non-empty by the guard; checked_div keeps the forbid-level
        // arithmetic lint out.
        v.iter()
            .sum::<u64>()
            .checked_div(v.len() as u64)
            .unwrap_or(0)
    }
}

/// Index of the p-th percentile in a sorted vector of length `len`:
/// `round(p/100 * (len-1))`, computed in integers as
/// `(p*(len-1) + 50) / 100` (round-half-up — identical for p >= 0).
fn percentile_index(len: usize, p: u32) -> usize {
    let scaled = (len.saturating_sub(1) as u32).saturating_mul(p);
    (scaled.saturating_add(50) / 100) as usize
}

fn percentile(v: &[u64], p: u32) -> u64 {
    if v.is_empty() {
        return 0;
    }
    let mut sorted = v.to_vec();
    sorted.sort_unstable();
    let idx = percentile_index(sorted.len(), p);
    sorted.get(idx).copied().unwrap_or(0)
}

fn percentile_opt(v: &mut [u64], p: u32) -> Option<u64> {
    if v.is_empty() {
        return None;
    }
    v.sort_unstable();
    let idx = percentile_index(v.len(), p);
    v.get(idx).copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::{RequestSnapshot, ResponseSnapshot};

    fn rec(provider: &str, status: RequestStatus, total_ms: u64) -> RequestRecord {
        let mut r = RequestRecord::new(
            "r",
            "gpt-4o",
            provider,
            RequestSnapshot {
                model: "gpt-4o".into(),
                user_preview: "".into(),
                messages: 1,
                tools: 0,
                stream: false,
            },
        );
        r.timing.total_ms = total_ms;
        match status {
            RequestStatus::Success => r.mark_success(ResponseSnapshot {
                text_preview: "".into(),
                tool_calls: 0,
                finish_reason: "stop".into(),
                extraction_source: "dom".into(),
                pin_mode: None,
                usage: None,
            }),
            RequestStatus::Error => r.mark_error("x"),
            _ => {}
        }
        r
    }

    #[test]
    fn aggregates_basic() {
        let recs = vec![
            rec("a", RequestStatus::Success, 100),
            rec("a", RequestStatus::Error, 200),
            rec("b", RequestStatus::Success, 300),
        ];
        let s = compute(&recs, StatsWindow::All);
        assert_eq!(s.total, 3);
        assert_eq!(s.success, 2);
        assert_eq!(s.error, 1);
        assert_eq!(s.providers.len(), 2);
    }

    #[test]
    fn per_provider_stats() -> Result<(), Box<dyn std::error::Error>> {
        let recs = vec![
            rec("a", RequestStatus::Success, 100),
            rec("a", RequestStatus::Error, 200),
            rec("a", RequestStatus::Success, 300),
        ];
        let s = compute(&recs, StatsWindow::All);
        let a = s.providers.first().ok_or("expected provider stats")?;
        assert_eq!(a.provider, "a");
        assert_eq!(a.total, 3);
        assert!((a.error_rate - 0.333).abs() < 0.01);
        Ok(())
    }

    #[test]
    fn percentile_works() {
        let v = vec![10, 20, 30, 40, 50];
        assert_eq!(percentile(&v, 50), 30);
        assert_eq!(percentile(&v, 100), 50);
        assert_eq!(percentile(&v, 0), 10);
        // Integer round-half-up matches the old float formula on the
        // boundary cases.
        assert_eq!(percentile(&v, 95), 50);
        let two = vec![10, 20];
        assert_eq!(percentile(&two, 50), 20);
        assert_eq!(percentile(&two, 95), 20);
    }

    #[test]
    fn window_truncates() {
        let mut recs = Vec::new();
        for i in 0..100 {
            recs.push(rec("a", RequestStatus::Success, i));
        }
        let s = compute(&recs, StatsWindow::Last50);
        assert_eq!(s.total, 50);
    }

    #[test]
    fn per_provider_ignores_zero_durations() -> Result<(), Box<dyn std::error::Error>> {
        // Pending/sub-ms records must not drag provider latency down.
        // `mark_success` recomputes timing, so set the duration afterwards.
        let mut slow = rec("a", RequestStatus::Success, 0);
        slow.timing.total_ms = 1000;
        let recs = vec![
            rec("a", RequestStatus::Pending, 0),
            rec("a", RequestStatus::Pending, 0),
            slow,
        ];
        let s = compute(&recs, StatsWindow::All);
        let a = s
            .providers
            .iter()
            .find(|p| p.provider == "a")
            .ok_or("provider a missing")?;
        assert_eq!(a.avg_total_ms, s.avg_total_ms);
        assert_eq!(a.avg_total_ms, 1000);
        Ok(())
    }

    #[test]
    fn last_seen_is_newest_regardless_of_order() {
        let mut old = rec("a", RequestStatus::Success, 10);
        let new = rec("a", RequestStatus::Success, 10);
        old.started_at = new.started_at - std::time::Duration::from_secs(60);
        let newest = new.started_at;
        // Newest-first, as HistoryStore::recent returns.
        let s = compute(&[new, old], StatsWindow::All);
        assert_eq!(s.providers.first().and_then(|p| p.last_seen), Some(newest));
    }

    fn rec_with_tokens(
        provider: &str,
        profile: Option<&str>,
        status: RequestStatus,
        tokens_in: u32,
        tokens_out: u32,
    ) -> RequestRecord {
        let mut r = RequestRecord::new(
            "r",
            "gpt-4o",
            provider,
            RequestSnapshot {
                model: "gpt-4o".into(),
                user_preview: String::new(),
                messages: 1,
                tools: 0,
                stream: false,
            },
        );
        r.profile = profile.map(str::to_string);
        match status {
            RequestStatus::Success => r.mark_success(ResponseSnapshot {
                text_preview: String::new(),
                tool_calls: 0,
                finish_reason: "stop".into(),
                extraction_source: "dom".into(),
                pin_mode: None,
                usage: Some(uwa_core::types::openai::Usage {
                    prompt_tokens: tokens_in,
                    completion_tokens: tokens_out,
                    total_tokens: tokens_in.saturating_add(tokens_out),
                }),
            }),
            RequestStatus::Error => r.mark_error("x"),
            _ => {}
        }
        r
    }

    #[test]
    fn token_totals_are_summed() {
        let recs = vec![
            rec_with_tokens("a", None, RequestStatus::Success, 100, 50),
            rec_with_tokens("a", None, RequestStatus::Success, 40, 20),
        ];
        let s = compute(&recs, StatsWindow::All);
        assert_eq!(s.tokens_in, 140);
        assert_eq!(s.tokens_out, 70);
    }

    #[test]
    fn per_provider_tokens_are_rolled_up() {
        let recs = vec![
            rec_with_tokens("a", None, RequestStatus::Success, 100, 50),
            rec_with_tokens("b", None, RequestStatus::Success, 20, 10),
        ];
        let s = compute(&recs, StatsWindow::All);
        let a = s
            .providers
            .iter()
            .find(|p| p.provider == "a")
            .unwrap_or_else(|| unreachable!("provider a"));
        let b = s
            .providers
            .iter()
            .find(|p| p.provider == "b")
            .unwrap_or_else(|| unreachable!("provider b"));
        assert_eq!(a.tokens_in, 100);
        assert_eq!(a.tokens_out, 50);
        assert_eq!(b.tokens_in, 20);
    }

    #[test]
    fn per_profile_groups_default_and_named() {
        let recs = vec![
            rec_with_tokens("a", None, RequestStatus::Success, 10, 5),
            rec_with_tokens("a", Some("work"), RequestStatus::Success, 30, 15),
            rec_with_tokens("a", Some("work"), RequestStatus::Error, 0, 0),
        ];
        let s = compute(&recs, StatsWindow::All);
        let work = s
            .profiles
            .iter()
            .find(|p| p.profile == "work")
            .unwrap_or_else(|| unreachable!("work profile"));
        assert_eq!(work.total, 2);
        assert_eq!(work.success, 1);
        assert_eq!(work.error, 1);
        assert_eq!(work.tokens_in, 30);
        assert_eq!(work.tokens_out, 15);
        assert!(s.profiles.iter().any(|p| p.profile == "default"));
    }

    #[test]
    fn per_minute_buckets_have_60_slots() {
        let recs = vec![rec_with_tokens("a", None, RequestStatus::Success, 5, 3)];
        let s = compute(&recs, StatsWindow::All);
        assert_eq!(s.tokens_in_per_minute.len(), 60);
        assert_eq!(s.tokens_out_per_minute.len(), 60);
        // The event just happened, so it lands in the last bucket.
        assert_eq!(s.tokens_in_per_minute.last(), Some(&5));
        assert_eq!(s.tokens_out_per_minute.last(), Some(&3));
    }
}
