use std::path::PathBuf;

use super::tripwire::Mode;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ItemId(pub u16);

impl ItemId {
    pub fn dir_name(&self) -> String {
        format!("W-{:03}", self.0)
    }

    pub fn branch(&self) -> String {
        format!("devh/W-{:03}", self.0)
    }

    pub fn resolve(s: &str) -> Option<Self> {
        let s = s.trim();
        let digits = s.trim_start_matches(['W', 'w']).trim_start_matches('-');
        if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        digits.parse().ok().map(ItemId)
    }

    /// Ветка задачи → id (`devh/W-007` → W-007); чужая/не наша ветка → None.
    pub fn from_branch(b: &str) -> Option<Self> {
        Self::resolve(b.strip_prefix("devh/")?)
    }
}

impl std::fmt::Display for ItemId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.dir_name())
    }
}

/// Настройки rate-гейта (devh rate): нагрузочные окна R18/R19/R20.
///
/// build/check/test — не чаще 1 раза в 15 мин; мутационный аудит — не чаще
/// 1 раза в 2 ч; CPU/system load — не выше 80%. None = дефолты
/// (900 / 7200 / 80). Ключи конфига: RATE_BUILD_MAX_DURATION,
/// RATE_MUTATE_MAX_DURATION, RATE_CPU_MAX_PCT, RATE_HISTORY_DAYS.
#[derive(Debug, Clone)]
pub struct RateCfg {
    /// Лимит длительности build (секунды). Дефолт 900 (15 мин).
    pub build_max_secs: u32,
    /// Лимит длительности mutate/audit (секунды). Дефолт 7200 (2 ч).
    pub mutate_max_secs: u32,
    /// Лимит CPU%. Дефолт 80.
    pub cpu_max_pct: u32,
    /// Сколько дней хранить историю запусков. Дефолт 7.
    pub history_days: u32,
}

#[derive(Debug, Clone)]
pub struct MutateCfg {
    pub tool: String,
    pub scope: String,
    pub min_kill: u32,
    /// F-3 (MUT_BLIND): слепой гейт — что делать, когда «0 мутантов» без
    /// честной причины (exclude_re/skip/пустой diff не найдены): off —
    /// прежний тихий Pass (код-дефолт), warn — Pass + ⚠, enforce — Fail
    /// (дыра W3: поимки нет вообще). Ставится в шаблоне init (warn).
    pub blind: crate::core::gates::BlindMode,
    /// P7-2.3: ранний выход — DOOM (kill-rate недостижим) / SAMPLE
    /// (rule-of-three при --shuffle). По умолчанию выключен.
    pub early: bool,
    /// Problem-first: останов синхронного прогона на ПЕРВОЙ проблеме
    /// (выжил / таймаут / не собрался / без тестовой фазы / неизвестный
    /// исход). По умолчанию ВКЛЮЧЁН: проблема actionable, ждать конца
    /// прогона незачем. Выключатель: `MUTATE_FIRST_PROBLEM=0` в
    /// `.harness/config` (или `HARNESS_MUTATE_FIRST_PROBLEM=0` в окружении);
    /// пустое значение и мусор гейт НЕ отключают.
    pub first_problem: bool,
    /// P7-2.4: покрытийный отбор тестов (llvm-cov -> nextest -E).
    /// По умолчанию выключен (требует nightly rustc для -Zinstrument-coverage).
    pub cov: bool,
    /// P8-1: третья петля — MissedMutant'ы прогона в `.harness/observations.md`
    /// (seen-дедуп + killmap + survivors.log). По умолчанию выключен.
    pub survivors: bool,
}

#[derive(Debug, Clone)]
pub struct CrapCfg {
    pub tool: String,
    pub scope: String,
    pub max: u32,
}

/// Каталог worktree'ов по умолчанию: НЕ `/tmp`.
///
/// `/tmp` живёт только до ребута: незакоммиченная работа агента исчезала
/// вместе с каталогом. Каноническое место — `$XDG_DATA_HOME/devh/worktrees`,
/// иначе `~/.local/share/devh/worktrees`; `/tmp` — только запасной вариант.
pub fn default_wt_root() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
        let xdg = xdg.trim();
        if !xdg.is_empty() {
            return PathBuf::from(xdg).join("devh").join("worktrees");
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        let home = home.trim();
        if !home.is_empty() {
            return PathBuf::from(home)
                .join(".local")
                .join("share")
                .join("devh")
                .join("worktrees");
        }
    }
    PathBuf::from("/tmp")
}

