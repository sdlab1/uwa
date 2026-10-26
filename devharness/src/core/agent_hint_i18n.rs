//! Подсказка оркестратора агенту — на ЕГО родном языке.
//!
//! Самая важная информация оркестратора (потолок, рейтинг относительно
//! эталона, следующий шаг) должна быть на языке модели: тогда её понимают
//! без искажений и выполняют.
//!
//! Язык выбирается по модели (`core::i18n::Lang::for_model`): китайские
//! модели → zh, mistral → fr, остальные → en.
//!
//! Рейтинг берётся из честного сравнения с эталоном уровня
//! ([`crate::core::calibration`]): отношение среднего балла агента к баллу
//! эталона, переведённое в шкалу «неудовлетворительно…отлично».

use crate::core::calibration::{self, Rating};
use crate::core::i18n::{self, Lang};
use std::path::Path;

/// Подсказка агенту.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentHint {
    /// Канонический адрес агента.
    pub agent: String,
    /// Язык подсказки.
    pub lang: Lang,
    /// Текущий потолок агента.
    pub ceiling: u8,
    /// Рейтинг относительно эталона (None — эталона ещё нет).
    pub rating: Option<Rating>,
    /// Отношение балла агента к эталону (1.0 = равенство).
    pub ratio: Option<f64>,
    /// Комплексный float отличия (со знаком), если посчитан.
    pub deviation: Option<f64>,
    /// Следующий шаг (команда).
    pub next_step: String,
    /// Готовый текст подсказки.
    pub text: String,
}

/// Локализованное название оценки.
pub fn rating_label(rating: Rating, lang: Lang) -> &'static str {
    match (rating, lang) {
        (Rating::Unsatisfactory, Lang::En) => "unsatisfactory",
        (Rating::Unsatisfactory, Lang::Zh) => "不合格",
        (Rating::Unsatisfactory, Lang::Fr) => "insuffisant",
        (Rating::Poor, Lang::En) => "poor",
        (Rating::Poor, Lang::Zh) => "较差",
        (Rating::Poor, Lang::Fr) => "faible",
        (Rating::Normal, Lang::En) => "normal",
        (Rating::Normal, Lang::Zh) => "正常",
        (Rating::Normal, Lang::Fr) => "normal",
        (Rating::Good, Lang::En) => "good",
        (Rating::Good, Lang::Zh) => "良好",
        (Rating::Good, Lang::Fr) => "bon",
        (Rating::Excellent, Lang::En) => "excellent",
        (Rating::Excellent, Lang::Zh) => "优秀",
        (Rating::Excellent, Lang::Fr) => "excellent",
    }
}

/// Текст подсказки по частям (без обращения к диску).
pub fn render_text(
    lang: Lang,
    ceiling: u8,
    rating: Option<Rating>,
    ratio: Option<f64>,
    next_step: &str,
) -> String {
    let rating_text = match rating {
        Some(r) => rating_label(r, lang).to_string(),
        None => match lang {
            Lang::En => "no reference yet".to_string(),
            Lang::Zh => "эталон ещё не собран".to_string(),
            Lang::Fr => "pas encore de référence".to_string(),
        },
    };
    let ratio_text = match ratio {
        Some(r) => format!("{r:.3}"),
        None => "-".to_string(),
    };
    let template = match lang {
        Lang::En => {
            "Your current ceiling: C{N}. Rating vs reference: {R} (ratio {RATIO}). Next step: {CMD}"
        }
        Lang::Zh => {
            "你当前的级别上限：C{N}。相对基准的评级：{R}（比率 {RATIO}）。下一步：{CMD}"
        }
        Lang::Fr => {
            "Ton plafond actuel : C{N}. Note par rapport à la référence : {R} (ratio {RATIO}). Étape suivante : {CMD}"
        }
    };
    i18n::render(
        template,
        &[
            ("N", &ceiling.to_string()),
            ("R", &rating_text),
            ("RATIO", &ratio_text),
            ("CMD", next_step),
        ],
    )
}

