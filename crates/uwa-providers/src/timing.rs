//! Runtime pacing gate.
//!
//! `GenericProvider` consults a `TimingGate` before each pacing-
//! sensitive step: focusing the composer, pressing a key, clicking
//! Send, reading a response. The gate is the only place jitter is
//! introduced; everything else stays deterministic so tests and the
//! conformance suite can be exact when they want to be.

use std::sync::Arc;
use std::time::{Duration, Instant};

use uwa_config::{TimingCfg, TimingProfile};

/// Source of randomness. Real runs use thread-local `rand::rng()`; the
/// conformance suite and tests inject a deterministic one.
pub trait Rng: Send + Sync {
    /// Uniform in `[0, 1)`. Same contract as `rand::Rng::gen`.
    fn next_f64(&self) -> f64;
}

pub struct ThreadRng;
impl Rng for ThreadRng {
    fn next_f64(&self) -> f64 {
        rand::random::<f64>()
    }
}

/// `[lo, hi]` → a value in between, uniform.
fn pick(r: &dyn Rng, range: [u64; 2]) -> Duration {
    if range[1] <= range[0] {
        return Duration::from_millis(range[0]);
    }
    let span = range[1].saturating_sub(range[0]) as f64;
    let v = range[0] as f64 + r.next_f64() * span;
    Duration::from_millis(v.round() as u64)
}

/// Hard ceiling, in milliseconds, on the total sleep a single
/// [`TimingGate::type_run`] may produce. Pacing is a cosmetic signal; a
/// multi-thousand-character prompt on a non-`fast` profile otherwise
/// blocks the caller for minutes.
const MAX_TYPE_SLEEP_MS: u64 = 2_000;

/// `true` while `need` ms still fits in the [`MAX_TYPE_SLEEP_MS`] budget
/// spent since `started`.
///
/// `need` is the top of a configured range, so a sleep that is allowed to
/// start is also allowed to finish: the ceiling is overshot only by timer
/// slack, never by a whole range.
fn room_for(need: u64, started: Instant) -> bool {
    let used_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    MAX_TYPE_SLEEP_MS.saturating_sub(used_ms) >= need
}

pub struct TimingGate {
    cfg: TimingCfg,
    rng: Arc<dyn Rng>,
}

impl TimingGate {
    pub fn new(cfg: TimingCfg) -> Self {
        Self {
            cfg,
            rng: Arc::new(ThreadRng),
        }
    }

    pub fn with_rng(cfg: TimingCfg, rng: Arc<dyn Rng>) -> Self {
        Self { cfg, rng }
    }

    /// A gate that never waits. Used when a provider doesn't declare a
    /// timing profile and the caller wants the old behavior.
    pub fn disabled() -> Self {
        Self::new(TimingCfg::default().apply_profile(TimingProfile::Fast))
    }

    pub fn profile(&self) -> TimingProfile {
        self.cfg.profile
    }

    pub async fn focus(&self) {
        sleep(pick(self.rng.as_ref(), self.cfg.focus_ms)).await;
    }

    pub async fn key(&self) {
        sleep(pick(self.rng.as_ref(), self.cfg.key_ms)).await;
    }

    pub async fn pre_send(&self) {
        sleep(pick(self.rng.as_ref(), self.cfg.pre_send_ms)).await;
    }

    pub async fn post_send(&self) {
        sleep(pick(self.rng.as_ref(), self.cfg.post_send_ms)).await;
    }

    pub async fn burst_pause(&self) {
        sleep(pick(self.rng.as_ref(), self.cfg.burst_pause_ms)).await;
    }

    /// Type a chunk of text one "keystroke" at a time, honouring burst
    /// configuration. The text is already delivered to the input by the
    /// caller; this only produces the *pauses between* keystrokes so the
    /// injection isn't a single instantaneous event from the site's
    /// point of view.
    ///
    /// `chars` is the total char count and there is no partial state.
    /// Pauses stop once `MAX_TYPE_SLEEP_MS` has been spent on the call, so
    /// a long prompt never stalls the run.
    pub async fn type_run(&self, chars: usize) {
        if chars == 0 {
            return;
        }
        let started = Instant::now();
        if !self.cfg.burst {
            for _ in 0..chars {
                if !room_for(self.cfg.key_ms[1], started) {
                    break;
                }
                self.key().await;
            }
            return;
        }
        let burst = self.cfg.burst_chars.max(1) as usize;
        let mut left = chars;
        while left > 0 {
            if !room_for(self.cfg.key_ms[1], started) {
                break;
            }
            let take = left.min(burst);
            for _ in 0..take {
                if !room_for(self.cfg.key_ms[1], started) {
                    break;
                }
                self.key().await;
            }
            left = left.saturating_sub(take);
            if left > 0 && room_for(self.cfg.burst_pause_ms[1], started) {
                self.burst_pause().await;
            }
        }
    }
}

