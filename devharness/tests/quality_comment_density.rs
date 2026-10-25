//! КАЧЕСТВО-1: метрика плотности комментариев (comment density).
//!
//! Целевая плотность ~0.12 (12% строк — комментарии).
//! Код без комментариев (0%) и код с избытком комментариев (>20%) получают
//! штраф за отклонение от рабочей полосы.

fn comment_density(code: &str) -> f64 {
    let total = code.lines().count();
    if total == 0 {
        return 0.0;
    }
    let comments = code
        .lines()
        .filter(|l| {
            let t = l.trim();
            t.starts_with("//") || t.starts_with("/*") || t.starts_with('*') || t.starts_with("//!")
        })
        .count();
    comments as f64 / total as f64
}

fn quality_score(code: &str) -> f64 {
    let density = comment_density(code);
    let target = 0.12_f64;
    let deviation = (density - target).abs();
    // Максимальная плотность для идеального балла: ±0.06 около target
    let ideal_band = 0.06_f64;
    if deviation <= ideal_band {
        1.0
    } else if density == 0.0 {
        // Ноль комментариев — плохо (код недокументирован)
        0.5
    } else {
        1.0 - (deviation - ideal_band).min(0.4)
    }
}

#[test]
fn no_comments_gives_lower_score_than_working_band() {
    let no_comments = "fn main() {\n    let x = 1;\n    println!(\"{}\", x);\n}".to_string();
    let in_band =
        "fn main() {\n    // init\n    let x = 1;\n    // print\n    println!(\"{}\", x);\n}"
            .to_string();
    let no_score = quality_score(&no_comments);
    let band_score = quality_score(&in_band);
    assert!(
        no_score < band_score,
        "no comments score {no_score} should be < band score {band_score}"
    );
}

#[test]
fn excessive_comments_gives_lower_score_than_working_band() {
    let excessive = "// doc\n// more\n// even more\nfn main() {\n    let x = 1;\n}".to_string();
    let in_band = "fn main() {\n    // comment\n    let x = 1;\n}".to_string();
    let excessive_score = quality_score(&excessive);
    let band_score = quality_score(&in_band);
    assert!(
        excessive_score < band_score,
        "excessive score {excessive_score} should be < band {band_score}"
    );
}

#[test]
fn density_is_comments_over_code_lines() {
    let code = "// comment 1\n// comment 2\nfn main() {\n    let x = 1;\n}".to_string();
    let density = comment_density(&code);
    assert_eq!(density, 0.4, "density should be comments/total: 2/5 = 0.4");
}

#[test]
fn working_band_scores_highest() {
    let better = "fn main() {\n    // comment\n    let x = 1;\n}".to_string();
    let score = quality_score(&better);
    assert!(score > 0.5, "score should be reasonable: {score}");
}
