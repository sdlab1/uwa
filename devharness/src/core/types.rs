use std::path::PathBuf;

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

#[derive(Debug, Clone)]
pub struct MutateCfg {
    pub tool: String,
    pub scope: String,
    pub min_kill: u32,
    /// P7-2.3: ранний выход — DOOM (kill-rate недостижим) / SAMPLE
    /// (rule-of-three при --shuffle). По умолчанию выключен.
    pub early: bool,
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
    /// Глобальный allowlist FREEZE (пробел-разделённые глобы), поверх
    /// per-item items/W-NNN/files из спеки.
    pub freeze_allow: Vec<String>,
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
    pub msg_deny: Option<String>,
    pub rules: Option<PathBuf>,
    pub rules_log: Option<PathBuf>,
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
            wt_root: PathBuf::from("/tmp"),
            wt_reuse: false,
            log_days: 30,
            mutate: None,
            red_mutate: None,
            red_expect: None,
            crap: None,
            bench: None,
            no_ignored: true,
            ignored_allowlist: Vec::new(),
            freeze: false,
            freeze_allow: Vec::new(),
            lint: true,
            lint_cmd: String::new(),
            review: true,
            review_diff_max: 2000,
            autonomy: true,
            corpus_fresh_days: crate::core::escapes::CORPUS_FRESH_DAYS,
            msg_deny: None,
            rules: None,
            rules_log: None,
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
