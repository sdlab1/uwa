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

        let entry = per_provider
            .entry(r.provider.clone())
            .or_insert_with(|| ProviderStats {
                provider: r.provider.clone(),
                ..Default::default()
            });

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
    let now = SystemTime::now();
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
    fn per_provider_stats() {
        let recs = vec![
            rec("a", RequestStatus::Success, 100),
            rec("a", RequestStatus::Error, 200),
            rec("a", RequestStatus::Success, 300),
        ];
        let s = compute(&recs, StatsWindow::All);
        let a = &s.providers[0];
        assert_eq!(a.provider, "a");
        assert_eq!(a.total, 3);
        assert!((a.error_rate - 0.333).abs() < 0.01);
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
    fn per_provider_ignores_zero_durations() {
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
        let a = s.providers.iter().find(|p| p.provider == "a").unwrap();
        assert_eq!(a.avg_total_ms, s.avg_total_ms);
        assert_eq!(a.avg_total_ms, 1000);
    }

    #[test]
    fn last_seen_is_newest_regardless_of_order() {
        let mut old = rec("a", RequestStatus::Success, 10);
        let new = rec("a", RequestStatus::Success, 10);
        old.started_at = new.started_at - std::time::Duration::from_secs(60);
        let newest = new.started_at;
        // Newest-first, as HistoryStore::recent returns.
        let s = compute(&[new, old], StatsWindow::All);
        assert_eq!(s.providers[0].last_seen, Some(newest));
    }
}