async fn sleep(d: Duration) {
    if d.is_zero() {
        return;
    }
    tokio::time::sleep(d).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Deterministic RNG that returns a fixed value scaled across an
    /// internal counter, so a test can predict the exact sequence.
    struct SeqRng {
        calls: AtomicU64,
    }
    impl SeqRng {
        fn new() -> Self {
            Self {
                calls: AtomicU64::new(0),
            }
        }
    }
    impl Rng for SeqRng {
        fn next_f64(&self) -> f64 {
            // 0.0, 0.25, 0.5, 0.75, then back to 0.0 ...
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            ((n % 4) as f64) / 4.0
        }
    }

    fn cfg_fast() -> TimingCfg {
        TimingCfg::default().apply_profile(TimingProfile::Fast)
    }

    #[tokio::test]
    async fn fast_profile_has_no_measurable_wait() {
        let gate = TimingGate::new(cfg_fast());
        let start = std::time::Instant::now();
        gate.focus().await;
        gate.key().await;
        gate.pre_send().await;
        gate.post_send().await;
        assert!(start.elapsed() < Duration::from_millis(50));
    }

    #[tokio::test]
    async fn natural_adds_some_delay() {
        let gate = TimingGate::new(TimingCfg::default().apply_profile(TimingProfile::Natural));
        let start = std::time::Instant::now();
        gate.pre_send().await;
        // Lower bound of the range.
        assert!(start.elapsed() >= Duration::from_millis(100));
    }

    #[tokio::test]
    async fn type_run_fast_returns_quickly_for_long_text() {
        let gate = TimingGate::new(cfg_fast());
        let start = std::time::Instant::now();
        gate.type_run(1000).await;
        // Even 1000 iterations of "sleep 0" must finish well under a
        // second; anything else means we're actually sleeping.
        assert!(start.elapsed() < Duration::from_millis(200));
    }

    #[tokio::test]
    async fn pick_returns_lo_when_hi_le_lo() {
        let r = SeqRng::new();
        assert_eq!(pick(&r, [5, 5]), Duration::from_millis(5));
        assert_eq!(pick(&r, [10, 5]), Duration::from_millis(10));
    }

    #[tokio::test]
    async fn pick_stays_in_range() {
        let r = SeqRng::new();
        for _ in 0..20 {
            let d = pick(&r, [40, 140]);
            assert!(d >= Duration::from_millis(40));
            assert!(d <= Duration::from_millis(140));
        }
    }

    /// Always draws 0.0 — so `pick` returns the low end of a range — but
    /// records every draw, letting a test assert exactly how many sleeps
    /// `type_run` asked for. (`pick` skips the RNG when `lo == hi`, hence
    /// the `[0, 1]` ranges: 0 ms sleeps that still draw.)
    struct CountingRng {
        draws: AtomicU64,
    }
    impl CountingRng {
        fn new() -> Self {
            Self {
                draws: AtomicU64::new(0),
            }
        }
        fn draws(&self) -> u64 {
            self.draws.load(Ordering::SeqCst)
        }
    }
    impl Rng for CountingRng {
        fn next_f64(&self) -> f64 {
            self.draws.fetch_add(1, Ordering::SeqCst);
            0.0
        }
    }

    /// Burst-on config whose every sleep is 0 ms, so the run never meets
    /// the sleep budget and the draw count stays exact.
    fn cfg_burst5() -> TimingCfg {
        TimingCfg {
            key_ms: [0, 1],
            burst: true,
            burst_chars: 5,
            burst_pause_ms: [0, 1],
            ..TimingCfg::default()
        }
    }

    fn counting_gate(cfg: TimingCfg) -> (TimingGate, Arc<CountingRng>) {
        let r = Arc::new(CountingRng::new());
        (TimingGate::with_rng(cfg, r.clone()), r)
    }

    #[tokio::test]
    async fn type_run_zero_chars_is_noop() {
        let (gate, r) = counting_gate(cfg_burst5());
        let start = Instant::now();
        gate.type_run(0).await;
        assert!(start.elapsed() < Duration::from_millis(10));
        assert_eq!(r.draws(), 0);
    }

    /// A single character is one keystroke: no `burst_pause` after it.
    #[tokio::test]
    async fn type_run_one_char_no_burst_pause() {
        let (gate, r) = counting_gate(cfg_burst5());
        gate.type_run(1).await;
        assert_eq!(r.draws(), 1);
    }

    /// Text exactly the length of a burst: `burst_chars` keystrokes and
    /// no pause, because nothing is left to start a second burst with.
    #[tokio::test]
    async fn type_run_exact_burst_boundary() {
        let (gate, r) = counting_gate(cfg_burst5());
        gate.type_run(5).await;
        assert_eq!(r.draws(), 5);
    }

    /// 5 chars + pause + 2 chars: one pause, 7 keystrokes → 8 draws.
    #[tokio::test]
    async fn type_run_partial_last_burst() {
        let (gate, r) = counting_gate(cfg_burst5());
        gate.type_run(7).await;
        assert_eq!(r.draws(), 8);
    }

    /// Real `natural` pacing: ten keystrokes must actually take time.
    #[tokio::test]
    async fn type_run_short_text_still_sleeps() {
        let gate = TimingGate::new(TimingCfg::default().apply_profile(TimingProfile::Natural));
        let start = Instant::now();
        gate.type_run(10).await;
        let took = start.elapsed();
        // `key_ms = [40, 140]` for ten characters → 400..1400 ms.
        assert!(took >= Duration::from_millis(300), "{took:?}");
        assert!(took < Duration::from_millis(2_000), "{took:?}");
    }

    /// Regression: without a sleep budget this took minutes.
    #[tokio::test]
    async fn type_run_bounded_even_for_very_long_text() {
        let gate = TimingGate::new(TimingCfg::default().apply_profile(TimingProfile::Natural));
        let start = Instant::now();
        gate.type_run(10_000).await;
        let took = start.elapsed();
        assert!(took >= Duration::from_millis(100), "{took:?}");
        assert!(took < Duration::from_millis(2_500), "{took:?}");
    }
}
