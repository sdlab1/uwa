//! ЯЗЫК ВАЖНОЙ ИНФОРМАЦИИ ОРКЕСТРАТОРА.
//!
//! Самая важная информация для агента (его потолок, что скрыто, что делать
//! дальше) должна быть на ЕГО родном языке — тогда модель понимает её лучше
//! и сотрудничает охотнее.
//!
//! Язык выбирается по модели:
//! * `zh` — китайские модели (deepseek, qwen, kimi, glm, minimax);
//! * `fr` — mistral;
//! * `en` — всё остальное (английский — lingua franca).

use serde::{Deserialize, Serialize};

/// Язык вывода важной информации оркестратора.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Lang {
    /// Английский (по умолчанию).
    En,
    /// Китайский.
    Zh,
    /// Французский (mistral).
    Fr,
}

impl Lang {
    /// Код языка (для вывода и ledger'а).
    pub fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::Zh => "zh",
            Lang::Fr => "fr",
        }
    }

    /// Язык по коду: неизвестный код → английский.
    pub fn from_code(code: &str) -> Lang {
        match code.trim().to_ascii_lowercase().as_str() {
            "zh" | "cn" | "chinese" => Lang::Zh,
            "fr" | "french" => Lang::Fr,
            _ => Lang::En,
        }
    }

    /// Язык по имени модели.
    ///
    /// Mistral → французский, китайские модели → китайский, остальное →
    /// английский.
    pub fn for_model(model: &str) -> Lang {
        let m = model.trim().to_ascii_lowercase();
        if m.is_empty() {
            return Lang::En;
        }
        if m.contains("mistral") || m.contains("mixtral") {
            return Lang::Fr;
        }
        if m.contains("deepseek")
            || m.contains("qwen")
            || m.contains("kimi")
            || m.contains("moonshot")
            || m.contains("glm")
            || m.contains("chatglm")
            || m.contains("minimax")
            || m.contains("yi-")
            || m.contains("baichuan")
        {
            return Lang::Zh;
        }
        Lang::En
    }
}

/// Ключи важной информации оркестратора.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Key {
    /// «Твой текущий потолок: C{N}».
    CeilingNow,
    /// «Скрыто задач выше потолка: {N}».
    HiddenAbove,
    /// «Задача уже взята (статус {S}), двойная работа запрещена».
    TaskTaken,
    /// «Твой потолок понижен: {C1} → {C2}».
    CeilingLowered,
    /// «Сначала ответь, какая ты модель (три строки MODEL/VERSION/DATE)».
    IdentificationNeeded,
    /// «Проверка не прошла: {CMD} (код {CODE})».
    CheckFailed,
    /// «Следующий шаг: {CMD}».
    SubmitTask,
}

