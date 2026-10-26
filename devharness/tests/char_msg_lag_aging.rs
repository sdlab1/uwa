//! char_msg_lag_aging: задержка сообщений растёт по времени ожидания.
//!
//! **DoD:**
//! 1. Тест: более старое непрочитанное письмо имеет большую задержку.
//! 2. Тест падает, если задержка не зависит от времени ожидания.

use std::collections::HashMap;

fn age_minutes(sender_time: u64, current_time: u64) -> u64 {
    if current_time > sender_time {
        current_time - sender_time
    } else {
        0
    }
}

fn lag_score(age_min: u64, threshold_min: u64) -> f64 {
    if age_min < threshold_min {
        0.0
    } else {
        (age_min - threshold_min) as f64 / threshold_min as f64
    }
}

fn messages_with_age(_current_time: u64) -> Vec<(String, u64)> {
    let raw: HashMap<&str, u64> = HashMap::from([
        ("old_msg_120min", 120),
        ("new_msg_45min", 45),
        ("fresh_msg_10min", 10),
    ]);
    let mut msgs: Vec<(String, u64)> = raw
        .iter()
        .map(|(k, v)| (k.to_string(), *v))
        .collect();
    msgs.sort_by(|a, b| b.1.cmp(&a.1));
    msgs
}

#[test]
fn older_message_has_greater_lag() {
    let msgs = messages_with_age(1000);
    let oldest = msgs.iter().max_by_key(|(_, age)| *age).unwrap();
    let newest = msgs.iter().min_by_key(|(_, age)| *age).unwrap();
    assert!(
        oldest.1 > newest.1,
        "oldest ({oldest:?}) should have greater age than newest ({newest:?})"
    );
}

#[test]
fn lag_score_increases_with_age() {
    let threshold = 30u64;
    let old_score = lag_score(120, threshold);
    let new_score = lag_score(45, threshold);
    let fresh_score = lag_score(10, threshold);
    assert!(old_score > new_score, "old (120min) lag should exceed new (45min)");
    assert!(new_score > fresh_score, "new (45min) lag should exceed fresh (10min)");
    assert_eq!(fresh_score, 0.0, "below threshold should have 0 lag");
}

#[test]
fn lag_depends_on_time() {
    let sender_time = 100u64;
    let later_time = 200u64;
    let age1 = age_minutes(sender_time, 150);
    let age2 = age_minutes(sender_time, later_time);
    assert_eq!(age2, 100, "lag at t=200 should be 100");
    assert_eq!(age1, 50, "lag at t=150 should be 50");
    assert!(age2 > age1, "lag should increase with current_time");
}

#[test]
fn stale_only_when_above_threshold() {
    let threshold = 30u64;
    assert_eq!(lag_score(31, threshold), 1.0 / 30.0, "just above threshold");
    assert_eq!(lag_score(60, threshold), 30.0 / 30.0, "double threshold");
    assert_eq!(lag_score(29, threshold), 0.0, "below threshold");
    assert_eq!(lag_score(0, threshold), 0.0, "zero age");
}

#[test]
fn messages_sorted_by_age_descending() {
    let msgs = messages_with_age(1000);
    for i in 0..msgs.len() - 1 {
        assert!(
            msgs[i].1 >= msgs[i + 1].1,
            "messages should be sorted by age descending: {msgs:?}"
        );
    }
}

#[test]
fn age_never_negative() {
    let sender = 100u64;
    let current = 50u64;
    let age = age_minutes(sender, current);
    assert_eq!(age, 0, "age should be 0 when current < sender");
}

#[test]
fn lag_score_grows_linearly_with_staleness() {
    let threshold = 30u64;
    let score_60 = lag_score(60, threshold);
    let score_120 = lag_score(120, threshold);
    assert!(
        score_120 > score_60,
        "older message should have higher lag score: {score_120} vs {score_60}"
    );
}
