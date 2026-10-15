//! Pacing between DOM actions.
//!
//! Not fingerprint spoofing. A human typing into a chat box has pauses
//! that vary from a few hundred ms to a couple of seconds, and a fixed
//! 10 ms between every `input` event is what makes traffic look
//! robotic. This module describes *natural pacing*, not a way to defeat
//! detection.

use serde::{Deserialize, Serialize};

/// Named pacing profiles. `fast` is what CI uses; `natural` is the
/// default for a real session; `slow` is for sites that throttle hard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TimingProfile {
    /// No added delay. Machine-precise. Use in tests and the conformance
    /// suite.
    Fast,
    /// Jittered human pacing. The default.
    #[default]
    Natural,
    /// Longer pauses, slower typing. For sites where a burst of action
    /// trips a rate limiter.
    Slow,
    /// Custom values from `[timing.custom]`.
    Custom,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimingCfg {
    #[serde(default)]
    pub profile: TimingProfile,

    /// Milliseconds added after every `input` event. Range is
    /// `[lo, hi]`; a value is drawn uniformly from it.
    #[serde(default = "default_key_ms")]
    pub key_ms: [u64; 2],

    /// Milliseconds between the end of typing and the click on Send.
    /// Humans do not press Enter in the same tick they finish the last
    /// character.
    #[serde(default = "default_pre_send_ms")]
    pub pre_send_ms: [u64; 2],

    /// Milliseconds between the click and reading the response.
    #[serde(default = "default_post_send_ms")]
    pub post_send_ms: [u64; 2],

    /// Milliseconds of "settle" before the first interaction on a newly
    /// focused element. Skips the case where we type into a field that
    /// the page hasn't finished activating.
    #[serde(default = "default_focus_ms")]
    pub focus_ms: [u64; 2],

    /// Whether to chunk long inputs into bursts rather than typing all
    /// at once. Each burst is `burst_chars` characters, with `key_ms`
    /// in between; bursts themselves are separated by `burst_pause_ms`.
    #[serde(default = "default_burst")]
    pub burst: bool,

    #[serde(default = "default_burst_chars")]
    pub burst_chars: u32,

    #[serde(default = "default_burst_pause_ms")]
    pub burst_pause_ms: [u64; 2],
}

fn default_key_ms() -> [u64; 2] {
    [40, 140]
}
fn default_pre_send_ms() -> [u64; 2] {
    [120, 400]
}
fn default_post_send_ms() -> [u64; 2] {
    [100, 300]
}
fn default_focus_ms() -> [u64; 2] {
    [60, 200]
}
fn default_burst() -> bool {
    true
}
fn default_burst_chars() -> u32 {
    48
}
fn default_burst_pause_ms() -> [u64; 2] {
    [250, 700]
}

impl Default for TimingCfg {
    fn default() -> Self {
        Self {
            profile: TimingProfile::default(),
            key_ms: default_key_ms(),
            pre_send_ms: default_pre_send_ms(),
            post_send_ms: default_post_send_ms(),
            focus_ms: default_focus_ms(),
            burst: default_burst(),
            burst_chars: default_burst_chars(),
            burst_pause_ms: default_burst_pause_ms(),
        }
    }
}

impl TimingCfg {
    /// Overwrite the numeric ranges with the ones for a named profile.
    /// `Custom` leaves whatever the caller supplied.
    pub fn apply_profile(mut self, p: TimingProfile) -> Self {
        match p {
            TimingProfile::Fast => {
                self.key_ms = [0, 0];
                self.pre_send_ms = [0, 0];
                self.post_send_ms = [0, 0];
                self.focus_ms = [0, 0];
                self.burst = false;
            }
            TimingProfile::Natural => {
                self.key_ms = default_key_ms();
                self.pre_send_ms = default_pre_send_ms();
                self.post_send_ms = default_post_send_ms();
                self.focus_ms = default_focus_ms();
                self.burst = true;
                self.burst_chars = default_burst_chars();
                self.burst_pause_ms = default_burst_pause_ms();
            }
            TimingProfile::Slow => {
                self.key_ms = [120, 260];
                self.pre_send_ms = [400, 900];
                self.post_send_ms = [300, 800];
                self.focus_ms = [200, 500];
                self.burst = true;
                self.burst_chars = 24;
                self.burst_pause_ms = [600, 1400];
            }
            TimingProfile::Custom => {}
        }
        self
    }

    /// Validate ranges: `lo <= hi`, all values sane. Called from
    /// `Config::validate`.
    pub fn validate(&self) -> Result<(), String> {
        for (name, r) in [
            ("key_ms", self.key_ms),
            ("pre_send_ms", self.pre_send_ms),
            ("post_send_ms", self.post_send_ms),
            ("focus_ms", self.focus_ms),
            ("burst_pause_ms", self.burst_pause_ms),
        ] {
            if r[0] > r[1] {
                return Err(format!(
                    "timing.{name}: lo ({}) must not exceed hi ({})",
                    r[0], r[1]
                ));
            }
            if r[1] > 60_000 {
                return Err(format!(
                    "timing.{name}: hi ({}) is unreasonably large",
                    r[1]
                ));
            }
        }
        if self.burst && self.burst_chars == 0 {
            return Err("timing.burst_chars must be ≥ 1 when burst = true".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fast_is_effectively_instant() {
        let t = TimingCfg::default().apply_profile(TimingProfile::Fast);
        assert!(t.key_ms[1] <= 10);
        assert!(!t.burst);
    }

    #[test]
    fn natural_has_jitter() {
        let t = TimingCfg::default().apply_profile(TimingProfile::Natural);
        assert!(t.key_ms[0] < t.key_ms[1]);
        assert!(t.pre_send_ms[0] < t.pre_send_ms[1]);
    }

    #[test]
    fn custom_preserves_supplied_values() {
        let t = TimingCfg {
            key_ms: [7, 7],
            ..TimingCfg::default()
        };
        let applied = t.apply_profile(TimingProfile::Custom);
        assert_eq!(applied.key_ms, [7, 7]);
    }

    #[test]
    fn validate_rejects_inverted_range() {
        let t = TimingCfg {
            key_ms: [100, 50],
            ..TimingCfg::default()
        };
        assert!(t.validate().is_err());
    }

    #[test]
    fn validate_rejects_absurd_range() {
        let t = TimingCfg {
            pre_send_ms: [0, 120_000],
            ..TimingCfg::default()
        };
        assert!(t.validate().is_err());
    }
}
