//! T-392: сумма весов качества = 1.0, индекс способности в [0,1]

use devh::core::quality::{capability_index, QualityVector, QUALITY_WEIGHTS, QUALITY_TERMS};

#[test]
fn quality_weights_sum_is_one() {
    let sum: f64 = QUALITY_WEIGHTS.iter().sum();
    assert!((sum - 1.0).abs() < 1e-9, "сумма весов должна быть 1.0, получено: {sum}");
}

#[test]
fn capability_index_is_in_0_1_for_all_zeros() {
    let zero: QualityVector = [0.0; QUALITY_TERMS];
    let idx = capability_index(&zero);
    assert!((0.0..=1.0).contains(&idx), "capability_index(zeros) должен быть в [0,1], получено: {idx}");
}

#[test]
fn capability_index_is_in_0_1_for_all_ones() {
    let ones: QualityVector = [1.0; QUALITY_TERMS];
    let idx = capability_index(&ones);
    assert!((0.0..=1.0).contains(&idx), "capability_index(ones) должен быть в [0,1], получено: {idx}");
}

#[test]
fn capability_index_is_in_0_1_for_random_vectors() {
    // случайные векторы должны давать индекс в [0,1] благодаря clamp
    let vectors: Vec<QualityVector> = vec![
        [0.5; 6],
        [0.1, 0.2, 0.3, 0.4, 0.5, 0.6],
        [0.0, 1.0, 0.0, 1.0, 0.0, 1.0],
        [1.0, 0.0, 1.0, 0.0, 1.0, 0.0],
    ];
    for v in vectors {
        let idx = capability_index(&v);
        assert!((0.0..=1.0).contains(&idx), "capability_index должен быть в [0,1] для {:?}, получено: {}", v, capability_index(&v));
    }
}

#[test]
fn capability_index_out_of_bounds_fails() {
    // capability_index всегда в [0,1] благодаря clamp, но проверяем границы
    let mut v = [1.0; 6];
    v[0] = 2.0; // значение > 1.0
    let idx = capability_index(&v);
    assert!((0.0..=1.0).contains(&idx), "clamp должен держать в [0,1] даже для значений > 1");
    
    v[0] = -1.0; // значение < 0
    let idx = capability_index(&v);
    assert!((0.0..=1.0).contains(&idx), "clamp должен держать в [0,1] даже для значений < 0");
}

#[test]
fn weights_sum_changes_fails_test() {
    // Этот тест проверяет, что если веса изменятся, тест упадёт
    // (демонстрационный - в реальности веса константы)
    let original_sum: f64 = devh::core::quality::QUALITY_WEIGHTS.iter().sum();
    assert!((original_sum - 1.0).abs() < 1e-9, "сумма весов должна быть 1.0");
}