#[derive(Debug, Clone)]
pub struct RunConfig {
    pub verify: String,
    pub bootstrap: String,
    pub agent: String,
    pub agent_timeout: u32,
    pub max_fix: u32,
    /// A8: повторов VERIFY при падении в раунде 1 (контеншен-флейк).
    /// Код-дефолт 0 = текущее поведение; шаблон init ставит 0 с комментарием.
    pub verify_retries: u32,
    pub max_active: u32,
    pub remote: String,
    pub main_branch: String,
    pub wt_root: PathBuf,
    /// P1a: один стабильный worktree-путь на все задачи (тёплый инкремент).
    /// По умолчанию выключен; `devh run` включает, `bg`/параллель — нет.
    pub wt_reuse: bool,
    pub log_days: u32,
    pub mutate: Option<MutateCfg>,
    pub red_mutate: Option<MutateCfg>,
    /// R18/R19/R20: нагрузочные окна (build 15 мин, аудит 2 ч, CPU 80%).
    /// None = используются дефолты RateCfg (900/7200/80).
    pub rate: Option<RateCfg>,
    /// P7 (red.expect): ожидаемая подпись падения red-фазы (regex). Глобальный
    /// дефолт; пер-тасковый источник — строка спеки `RED-EXPECT:` или маркер
    /// `# red-expect:` в red.cmd (приоритет: маркер → спека → этот ключ).
    pub red_expect: Option<String>,
    pub crap: Option<CrapCfg>,
    pub bench: Option<String>,
    pub no_ignored: bool,
    pub ignored_allowlist: Vec<String>,
    /// A6 FREEZE-гейт: дифф сессии не выходит за заявленные файлы.
    /// Код-дефолт false (шаблон init ставит 1): гейт первым в лестнице,
    /// считается по диффу, allowlist = spec FILES: + FREEZE_ALLOW.
    pub freeze: bool,
    /// F-2.1: scope обязателен. Код-дефолт false; шаблон init ставит 1.
    /// При `true` пустой scope без `no-files` — SpecBlock, а не Pass.
    pub freeze_required: bool,
    /// Глобальный allowlist FREEZE (пробел-разделённые глобы), поверх
    /// per-item items/W-NNN/files из спеки.
    pub freeze_allow: Vec<String>,
    /// F-2.2: детектор ослабления тестов off|warn|enforce.
    /// Код-дефолт off; шаблон init ставит enforce.
    pub weaken: crate::core::weaken::Mode,
    pub lint: bool,
    pub lint_cmd: String,
    pub review: bool,
    /// F-0.3: предел размера диффа (строк) для REVIEW-гейта. Превышение —
    /// `SpecBlock`: это спека слишком велика, её дробит человек, а не
    /// агент fix-раундами и не ревьюер на половине диффа. `0` = без лимита
    /// (аварийный клапан).
    pub review_diff_max: u32,
    /// B1: печатать строку уровня автономии в конце `devh merge`
    /// (и в `devh escapes`). Только отображение: механических запретов
    /// уровень не создаёт — решения человека, цифры машинные.
    pub autonomy: bool,
    /// E3: порог свежести корпуса — снимок прогона test.sh старше N дней
    /// не даёт права на уровень (уровень растёт только по свежему 100%).
    /// Референс — дата последней сессии log.md, НЕ wall clock.
    pub corpus_fresh_days: u32,
    /// F-1: Tripwire — защита входов гейтов от правок агента.
    pub tripwire: Mode,
    /// Дополнительные защищённые глобы (относительно worktree).
    pub protected: Vec<String>,
    pub msg_deny: Option<String>,
    pub rules: Option<PathBuf>,
    pub rules_log: Option<PathBuf>,
    /// Автотюн потолка сложности по эмпирике `cargo check` (core::autotune).
    /// Включён по умолчанию; выключается `AUTOTUNE=0`.
    pub autotune_enabled: bool,
    /// Вес ошибок компиляции в штрафе потолка (по умолчанию 1.2).
    pub autotune_error_weight: f64,
    /// Вес предупреждений компиляции в штрафе потолка (по умолчанию 0.35).
    pub autotune_warning_weight: f64,
    /// Вклад доли успешных сборок в штраф (по умолчанию 1.0).
    pub autotune_success_bonus: f64,
    /// Минимум наблюдений, иначе эмпирика не доверена (по умолчанию 3).
    pub autotune_min_samples: u32,
    /// TTL идентификации модели: сколько живёт декларация (секунды, 3600).
    pub identify_ttl_secs: u64,
    /// Вес числа повторных прогонов `cargo check` в штрафе (по умолчанию 0.15).
    pub autotune_runs_weight: f64,
    /// Вес медленной починки ошибок в штрафе (по умолчанию 0.4).
    pub autotune_fix_latency_weight: f64,
    /// Референс времени починки (секунды, по умолчанию 300).
    pub autotune_fix_latency_ref_secs: f64,
    /// Бонус за фактически починенные ошибки (по умолчанию 0.5).
    pub autotune_fix_bonus: f64,
    /// Вес «грязных» сборок (успешных, но с предупреждениями, 0.2).
    pub autotune_neatness_weight: f64,
    /// AUTOWIP (T-321/T-643): период автоматического WIP-сохранения работы
    /// агента, минут. `None` = выключено (`WIP_INTERVAL_MIN=off`).
    ///
    /// Правила разбора: пусто/нет ключа/0/мусор → дефолт 10 минут;
    /// `off` → выключено; иначе число минут (>0).
    pub wip_interval_min: Option<u32>,
}

