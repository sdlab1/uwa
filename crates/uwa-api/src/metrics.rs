//! Metrics facade.
//!
//! `metrics` macros are inert until a recorder is installed (Phase 14 wires
//! up Prometheus), so calling these from the hot path costs nothing today.

use metrics::gauge;

/// Publish the circuit-breaker state of a provider as a gauge:
/// `0.0` closed, `1.0` half-open, `2.0` open.
pub fn circuit_state(provider: &str, state: &'static str) {
    let v = match state {
        "closed" => 0.0,
        "half_open" => 1.0,
        _ => 2.0,
    };
    gauge!("uwa_circuit_state", "provider" => provider.to_string()).set(v);
}
