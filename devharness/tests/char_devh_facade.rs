//! char_devh_facade: devh-скрипт использует --bin devh в cargo run.
//!
//! **DoD T-639**: после появления второго бинаря (devh-panel), `cargo run`
//! без `--bin` падает с "could not determine which binary to run".
//! Тест проверяет содержимое devharness/devh на наличие --bin devh.
//!
//! Цикломатическая сложность: C1 (одна проверка содержимого файла).

use std::fs;

#[test]
fn devh_uses_bin_flag() {
    let content = fs::read_to_string("devharness/devh")
        .expect("devharness/devh must exist");
    // Исправление T-639: cargo run --bin devh
    assert!(
        content.contains("--bin devh"),
        "devharness/devh должен использовать 'cargo run --bin devh', иначе после появления \
         второго бинаря (devh-panel) cargo run падает с 'could not determine which binary to run'"
    );
}