impl Key {
    /// Текст ключа на нужном языке.
    ///
    /// `{N}`, `{C1}`, `{C2}`, `{S}`, `{CMD}`, `{CODE}` — подставляемые места.
    pub fn text(self, lang: Lang) -> &'static str {
        match (self, lang) {
            // --- CeilingNow ---
            (Key::CeilingNow, Lang::En) => "Your current ceiling: C{N}",
            (Key::CeilingNow, Lang::Zh) => "你当前的级别上限：C{N}",
            (Key::CeilingNow, Lang::Fr) => "Ton plafond actuel : C{N}",
            // --- HiddenAbove ---
            (Key::HiddenAbove, Lang::En) => "Hidden tasks above your ceiling: {N}",
            (Key::HiddenAbove, Lang::Zh) => "有 {N} 个任务高于你的上限，已隐藏",
            (Key::HiddenAbove, Lang::Fr) => "Tâches cachées au-dessus de ton plafond : {N}",
            // --- TaskTaken ---
            (Key::TaskTaken, Lang::En) => "Task already taken (status {S}) — double work is forbidden",
            (Key::TaskTaken, Lang::Zh) => "任务已被领取（状态 {S}）——禁止重复劳动",
            (Key::TaskTaken, Lang::Fr) => "Tâche déjà prise (statut {S}) — le double travail est interdit",
            // --- CeilingLowered ---
            (Key::CeilingLowered, Lang::En) => "Your ceiling was lowered: {C1} -> {C2}",
            (Key::CeilingLowered, Lang::Zh) => "你的级别上限被下调：{C1} → {C2}",
            (Key::CeilingLowered, Lang::Fr) => "Ton plafond a été abaissé : {C1} -> {C2}",
            // --- IdentificationNeeded ---
            (Key::IdentificationNeeded, Lang::En) => {
                "Identify yourself first: MODEL / VERSION / DATE (three lines, verbatim)"
            }
            (Key::IdentificationNeeded, Lang::Zh) => {
                "请先表明身份：MODEL / VERSION / DATE（三行，逐字回答）"
            }
            (Key::IdentificationNeeded, Lang::Fr) => {
                "Identifie-toi d'abord : MODEL / VERSION / DATE (trois lignes, verbatim)"
            }
            // --- CheckFailed ---
            (Key::CheckFailed, Lang::En) => "Check failed: {CMD} (exit code {CODE})",
            (Key::CheckFailed, Lang::Zh) => "检查未通过：{CMD}（退出码 {CODE}）",
            (Key::CheckFailed, Lang::Fr) => "Échec de la vérification : {CMD} (code {CODE})",
            // --- SubmitTask ---
            (Key::SubmitTask, Lang::En) => "Next step: {CMD}",
            (Key::SubmitTask, Lang::Zh) => "下一步：{CMD}",
            (Key::SubmitTask, Lang::Fr) => "Étape suivante : {CMD}",
        }
    }
}

/// Текст ключа с подстановкой: `{{N}}` → значение.
pub fn render(text: &str, args: &[(&str, &str)]) -> String {
    let mut out = text.to_string();
    for (k, v) in args {
        out = out.replace(&format!("{{{k}}}"), v);
    }
    out
}

/// Удобно: текст ключа на языке модели с подстановкой.
pub fn text_for(model: &str, key: Key, args: &[(&str, &str)]) -> String {
    render(key.text(Lang::for_model(model)), args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_follows_model() {
        assert_eq!(Lang::for_model("mistral-large-latest"), Lang::Fr);
        assert_eq!(Lang::for_model("deepseek-v3"), Lang::Zh);
        assert_eq!(Lang::for_model("MiniMax-M3.1"), Lang::Zh);
        assert_eq!(Lang::for_model("kilo/poolside/laguna-s-2.1:free"), Lang::En);
        assert_eq!(Lang::for_model(""), Lang::En);
    }

    #[test]
    fn every_key_has_all_three_languages() {
        for key in [
            Key::CeilingNow,
            Key::HiddenAbove,
            Key::TaskTaken,
            Key::CeilingLowered,
            Key::IdentificationNeeded,
            Key::CheckFailed,
            Key::SubmitTask,
        ] {
            for lang in [Lang::En, Lang::Zh, Lang::Fr] {
                let t = key.text(lang);
                assert!(!t.trim().is_empty(), "{key:?} {lang:?} пусто");
            }
            // Строки на разных языках не должны совпадать.
            assert_ne!(key.text(Lang::En), key.text(Lang::Zh), "{key:?}");
            assert_ne!(key.text(Lang::En), key.text(Lang::Fr), "{key:?}");
        }
    }

    #[test]
    fn placeholders_are_rendered() {
        let s = text_for("mistral-large", Key::CeilingNow, &[("N", "2")]);
        assert!(s.contains('2'), "{s}");
        assert!(!s.contains("{N}"), "{s}");
    }

    #[test]
    fn from_code_is_lenient() {
        assert_eq!(Lang::from_code("ZH"), Lang::Zh);
        assert_eq!(Lang::from_code(" fr "), Lang::Fr);
        assert_eq!(Lang::from_code("de"), Lang::En);
    }
}
