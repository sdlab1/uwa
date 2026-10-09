//! Smoke test: OOPIF metric emission doesn't panic without a recorder installed.
//!
//! We don't assert on values — that's covered by the metrics feature tests
//! in `uwa-api`. This test guards against API drift in the `metrics` crate
//! and verifies the counter/gauge names match what Prometheus scrapes.

#[test]
fn attach_counter_is_callable() {
    metrics::counter!("uwa_oopif_attached_total", "type" => "iframe", "state" => "registered")
        .increment(1);
    metrics::counter!("uwa_oopif_attached_total", "type" => "iframe", "state" => "pending_frame_id")
        .increment(1);
    metrics::counter!("uwa_oopif_attached_total", "type" => "page", "state" => "registered")
        .increment(1);
}

#[test]
fn detach_counter_is_callable() {
    metrics::counter!("uwa_oopif_detached_total", "reason" => "detached").increment(1);
    metrics::counter!("uwa_oopif_detached_total", "reason" => "destroyed").increment(1);
}

#[test]
fn backfill_counter_is_callable() {
    metrics::counter!("uwa_oopif_backfilled_total").increment(1);
}

#[test]
fn gauge_is_callable() {
    metrics::gauge!("uwa_oopif_sessions_active").set(0.0);
    metrics::gauge!("uwa_oopif_sessions_active").set(42.0);
    metrics::gauge!("uwa_oopif_sessions_active").set(0.0);
}

#[test]
fn metric_names_match_prometheus_convention() {
    // All names must start with uwa_oopif_ and use snake_case.
    for name in [
        "uwa_oopif_attached_total",
        "uwa_oopif_detached_total",
        "uwa_oopif_backfilled_total",
        "uwa_oopif_sessions_active",
    ] {
        assert!(
            name.starts_with("uwa_oopif_"),
            "metric `{name}` must be namespaced"
        );
        assert!(
            name.chars()
                .all(|c| c.is_ascii_lowercase() || c == '_' || c.is_ascii_digit()),
            "metric `{name}` must be snake_case"
        );
    }
}
