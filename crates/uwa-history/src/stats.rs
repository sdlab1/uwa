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
    let slice = if records.len() > take {
        &records[..take]
    } else {
        records
    };

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
            RequestStatus::Success => s.success += 1,
            RequestStatus::Error => s.error += 1,
            RequestStatus::Pending => s.pending += 1,
        }

        if r.timing.total_ms > 0 {
            durations.push(r.timing.total_ms);
        }

        if let Some(resp) = &r.response {
            *s.finish_reasons
                .entry(resp.finish_reason.clone())
                .or_insert(0) += 1;
        }

        if let Some(tab) = &r.tab_id {
            *s.tab_utilization.entry(tab.clone()).or_insert(0) += 1;
        }

        let entry = per_provider
            .entry(r.provider.clone())
            .or_insert_with(|| ProviderStats {
                provider: r.provider.clone(),
                ..Default::default()
            });

        entry.total += 1;
        match r.status {
            RequestStatus::Success => entry.success += 1,
            RequestStatus::Error => entry.error += 1,
            RequestStatus::Pending => entry.pending += 1,
        }
        if let Some(resp) = &r.response {
            match resp.extraction_source.as_str() {
                "network" => entry.network_count += 1,
                "dom" => entry.dom_count += 1,
                _ => {}
            }
        }
        entry.last_seen = Some(r.started_at);
        per_provider_durations
            .entry(r.provider.clone())
            .or_default()
            .push(r.timing.total_ms);
    }

    s.avg_total_ms = avg(&durations);
    s.p50_total_ms = percentile(&durations, 50.0);
    s.p95_total_ms = percentile(&durations, 95.0);

    for (name, mut stats) in per_provider {
        let total = stats.total.max(1);
        stats.error_rate = stats.error as f32 / total as f32;
        let durs = per_provider_durations
            .get(&name)
            .cloned()
            .unwrap_or_default();
        stats.avg_total_ms = avg(&durs);
        stats.p50_total_ms = percentile(&durs, 50.0);
        stats.p95_total_ms = percentile(&durs, 95.0);
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
            let mins = age.as_secs() / 60;
            if (mins as usize) < 60 {
                buckets[59 - mins as usize] += 1;
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
        v.iter().sum::<u64>() / v.len() as u64
    }
}

fn percentile(v: &[u64], p: f64) -> u64 {
    if v.is_empty() {
        return 0;
    }
    let mut sorted = v.to_vec();
    sorted.sort_unstable();
    let idx = ((p / 100.0) * (sorted.len() as f64 - 1.0)).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
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
        assert_eq!(percentile(&v, 50.0), 30);
        assert_eq!(percentile(&v, 100.0), 50);
        assert_eq!(percentile(&v, 0.0), 10);
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
}