/// AUTOWIP: период по умолчанию (минут).
pub const DEFAULT_WIP_INTERVAL_MIN: u32 = 10;

/// Разобрать настройку `WIP_INTERVAL_MIN`.
///
/// `None`/пусто → дефолт 10 минут; `off` → выключено (`None`); `0` и мусор →
/// дефолт (включение НЕ ломается мусором — как у прочих гейтов).
pub fn parse_wip_interval(raw: Option<&str>) -> Option<u32> {
    let Some(v) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Some(DEFAULT_WIP_INTERVAL_MIN);
    };
    if v.eq_ignore_ascii_case("off") || v.eq_ignore_ascii_case("выкл") {
        return None;
    }
    match v.parse::<u32>() {
        Ok(n) if n > 0 => Some(n),
        _ => Some(DEFAULT_WIP_INTERVAL_MIN),
    }
}

impl Default for RunConfig {
    fn default() -> Self {
        Self {
            verify: String::new(),
            bootstrap: String::new(),
            agent: "goose run --no-session -t".into(),
            agent_timeout: 1800,
            max_fix: 3,
            verify_retries: 0,
            max_active: 3,
            remote: "origin".into(),
            main_branch: "main".into(),
            wt_root: default_wt_root(),
            wt_reuse: false,
            log_days: 30,
            mutate: None,
            red_mutate: None,
            rate: None,
            red_expect: None,
            crap: None,
            bench: None,
            no_ignored: true,
            ignored_allowlist: Vec::new(),
            freeze: false,
            freeze_required: false,
            freeze_allow: Vec::new(),
            weaken: crate::core::weaken::Mode::Off,
            lint: true,
            lint_cmd: String::new(),
            review: true,
            review_diff_max: 2000,
            autonomy: true,
            corpus_fresh_days: crate::core::escapes::CORPUS_FRESH_DAYS,
            tripwire: Mode::Off,
            protected: Vec::new(),
            msg_deny: None,
            rules: None,
            rules_log: None,
            autotune_enabled: true,
            autotune_error_weight: crate::core::eval_config::DEFAULTS.autotune_error_weight,
            autotune_warning_weight: crate::core::eval_config::DEFAULTS.autotune_warning_weight,
            autotune_success_bonus: crate::core::eval_config::DEFAULTS.autotune_success_bonus,
            autotune_min_samples: crate::core::eval_config::DEFAULTS.autotune_min_samples,
            identify_ttl_secs: 3600,
            autotune_runs_weight: crate::core::eval_config::DEFAULTS.autotune_runs_weight,
            autotune_fix_latency_weight: crate::core::eval_config::DEFAULTS
                .autotune_fix_latency_weight,
            autotune_fix_latency_ref_secs: crate::core::eval_config::DEFAULTS
                .autotune_fix_latency_ref_secs,
            autotune_fix_bonus: crate::core::eval_config::DEFAULTS.autotune_fix_bonus,
            autotune_neatness_weight: crate::core::eval_config::DEFAULTS.autotune_neatness_weight,
            wip_interval_min: Some(DEFAULT_WIP_INTERVAL_MIN),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_accepts_variants() {
        assert_eq!(ItemId::resolve("40"), Some(ItemId(40)));
        assert_eq!(ItemId::resolve("W-040"), Some(ItemId(40)));
        assert_eq!(ItemId::resolve("w-40"), Some(ItemId(40)));
        assert_eq!(ItemId::resolve("008"), Some(ItemId(8)));
        assert_eq!(ItemId::resolve("W-"), None);
        assert_eq!(ItemId::resolve("abc"), None);
        assert_eq!(ItemId::resolve("+42"), None);
        assert_eq!(ItemId::resolve("W-+42"), None);
    }

    #[test]
    fn dir_name_is_zero_padded() {
        assert_eq!(ItemId(7).dir_name(), "W-007");
        assert_eq!(ItemId(7).branch(), "devh/W-007");
    }
}