/// Собрать подсказку агенту: потолок + рейтинг относительно эталона + шаг.
pub fn build(root: &Path, agent: &str, model: &str, ceiling: u8, next_step: &str) -> AgentHint {
    let lang = Lang::for_model(model);
    let canon = crate::core::identity::canonical(agent);
    let mut rating: Option<Rating> = None;
    let mut ratio: Option<f64> = None;
    let mut deviation: Option<f64> = None;

    // Уровни от новых к старым: если эталонов несколько, берём худший
    // (осторожная оценка) — иначе агент увидит «отлично» по C1 и не заметит,
    // что на C3 он провалился.
    for level in 0..=crate::core::calibration_guard::LEVELS_MAX {
        let Some(reference) = calibration::latest_reference(root, level) else {
            continue;
        };
        let ms = calibration::measure_tasks(root, &canon, Some(level));
        if ms.is_empty() {
            continue;
        }
        let avg = ms.iter().map(|m| m.score()).sum::<f64>() / ms.len() as f64;
        if reference.metrics.score <= 0.0 {
            continue;
        }
        let r = avg / reference.metrics.score;
        let dev = ms
            .iter()
            .map(|m| calibration::deviation_float(m, &reference))
            .sum::<f64>()
            / ms.len() as f64;
        let worse = match ratio {
            None => true,
            Some(prev) => r < prev,
        };
        if worse {
            rating = Some(calibration::rating_by_ratio(r));
            ratio = Some(r);
            deviation = Some(dev);
        }
    }

    let text = render_text(lang, ceiling, rating, ratio, next_step);
    AgentHint {
        agent: canon,
        lang,
        ceiling,
        rating,
        ratio,
        deviation,
        next_step: next_step.to_string(),
        text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::quality::QualitySample;
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::PathBuf;

    fn tmp_root(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("devh-agent-hint-{}-{}", tag, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("создать tmp");
        dir
    }

    fn sample(task: &str, level: u8, agent: &str) -> QualitySample {
        QualitySample {
            ts: 1,
            agent: agent.to_string(),
            model: "test".to_string(),
            task_id: Some(task.to_string()),
            c_level: level,
            added_lines: 10,
            comment_lines: 2,
            code_lines: 8,
            check_runs: 1,
            errors_final: 0,
            warnings_final: 0,
            errors_weighted: 0.0,
            warnings_weighted: 0.0,
            error_categories: BTreeMap::new(),
            warning_categories: BTreeMap::new(),
            build_ok: true,
            duration_secs: Some(10.0),
            deliberation_actions: 3,
            rework_rounds: 0,
            identified: true,
        }
    }

    #[test]
    fn language_follows_model() {
        assert_eq!(Lang::for_model("deepseek-v4.1-flash-free"), Lang::Zh);
        assert_eq!(Lang::for_model("mistral-large"), Lang::Fr);
        assert_eq!(Lang::for_model("gpt-5"), Lang::En);
    }

    #[test]
    fn text_has_ceiling_rating_and_next_step() {
        let text = render_text(Lang::En, 3, Some(Rating::Normal), Some(1.0), "devh next");
        assert!(text.contains("C3"));
        assert!(text.contains("normal"));
        assert!(text.contains("devh next"));
    }

    #[test]
    fn chinese_and_french_variants() {
        let zh = render_text(Lang::Zh, 2, Some(Rating::Good), Some(1.1), "devh next");
        assert!(zh.contains("C2"));
        assert!(zh.contains("良好"));
        let fr = render_text(Lang::Fr, 2, Some(Rating::Good), Some(1.1), "devh next");
        assert!(fr.contains("C2"));
        assert!(fr.contains("bon"));
    }

    #[test]
    fn build_reads_reference_and_measures_agent() {
        let root = tmp_root("build");
        for i in 1..=4 {
            crate::core::quality::append_sample(&root, &sample(&format!("R-{i}"), 1, "minimax"))
                .unwrap();
        }
        let reference = calibration::build_reference(&root, "minimax", 1).expect("эталон");
        calibration::append_reference(&root, &reference).unwrap();
        for i in 1..=2 {
            crate::core::quality::append_sample(&root, &sample(&format!("A-{i}"), 1, "beam"))
                .unwrap();
        }
        let hint = build(&root, "beam", "deepseek-v4", 2, "devh next");
        assert_eq!(hint.lang, Lang::Zh);
        assert!(hint.rating.is_some());
        assert!(hint.ratio.is_some());
        assert!(hint.text.contains("C2"));
        assert!(hint.text.contains("下一步"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn no_reference_means_unknown_rating() {
        let root = tmp_root("noref");
        let hint = build(&root, "beam", "gpt-5", 1, "devh next");
        assert!(hint.rating.is_none());
        assert!(hint.text.contains("no reference yet"));
        let _ = fs::remove_dir_all(&root);
    }
}
