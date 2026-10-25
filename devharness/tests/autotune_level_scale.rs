//! АВТОТЮН-6: штраф нормирован по уровню сложности задачи (C-уровень).
//!
//! Тесты проверяют что:
//! 1. Одинаковые ошибки на простой задаче штрафуют сильнее, чем на сложной
//! 2. Нормировка использует level_scale = 1 + C

use std::collections::HashMap;

fn level_scale(c: usize) -> f64 {
    1.0 + c as f64
}

fn penalty(error: &str, c: usize) -> f64 {
    let base: f64 = match error {
        "E0433" => 10.0,
        "E0308" => 5.0,
        "E0277" => 7.0,
        "E0507" => 8.0,
        _ => 1.0,
    };
    base / level_scale(c)
}

#[test]
fn same_error_penalty_scales_with_c() {
    let error = "E0308";
    let c0_penalty = penalty(error, 0);
    let c3_penalty = penalty(error, 3);
    let c5_penalty = penalty(error, 5);

    assert!(
        c0_penalty > c3_penalty,
        "C0 penalty ({c0_penalty}) должен быть больше C3 ({c3_penalty})"
    );
    assert!(
        c3_penalty > c5_penalty,
        "C3 penalty ({c3_penalty}) должен быть больше C5 ({c5_penalty})"
    );
}

#[test]
fn level_scale_formula() {
    assert_eq!(level_scale(0), 1.0);
    assert_eq!(level_scale(1), 2.0);
    assert_eq!(level_scale(3), 4.0);
    assert_eq!(level_scale(5), 6.0);
    assert_eq!(level_scale(10), 11.0);
}

#[test]
fn c0_hardest_c5_softest() {
    let errors = vec!["E0433", "E0308", "E0277", "E0507"];
    for e in &errors {
        let p0 = penalty(e, 0);
        let p5 = penalty(e, 5);
        assert!(
            p0 > p5,
            "Ошибка {e}: penalty C0 ({p0}) > C5 ({p5}) должно быть"
        );
    }
}

#[test]
fn penalty_normalized_by_level() {
    let mut penalties: HashMap<usize, f64> = HashMap::new();
    let error = "E0433";
    for c in 0..=5 {
        penalties.insert(c, penalty(error, c));
    }
    let p0 = penalties[&0];
    let p3 = penalties[&3];
    let p5 = penalties[&5];
    assert!(p0 > p3 && p3 > p5, "Нормировка: {p0} > {p3} > {p5}");
}
